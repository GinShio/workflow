//! The review half of the forge boundary: the normalized types the `review`
//! command speaks in, kept free of any platform's JSON shape.
//!
//! Where the MR half (in `super`) is four small primitives, review touches
//! corners of the platforms that do *not* normalize cleanly — batched reviews,
//! approve-as-a-separate-call, GraphQL-only thread resolution. Those differences
//! are trapped inside each host module and surfaced honestly in
//! `docs/review/design.md`'s capability matrix; here we define only the shapes
//! that cross the boundary. Nothing above the [`Forge`](super::Forge) trait ever
//! sees raw JSON.

use serde::{Deserialize, Serialize};

/// Which side of a diff a line lives on. The `New` side is the post-image
/// (added and context lines); `Old` is the pre-image, used only for a line that
/// a change deleted. Shared with the local model so an anchor round-trips
/// without translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }
}

/// One endpoint of a line anchor: a file line and the diff side it lives on.
/// Each endpoint carries its own side so a multi-line span can cross sides — a
/// comment starting on a deleted (old-side) line and ending on an added
/// (new-side) one. This is the single nested shape every placement speaks in;
/// each forge translates it to its native anchor form (GitHub `line`/`side`/
/// `start_line`/`start_side`; GitLab `position.line_range{start,end}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRef {
    pub line: u32,
    pub side: Side,
}

/// A reviewer's verdict on an MR. `RequestChanges` is a GitHub/Gitea concept;
/// GitLab has no native equivalent and maps it to "leave a comment review and do
/// not approve" (see the capability matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

impl Verdict {
    /// A short, stable display string for logs and dry-run output.
    pub fn display_str(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::RequestChanges => "request-changes",
            Verdict::Comment => "comment",
        }
    }
}

/// The lifecycle states a feed pulls. `merged`/`closed` are deliberately never
/// fetched — a review inbox is about live work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedStates {
    pub open: bool,
    pub draft: bool,
}

impl Default for FeedStates {
    /// Open and draft together — the default a feed carries when unspecified.
    fn default() -> Self {
        Self {
            open: true,
            draft: true,
        }
    }
}

/// A feed's faceted filter, translated to each platform's list/search query and
/// pushed down server-side (never applied client-side after a full fetch). Fields
/// are AND-ed together; the multiple values within a field are OR-ed.
#[derive(Debug, Clone, Default)]
pub struct FeedQuery {
    pub states: FeedStates,
    pub labels: Vec<String>,
    pub exclude_labels: Vec<String>,
    pub author: Option<String>,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    /// A raw platform search string, the escape hatch for the full-text case.
    pub search: Option<String>,
    /// A hard cap on how many MRs to pull, so a huge repo can't flood the inbox.
    pub limit: usize,
}

/// The rich per-MR view the inbox needs — more than the terse [`MergeRequest`]
/// the stack verbs use, because a reviewer scans by title/author/staleness. This
/// is also the persisted MR-metadata shape (`info.json`'s `mr`, and the `--json`
/// `mr` object) — the read model stores it directly rather than mirroring it into
/// a second near-identical struct.
///
/// [`MergeRequest`]: super::MergeRequest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MrSummary {
    pub id: String,
    pub display: String,
    pub state: super::MrState,
    pub draft: bool,
    pub title: String,
    pub author: String,
    /// The MR's target branch — what it merges into.
    pub base: String,
    /// The MR's source branch, so a stack can be reconstructed by linking one
    /// MR's `base` to another's `source` (empty when the platform withholds it,
    /// e.g. a search result).
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head_sha: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub web_url: String,
}

/// The diff coordinates a comment must carry to anchor on the forge. GitLab needs
/// all three (`base`, `start`, `head`); GitHub needs only `head_sha` as the
/// `commit_id`, so its `start_sha` mirrors `base_sha`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffVersion {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

