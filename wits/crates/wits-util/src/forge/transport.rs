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

/// Send a request with retry on transient failures (429, 502, 503). Returns
/// the raw `ureq::Response` on success so callers can read headers before
/// consuming the body.
///
/// Exponential backoff (1s → 2s → 4s, capped at 30s) with `Retry-After`
/// honoured. All forge mutations here are either idempotent or additive, so
/// a retry on a definitive server response never creates duplicate side
/// effects.
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
            Err(ureq::Error::Status(code, r)) if is_retryable(code) && attempt < max_retries => {
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

/// Whether a status code warrants a retry. Only codes where the server
/// explicitly signals a transient condition are included — 4xx errors (other
/// than 429) are permanent and retrying them wastes time.
fn is_retryable(code: u16) -> bool {
    matches!(code, 429 | 502 | 503)
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
    mut parse_page: impl FnMut(&Value, &[(String, String)]) -> (Vec<T>, Option<String>),
) -> anyhow::Result<Vec<T>> {
    let mut all = Vec::new();
    let mut url = Some(initial_url.to_owned());
    let mut pages = 0;
    while let Some(u) = url {
        if pages >= max_pages {
            break;
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
    Ok(all)
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

    #[test]
    fn encode_path_preserves_slashes() {
        assert_eq!(encode_path("src/a b.c"), "src/a%20b.c");
        assert_eq!(encode_path("plain.rs"), "plain.rs");
        assert_eq!(encode_path("d/e/f.txt"), "d/e/f.txt");
    }
}
