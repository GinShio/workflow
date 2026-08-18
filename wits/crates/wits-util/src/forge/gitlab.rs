//! GitLab merge requests.
//!
//! GitLab is the platform whose vocabulary the rest of the tool borrows ("MR",
//! "!"). Two shape differences matter for the same-project case: a project is
//! addressed by a URL-encoded `group/sub/repo` id, and a draft is a `Draft:`
//! title prefix rather than a field.
//!
//! Cross-project (fork) MRs are the awkward part. Unlike GitHub/Gitea, GitLab has
//! no `owner:branch` head string: an MR from a fork is **created on the source
//! project** carrying a numeric `target_project_id`, but the MR itself then lives
//! in the **target** project (its iid belongs there), so reads and edits address
//! the target. Numeric project ids are required for the `target_project_id` body
//! field and the `source_project_id` list filter, so we resolve them once up
//! front. When source and target are the same project, none of this applies and
//! we stay on the cheap single-project path.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::RemoteInfo;
use super::{
    encode, pick, request, ActionKey, Anchor, Attributes, Auth, BatchAction, BatchOutcome,
    DiffVersion, FeedQuery, Forge, LineRef, MergeRequest, MrDetails, MrState, MrSummary, NewMr,
    RemoteComment, RemoteThread, ReviewBatch, Side, StateFilter, Verdict, SELF_REF,
};

const DRAFT_PREFIX: &str = "Draft: ";

/// Upper bound on concurrent draft-note POSTs in one submission — a large review
/// must not open a connection per comment and trip the rate limiter.
const MAX_DRAFT_PARALLEL: usize = 8;

pub struct GitLab {
    api_base: String,
    /// The web root of the target project (`https://host/group/repo`), for blob
    /// permalinks — distinct from `api_base`.
    web_base: String,
    /// Encoded path of the project where the MR resides — the target. Reads and
    /// edits always go here; for a same-project MR this is also where it's made.
    target_path: String,
    auth: Auth,
    /// Present only for a fork MR (source project differs from target).
    fork: Option<Fork>,
}

/// The extra coordinates a cross-project MR needs, resolved once at construction.
struct Fork {
    source_path: String,
    source_id: u64,
    target_id: u64,
}

impl GitLab {
    pub fn new(
        target: RemoteInfo,
        source: Option<RemoteInfo>,
        token: String,
        api_url_override: Option<String>,
    ) -> anyhow::Result<Self> {
        let api_base =
            api_url_override.unwrap_or_else(|| format!("https://{}/api/v4", target.host));
        let auth = Auth::PrivateToken(token);
        let target_path = encode(&target.project_path());

        // A fork MR only makes sense when the source is a *different* project on
        // the *same* GitLab instance; cross-instance forks don't exist.
        let fork = match source {
            Some(src) if src.host == target.host && src.project_path() != target.project_path() => {
                let source_path = encode(&src.project_path());
                let source_id = project_id(&api_base, &auth, &source_path)?;
                let target_id = project_id(&api_base, &auth, &target_path)?;
                Some(Fork {
                    source_path,
                    source_id,
                    target_id,
                })
            }
            _ => None,
        };

        let web_base = format!("https://{}/{}", target.host, target.project_path());
        Ok(Self {
            api_base,
            web_base,
            target_path,
            auth,
            fork,
        })
    }

    /// Resolve or unresolve a discussion (a separate call — a GitLab draft note
    /// needs a body, so a bare resolve can't ride `bulk_publish`).
    fn resolve_discussion(&self, id: &str, thread: &str, resolved: bool) -> anyhow::Result<()> {
        let url = format!("{}/{id}/discussions/{thread}", self.mrs_url());
        request(
            "PUT",
            &url,
            &self.auth,
            Some(&json!({ "resolved": resolved })),
        )?;
        Ok(())
    }

    /// Record a real MR approval via the dedicated `POST …/approve` endpoint.
    ///
    /// GitLab has **no released public API** to set a reviewer's `reviewed` /
    /// `requested_changes` state: the only mechanism is the `reviewer_state`
    /// parameter on `bulk_publish`, which is still an *unmerged* proposal
    /// (gitlab-org/gitlab!237813) and absent from every shipped release — and
    /// even it routes through `UpdateReviewerStateService`, which is *not* a
    /// formal approval. So the verdicts map onto what the released API actually
    /// exposes: `approve` here, `request-changes` → [`unapprove`](Self::unapprove)
    /// (its concrete released effect — a reviewer requesting changes has their
    /// approval removed), and `comment` is a no-op (leaving notes no longer
    /// changes reviewer state on its own). Returns whether it landed.
    fn approve(&self, id: &str) -> bool {
        let url = format!("{}/{id}/approve", self.mrs_url());
        match request("POST", &url, &self.auth, None) {
            Ok(_) => true,
            Err(e) => {
                log::warn!("MR {id}: approve failed: {e}");
                false
            }
        }
    }

