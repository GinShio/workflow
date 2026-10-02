//! GitLab merge requests.
//!
//! GitLab is the platform whose vocabulary the rest of the tool borrows ("MR",
//! "!"). Two shape differences matter for the same-project case: a project is
//! addressed by a URL-encoded `group/sub/repo` id, and a draft is a `Draft:`
//! title prefix rather than a field.
//!
//! Cross-project (fork) MRs are the awkward part. Unlike GitHub and Gitea, a
//! create request cannot name a head in another project: an MR from a fork is **created on the source
//! project** carrying a numeric `target_project_id`, but the MR itself then lives
//! in the **target** project (its iid belongs there), so reads and edits address
//! the target. Numeric project ids are required for the `target_project_id` body
//! field and the `source_project_id` list filter, so they are resolved once, by
//! the first request that needs them. When source and target are the same
//! project, none of this applies and we stay on the cheap single-project path.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::RemoteInfo;
use super::{
    dedup_mrs, encode, request, request_every_page, ActionKey, Anchor, Attributes, Auth,
    BatchAction, BatchOutcome, DiffVersion, FeedQuery, Forge, HeadRepo, LineRef, MergeRequest,
    MrComment, MrDetails, MrState, MrSummary, NewMr, RemoteComment, RemoteThread, ReviewBatch,
    Side, Verdict, SELF_REF,
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
    /// Present only when a stack's branches are pushed to another project — a
    /// fork — than the one MRs merge into.
    fork: Option<Fork>,
    /// The authenticated user's id and username, read once by the first
    /// request that needs either.
    me: OnceLock<(u64, String)>,
}

/// The source project of a cross-project MR. Its numeric ids are looked up by
/// the first request that needs them, so a run that never opens or finds a
/// stack's MR — any `wits review` — pays nothing for them.
struct Fork {
    source_path: String,
    /// `(source, target)` project ids.
    ids: OnceLock<(u64, u64)>,
}

impl GitLab {
    pub fn new(
        target: RemoteInfo,
        push_repo: Option<RemoteInfo>,
        token: String,
        api_url_override: Option<String>,
    ) -> Self {
        let api_base =
            api_url_override.unwrap_or_else(|| format!("https://{}/api/v4", target.host));
        let fork = push_repo.map(|push| Fork {
            source_path: encode(&push.project_path()),
            ids: OnceLock::new(),
        });
        Self {
            api_base,
            web_base: format!("https://{}/{}", target.host, target.project_path()),
            target_path: encode(&target.project_path()),
            auth: Auth::PrivateToken(token),
            fork,
            me: OnceLock::new(),
        }
    }

    /// A fork MR's source path with its `(source, target)` project ids, resolved
    /// once; `None` for a same-project MR.
    fn fork_ids(&self) -> anyhow::Result<Option<(&str, u64, u64)>> {
        let Some(fork) = &self.fork else {
            return Ok(None);
        };
        let &(source, target) = match fork.ids.get() {
            Some(ids) => ids,
            None => {
                let ids = (
                    project_id(&self.api_base, &self.auth, &fork.source_path)?,
                    project_id(&self.api_base, &self.auth, &self.target_path)?,
                );
                // Lookups run in parallel, so another one may have stored the same ids first.
                fork.ids.get_or_init(|| ids)
            }
        };
        Ok(Some((&fork.source_path, source, target)))
    }

    /// The authenticated user's id and username, read once.
    fn me(&self) -> anyhow::Result<&(u64, String)> {
        if let Some(me) = self.me.get() {
            return Ok(me);
        }
        let v = request("GET", &format!("{}/user", self.api_base), &self.auth, None)?;
        let (Some(id), Some(username)) = (v["id"].as_u64(), v["username"].as_str()) else {
            anyhow::bail!("could not read the authenticated user");
        };
        // Requests run in parallel, so another one may have stored the same user first.
        Ok(self.me.get_or_init(|| (id, username.to_owned())))
    }

