//! Shared HTTP/JSON plumbing the host backends build on.
//!
//! Every backend — GitHub's GraphQL, GitLab's and Gitea's REST — sends requests
//! the same way: one credential header per platform, retry on the handful of
//! transient statuses, and the platform's own error body surfaced on failure
//! (that text explains *why* far better than a bare status code). Keeping that
//! here means the trait, the normalized types, and the per-platform mapping in
//! [`super`] never touch `ureq` directly; adding a backend is a mapping exercise
//! over these primitives, not a fresh HTTP client.
//!
//! Transport is plain blocking REST (`ureq`), which keeps every platform on the
//! same footing and avoids a dependency on whatever CLI the user did or didn't
//! install. Whether a mutation may happen at all (dry-run) is decided at the
//! orchestration layer; by the time a primitive here is called, it calls the
//! network.

use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How a platform expects credentials presented. The differences are small but
/// real: GitHub takes a bearer/token header, GitLab a `PRIVATE-TOKEN`.
#[derive(Debug, Clone)]
pub(crate) enum Auth {
    /// `Authorization: Bearer <t>` — GitHub accepts this for every token kind.
    Bearer(String),
    /// `Authorization: token <t>` — what Gitea/Forgejo personal tokens expect.
    Token(String),
    /// `PRIVATE-TOKEN: <t>` — GitLab's own header.
    PrivateToken(String),
}

/// The `User-Agent` every forge request carries. One honest identity for the
/// whole tool (`stack` and `review` share this transport), version-stamped.
const USER_AGENT: &str = concat!("wits/", env!("CARGO_PKG_VERSION"));

/// The literal a caller passes for "the authenticated user".
pub(crate) const SELF_REF: &str = "@me";

/// The longest a refused request waits before it is sent again. A server asking
/// for longer is describing a quota that refills on its own clock — GitHub's
/// hourly limit, Codeberg's ten-minute window — which a command reports rather
/// than sleeps through.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// A request the server answered with a status that is not success: the code,
/// for a caller that treats one as an answer (a `404` that means "already
/// gone"), and the platform's own message, which explains *why* far better than
/// the code does.
#[derive(Debug)]
pub(crate) struct HttpStatus {
    pub code: u16,
    pub detail: String,
}

impl fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {}: {}", self.code, self.detail)
    }
}

impl std::error::Error for HttpStatus {}

/// The status a failed request was answered with, when it got that far.
pub(crate) fn status_of(error: &anyhow::Error) -> Option<u16> {
    error.downcast_ref::<HttpStatus>().map(|s| s.code)
}

/// The one HTTP client of the process: connections stay alive across the many
/// requests a run makes, and every request carries the same limits.
///
/// Redirects are not followed. On a 301–303 `ureq` replays a POST or PATCH as a
/// GET without its body, and drops the credentials on the way, so a write to a
/// renamed repository would come back `200` having done nothing. A forge
/// answers for a moved repository with a redirect (Gitea and Forgejo on every
/// method, GitLab on reads), and [`send_with_retry`] reports it instead.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            // Far longer than a forge takes to start answering, so only a
            // stalled connection trips it; without it, one hangs the run.
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(60))
            .redirects(0)
            .user_agent(USER_AGENT)
            .build()
    })
}

/// Send a request, retrying the failures [`retry_wait`] allows. Returns the
/// raw `ureq::Response` on success so callers can read headers before consuming
/// the body; a failure is an [`HttpStatus`] once the server answered.
///
/// A retry waits as long as the server asks (`Retry-After`, or until a spent
/// quota resets), else backs off exponentially (1s → 2s → 4s); a server asking
/// for more than [`MAX_WAIT`] gets an error saying so instead.
fn send_with_retry(
    method: &str,
    url: &str,
    auth: &Auth,
    body: Option<&Value>,
) -> anyhow::Result<ureq::Response> {
    let max_retries = 3u32;
    let mut backoff = Duration::from_secs(1);

    for attempt in 0..=max_retries {
        let req = apply_auth(
            agent()
                .request(method, url)
                .set("Accept", "application/json"),
            auth,
        );

        let response = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };

        let r = match response {
            Ok(r) if (300..400).contains(&r.status()) => {
                return Err(moved(r.status(), r.header("location")).into())
            }
            Ok(r) => return Ok(r),
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(anyhow::anyhow!("request to {url} failed: {e}")),
        };
        let code = r.status();
        let headers = headers_of(&r);
        let detail = r.into_string().unwrap_or_default().trim().to_owned();
        let Some(wait) = retry_wait(code, method, &headers, &detail, backoff, unix_now())
            .filter(|_| attempt < max_retries)
        else {
            return Err(HttpStatus { code, detail }.into());
        };
        if wait > MAX_WAIT {
            let detail = format!(
                "{detail} (the server asks to wait {}s before retrying, longer than the {}s \
                 wits waits; try again later)",
                wait.as_secs(),
                MAX_WAIT.as_secs()
            );
            return Err(HttpStatus { code, detail }.into());
        }
        log::info!(
            "HTTP {code} from {url}, retrying in {}s (attempt {}/{max_retries})",
            wait.as_secs_f32(),
            attempt + 1,
        );
        std::thread::sleep(wait);
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
    unreachable!("retry loop must return before exhausting attempts")
}

