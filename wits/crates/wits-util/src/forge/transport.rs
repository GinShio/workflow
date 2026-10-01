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

/// Send a request with retry on transient failures. Returns the raw
/// `ureq::Response` on success so callers can read headers before consuming the
/// body.
///
/// Exponential backoff (1s → 2s → 4s, capped at 30s) with `Retry-After`
/// honoured. *Which* failures are retried depends on the method — see
/// [`is_retryable`], where the duplicate-submission hazard is decided.
fn send_with_retry(
    method: &str,
    url: &str,
    auth: &Auth,
    body: Option<&Value>,
) -> anyhow::Result<ureq::Response> {
    let max_retries = 3u32;
    let mut backoff_ms = 1000u64;

    for attempt in 0..=max_retries {
        let req = apply_auth(
            ureq::request(method, url)
                .set("Accept", "application/json")
                .set("User-Agent", USER_AGENT),
            auth,
        );

        let response = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };

        match response {
            Ok(r) => return Ok(r),
            Err(ureq::Error::Status(code, r))
                if is_retryable(code, method) && attempt < max_retries =>
            {
                let wait_ms = r
                    .header("retry-after")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|secs| secs * 1000)
                    .unwrap_or(backoff_ms);
                log::info!(
                    "HTTP {code} from {url}, retrying in {wait_ms}ms \
                     (attempt {}/{max_retries})",
                    attempt + 1,
                );
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                anyhow::bail!("HTTP {code}: {}", detail.trim());
            }
            Err(e) => return Err(anyhow::anyhow!("request to {url} failed: {e}")),
        }
    }
    unreachable!("retry loop must return before exhausting attempts")
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

/// Whether a `method` request that failed with `code` may be sent again.
///
/// The dividing line is whether the server has *told* us it did not act. A 429
/// is a refusal by the rate limiter before the request reached anything that
/// could commit (RFC 6585 §4) — which is why `Retry-After` is defined on it — so
/// replaying it cannot duplicate an effect. A 502/503 from a gateway is ambiguous
/// in exactly the wrong way: the upstream may have committed the write and only
/// the response was lost. Replaying a state-changing request there is how one
/// `stack submit` opens two merge requests. GET is the only safe method this
/// transport sends (RFC 9110 §9.2.1), so it is the only one a gateway error may
/// be replayed on.
///
/// Keyed on the method rather than the endpoint because the backends send reads
/// and mutations through the same POST — GitHub's GraphQL queries included, which
/// therefore give up gateway retries. That is the accepted cost of not making
/// every call site declare its own idempotency; note it also means a *bodyless*
/// POST (GitLab's approve/unapprove) is treated as unsafe even though it happens
/// to be idempotent.
fn is_retryable(code: u16, method: &str) -> bool {
    match code {
        429 => true,
        502 | 503 => method.eq_ignore_ascii_case("GET"),
        _ => false,
    }
}

/// DELETE a resource, treating **404 as success** (already gone). Deferred
/// cleanup re-deletes ids a prior attempt recorded; by the time it runs the
/// object may have been removed already, or published (so it is no longer a
/// draft) — either way it is "gone", which is exactly what the cleanup wanted.
/// A real failure (auth, 5xx, network) is still an error, so a caller that must
/// confirm the id is gone before proceeding (GitLab) can abort on it.
pub(crate) fn delete_idempotent(url: &str, auth: &Auth) -> anyhow::Result<()> {
    match apply_auth(ureq::request("DELETE", url), auth).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => Ok(()),
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            anyhow::bail!("HTTP {code}: {}", detail.trim())
        }
        Err(e) => Err(anyhow::anyhow!("DELETE {url} failed: {e}")),
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
        let headers: Vec<(String, String)> = response
            .headers_names()
            .into_iter()
            .filter_map(|name| {
                response
                    .header(&name)
                    .map(|value| (name.to_lowercase(), value.to_owned()))
            })
            .collect();
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

    /// A gateway error leaves it unknown whether the upstream committed, so only
    /// GET may be replayed on one. Retrying a create-MR POST there is what opens
    /// the same merge request twice.
    #[test]
    fn a_gateway_error_is_replayed_only_for_reads() {
        for code in [502, 503] {
            assert!(is_retryable(code, "GET"), "{code} on GET");
            assert!(!is_retryable(code, "POST"), "{code} on POST");
            assert!(!is_retryable(code, "PATCH"), "{code} on PATCH");
            assert!(!is_retryable(code, "PUT"), "{code} on PUT");
        }
    }

    /// A rate limiter refuses before anything can commit, so every method is
    /// safe to replay — that is the whole point of `Retry-After`.
    #[test]
    fn a_rate_limit_is_replayed_for_every_method() {
        for method in ["GET", "POST", "PATCH", "PUT"] {
            assert!(is_retryable(429, method), "429 on {method}");
        }
    }

    /// Anything the server answered definitively stays answered; replaying it
    /// only wastes time.
    #[test]
    fn a_definitive_status_is_never_replayed() {
        for code in [400, 401, 403, 404, 409, 422, 500] {
            assert!(!is_retryable(code, "GET"), "{code} on GET");
            assert!(!is_retryable(code, "POST"), "{code} on POST");
        }
    }
}