    /// Whether `name` is a label of the target project or of a group above it.
    /// `add_labels` creates any label it does not find ("If a label does not
    /// already exist, this creates a new project label", the merge requests
    /// API), so an unchecked typo would add a label to the project itself.
    fn label_exists(&self, name: &str) -> anyhow::Result<bool> {
        let url = format!(
            "{}/projects/{}/labels/{}",
            self.api_base,
            self.target_path,
            encode(name)
        );
        match request("GET", &url, &self.auth, None) {
            Ok(_) => Ok(true),
            Err(e) if super::status_of(&e) == Some(404) => Ok(false),
            Err(e) => Err(e),
        }
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

    /// Approve the MR at `sha`, the head that was reviewed: GitLab refuses with
    /// a `409` once the MR has moved past it (the approvals API's `sha`), rather
    /// than approving commits nobody looked at. A second approval is refused
    /// too — a `401`, `already_approved` (`MergeRequests::ApprovalService`) — so
    /// a refusal is checked against the approvals, and one already given counts
    /// as landed; otherwise a resubmit after a partial success never could.
    /// Returns whether the MR stands approved by the user.
    fn approve(&self, id: &str, sha: &str) -> bool {
        let url = format!("{}/{id}/approve", self.mrs_url());
        let body = (!sha.is_empty()).then(|| json!({ "sha": sha }));
        match request("POST", &url, &self.auth, body.as_ref()) {
            Ok(_) => true,
            Err(e) if super::status_of(&e) == Some(409) => {
                log::warn!(
                    "MR {id}: not approved — it has moved past the head you reviewed; fetch \
                     it and review again"
                );
                false
            }
            Err(e) => match self.approved_by_me(id) {
                Ok(true) => true,
                _ => {
                    log::warn!("MR {id}: approve failed: {e}");
                    false
                }
            },
        }
    }

    /// Whether the authenticated user has approved the MR.
    fn approved_by_me(&self, id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/{id}/approvals", self.mrs_url());
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v["user_has_approved"].as_bool() == Some(true))
    }

    /// Whether the authenticated user's review of the MR stands at `state`.
    /// `bulk_publish` sets `reviewer_state` and answers `204` whatever came of
    /// it — the state update's own result is dropped — and that update needs
    /// permission to update the MR, so a reviewer without it is refused in
    /// silence; only the reviewer list tells.
    fn reviewer_state_is(&self, id: &str, state: &str) -> bool {
        let found = self.me().map(|me| me.0).and_then(|me| {
            let url = format!("{}/{id}/reviewers?per_page=100", self.mrs_url());
            let reviewers = request("GET", &url, &self.auth, None)?;
            Ok(reviewers.as_array().is_some_and(|all| {
                all.iter()
                    .any(|r| r["user"]["id"].as_u64() == Some(me) && r["state"] == state)
            }))
        });
        found.unwrap_or(false)
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
            return Ok(Some(self.me()?.0));
        }
        let url = format!("{}/users?username={}", self.api_base, encode(name));
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(|u| u["id"].as_u64()))
    }

    /// The users of `names` that resolve, each with its id; the rest warn.
    fn resolve_user_ids<'a>(&self, names: &'a [String]) -> Vec<(&'a str, u64)> {
        names
            .iter()
            .filter_map(|name| match self.user_id(name) {
                Ok(Some(uid)) => Some((name.as_str(), uid)),
                Ok(None) => {
                    log::warn!("user '{name}' not found");
                    None
                }
                Err(e) => {
                    log::warn!("resolving user '{name}': {e}");
                    None
                }
            })
            .collect()
    }

    /// A feed filter value, with `@me` expanded to the authenticated username.
    fn filter_user(&self, name: &str) -> anyhow::Result<String> {
        if name == SELF_REF {
            Ok(self.me()?.1.clone())
        } else {
            Ok(name.to_owned())
        }
    }

    /// The MR-scoped draft-notes endpoint (always on the target project).
    fn draft_notes_url(&self, id: &str) -> String {
        format!("{}/{id}/draft_notes", self.mrs_url())
    }

    /// The ids of the user's draft notes still pending on the MR — what a
    /// publish did not take. The list comes whole (`load_draft_notes`, no
    /// paging).
    fn pending_drafts(&self, id: &str) -> anyhow::Result<HashSet<u64>> {
        let v = request("GET", &self.draft_notes_url(id), &self.auth, None)?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|draft| draft["id"].as_u64())
            .collect())
    }
}

/// A conversation note as an [`MrComment`], or `None` for a system note (a
/// label or state change) or a diff note, which belongs to a review thread.
/// `mr_url` is the MR's web page, which the note's anchor is relative to.
fn parse_comment(note: &Value, mr_url: &str, me: u64) -> Option<MrComment> {
    if note["system"].as_bool().unwrap_or(false) || note["type"].as_str() == Some("DiffNote") {
        return None;
    }
    let id = note["id"].as_u64()?;
    Some(MrComment {
        id: id.to_string(),
        body: note["body"].as_str().unwrap_or_default().to_owned(),
        url: format!("{mr_url}#note_{id}"),
        own: note["author"]["id"].as_u64() == Some(me),
    })
}