/// The error for a redirect — for the requests made here, the sign of a
/// repository that moved (see [`agent`]).
fn moved(code: u16, location: Option<&str>) -> HttpStatus {
    let detail = match location {
        Some(to) => format!(
            "moved to {to}; the repository was probably renamed or transferred, so point the \
             remote at its new URL"
        ),
        None => "redirected without a location".to_owned(),
    };
    HttpStatus { code, detail }
}

/// A response's headers, names lowercased.
fn headers_of(response: &ureq::Response) -> Vec<(String, String)> {
    response
        .headers_names()
        .into_iter()
        .filter_map(|name| {
            response
                .header(&name)
                .map(|value| (name.to_lowercase(), value.to_owned()))
        })
        .collect()
}

/// The value of header `name` (lowercase) in `headers`.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.trim())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Attach the platform's credential header to a request. The differences are
/// small but real (see [`Auth`]); centralised so every code path presents
/// credentials identically.
fn apply_auth(req: ureq::Request, auth: &Auth) -> ureq::Request {
    match auth {
        Auth::Bearer(t) => req.set("Authorization", &format!("Bearer {t}")),
        Auth::Token(t) => req.set("Authorization", &format!("token {t}")),
        Auth::PrivateToken(t) => req.set("PRIVATE-TOKEN", t),
    }
}

/// How long to wait before sending a `method` request that failed with `code`
/// again, or `None` when it must not be sent again. `backoff` is the wait when
/// the server names none.
///
/// The dividing line is whether the server has *told* us it did not act. A
/// rate limiter refuses before the request reaches anything that could commit
/// (RFC 6585 §4), so replaying it cannot duplicate an effect, whatever the
/// method. A gateway error (502, 503, 504) or a timeout (408) is ambiguous in
/// exactly the wrong way: the upstream may have committed the write and only
/// the response was lost. Replaying a state-changing request there is how one
/// `stack submit` opens two merge requests. GET is the only safe method this
/// transport sends (RFC 9110 §9.2.1), so it is the only one those may be
/// replayed on.
///
/// Keyed on the method rather than the endpoint because the backends send reads
/// and mutations through the same POST — GitHub's GraphQL queries included, which
/// therefore give up gateway retries. That is the accepted cost of not making
/// every call site declare its own idempotency; note it also means a *bodyless*
/// POST (GitLab's approve/unapprove) is treated as unsafe even though it happens
/// to be idempotent.
fn retry_wait(
    code: u16,
    method: &str,
    headers: &[(String, String)],
    body: &str,
    backoff: Duration,
    now: u64,
) -> Option<Duration> {
    if is_rate_limited(code, headers, body) {
        // GitHub, for a secondary limit that names no time: "wait for at least
        // one minute before retrying" ("Rate limits for the REST API").
        let unnamed = if code == 403 {
            Duration::from_secs(60)
        } else {
            backoff
        };
        return Some(requested_wait(headers, now).unwrap_or(unnamed));
    }
    let transient = matches!(code, 408 | 502 | 503 | 504) && method.eq_ignore_ascii_case("GET");
    transient.then(|| requested_wait(headers, now).unwrap_or(backoff))
}

/// Whether a refusal came from a rate limiter. Every forge uses 429; GitHub
/// also answers its primary and secondary limits with 403 ("Rate limits for the
/// REST API"; its GraphQL API's secondary limit is "200 or 403"). Otherwise a
/// 403 means "not allowed", so it counts only when it says it is a limit, by
/// header or by message.
fn is_rate_limited(code: u16, headers: &[(String, String)], body: &str) -> bool {
    match code {
        429 => true,
        403 => {
            header(headers, "retry-after").is_some()
                || header(headers, "x-ratelimit-remaining") == Some("0")
                || body.to_ascii_lowercase().contains("rate limit")
        }
        _ => false,
    }
}