/// Everything the forge can tell us about one MR that local git cannot: its
/// metadata and the current diff-version SHAs. The commit list and file set are
/// derived locally from the fetched objects, not from here.
#[derive(Debug, Clone)]
pub struct MrDetails {
    pub summary: MrSummary,
    pub version: DiffVersion,
}

/// A code anchor — a single line, a multi-line span, or a whole changed file —
/// normalized across platforms and shared by the read model, the submit
/// boundary, and (via `model`) the local model. Its *absence* — an
/// `Option<Anchor>` of `None` — is the MR-level conversation, which has no code
/// anchor at all.
///
/// Each [`LineRef`] endpoint carries its own side so a span can cross the
/// delete/add boundary (an old-side start through to a new-side end). Every
/// forge translates this one shape to its native anchor form (GitHub
/// `line`/`side`/`startLine`/`startSide`; GitLab `position.line_range`).
///
/// The `#[serde(tag = "kind")]` shape (`{"kind":"line",…}` / `{"kind":"file",…}`)
/// is the wire form the `--json` read contract emits directly — there is no
/// second "placement" mirror type to keep in step. An MR-level conversation
/// (no code anchor) is `Option<Anchor>::None`, serialized as an absent field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Anchor {
    /// A code line of a changed file. `end` is the anchor line; `start`, when
    /// present, makes it a multi-line span (and may sit on the other side).
    Line {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        old_path: Option<String>,
        end: LineRef,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        start: Option<LineRef>,
    },
    /// A changed file, no specific line.
    File { path: String },
}

impl Anchor {
    /// The new-side path the anchor sits on.
    pub fn path(&self) -> &str {
        match self {
            Anchor::Line { path, .. } | Anchor::File { path } => path,
        }
    }
}

/// One comment inside a remote thread.
#[derive(Debug, Clone)]
pub struct RemoteComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A discussion thread as it currently exists on the forge.
#[derive(Debug, Clone)]
pub struct RemoteThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    /// The code anchor, or `None` for an MR-level conversation thread.
    pub anchor: Option<Anchor>,
    /// The commit SHA the thread's anchor was written against, when the forge
    /// reports one (GitLab `position.head_sha`, GitHub `original_commit_id`).
    /// Feeds the read view and local outdate computation.
    pub commit: Option<String>,
    pub comments: Vec<RemoteComment>,
}

/// A stable per-submission identifier for one action, so the orchestration layer
/// reconciles by *identity*, not by position — regardless of how a backend
/// reorders, splits, or batches the work. For `wits review`, this is the
/// persisted local action id.
pub type ActionKey = String;

/// One thing to do inside a review submission, forge-neutral. Every kind of
/// action can travel in the same batch; each [`Forge`] folds as many as its
/// native primitive allows into one notification and reports the rest honestly.
///
/// [`Forge`]: super::Forge
#[derive(Debug, Clone)]
pub enum BatchAction {
    /// A new comment. `anchor` is the code anchor, or `None` for an MR-level
    /// conversation comment. `version` is the snapshot the comment's lines were
    /// written against (resolved from the snapshot history at build time), so an
    /// outdated anchor is honoured rather than silently re-based.
    Comment {
        key: ActionKey,
        anchor: Option<Anchor>,
        version: DiffVersion,
        body: String,
    },
    /// A reply into an existing remote thread (the bare forge thread id).
    Reply {
        key: ActionKey,
        thread: String,
        body: String,
    },
    /// Resolve / unresolve an existing remote thread.
    Resolve {
        key: ActionKey,
        thread: String,
        resolved: bool,
    },
}

impl BatchAction {
    pub fn key(&self) -> ActionKey {
        match self {
            BatchAction::Comment { key, .. }
            | BatchAction::Reply { key, .. }
            | BatchAction::Resolve { key, .. } => key.clone(),
        }
    }
}