    /// Remove the authenticated user's approval — the released proxy for a
    /// `request-changes` verdict (see [`approve`](Self::approve)). Idempotent:
    /// GitLab answers `404` when there was no approval to remove, which is the
    /// goal state ("not approved"), so it counts as success.
    fn unapprove(&self, id: &str) -> bool {
        let url = format!("{}/{id}/unapprove", self.mrs_url());
        match request("POST", &url, &self.auth, None) {
            Ok(_) => true,
            Err(e) if e.to_string().contains("404") => true,
            Err(e) => {
                log::warn!("MR {id}: unapprove (request-changes) failed: {e}");
                false
            }
        }
    }

    /// Where the MR lives — the endpoint for finding and editing it.
    fn mrs_url(&self) -> String {
        format!(
            "{}/projects/{}/merge_requests",
            self.api_base, self.target_path
        )
    }

    /// Resolve a username (or `@me`) to GitLab's numeric user id, which is what
    /// the `assignee_ids` / `reviewer_ids` fields require.
    fn user_id(&self, name: &str) -> anyhow::Result<Option<u64>> {
        if name == SELF_REF {
            let v = request("GET", &format!("{}/user", self.api_base), &self.auth, None)?;
            return Ok(v["id"].as_u64());
        }
        let url = format!("{}/users?username={}", self.api_base, encode(name));
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(|u| u["id"].as_u64()))
    }

    /// Union the resolved ids of `names` into `ids` (additive, deduped).
    fn add_user_ids(&self, ids: &mut Vec<u64>, names: &[String]) {
        for name in names {
            match self.user_id(name) {
                Ok(Some(uid)) if !ids.contains(&uid) => ids.push(uid),
                Ok(Some(_)) => {}
                Ok(None) => log::warn!("user '{name}' not found"),
                Err(e) => log::warn!("resolving user '{name}': {e}"),
            }
        }
    }

    /// The authenticated user's username, for resolving `@me` in a feed filter.
    fn current_username(&self) -> anyhow::Result<String> {
        let v = request("GET", &format!("{}/user", self.api_base), &self.auth, None)?;
        v["username"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("could not read the authenticated user"))
    }

    /// A feed filter value, with `@me` expanded to the authenticated username.
    fn filter_user(&self, name: &str) -> anyhow::Result<String> {
        if name == SELF_REF {
            self.current_username()
        } else {
            Ok(name.to_owned())
        }
    }

    /// The MR-scoped draft-notes endpoint (always on the target project).
    fn draft_notes_url(&self, id: &str) -> String {
        format!("{}/{id}/draft_notes", self.mrs_url())
    }

    /// The branch's candidate MRs, one fetch, all states. Match by source branch,
    /// never target, so a drifted base is still found. For a fork, the same
    /// branch name can exist in several source projects, so we pin to ours with
    /// `source_project_id`.
    fn candidates(&self, branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let mut url = format!(
            "{}?source_branch={}&state=all",
            self.mrs_url(),
            encode(branch),
        );
        if let Some(fork) = &self.fork {
            url += &format!("&source_project_id={}", fork.source_id);
        }
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v.as_array()
            .map(|arr| arr.iter().filter_map(parse_mr).collect())
            .unwrap_or_default())
    }
}

/// Build the `position` object a GitLab diff note needs. The three version SHAs
/// pin the comment to the exact diff it was written against; when that is behind
/// the MR's current state the note simply lands on that older version.
fn diff_position(
    version: &DiffVersion,
    path: &str,
    old_path: Option<&str>,
    end: LineRef,
    start: Option<LineRef>,
) -> Value {
    let mut pos = file_position(version, path, old_path);
    pos["position_type"] = json!("text");
    match end.side {
        Side::New => pos["new_line"] = json!(end.line),
        Side::Old => pos["old_line"] = json!(end.line),
    }
    if let Some(s) = start {
        pos["line_range"] = json!({
            "start": range_endpoint(path, s),
            "end": range_endpoint(path, end),
        });
    }
    pos
}

/// The `position` object a GitLab *file-level* diff note carries — the same
/// three version SHAs and paths as a line note, but `position_type: "file"` and
/// no line. This is what makes a file comment anchored to the file (so
/// `list_threads` reads it back as `position_type:"file"`, and `show --file`
/// finds it); a plain position-less note would degrade to an MR-level remark.
fn file_position(version: &DiffVersion, path: &str, old_path: Option<&str>) -> Value {
    json!({
        "position_type": "file",
        "base_sha": version.base_sha,
        "start_sha": version.start_sha,
        "head_sha": version.head_sha,
        "new_path": path,
        "old_path": old_path.unwrap_or(path),
    })
}