/// How long the server asks a client to hold off: `Retry-After` in its
/// delay-seconds form (RFC 9110 §10.2.3, which every forge here sends), else,
/// once GitHub's `x-ratelimit-remaining` is spent, until its
/// `x-ratelimit-reset`, a Unix time — plus a second, since the reset is
/// truncated to one.
fn requested_wait(headers: &[(String, String)], now: u64) -> Option<Duration> {
    if let Some(secs) = header(headers, "retry-after").and_then(|v| v.parse::<u64>().ok()) {
        return Some(Duration::from_secs(secs));
    }
    if header(headers, "x-ratelimit-remaining") != Some("0") {
        return None;
    }
    let reset = header(headers, "x-ratelimit-reset")?.parse::<u64>().ok()?;
    Some(Duration::from_secs(reset.saturating_sub(now) + 1))
}

/// Wait out the rate limit `headers` describe, as a refused request would be —
/// for GitHub's GraphQL API, which reports a spent quota as `200 OK` with a
/// `RATE_LIMITED` error. With no time named it waits a minute, GitHub's floor;
/// a wait past [`MAX_WAIT`] is an error instead.
pub(crate) fn wait_out_rate_limit(url: &str, headers: &[(String, String)]) -> anyhow::Result<()> {
    let wait = requested_wait(headers, unix_now()).unwrap_or(Duration::from_secs(60));
    if wait > MAX_WAIT {
        anyhow::bail!(
            "{url}: rate limited for another {}s, longer than the {}s wits waits; try again later",
            wait.as_secs(),
            MAX_WAIT.as_secs()
        );
    }
    log::info!("rate limited by {url}, retrying in {}s", wait.as_secs());
    std::thread::sleep(wait);
    Ok(())
}

/// DELETE a resource, treating **404 as success** (already gone). Deferred
/// cleanup re-deletes ids a prior attempt recorded; by the time it runs the
/// object may have been removed already, or published (so it is no longer a
/// draft) — either way it is "gone", which is exactly what the cleanup wanted.
/// A real failure (auth, 5xx, network) is still an error, so a caller that must
/// confirm the id is gone before proceeding (GitLab) can abort on it.
pub(crate) fn delete_idempotent(url: &str, auth: &Auth) -> anyhow::Result<()> {
    match send_with_retry("DELETE", url, auth, None) {
        Err(e) if status_of(&e) == Some(404) => Ok(()),
        result => result.map(drop),
    }
}

/// Issue one request and decode the JSON reply. A non-2xx status is turned into
/// an error carrying the platform's own message body, because that text is
/// usually the only thing that explains *why* (a stale token, a base that
/// doesn't exist) far better than a bare status code would.
pub(crate) fn request(
    method: &str,
    url: &str,
    auth: &Auth,
    body: Option<&Value>,
) -> anyhow::Result<Value> {
    let r = send_with_retry(method, url, auth, body)?;
    Ok(r.into_json().unwrap_or(Value::Null))
}

/// [`request`], with the response's headers (names lowercased) beside the
/// reply — for a caller that must read a limit off them.
pub(crate) fn request_with_headers(
    method: &str,
    url: &str,
    auth: &Auth,
    body: Option<&Value>,
) -> anyhow::Result<(Value, Vec<(String, String)>)> {
    let r = send_with_retry(method, url, auth, body)?;
    let headers = headers_of(&r);
    Ok((r.into_json().unwrap_or(Value::Null), headers))
}

/// Issue a paginated request, accumulating items across pages. Each backend
/// supplies a parser that extracts items from one page and the next-page URL
/// (parsed from `Link` headers on GitHub, `X-Next-Page` on GitLab, etc.).
///
/// The parser is called once per page; its items are appended to the
/// accumulator before the next page is fetched. A `None` next-URL (or
/// exceeding `max_pages`) terminates the loop.
pub(crate) fn request_paginated<T>(
    initial_url: &str,
    auth: &Auth,
    max_pages: usize,
    parse_page: impl FnMut(&Value, &[(String, String)]) -> (Vec<T>, Option<String>),
) -> anyhow::Result<Vec<T>> {
    Ok(paginate(initial_url, auth, max_pages, parse_page)?.0)
}

/// How many pages [`request_every_page`] reads before giving up. No list a
/// stack asks about — the MRs of one branch, the comments on one MR — comes
/// near it, so reaching it means something is wrong, not that the list is long.
pub(crate) const EVERY_PAGE_LIMIT: usize = 1000;