/// The whole review to flush for one MR, forge-neutral: a verdict, an optional
/// summary body, and the ordered actions. `version` is the current snapshot —
/// the fallback anchor for a comment, and GitHub's single review `commitOID`.
///
/// `stale` carries the forge-side, unpublished in-flight ids a *prior* failed
/// attempt left behind (a GitHub pending-review id, GitLab draft-note ids), so
/// this attempt can clean them up **before** doing anything — the "delete next
/// time, keyed to what we recorded" discipline that makes retries idempotent
/// without ever touching drafts the user created by hand (§ failure handling).
#[derive(Debug, Clone)]
pub struct ReviewBatch {
    pub verdict: Option<Verdict>,
    pub summary: Option<String>,
    pub actions: Vec<BatchAction>,
    pub version: DiffVersion,
    pub stale: Vec<String>,
}

impl ReviewBatch {
    /// A batch that would post nothing.
    pub fn is_empty(&self) -> bool {
        self.verdict.is_none() && self.summary.is_none() && self.actions.is_empty()
    }
}

/// The granular result of a [`Forge::submit`] — never an `Err` for a *partial*
/// success, only for a total one (the MR was unreachable, or an atomic batch was
/// rolled back to nothing). Each action reports its own landing *by key*, so the
/// orchestration layer reconciles independently: a landed action is cleared from
/// the draft, a failed one stays for retry, and a verdict failure never poisons
/// comments already posted.
///
/// `Err` from `submit` means *nothing* landed; the caller keeps the whole draft.
///
/// [`Forge::submit`]: super::Forge::submit
#[derive(Debug, Clone, Default)]
pub struct BatchOutcome {
    /// One entry per action key attempted: `true` = live on the forge, `false` =
    /// failed or rolled back. A key absent from the map counts as not landed.
    pub landed: std::collections::HashMap<ActionKey, bool>,
    /// Whether the summary body (if any) landed.
    pub summary_ok: bool,
    /// `None` when no verdict was in the batch; `Some(true)`/`Some(false)` when
    /// it landed / failed (and stays for retry).
    pub verdict_ok: Option<bool>,
    /// How many forge notifications this submission actually produced — an
    /// honest, testable number, not a promise (a reviewer minimises it, but the
    /// platform ultimately decides).
    pub notifications: u32,
    /// Forge-side, unpublished in-flight ids this attempt created but did **not**
    /// publish (an orphaned GitHub pending review, GitLab draft notes). The
    /// caller persists these and feeds them back as [`ReviewBatch::stale`] so the
    /// *next* attempt deletes them first. Empty on a clean, fully-published run.
    pub inflight: Vec<String>,
}

impl BatchOutcome {
    /// Whether the action `key` landed on the forge.
    pub fn landed(&self, key: &str) -> bool {
        self.landed.get(key).copied().unwrap_or(false)
    }

    /// A fully-successful outcome: every listed action key, the summary, and the
    /// verdict (if present) landed, in `notifications` notification(s). Nothing
    /// left in flight.
    pub fn all_ok(
        keys: impl IntoIterator<Item = ActionKey>,
        has_verdict: bool,
        notifications: u32,
    ) -> Self {
        BatchOutcome {
            landed: keys.into_iter().map(|k| (k, true)).collect(),
            summary_ok: true,
            verdict_ok: has_verdict.then_some(true),
            notifications,
            inflight: Vec::new(),
        }
    }

    /// The outcome of an aborted attempt: *nothing* landed, so the whole draft
    /// stays for retry, and `inflight` records the forge-side ids (if any) this
    /// attempt left unpublished for the next run to clean up.
    pub fn none_landed(batch: &ReviewBatch, inflight: Vec<String>) -> Self {
        BatchOutcome {
            landed: batch.actions.iter().map(|a| (a.key(), false)).collect(),
            summary_ok: false,
            verdict_ok: batch.verdict.is_some().then_some(false),
            notifications: 0,
            inflight,
        }
    }
}