/// One endpoint of a GitLab `position.line_range`: the side as `type`, the
/// side-appropriate line, and — crucially — a `line_code`. GitLab **rejects** a
/// `line_range` endpoint without one (`400 … line_code can't be blank`), so a
/// multi-line note is impossible without it; we compute it rather than omit it.
fn range_endpoint(path: &str, r: LineRef) -> Value {
    let (old, new) = match r.side {
        Side::New => (0, r.line),
        Side::Old => (r.line, 0),
    };
    let mut o = json!({
        "type": match r.side { Side::New => "new", Side::Old => "old" },
        "line_code": line_code(path, old, new),
    });
    match r.side {
        Side::New => o["new_line"] = json!(r.line),
        Side::Old => o["old_line"] = json!(r.line),
    }
    o
}

/// GitLab's `line_code` for a diff line: `SHA1(file_path)_<old_line>_<new_line>`,
/// with `0` for the side the line does not exist on (a pure addition is
/// `hash_0_N`, a deletion `hash_N_0`). The hash is over the *new* path (the file
/// identifier GitLab keys line codes by). Required by `line_range`; see
/// [`range_endpoint`].
fn line_code(path: &str, old_line: u32, new_line: u32) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(path.as_bytes());
    format!("{:x}_{old_line}_{new_line}", hasher.finalize())
}

/// Read one `line_range` endpoint back into a [`LineRef`]. `type` selects the
/// side; the matching `new_line`/`old_line` gives the line.
fn parse_range_endpoint(v: &Value) -> Option<LineRef> {
    let side = match v["type"].as_str() {
        Some("old") => Side::Old,
        Some("new") => Side::New,
        _ => return None,
    };
    let line = v[if side == Side::New {
        "new_line"
    } else {
        "old_line"
    }]
    .as_u64()
    .map(|l| l as u32)
    // A side endpoint on a line that only exists on the other side (e.g. a
    // pure addition viewed from the old side) carries no line for that side;
    // fall back to whichever the object does carry so the span round-trips.
    .or_else(|| v["new_line"].as_u64().map(|l| l as u32))
    .or_else(|| v["old_line"].as_u64().map(|l| l as u32))?;
    Some(LineRef { line, side })
}

/// Draft-note ids as the forge-neutral string tokens carried in
/// [`BatchOutcome::inflight`] / [`ReviewBatch::stale`].
fn u64_ids(ids: &[u64]) -> Vec<String> {
    ids.iter().map(u64::to_string).collect()
}

/// Numeric ids of the users currently in `field` (`assignees`/`reviewers`) on an
/// MR JSON object — the base for an additive update.
fn current_user_ids(mr: &Value, field: &str) -> Vec<u64> {
    mr[field]
        .as_array()
        .map(|arr| arr.iter().filter_map(|u| u["id"].as_u64()).collect())
        .unwrap_or_default()
}

/// The next-page URL for a GitLab list response, read from the `X-Next-Page`
/// header (GitLab paginates by page number). An absent or empty header means
/// this was the last page. The page number is appended to `base`, which already
/// carries the full query string, so each page re-derives from the same base.
fn next_page(base: &str, headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name == "x-next-page")
        .and_then(|(_, value)| {
            let v = value.trim();
            (!v.is_empty()).then(|| format!("{base}&page={v}"))
        })
}