/// Read a paginated list to its end, for a caller whose answer is only right
/// once every item has been seen — above all "there is none", which a
/// truncated list asserts falsely. So unlike [`request_paginated`] it never
/// answers from part of the list: one longer than [`EVERY_PAGE_LIMIT`] pages is
/// an error.
pub(crate) fn request_every_page<T>(
    initial_url: &str,
    auth: &Auth,
    parse_page: impl FnMut(&Value, &[(String, String)]) -> (Vec<T>, Option<String>),
) -> anyhow::Result<Vec<T>> {
    let (items, truncated) = paginate(initial_url, auth, EVERY_PAGE_LIMIT, parse_page)?;
    if truncated {
        anyhow::bail!(
            "{initial_url}: the list runs past {EVERY_PAGE_LIMIT} pages; refusing to answer from \
             part of it"
        );
    }
    Ok(items)
}

/// The page loop behind [`request_paginated`] and [`request_every_page`]: the
/// items read, and whether a next page was left unread at `max_pages`.
fn paginate<T>(
    initial_url: &str,
    auth: &Auth,
    max_pages: usize,
    mut parse_page: impl FnMut(&Value, &[(String, String)]) -> (Vec<T>, Option<String>),
) -> anyhow::Result<(Vec<T>, bool)> {
    let mut all = Vec::new();
    let mut url = Some(initial_url.to_owned());
    let mut pages = 0;
    while let Some(u) = url {
        if pages >= max_pages {
            return Ok((all, true));
        }
        let response = send_with_retry("GET", &u, auth, None)?;
        let headers = headers_of(&response);
        let v: Value = response.into_json().unwrap_or(Value::Null);
        let (items, next) = parse_page(&v, &headers);
        all.extend(items);
        url = next;
        pages += 1;
    }
    Ok((all, false))
}

/// The `rel="next"` target of an RFC 8288 `Link` header — how Gitea and Forgejo
/// (and GitHub's REST API) point at the following page — or `None` on the last
/// page. `headers` are lowercased, as [`request_paginated`] hands them over.
///
/// Entries are split on `<` rather than `,`: a URL cannot hold an unescaped `<`
/// (RFC 3986), but its query can hold a comma.
pub(crate) fn next_link(headers: &[(String, String)]) -> Option<String> {
    let (_, link) = headers.iter().find(|(name, _)| name == "link")?;
    link.split('<').skip(1).find_map(|entry| {
        let (target, params) = entry.split_once('>')?;
        let rel = params
            .split(';')
            .find_map(|param| param.trim().strip_prefix("rel="))?;
        // A relation may list several types (`rel="next last"`), and the last
        // parameter carries the `,` that separates it from the next entry.
        let rel = rel.trim_end_matches([',', ' ']).trim_matches('"');
        rel.split_whitespace()
            .any(|kind| kind == "next")
            .then(|| target.to_owned())
    })
}

/// Replace any `@me` in `items` with the resolved name. Used by hosts that take
/// usernames (GitHub, Gitea); GitLab resolves to a numeric id separately.
pub(crate) fn resolve_self(items: &[String], me: &str) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            if item == SELF_REF {
                me.to_owned()
            } else {
                item.clone()
            }
        })
        .collect()
}

/// Read one string field off `GET {api_base}/user`. The field name differs by
/// platform (`login` on GitHub/Gitea), so it is passed in.
pub(crate) fn current_user(api_base: &str, auth: &Auth, field: &str) -> anyhow::Result<String> {
    let v = request("GET", &format!("{api_base}/user"), auth, None)?;
    v[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("could not read the authenticated user"))
}