/// Whether an MR comes from the expected source project: `project` when given
/// (a fork), else the very project it merges into. The branch name alone
/// matches every fork's same-named branch too. `source_project_id` is honoured
/// by the project endpoint even though only the group endpoint documents it
/// (`lib/api/helpers/merge_requests_helpers.rb`, `MergeRequestsFinder`); the
/// check is repeated here so a server that ignored it still could not widen
/// the answer.
fn sourced_from(mr: &Value, project: Option<u64>) -> bool {
    let source = mr["source_project_id"].as_u64();
    match project {
        Some(id) => source == Some(id),
        None => source.is_some() && source == mr["target_project_id"].as_u64(),
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
    let (old, new) = line_numbers(end);
    if let Some(l) = old {
        pos["old_line"] = json!(l);
    }
    if let Some(l) = new {
        pos["new_line"] = json!(l);
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

/// A line's `(old_line, new_line)` in GitLab's terms, each absent on the side
/// the line does not exist on: an added line has only its new number, a removed
/// one only its old, an unchanged one both — the one form GitLab places it by.
fn line_numbers(r: LineRef) -> (Option<u32>, Option<u32>) {
    match r.side {
        Side::Old => (Some(r.line), None),
        Side::New => (r.old_line, Some(r.line)),
    }
}

/// One endpoint of a GitLab `position.line_range`, shaped as GitLab builds one
/// itself (`ResolveDiffPositionService#build_line_range_entry`): the side as
/// `type` and that side's number, or both numbers and no `type` for an
/// unchanged line — with the `line_code` the discussions API lists as required.
fn range_endpoint(path: &str, r: LineRef) -> Value {
    let (old, new) = line_numbers(r);
    let mut o = json!({ "line_code": line_code(path, old.unwrap_or(0), new.unwrap_or(0)) });
    match (old, new) {
        (Some(_), Some(_)) => {}
        (Some(_), None) => o["type"] = json!("old"),
        (None, _) => o["type"] = json!("new"),
    }
    if let Some(l) = old {
        o["old_line"] = json!(l);
    }
    if let Some(l) = new {
        o["new_line"] = json!(l);
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
/// side and the matching `new_line`/`old_line` gives the line; an endpoint
/// without a `type` is an unchanged line, read by its numbers.
fn parse_range_endpoint(v: &Value) -> Option<LineRef> {
    let number = |key: &str| v[key].as_u64().map(|l| l as u32);
    let side = match v["type"].as_str() {
        Some("old") => Side::Old,
        Some("new") => Side::New,
        _ => return position_line(number("old_line"), number("new_line")),
    };
    let line = number(if side == Side::New {
        "new_line"
    } else {
        "old_line"
    })
    // A side endpoint on a line that only exists on the other side (e.g. a
    // pure addition viewed from the old side) carries no line for that side;
    // fall back to whichever the object does carry so the span round-trips.
    .or_else(|| number("new_line"))
    .or_else(|| number("old_line"))?;
    Some(LineRef {
        line,
        side,
        old_line: None,
    })
}

/// The line a pair of GitLab numbers names: the new side when there is a new
/// number — carrying the old one too, for an unchanged line — else the old.
fn position_line(old: Option<u32>, new: Option<u32>) -> Option<LineRef> {
    match (old, new) {
        (old_line, Some(line)) => Some(LineRef {
            line,
            side: Side::New,
            old_line,
        }),
        (Some(line), None) => Some(LineRef {
            line,
            side: Side::Old,
            old_line: None,
        }),
        (None, None) => None,
    }
}

/// One draft note a submission posts, and the actions it carries: a comment's
/// one, or every reply into one thread.
struct Draft {
    keys: Vec<ActionKey>,
    /// For logging.
    label: String,
    body: Value,
    reply: bool,
}

/// The draft notes a batch posts: one per comment, and one per thread for all
/// its replies, their bodies a paragraph apart. GitLab keeps one draft per
/// author and thread (`DraftNote`'s uniqueness on the discussion), so a second
/// reply to a thread would be refused — and refused again on every retry.
fn drafts_of(batch: &ReviewBatch) -> Vec<Draft> {
    let mut drafts: Vec<Draft> = Vec::new();
    let mut by_thread: HashMap<&str, usize> = HashMap::new();
    for a in &batch.actions {
        match a {
            BatchAction::Comment {
                key,
                anchor,
                version,
                body,
            } => {
                // Each comment anchors to its own snapshot version (resolved at
                // build time) — the heart of cross-snapshot drafting.
                let body = match anchor {
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
                drafts.push(Draft {
                    keys: vec![key.clone()],
                    label: format!("action {key}"),
                    body,
                    reply: false,
                });
            }
            BatchAction::Reply { key, thread, body } => match by_thread.get(thread.as_str()) {
                Some(&i) => {
                    let draft = &mut drafts[i];
                    let note = format!(
                        "{}\n\n{body}",
                        draft.body["note"].as_str().unwrap_or_default()
                    );
                    draft.body["note"] = json!(note);
                    draft.keys.push(key.clone());
                    draft.label = format!("{}, {key}", draft.label);
                }
                None => {
                    by_thread.insert(thread, drafts.len());
                    drafts.push(Draft {
                        keys: vec![key.clone()],
                        label: format!("action {key}"),
                        body: json!({ "note": body, "in_reply_to_discussion_id": thread }),
                        reply: true,
                    });
                }
            },
            BatchAction::Resolve { .. } => {}
        }
    }
    drafts
}

/// Whether GitLab took a line comment's draft without finding its line. A draft
/// answers with the `line_code` GitLab resolved its position to
/// (`DraftNote#line_code`); a null one means the line is not in that diff, and
/// publishing would drop the note — logged, its draft deleted, the publish
/// still a `204` (`DraftNotes::PublishService`) — so the comment would be
/// reported as posted and be gone.
fn unplaced(sent: &Value, answer: &Value) -> bool {
    sent["position"]["position_type"] == "text" && answer["line_code"].is_null()
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

    fn mrs_for_branch(&self, head: HeadRepo, branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        // A fork MR comes from the fork's project; any other from the project it
        // merges into.
        let source = match head {
            HeadRepo::Origin => self.fork_ids()?.map(|(_, source, _)| source),
            HeadRepo::Target => None,
        };
        let mut url = format!(
            "{}?source_branch={}&state=all&order_by=updated_at&sort=desc&per_page=100",
            self.mrs_url(),
            encode(branch),
        );
        if let Some(id) = source {
            url += &format!("&source_project_id={id}");
        }
        request_every_page(&url, &self.auth, |v, headers| {
            let mrs = v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|mr| sourced_from(mr, source))
                        .filter_map(parse_mr)
                        .collect()
                })
                .unwrap_or_default();
            (mrs, next_page(&url, headers))
        })
        .map(dedup_mrs)
    }

    fn find_children(&self, base_branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let url = format!(
            "{}?target_branch={}&state=opened&per_page=100",
            self.mrs_url(),
            encode(base_branch),
        );
        request_every_page(&url, &self.auth, |v, headers| {
            let mrs = v
                .as_array()
                .map(|arr| arr.iter().filter_map(parse_mr).collect())
                .unwrap_or_default();
            (mrs, next_page(&url, headers))
        })
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
        let url = match self.fork_ids()? {
            Some((source_path, _, target_id)) => {
                body["target_project_id"] = json!(target_id);
                format!("{}/projects/{source_path}/merge_requests", self.api_base)
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

        // Labels have a native additive verb, given only the labels that exist
        // (see `label_exists`).
        let labels: Vec<&str> = attrs
            .labels
            .iter()
            .map(String::as_str)
            .filter(|name| match self.label_exists(name) {
                Ok(true) => true,
                Ok(false) => {
                    log::warn!("label '{name}' does not exist in this project or its groups");
                    false
                }
                Err(e) => {
                    log::warn!("label '{name}': {e}");
                    false
                }
            })
            .collect();
        if !labels.is_empty() {
            body.insert("add_labels".into(), json!(labels.join(",")));
        }
        // Users have none, so we read the current ids and union ours in, then
        // PUT the full set.
        let mut wanted = Vec::new();
        if !attrs.assignees.is_empty() || !attrs.reviewers.is_empty() {
            let mr = request(
                "GET",
                &format!("{}/{}", self.mrs_url(), id),
                &self.auth,
                None,
            )?;
            for (role, names) in [
                ("assignees", &attrs.assignees),
                ("reviewers", &attrs.reviewers),
            ] {
                if names.is_empty() {
                    continue;
                }
                let users = self.resolve_user_ids(names);
                let mut ids = current_user_ids(&mr, role);
                for &(_, uid) in &users {
                    if !ids.contains(&uid) {
                        ids.push(uid);
                    }
                }
                let field = if role == "assignees" {
                    "assignee_ids"
                } else {
                    "reviewer_ids"
                };
                body.insert(field.into(), json!(ids));
                wanted.push((role, users));
            }
        }

        if body.is_empty() {
            return Ok(());
        }
        let url = format!("{}/{}", self.mrs_url(), id);
        let updated = request("PUT", &url, &self.auth, Some(&Value::Object(body)))?;
        // An update GitLab will not carry out in full still succeeds: past the
        // first assignee or reviewer on a tier without multiple ones, and for a
        // reviewer the caller may not set (`MergeRequests::BaseService`).
        for (role, users) in wanted {
            let applied = current_user_ids(&updated, role);
            for (name, uid) in users {
                if !applied.contains(&uid) {
                    log::warn!("{name} was not added to the {role}; GitLab dropped it");
                }
            }
        }
        Ok(())
    }

    fn list_comments(&self, mr: &str) -> anyhow::Result<Vec<MrComment>> {
        let me = self.me()?.0;
        let mr_url = format!("{}/-/merge_requests/{mr}", self.web_base);
        let url = format!(
            "{}/{mr}/notes?sort=asc&order_by=created_at&per_page=100",
            self.mrs_url()
        );
        request_every_page(&url, &self.auth, |v, headers| {
            let notes = v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|note| parse_comment(note, &mr_url, me))
                        .collect()
                })
                .unwrap_or_default();
            (notes, next_page(&url, headers))
        })
    }

    fn add_comment(&self, mr: &str, body: &str) -> anyhow::Result<()> {
        // A note posted here is a standard comment, not a thread, and a
        // standard comment cannot be resolved (GitLab's "Comments and threads"
        // docs), so it never holds up "all threads must be resolved".
        let url = format!("{}/{mr}/notes", self.mrs_url());
        request("POST", &url, &self.auth, Some(&json!({ "body": body })))?;
        Ok(())
    }

    fn edit_comment(&self, mr: &str, comment: &str, body: &str) -> anyhow::Result<()> {
        let url = format!("{}/{mr}/notes/{comment}", self.mrs_url());
        request("PUT", &url, &self.auth, Some(&json!({ "body": body })))?;
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
        // Every page: a thousand discussions — label and push events count —
        // is within a long MR's reach.
        let url = format!("{}/{id}/discussions?per_page=100", self.mrs_url());
        let discussions: Vec<Value> = request_every_page(&url, &self.auth, |v, headers| {
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
                // `new_line`, `old_line`, or both for an unchanged line. The
                // top-level pair is the range's last line too (GitLab's own
                // `ResolveDiffPositionService` builds it so), which reads an
                // end endpoint that names none. (A position with no line at
                // all is a degenerate note we surface as a line-0 anchor
                // rather than dropping.)
                let number = |key: &str| p[key].as_u64().map(|l| l as u32);
                let last =
                    position_line(number("old_line"), number("new_line")).unwrap_or(LineRef {
                        line: 0,
                        side: Side::New,
                        old_line: None,
                    });
                let anchor = if p["position_type"].as_str() == Some("file") {
                    Anchor::File { path }
                } else if let Some(lr) = p["line_range"].as_object() {
                    Anchor::Line {
                        path,
                        old_path,
                        end: parse_range_endpoint(&lr["end"]).unwrap_or(last),
                        start: parse_range_endpoint(&lr["start"]),
                    }
                } else {
                    Anchor::Line {
                        path,
                        old_path,
                        end: last,
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
                // computation (`docs/reference/review-design.rst`, "Outdating —
                // anchor to what you reviewed, let the forge mark it") supersedes
                // this.
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
        // (line / file / MR-level) and replies become draft notes, published
        // together as one review — one notification — and the summary and the
        // reviewer state ride the same call. That two-phase shape puts
        // atomicity on us, so it is all-or-nothing *per attempt*: any draft
        // failure aborts the publish and defers cleanup of what posted, for a
        // clean retry (no orphans, no duplicates). Resolves are not draft notes
        // (a draft needs a body), so they are separate PUTs (phase 3), and an
        // approval is its own call (phase 2b).

        // --- Phase 1: the drafts, POSTed in bounded-parallel batches. A cap
        // keeps a big review from opening a connection per note. ---
        let drafts = drafts_of(batch);
        let posted: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
        let ok = Mutex::new(true);
        let draft_url_ref = &draft_url;
        let auth = &self.auth;
        let posted_ref = &posted;
        let ok_ref = &ok;
        for (chunk_no, chunk) in drafts.chunks(MAX_DRAFT_PARALLEL).enumerate() {
            std::thread::scope(|scope| {
                for (i, draft) in chunk.iter().enumerate() {
                    let index = chunk_no * MAX_DRAFT_PARALLEL + i;
                    scope.spawn(move || {
                        let label = &draft.label;
                        match request("POST", draft_url_ref, auth, Some(&draft.body)) {
                            Ok(v) => {
                                if let Some(did) = v["id"].as_u64() {
                                    posted_ref.lock().unwrap().push((index, did));
                                }
                                if unplaced(&draft.body, &v) {
                                    log::warn!(
                                        "MR {id}: GitLab cannot place the comment ({label}) on \
                                         a line of the diff it was written against; nothing \
                                         was published"
                                    );
                                    *ok_ref.lock().unwrap() = false;
                                }
                            }
                            Err(e) => {
                                let hint = if draft.reply && super::status_of(&e) == Some(400) {
                                    "; GitLab keeps one draft per thread, so a draft reply \
                                     you started there blocks this one — publish or delete it \
                                     on GitLab"
                                } else {
                                    ""
                                };
                                log::warn!("MR {id}: draft note ({label}) failed: {e}{hint}");
                                *ok_ref.lock().unwrap() = false;
                            }
                        }
                    });
                }
            });
        }
        let drafts_all_ok = ok.into_inner().unwrap();
        let posted = posted.into_inner().unwrap();
        let posted_ids: Vec<u64> = posted.iter().map(|&(_, did)| did).collect();

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

        // --- Phase 2: one `bulk_publish` publishes all of the user's pending
        // drafts on the MR as a single review. Since GitLab 19.2 the same call
        // posts the summary as a plain note (`note`) — not as a draft, which
        // would publish as a resolvable thread and could hold up "all threads
        // must be resolved" — at the cost of a notification of its own, and
        // sets the reviewer state a request-changes or comment verdict means
        // (`reviewer_state`, which also withdraws an approval on request-
        // changes). ---
        let reviewer_state = match batch.verdict {
            Some(Verdict::RequestChanges) => Some("requested_changes"),
            Some(Verdict::Comment) => Some("reviewed"),
            Some(Verdict::Approve) | None => None,
        };
        let mut publish = serde_json::Map::new();
        if let Some(summary) = &batch.summary {
            publish.insert("note".into(), json!(summary));
        }
        if let Some(state) = reviewer_state {
            publish.insert("reviewer_state".into(), json!(state));
        }

        // Which drafts went out: all of them once the publish answers. A
        // failure can come after it published them — the summary note is
        // created only then — so after one, the drafts still pending tell.
        let mut published = vec![true; drafts.len()];
        let mut inflight = Vec::new();
        let mut summary_ok = batch.summary.is_none();
        let mut state_set = false;
        let mut notifications = 0u32;
        if !posted.is_empty() || !publish.is_empty() {
            let url = format!("{draft_url}/bulk_publish");
            match request("POST", &url, &self.auth, Some(&Value::Object(publish))) {
                Ok(_) => {
                    notifications =
                        u32::from(!posted.is_empty()) + u32::from(batch.summary.is_some());
                    summary_ok = true;
                    state_set = true;
                }
                Err(e) => {
                    let still = self.pending_drafts(id).map(|pending| {
                        posted
                            .iter()
                            .copied()
                            .filter(|(_, did)| pending.contains(did))
                            .collect::<Vec<_>>()
                    });
                    let Ok(still) = still else {
                        log::warn!(
                            "MR {id}: bulk_publish failed ({e}); deferring cleanup of {} \
                             draft(s)",
                            posted_ids.len()
                        );
                        return Ok(BatchOutcome::none_landed(batch, u64_ids(&posted_ids)));
                    };
                    for (index, did) in still {
                        published[index] = false;
                        inflight.push(did.to_string());
                    }
                    if inflight.len() == posted.len() {
                        log::warn!(
                            "MR {id}: bulk_publish failed ({e}); deferring cleanup of {} \
                             draft(s)",
                            inflight.len()
                        );
                        return Ok(BatchOutcome::none_landed(batch, inflight));
                    }
                    log::warn!(
                        "MR {id}: the review was published, but bulk_publish failed after it: {e}"
                    );
                    notifications = 1;
                }
            }
        }

        // --- Phase 2b: the verdict. Approval is its own call; a requested
        // change rode the publish and is checked, since GitLab does not say
        // whether it took it; `reviewed` is a courtesy to the author, not a
        // gate, so it lands with the publish. ---
        let verdict_ok = match batch.verdict {
            Some(Verdict::Approve) => Some(self.approve(id, &batch.version.head_sha)),
            Some(Verdict::RequestChanges) => {
                let ok = state_set && self.reviewer_state_is(id, "requested_changes");
                if state_set && !ok {
                    log::warn!(
                        "MR {id}: GitLab did not record the request for changes; a reviewer \
                         state takes permission to update the MR"
                    );
                }
                Some(ok)
            }
            Some(Verdict::Comment) => Some(state_set),
            None => None,
        };
        if verdict_ok == Some(true) && notifications == 0 {
            notifications = 1;
        }

        let mut landed: HashMap<ActionKey, bool> = HashMap::new();
        for (draft, &went) in drafts.iter().zip(&published) {
            for key in &draft.keys {
                landed.insert(key.clone(), went);
            }
        }

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
            inflight,
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
    fn a_conversation_note_is_kept_and_marked_by_its_author() {
        let mr_url = "https://gitlab.com/g/r/-/merge_requests/7";
        let note = |id: u64, author: u64, system: bool, kind: Value| {
            json!({ "id": id, "body": "b", "system": system, "type": kind,
                    "author": { "id": author } })
        };
        let mine = parse_comment(&note(11, 5, false, Value::Null), mr_url, 5).unwrap();
        assert!(mine.own);
        assert_eq!(mine.id, "11");
        assert_eq!(
            mine.url,
            "https://gitlab.com/g/r/-/merge_requests/7#note_11"
        );
        let theirs = parse_comment(&note(12, 6, false, json!("DiscussionNote")), mr_url, 5);
        assert!(!theirs.unwrap().own);
        // A label change and a review comment on the diff are not conversation.
        assert!(parse_comment(&note(13, 5, true, Value::Null), mr_url, 5).is_none());
        assert!(parse_comment(&note(14, 5, false, json!("DiffNote")), mr_url, 5).is_none());
    }

    #[test]
    fn an_mr_counts_only_from_the_expected_source_project() {
        let mr = |source: Value, target: u64| json!({ "source_project_id": source, "target_project_id": target });
        // Same project: the MR must come from the project it merges into, so a
        // fork's same-named branch does not count.
        assert!(sourced_from(&mr(json!(7), 7), None));
        assert!(!sourced_from(&mr(json!(9), 7), None));
        // A fork: only the fork's project counts, not the target's own branch.
        assert!(sourced_from(&mr(json!(9), 7), Some(9)));
        assert!(!sourced_from(&mr(json!(7), 7), Some(9)));
        // A deleted fork leaves no source project, which never matches.
        assert!(!sourced_from(&mr(Value::Null, 7), None));
    }

    fn at(line: u32, side: Side, old_line: Option<u32>) -> LineRef {
        LineRef {
            line,
            side,
            old_line,
        }
    }

    fn sha(path: &str) -> String {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(path.as_bytes());
        format!("{:x}", h.finalize())
    }

    #[test]
    fn diff_position_single_and_multi_line() {
        let ver = version();
        let single = diff_position(&ver, "a.c", None, at(5, Side::New, None), None);
        assert_eq!(single["position_type"], "text");
        assert_eq!(single["new_line"], 5);
        assert!(single.get("old_line").is_none());
        assert_eq!(single["head_sha"], "h");
        assert_eq!(single["new_path"], "a.c");
        assert!(single.get("line_range").is_none());

        let multi = diff_position(
            &ver,
            "a.c",
            None,
            at(5, Side::New, None),
            Some(at(3, Side::Old, None)),
        );
        assert_eq!(multi["line_range"]["start"]["type"], "old");
        assert_eq!(multi["line_range"]["start"]["old_line"], 3);
        assert_eq!(multi["line_range"]["end"]["type"], "new");
        assert_eq!(multi["line_range"]["end"]["new_line"], 5);
        // Every endpoint carries the line_code the API lists as required.
        assert_eq!(
            multi["line_range"]["start"]["line_code"],
            format!("{}_3_0", sha("a.c"))
        );
        assert_eq!(
            multi["line_range"]["end"]["line_code"],
            format!("{}_0_5", sha("a.c"))
        );
    }

    /// GitLab places an unchanged line only by both of its numbers; given the
    /// new one alone it drops the note when the review is published.
    #[test]
    fn an_unchanged_line_is_placed_by_both_numbers() {
        let single = diff_position(&version(), "a.c", None, at(12, Side::New, Some(10)), None);
        assert_eq!(
            (single["old_line"].clone(), single["new_line"].clone()),
            (json!(10), json!(12))
        );

        let range = diff_position(
            &version(),
            "a.c",
            None,
            at(12, Side::New, Some(10)),
            Some(at(11, Side::New, None)),
        );
        let end = &range["line_range"]["end"];
        assert!(end.get("type").is_none(), "an unchanged line has no side");
        assert_eq!(
            (end["old_line"].clone(), end["new_line"].clone()),
            (json!(10), json!(12))
        );
        assert_eq!(end["line_code"], format!("{}_10_12", sha("a.c")));
        // The added start line keeps its side and one number.
        assert_eq!(range["line_range"]["start"]["type"], "new");
        assert!(range["line_range"]["start"].get("old_line").is_none());
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
        for line in [
            at(9, Side::New, None),
            at(4, Side::Old, None),
            at(12, Side::New, Some(10)),
        ] {
            assert_eq!(
                parse_range_endpoint(&range_endpoint("a.c", line)),
                Some(line)
            );
        }
    }

    /// GitLab keeps one draft per author and thread, so the replies into one
    /// thread travel as one draft; every other action keeps its own.
    #[test]
    fn replies_into_one_thread_share_a_draft() {
        let reply = |key: &str, thread: &str, body: &str| BatchAction::Reply {
            key: key.into(),
            thread: thread.into(),
            body: body.into(),
        };
        let batch = ReviewBatch {
            verdict: None,
            summary: Some("overall".into()),
            actions: vec![
                reply("r1", "d1", "first"),
                BatchAction::Comment {
                    key: "c".into(),
                    anchor: None,
                    version: version(),
                    body: "note".into(),
                },
                reply("r2", "d2", "elsewhere"),
                reply("r3", "d1", "second"),
                BatchAction::Resolve {
                    key: "x".into(),
                    thread: "d1".into(),
                    resolved: true,
                },
            ],
            version: version(),
            stale: Vec::new(),
        };
        let drafts = drafts_of(&batch);
        let keys: Vec<Vec<&str>> = drafts
            .iter()
            .map(|d| d.keys.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(keys, [vec!["r1", "r3"], vec!["c"], vec!["r2"]]);
        assert_eq!(drafts[0].body["note"], "first\n\nsecond");
        assert_eq!(drafts[0].body["in_reply_to_discussion_id"], "d1");
        assert!(drafts[0].reply && !drafts[1].reply);
        // The summary is no draft: it rides the publish as a plain note.
        assert!(drafts.iter().all(|d| d.body["note"] != "overall"));
    }

    #[test]
    fn a_line_draft_without_a_line_code_was_not_placed() {
        let line = json!({ "note": "n", "position": { "position_type": "text", "new_line": 3 } });
        assert!(unplaced(&line, &json!({ "id": 1, "line_code": null })));
        assert!(!unplaced(&line, &json!({ "id": 1, "line_code": "x_0_3" })));
        // File and MR-level drafts have no line to place.
        let file = json!({ "note": "n", "position": { "position_type": "file" } });
        assert!(!unplaced(&file, &json!({ "id": 1, "line_code": null })));
        assert!(!unplaced(
            &json!({ "note": "n" }),
            &json!({ "id": 1, "line_code": null })
        ));
    }

    /// A range made in GitLab's own diff view gives an unchanged endpoint no
    /// `type`; it reads back as the new-side line it is, not as nothing.
    #[test]
    fn an_untyped_endpoint_is_an_unchanged_line() {
        let endpoint =
            json!({ "line_code": "x_10_12", "type": null, "old_line": 10, "new_line": 12 });
        assert_eq!(
            parse_range_endpoint(&endpoint),
            Some(at(12, Side::New, Some(10)))
        );
        assert_eq!(parse_range_endpoint(&json!({ "type": null })), None);
    }

    #[test]
    fn permalink_encodes_path() {
        let gl = GitLab::new(info("gitlab.com", "g", "r"), None, "t".into(), None);
        assert_eq!(
            gl.permalink("head", "src/a b.c", Some((5, Some(9)))),
            "https://gitlab.com/g/r/-/blob/head/src/a%20b.c#L5-9"
        );
    }

    #[test]
    fn same_project_needs_no_fork_and_no_network() {
        // No push repository stays on the single-project path.
        let target = info("gitlab.com", "me", "widget");
        let gl = GitLab::new(target.clone(), None, "tok".into(), None);
        assert!(gl.fork.is_none());
        assert!(gl
            .mrs_url()
            .ends_with("/projects/me%2Fwidget/merge_requests"));

        // A fork is constructed without a lookup: its ids wait for the first
        // request that needs them, which a review never makes.
        let fork = GitLab::new(
            target,
            Some(info("gitlab.com", "you", "widget")),
            "tok".into(),
            None,
        );
        let source = fork.fork.as_ref().expect("a fork");
        assert_eq!(source.source_path, "you%2Fwidget");
        assert!(source.ids.get().is_none());
    }
}