/// Resolve a project's numeric id from its URL-encoded path. GitLab accepts the
/// path for addressing, but the `target_project_id`/`source_project_id` fields
/// insist on the numeric form, so a fork MR needs this lookup.
fn project_id(api_base: &str, auth: &Auth, encoded_path: &str) -> anyhow::Result<u64> {
    let url = format!("{api_base}/projects/{encoded_path}");
    let v = request("GET", &url, auth, None)?;
    v["id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("could not read numeric id of project from {url}"))
}

fn parse_summary(v: &Value) -> Option<MrSummary> {
    let iid = v["iid"].as_u64()?;
    let state = match v["state"].as_str().unwrap_or("opened") {
        "merged" => MrState::Merged,
        "opened" | "locked" => MrState::Open,
        _ => MrState::Closed,
    };
    let draft = v["draft"]
        .as_bool()
        .or_else(|| v["work_in_progress"].as_bool())
        .unwrap_or(false);
    let labels = v["labels"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(MrSummary {
        id: iid.to_string(),
        display: format!("!{iid}"),
        state,
        draft,
        title: v["title"].as_str().unwrap_or_default().to_owned(),
        author: v["author"]["username"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        base: v["target_branch"].as_str().unwrap_or_default().to_owned(),
        source: v["source_branch"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["sha"].as_str().map(str::to_owned),
        updated_at: v["updated_at"].as_str().unwrap_or_default().to_owned(),
        labels,
        web_url: v["web_url"].as_str().unwrap_or_default().to_owned(),
    })
}

fn parse_note(n: &Value) -> RemoteComment {
    RemoteComment {
        id: n["id"].as_u64().unwrap_or(0).to_string(),
        author: n["author"]["username"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        body: n["body"].as_str().unwrap_or_default().to_owned(),
        created_at: n["created_at"].as_str().unwrap_or_default().to_owned(),
    }
}

fn parse_mr(v: &Value) -> Option<MergeRequest> {
    let iid = v["iid"].as_u64()?;
    let state = match v["state"].as_str().unwrap_or("opened") {
        "merged" => MrState::Merged,
        "opened" | "locked" => MrState::Open,
        _ => MrState::Closed,
    };
    Some(MergeRequest {
        id: iid.to_string(),
        display: format!("!{iid}"),
        state,
        base: v["target_branch"].as_str().unwrap_or_default().to_owned(),
        source: v["source_branch"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["sha"].as_str().map(str::to_owned),
        body: v["description"].as_str().unwrap_or_default().to_owned(),
        web_url: v["web_url"].as_str().unwrap_or_default().to_owned(),
    })
}

impl Forge for GitLab {
    fn noun(&self) -> &'static str {
        "MR"
    }

    fn find(&self, branch: &str, state: StateFilter) -> anyhow::Result<Option<MergeRequest>> {
        Ok(pick(&self.candidates(branch)?, state))
    }

    fn find_any(&self, branch: &str) -> anyhow::Result<Option<MergeRequest>> {
        Ok(super::pick_any(&self.candidates(branch)?))
    }

    fn find_children(&self, base_branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let url = format!(
            "{}?target_branch={}&state=opened",
            self.mrs_url(),
            encode(base_branch),
        );
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v.as_array()
            .map(|arr| arr.iter().filter_map(parse_mr).collect())
            .unwrap_or_default())
    }

    fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest> {
        let title = if req.draft {
            format!("{DRAFT_PREFIX}{}", req.title)
        } else {
            req.title.clone()
        };
        let mut body = json!({
            "source_branch": req.branch,
            "target_branch": req.base,
            "title": title,
            "description": req.body,
            "remove_source_branch": true,
        });

        // A fork MR is created on the source project, pointing at the target by
        // numeric id; a same-project MR is created where it lives.
        let url = match &self.fork {
            Some(fork) => {
                body["target_project_id"] = json!(fork.target_id);
                format!(
                    "{}/projects/{}/merge_requests",
                    self.api_base, fork.source_path
                )
            }
            None => self.mrs_url(),
        };

        let v = request("POST", &url, &self.auth, Some(&body))?;
        parse_mr(&v).ok_or_else(|| anyhow::anyhow!("unexpected create response: {v}"))
    }

    fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.mrs_url(), id);
        request(
            "PUT",
            &url,
            &self.auth,
            Some(&json!({ "target_branch": base })),
        )?;
        Ok(())
    }

    fn set_body(&self, id: &str, body: &str) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.mrs_url(), id);
        request(
            "PUT",
            &url,
            &self.auth,
            Some(&json!({ "description": body })),
        )?;
        Ok(())
    }

    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> anyhow::Result<()> {
        let mut body = serde_json::Map::new();

        // Labels have a native additive verb; users do not, so we read the
        // current ids and union ours in, then PUT the full set.
        if !attrs.labels.is_empty() {
            body.insert("add_labels".into(), json!(attrs.labels.join(",")));
        }
        if !attrs.assignees.is_empty() || !attrs.reviewers.is_empty() {
            let mr = request(
                "GET",
                &format!("{}/{}", self.mrs_url(), id),
                &self.auth,
                None,
            )?;
            if !attrs.assignees.is_empty() {
                let mut ids = current_user_ids(&mr, "assignees");
                self.add_user_ids(&mut ids, &attrs.assignees);
                body.insert("assignee_ids".into(), json!(ids));
            }
            if !attrs.reviewers.is_empty() {
                let mut ids = current_user_ids(&mr, "reviewers");
                self.add_user_ids(&mut ids, &attrs.reviewers);
                body.insert("reviewer_ids".into(), json!(ids));
            }
        }

        if body.is_empty() {
            return Ok(());
        }
        let url = format!("{}/{}", self.mrs_url(), id);
        request("PUT", &url, &self.auth, Some(&Value::Object(body)))?;
        Ok(())
    }

    fn list_mrs(&self, query: &FeedQuery) -> anyhow::Result<Vec<MrSummary>> {
        let per_page = query.limit.clamp(1, 100);
        let mut url = format!(
            "{}?state=opened&order_by=updated_at&sort=desc&per_page={per_page}",
            self.mrs_url()
        );
        // Drafts are open MRs; the boolean `draft` filter narrows within that.
        // (`draft` superseded the string `wip` in GitLab 19.0, which we target.)
        match (query.states.open, query.states.draft) {
            (true, false) => url += "&draft=false",
            (false, true) => url += "&draft=true",
            _ => {}
        }
        // Multiple labels are AND on GitLab too (all must be present).
        if !query.labels.is_empty() {
            url += &format!("&labels={}", encode(&query.labels.join(",")));
        }
        if !query.exclude_labels.is_empty() {
            url += &format!("&not[labels]={}", encode(&query.exclude_labels.join(",")));
        }
        if let Some(a) = &query.author {
            url += &format!("&author_username={}", encode(&self.filter_user(a)?));
        }
        if let Some(a) = &query.assignee {
            url += &format!("&assignee_username={}", encode(&self.filter_user(a)?));
        }
        if let Some(r) = &query.reviewer {
            url += &format!("&reviewer_username={}", encode(&self.filter_user(r)?));
        }
        if let Some(s) = &query.search {
            url += &format!("&search={}", encode(s));
        }
        let limit = query.limit;
        let mut collected = 0usize;
        let mut mrs = super::request_paginated(&url, &self.auth, 10, |v, headers| {
            if collected >= limit {
                return (Vec::new(), None);
            }
            let items: Vec<MrSummary> = v
                .as_array()
                .map(|arr| arr.iter().filter_map(parse_summary).collect())
                .unwrap_or_default();
            collected += items.len();
            let next = if collected >= limit {
                None
            } else {
                next_page(&url, headers)
            };
            (items, next)
        })?;
        // A hard cap: the final page can overshoot `limit`, so truncate.
        mrs.truncate(limit);
        Ok(mrs)
    }

    fn mr_details(&self, id: &str) -> anyhow::Result<MrDetails> {
        let v = request("GET", &format!("{}/{id}", self.mrs_url()), &self.auth, None)?;
        let summary =
            parse_summary(&v).ok_or_else(|| anyhow::anyhow!("unexpected MR response: {v}"))?;
        let dr = &v["diff_refs"];
        let version = DiffVersion {
            base_sha: dr["base_sha"].as_str().unwrap_or_default().to_owned(),
            start_sha: dr["start_sha"].as_str().unwrap_or_default().to_owned(),
            head_sha: dr["head_sha"].as_str().unwrap_or_default().to_owned(),
        };
        Ok(MrDetails { summary, version })
    }

    fn mr_ref(&self, id: &str) -> anyhow::Result<String> {
        Ok(format!("refs/merge-requests/{id}/head"))
    }

    fn list_threads(&self, id: &str) -> anyhow::Result<Vec<RemoteThread>> {
        let url = format!("{}/{id}/discussions?per_page=100", self.mrs_url());
        let discussions: Vec<Value> =
            super::request_paginated(&url, &self.auth, 10, |v, headers| {
                let items: Vec<Value> = v.as_array().map(|arr| arr.to_vec()).unwrap_or_default();
                (items, next_page(&url, headers))
            })?;

        let mut threads = Vec::new();
        for d in &discussions {
            let notes = d["notes"].as_array().cloned().unwrap_or_default();
            // System notes (label/state events) are not review discussion.
            let real: Vec<&Value> = notes
                .iter()
                .filter(|n| !n["system"].as_bool().unwrap_or(false))
                .collect();
            let Some(first) = real.first() else {
                continue;
            };
            let (anchor, commit) = if first["position"].is_object() {
                let p = &first["position"];
                let path = p["new_path"]
                    .as_str()
                    .or_else(|| p["old_path"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                let old_path = p["old_path"].as_str().map(str::to_owned);
                let commit = p["head_sha"].as_str().map(str::to_owned);
                // `position_type: "file"` is a file-level anchor; a `line_range`
                // is a multi-line span; otherwise a single-line note carries
                // `new_line` or `old_line`. (A position with neither line nor
                // range nor `file` type is a degenerate note we surface as a
                // line-0 anchor rather than dropping.)
                let anchor = if p["position_type"].as_str() == Some("file") {
                    Anchor::File { path }
                } else if let Some(lr) = p["line_range"].as_object() {
                    let end = parse_range_endpoint(&lr["end"]).unwrap_or(LineRef {
                        line: 0,
                        side: Side::New,
                    });
                    let start = parse_range_endpoint(&lr["start"]);
                    Anchor::Line {
                        path,
                        old_path,
                        end,
                        start,
                    }
                } else {
                    let (side, line) = if let Some(l) = p["new_line"].as_u64() {
                        (Side::New, l as u32)
                    } else if let Some(l) = p["old_line"].as_u64() {
                        (Side::Old, l as u32)
                    } else {
                        (Side::New, 0)
                    };
                    Anchor::Line {
                        path,
                        old_path,
                        end: LineRef { line, side },
                        start: None,
                    }
                };
                (Some(anchor), commit)
            } else {
                (None, None)
            };
            let resolvable: Vec<&&Value> = real
                .iter()
                .filter(|n| n["resolvable"].as_bool().unwrap_or(false))
                .collect();
            let resolved = !resolvable.is_empty()
                && resolvable
                    .iter()
                    .all(|n| n["resolved"].as_bool().unwrap_or(false));
            threads.push(RemoteThread {
                id: d["id"].as_str().unwrap_or_default().to_owned(),
                resolved,
                // GitLab exposes no cheap per-note outdated flag; left false in
                // v1 (see the review docs' capability matrix). Local outdate
                // computation (design.md §6) supersedes this.
                outdated: false,
                anchor,
                commit,
                comments: real.iter().map(|&n| parse_note(n)).collect(),
            });
        }
        Ok(threads)
    }

    fn submit(&self, id: &str, batch: &ReviewBatch) -> anyhow::Result<BatchOutcome> {
        let draft_url = self.draft_notes_url(id);

        // Pre-flight (deferred cleanup): delete the draft notes a *prior* failed
        // attempt left unpublished, before doing anything else. This must succeed
        // (404 = already gone) — an undeleted orphan would be swept into this
        // run's `bulk_publish` and duplicated — so on a real delete failure we
        // abort and keep the ids for the next attempt. We only ever delete ids we
        // ourselves recorded, so a draft the user wrote by hand is never touched.
        for did in &batch.stale {
            if let Err(e) = super::delete_idempotent(&format!("{draft_url}/{did}"), &self.auth) {
                log::warn!("MR {id}: could not clear stale draft {did} ({e}); deferring");
                return Ok(BatchOutcome::none_landed(batch, batch.stale.clone()));
            }
        }
        if batch.is_empty() {
            return Ok(BatchOutcome::default());
        }

        // GitLab's native batch is draft notes + `bulk_publish`: comments
        // (line / file / MR-level), replies, and the summary all become draft
        // notes, published together as one review — one notification. That
        // two-phase shape puts atomicity on us, so it is all-or-nothing *per
        // attempt*: any draft failure aborts the publish and defers cleanup of
        // what posted, for a clean retry (no orphans, no duplicates). Resolves
        // are not draft notes (a draft needs a body), so they are separate PUTs
        // (phase 3), and the verdict is a separate call (phase 2b) — the released
        // `bulk_publish` takes no body (see below).

        // --- Phase 1: a draft note per comment / reply action, plus the summary
        // as a position-less draft note so it publishes with the batch. The
        // `String` label is for logging only; landing is reconciled by key from
        // `batch.actions` below (the summary is not an action). ---
        let mut reqs: Vec<(String, Value)> = Vec::new();
        for a in &batch.actions {
            match a {
                BatchAction::Comment {
                    key,
                    anchor,
                    version,
                    body,
                } => {
                    // Each comment anchors to its own snapshot version (resolved
                    // at build time) — the heart of cross-snapshot drafting.
                    let b = match anchor {
                        Some(Anchor::Line {
                            path,
                            old_path,
                            end,
                            start,
                        }) => json!({
                            "note": body,
                            "position": diff_position(version, path, old_path.as_deref(), *end, *start),
                        }),
                        Some(Anchor::File { path }) => json!({
                            "note": body,
                            "position": file_position(version, path, None),
                        }),
                        None => json!({ "note": body }),
                    };
                    reqs.push((format!("action {key}"), b));
                }
                BatchAction::Reply { key, thread, body } => {
                    reqs.push((
                        format!("action {key}"),
                        json!({ "note": body, "in_reply_to_discussion_id": thread }),
                    ));
                }
                BatchAction::Resolve { .. } => {}
            }
        }
        // The summary body: GitLab has no released `bulk_publish` `note` param
        // (that is the unmerged gitlab-org/gitlab!237813), so it rides as an
        // ordinary position-less draft note and publishes with the rest.
        if let Some(summary) = &batch.summary {
            reqs.push(("summary".to_owned(), json!({ "note": summary })));
        }

        // POST the draft notes in bounded-parallel batches. A cap keeps a big
        // review from opening a connection per note.
        let posted: Mutex<Vec<u64>> = Mutex::new(Vec::new());
        let ok = Mutex::new(true);
        let draft_url_ref = &draft_url;
        let auth = &self.auth;
        let posted_ref = &posted;
        let ok_ref = &ok;
        for chunk in reqs.chunks(MAX_DRAFT_PARALLEL) {
            std::thread::scope(|scope| {
                for (label, body) in chunk {
                    scope.spawn(
                        move || match request("POST", draft_url_ref, auth, Some(body)) {
                            Ok(v) => {
                                if let Some(did) = v["id"].as_u64() {
                                    posted_ref.lock().unwrap().push(did);
                                }
                            }
                            Err(e) => {
                                log::warn!("MR {id}: draft note ({label}) failed: {e}");
                                *ok_ref.lock().unwrap() = false;
                            }
                        },
                    );
                }
            });
        }
        let drafts_all_ok = ok.into_inner().unwrap();
        let posted_ids: Vec<u64> = posted.into_inner().unwrap();

        // A draft failed → nothing lands. Defer cleanup: keep the posted-but-
        // unpublished draft ids so the *next* attempt deletes them first. We do
        // *not* delete now — a this-attempt delete that itself failed would leave
        // an orphan that a later `bulk_publish` republishes; deferring makes
        // cleanup idempotent and eventually-consistent.
        if !drafts_all_ok {
            log::warn!(
                "MR {id}: a draft note failed; deferring cleanup of {} posted draft(s)",
                posted_ids.len()
            );
            return Ok(BatchOutcome::none_landed(batch, u64_ids(&posted_ids)));
        }

        // --- Phase 2: one `bulk_publish` publishes ALL of the user's pending
        // drafts on the MR as a single review — one notification. The released
        // endpoint takes **no body** (the `note`/`reviewer_state` params are the
        // unmerged !237813, absent from every shipped release), so the summary
        // rode as a draft note in phase 1 and the verdict is a separate call
        // (phase 2b). The summary therefore lands exactly when the publish does. ---
        let has_publishable = !posted_ids.is_empty();

        let mut notifications = 0u32;
        let mut summary_ok = batch.summary.is_none();
        if has_publishable {
            match request(
                "POST",
                &format!("{draft_url}/bulk_publish"),
                &self.auth,
                None,
            ) {
                Ok(_) => {
                    notifications = 1;
                    summary_ok = true;
                }
                Err(e) => {
                    log::warn!(
                        "MR {id}: bulk_publish failed ({e}); deferring cleanup of {} draft(s)",
                        posted_ids.len()
                    );
                    return Ok(BatchOutcome::none_landed(batch, u64_ids(&posted_ids)));
                }
            }
        }

        // --- Phase 2b: the verdict, mapped onto the *released* API (see
        // [`GitLab::approve`]): `approve` → POST /approve; `request-changes` →
        // POST /unapprove (its concrete released effect); `comment` → nothing
        // (leaving notes no longer changes reviewer state, and there is no
        // released API to set `reviewed`). ---
        let verdict_ok = match batch.verdict {
            Some(Verdict::Approve) => {
                let ok = self.approve(id);
                if ok && !has_publishable {
                    notifications = notifications.max(1);
                }
                Some(ok)
            }
            Some(Verdict::RequestChanges) => {
                let ok = self.unapprove(id);
                if ok && !has_publishable {
                    notifications = notifications.max(1);
                }
                Some(ok)
            }
            Some(Verdict::Comment) => Some(true),
            None => None,
        };

        // Comment/reply actions all landed (we returned early on any failure).
        let mut landed: HashMap<ActionKey, bool> = batch
            .actions
            .iter()
            .filter(|a| !matches!(a, BatchAction::Resolve { .. }))
            .map(|a| (a.key(), true))
            .collect();

        // --- Phase 3: resolves (separate PUTs, per action). ---
        for a in &batch.actions {
            if let BatchAction::Resolve {
                key,
                thread,
                resolved,
            } = a
            {
                let ok = self.resolve_discussion(id, thread, *resolved).is_ok();
                if !ok {
                    log::warn!("MR {id}: resolve of thread {thread} failed");
                }
                landed.insert(key.clone(), ok);
            }
        }

        Ok(BatchOutcome {
            landed,
            summary_ok,
            verdict_ok,
            notifications,
            inflight: Vec::new(),
        })
    }

    fn permalink(&self, r#ref: &str, path: &str, lines: Option<(u32, Option<u32>)>) -> String {
        let frag = match lines {
            Some((a, Some(b))) => format!("#L{a}-{b}"),
            Some((a, None)) => format!("#L{a}"),
            None => String::new(),
        };
        format!(
            "{}/-/blob/{ref}/{}{frag}",
            self.web_base,
            super::encode_path(path)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::Service;
    use serde_json::json;

    fn info(host: &str, owner: &str, repo: &str) -> RemoteInfo {
        RemoteInfo {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
            service: Service::GitLab,
        }
    }

    fn version() -> DiffVersion {
        DiffVersion {
            base_sha: "b".into(),
            start_sha: "s".into(),
            head_sha: "h".into(),
        }
    }

    #[test]
    fn parses_a_gitlab_summary() {
        let v = json!({
            "iid": 7, "state": "opened", "draft": true, "title": "T",
            "author": { "username": "alice" }, "target_branch": "main",
            "source_branch": "feat", "sha": "deadbeef", "updated_at": "t",
            "labels": ["bug", "vk"], "web_url": "u"
        });
        let s = parse_summary(&v).unwrap();
        assert_eq!(s.id, "7");
        assert_eq!(s.display, "!7");
        assert_eq!(s.state, MrState::Open);
        assert!(s.draft);
        assert_eq!(s.base, "main");
        assert_eq!(s.source, "feat");
        assert_eq!(s.labels, ["bug", "vk"]);
    }

    #[test]
    fn diff_position_single_and_multi_line() {
        let ver = version();
        let single = diff_position(
            &ver,
            "a.c",
            None,
            LineRef {
                line: 5,
                side: Side::New,
            },
            None,
        );
        assert_eq!(single["position_type"], "text");
        assert_eq!(single["new_line"], 5);
        assert_eq!(single["head_sha"], "h");
        assert_eq!(single["new_path"], "a.c");
        assert!(single.get("line_range").is_none());

        let multi = diff_position(
            &ver,
            "a.c",
            None,
            LineRef {
                line: 5,
                side: Side::New,
            },
            Some(LineRef {
                line: 3,
                side: Side::Old,
            }),
        );
        assert_eq!(multi["line_range"]["start"]["type"], "old");
        assert_eq!(multi["line_range"]["start"]["old_line"], 3);
        assert_eq!(multi["line_range"]["end"]["type"], "new");
        assert_eq!(multi["line_range"]["end"]["new_line"], 5);
        // Every endpoint must carry a line_code or GitLab rejects the note.
        let sha = |p: &str| {
            use sha1::{Digest, Sha1};
            let mut h = Sha1::new();
            h.update(p.as_bytes());
            format!("{:x}", h.finalize())
        };
        assert_eq!(
            multi["line_range"]["start"]["line_code"],
            format!("{}_3_0", sha("a.c"))
        );
        assert_eq!(
            multi["line_range"]["end"]["line_code"],
            format!("{}_0_5", sha("a.c"))
        );
    }

    #[test]
    fn file_position_carries_versions_and_paths() {
        let p = file_position(&version(), "a.c", None);
        assert_eq!(p["position_type"], "file");
        assert_eq!(p["new_path"], "a.c");
        assert_eq!(p["old_path"], "a.c");
        assert_eq!(p["base_sha"], "b");
    }

    #[test]
    fn range_endpoint_round_trips() {
        let n = range_endpoint(
            "a.c",
            LineRef {
                line: 9,
                side: Side::New,
            },
        );
        assert_eq!(
            parse_range_endpoint(&n),
            Some(LineRef {
                line: 9,
                side: Side::New
            })
        );
        let o = range_endpoint(
            "a.c",
            LineRef {
                line: 4,
                side: Side::Old,
            },
        );
        assert_eq!(
            parse_range_endpoint(&o),
            Some(LineRef {
                line: 4,
                side: Side::Old
            })
        );
    }

    #[test]
    fn permalink_encodes_path() {
        let gl = GitLab::new(info("gitlab.com", "g", "r"), None, "t".into(), None).unwrap();
        assert_eq!(
            gl.permalink("head", "src/a b.c", Some((5, Some(9)))),
            "https://gitlab.com/g/r/-/blob/head/src/a%20b.c#L5-9"
        );
    }

    #[test]
    fn same_project_needs_no_fork_and_no_network() {
        // No source, or a source equal to the target, stays on the single-project
        // path — constructible without any project-id lookups.
        let target = info("gitlab.com", "me", "widget");
        let gl = GitLab::new(target.clone(), None, "tok".into(), None).unwrap();
        assert!(gl.fork.is_none());
        assert!(gl
            .mrs_url()
            .ends_with("/projects/me%2Fwidget/merge_requests"));

        let same = GitLab::new(target.clone(), Some(target), "tok".into(), None).unwrap();
        assert!(same.fork.is_none());
    }
}