/// Percent-encode a repo-relative path for a blob URL, preserving the `/`
/// separators (each segment is encoded on its own). A path like `dir/a file.c`
/// must survive, but its slashes must stay real path separators.
pub(crate) fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// Percent-encode one URL component. Branch names carry `/`, cross-fork heads
/// carry `:`, GitLab project ids are a whole `group/sub/repo` path — all of which
/// must survive intact inside a query string or path segment.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_self_replaces_only_the_marker() {
        let out = resolve_self(&["@me".into(), "alice".into()], "russell");
        assert_eq!(out, ["russell", "alice"]);
    }

    fn link(value: &str) -> Vec<(String, String)> {
        vec![("link".to_owned(), value.to_owned())]
    }

    #[test]
    fn next_link_follows_the_next_relation_only() {
        // The shape Gitea and Forgejo send on a middle page.
        let headers = link(
            "<https://h/api/v1/repos/o/r/pulls?limit=50&page=3&state=all>; rel=\"next\",\
             <https://h/api/v1/repos/o/r/pulls?limit=50&page=15&state=all>; rel=\"last\",\
             <https://h/api/v1/repos/o/r/pulls?limit=50&page=1&state=all>; rel=\"first\"",
        );
        assert_eq!(
            next_link(&headers).as_deref(),
            Some("https://h/api/v1/repos/o/r/pulls?limit=50&page=3&state=all")
        );
        // The last page links back but not forward.
        let last = link("<https://h/x?page=14>; rel=\"prev\", <https://h/x?page=1>; rel=\"first\"");
        assert_eq!(next_link(&last), None);
        assert_eq!(next_link(&[]), None);
    }

    #[test]
    fn next_link_reads_a_relation_list_and_a_comma_in_the_query() {
        let headers = link("<https://h/x?head=a,b&page=2>; rel=\"next last\"");
        assert_eq!(
            next_link(&headers).as_deref(),
            Some("https://h/x?head=a,b&page=2")
        );
    }

    #[test]
    fn encode_path_preserves_slashes() {
        assert_eq!(encode_path("src/a b.c"), "src/a%20b.c");
        assert_eq!(encode_path("plain.rs"), "plain.rs");
        assert_eq!(encode_path("d/e/f.txt"), "d/e/f.txt");
    }

    const BACKOFF: Duration = Duration::from_secs(1);

    fn wait(code: u16, method: &str, headers: &[(&str, &str)], body: &str) -> Option<Duration> {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        retry_wait(code, method, &headers, body, BACKOFF, 1_000)
    }

    /// A gateway error or a timeout leaves it unknown whether the upstream
    /// committed, so only GET may be replayed on one. Retrying a create-MR POST
    /// there is what opens the same merge request twice.
    #[test]
    fn a_gateway_error_is_replayed_only_for_reads() {
        for code in [408, 502, 503, 504] {
            assert_eq!(wait(code, "GET", &[], ""), Some(BACKOFF), "{code} on GET");
            for method in ["POST", "PATCH", "PUT", "DELETE"] {
                assert_eq!(wait(code, method, &[], ""), None, "{code} on {method}");
            }
        }
    }

    /// A rate limiter refuses before anything can commit, so every method is
    /// safe to replay — that is the whole point of `Retry-After`.
    #[test]
    fn a_rate_limit_is_replayed_for_every_method() {
        for method in ["GET", "POST", "PATCH", "PUT", "DELETE"] {
            assert_eq!(wait(429, method, &[], ""), Some(BACKOFF), "429 on {method}");
        }
    }

    /// Anything the server answered definitively stays answered; replaying it
    /// only wastes time.
    #[test]
    fn a_definitive_status_is_never_replayed() {
        for code in [400, 401, 403, 404, 409, 422, 500] {
            assert_eq!(wait(code, "GET", &[], ""), None, "{code} on GET");
            assert_eq!(wait(code, "POST", &[], ""), None, "{code} on POST");
        }
    }

    /// GitHub refuses with 403 for its limits too; only the signal tells it
    /// from a permission error.
    #[test]
    fn a_403_counts_as_a_limit_only_when_it_says_so() {
        let secondary = "You have exceeded a secondary rate limit.";
        assert_eq!(
            wait(403, "POST", &[], secondary),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            wait(403, "POST", &[("retry-after", "30")], ""),
            Some(Duration::from_secs(30))
        );
        // A spent quota waits for its reset, a second past it.
        let spent = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1010"),
        ];
        assert_eq!(wait(403, "GET", &spent, ""), Some(Duration::from_secs(11)));
        assert_eq!(
            wait(
                403,
                "POST",
                &[("x-ratelimit-remaining", "12")],
                "not allowed"
            ),
            None
        );
    }

    /// The server's own wait wins over the backoff, and is reported however
    /// long, for the caller to refuse.
    #[test]
    fn the_wait_a_server_names_wins() {
        assert_eq!(
            wait(429, "POST", &[("retry-after", "600")], ""),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            wait(503, "GET", &[("retry-after", "5")], ""),
            Some(Duration::from_secs(5))
        );
    }

    /// A redirect is the forge saying the repository moved; following it would
    /// turn a write into a bodiless GET.
    #[test]
    fn a_redirect_names_where_the_repository_went() {
        let err = moved(301, Some("https://h/api/v1/repos/o/renamed/pulls"));
        assert_eq!(err.code, 301);
        assert!(err
            .to_string()
            .contains("moved to https://h/api/v1/repos/o/renamed/pulls"));
        assert!(moved(302, None).to_string().starts_with("HTTP 302: "));
    }

    #[test]
    fn a_failure_keeps_its_status_through_anyhow() {
        let err: anyhow::Error = HttpStatus {
            code: 404,
            detail: "gone".into(),
        }
        .into();
        assert_eq!(status_of(&err), Some(404));
        assert_eq!(err.to_string(), "HTTP 404: gone");
        assert_eq!(status_of(&anyhow::anyhow!("network")), None);
    }
}
