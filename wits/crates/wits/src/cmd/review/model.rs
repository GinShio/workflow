//! The review model: the JSON types that back the three-file store and the
//! `--json` read contract.
//!
//! Every MR is described by three files (see `docs/review/store.md`), all JSON
//! because they are API-shaped data:
//!
//! - [`Info`]    — the MR's necessary metadata and diff state (drives the inbox).
//! - [`Comments`] — everything that happened on the forge (a refetchable cache).
//! - [`Local`]   — your unsubmitted actions, edited by hand or an editor; this
//!   is the sole *write* interface, so its shape is a public, versioned contract.
//!
//! There are no authoring commands: to review, you edit `local.json`, then
//! `submit` merges, posts, and clears it. Everything is versioned by [`SCHEMA`].

use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use wits_util::forge::{
    Anchor, DiffVersion, LineRef, MrState, MrSummary, RemoteComment, RemoteThread, Side, Verdict,
};

/// The origin tag the read view / `show` / `local.json` print in front of a
/// forge id (`remote:9987`). The identity is always the bare id; this prefix is
/// only a display facet, and lives here so nothing else spells the literal.
pub const REMOTE_PREFIX: &str = "remote:";

/// Tag a bare forge id with the `remote:` origin, for the read/output view.
pub fn remote_ref(id: &str) -> String {
    format!("{REMOTE_PREFIX}{id}")
}

/// A discussion thread's identity: the *bare* forge id. The read view and a
/// hand-edited `local.json` may spell it `remote:<id>`, but the identity is the
/// bare id — this newtype is the single place that truth lives, so a `remote:`
/// prefix can never leak into a forge URL or defeat an id match. Any `remote:`
/// prefix is stripped on the way in (parse, deserialize, `From<&str>`); the wire
/// and forge-facing forms are always bare, and [`ThreadId::remote_ref`] is the
/// only way to get the display form back.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadId(String);

impl ThreadId {
    /// The bare forge id — for a forge URL or an id comparison.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `remote:<id>` display form used by the read view and `show`.
    pub fn remote_ref(&self) -> String {
        remote_ref(&self.0)
    }
}

impl std::str::FromStr for ThreadId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ThreadId(
            s.strip_prefix(REMOTE_PREFIX).unwrap_or(s).to_owned(),
        ))
    }
}

impl From<&str> for ThreadId {
    fn from(s: &str) -> Self {
        s.parse().expect("ThreadId parse is infallible")
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ThreadId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ThreadId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(ThreadId::from(s.as_str()))
    }
}
use wits_util::git::Repository;

/// The store/JSON schema version. (Personal tooling — shapes change freely
/// without a bump; this stays `1`.)
pub const SCHEMA: u32 = 1;

/// Generate an opaque local action id. It is UUID-v4-shaped for interoperability,
/// but namespaced so it never looks like a forge id.
pub fn new_action_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "wits:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// The human/inbox state word, folding draft-ness into the lifecycle for a
/// one-glance label. `--json` keeps `state` and `draft` as separate typed fields
/// (below); this is only for the terse human line.
pub fn state_word(state: MrState, draft: bool) -> &'static str {
    match state {
        MrState::Open if draft => "draft",
        MrState::Open => "open",
        MrState::Merged => "merged",
        MrState::Closed => "closed",
    }
}

/// A commit in the reviewed range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCommit {
    pub sha: String,
    pub subject: String,
}

/// A file the MR touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub old_path: Option<String>,
    pub status: String,
}

/// A git range's commits (oldest first) and changed files, lifted into the
/// stored review shapes. The single place `repo.commits`/`changed_files` become
/// [`StoredCommit`]/[`StoredFile`] — shared by `fetch` (a snapshot's artifacts)
/// and `diff` (an ad-hoc range), so the mapping can't drift between them.
pub fn range_artifacts(repo: &Repository, range: &str) -> (Vec<StoredCommit>, Vec<StoredFile>) {
    let commits = repo
        .commits(range)
        .into_iter()
        .map(|c| StoredCommit {
            sha: c.hash,
            subject: c.subject,
        })
        .collect();
    let files = repo
        .changed_files(range)
        .into_iter()
        .map(|f| StoredFile {
            path: f.path,
            old_path: f.old_path,
            status: f.status.to_string(),
        })
        .collect();
    (commits, files)
}

/// `info.json` — the MR's necessary metadata and its snapshot history. A pure
/// **cache**: `fetch` overwrites it, so it is not meant to be hand-edited. A feed
/// refresh fills only `mr`, leaving the snapshot history empty until a full
/// `fetch <mr>`. `commits`/`files` describe the current (latest) snapshot.
///
/// A *snapshot* in the history is exactly a [`DiffVersion`] — the `base/start/
/// head` triple we diff against and pin (`refs/wits/review/*`), distinct from an
/// ad-hoc diff *range* (a throwaway query). When each was first seen is
/// deliberately **not** stored per-snapshot: dormancy is about the last time the
/// MR was synced, so that lives once on [`Info::fetched_at`] and is refreshed on
/// every fetch — a re-fetch of an unchanged head must not look dormant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub schema: u32,
    pub mr: MrSummary,
    /// Every review point we have fetched and pinned, oldest first; the last is
    /// the current one. Each is a `base/start/head` diff version.
    #[serde(default)]
    pub snapshots: Vec<DiffVersion>,
    /// Unix time (seconds) of the last `fetch` that synced this MR — updated on
    /// *every* fetch, even when the head hasn't moved, so `prune`'s dormancy
    /// reflects real staleness rather than when a snapshot first appeared. `0`
    /// means never fully fetched (a feed-only entry).
    #[serde(default)]
    pub fetched_at: i64,
    pub commits: Vec<StoredCommit>,
    pub files: Vec<StoredFile>,
}

impl Info {
    /// The current (latest) snapshot, if any has been fetched.
    pub fn current(&self) -> Option<&DiffVersion> {
        self.snapshots.last()
    }

    /// The current snapshot's head SHA — `None` until a full `fetch <mr>` has
    /// recorded a snapshot (a feed refresh leaves the history empty).
    pub fn head(&self) -> Option<&str> {
        self.current().map(|s| s.head_sha.as_str())
    }

    /// Record a freshly-fetched snapshot, appending it only when the head moved
    /// (so a re-fetch of an unchanged MR doesn't grow the history).
    pub fn record_snapshot(&mut self, version: DiffVersion) {
        if self.snapshots.last().map(|s| &s.head_sha) != Some(&version.head_sha) {
            self.snapshots.push(version);
        }
    }
}

/// The single source of the anchor-inference rule, shared by the read view
/// ([`Action::read_anchor`]) and submit (`build_batch`): `file`+`line` ⇒ a
/// line anchor, `file` alone ⇒ a file anchor, neither ⇒ `None` (an MR-level
/// conversation comment, which carries no code anchor). `side` defaults to
/// `New`; a multi-line `start`'s side defaults to `end`'s unless overridden.
/// `old_path` is looked up by the caller (submit knows the changed-file set;
/// the read view of a pending local comment does not, and passes `None`).
pub fn comment_anchor(
    file: Option<&str>,
    line: Option<u32>,
    side: Option<Side>,
    start_line: Option<u32>,
    start_side: Option<Side>,
    old_path: Option<String>,
) -> Option<Anchor> {
    let path = file?.to_owned();
    Some(match line {
        Some(line) => {
            let s = side.unwrap_or(Side::New);
            let end = LineRef { line, side: s };
            let start = start_line.map(|sl| LineRef {
                line: sl,
                side: start_side.unwrap_or(s),
            });
            Anchor::Line {
                path,
                old_path,
                end,
                start,
            }
        }
        None => Anchor::File { path },
    })
}

/// One comment in a thread, in the read/output view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub origin: String,
    pub body: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub created_at: String,
    pub state: String,
}

impl Comment {
    fn from_remote(c: RemoteComment) -> Self {
        Comment {
            id: remote_ref(&c.id),
            author: c.author,
            origin: "remote".into(),
            body: c.body,
            created_at: c.created_at,
            state: "published".into(),
        }
    }
}

/// A discussion thread in the read/output view. `anchor` is the code anchor
/// (absent for an MR-level conversation), serialized directly as `{"kind":…}`;
/// `commit` is the snapshot SHA the anchor was written against, and drives local
/// outdate computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub origin: String,
    pub resolved: bool,
    pub outdated: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit: Option<String>,
    pub comments: Vec<Comment>,
}

impl From<RemoteThread> for Thread {
    fn from(t: RemoteThread) -> Self {
        Thread {
            id: remote_ref(&t.id),
            origin: "remote".into(),
            resolved: t.resolved,
            outdated: t.outdated,
            anchor: t.anchor,
            commit: t.commit,
            comments: t.comments.into_iter().map(Comment::from_remote).collect(),
        }
    }
}

/// `comments.json` — the forge's discussion, as last fetched. A pure cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comments {
    pub schema: u32,
    pub threads: Vec<Thread>,
}

impl Default for Comments {
    fn default() -> Self {
        Comments {
            schema: SCHEMA,
            threads: Vec::new(),
        }
    }
}

/// One recorded action in `local.json`. Flat on purpose, so it is pleasant to
/// hand-edit: a comment infers its placement from which fields are present —
/// `file`+`line` is a line comment, `file` alone is file-level, neither is an
/// MR-level conversation comment.
///
/// A comment's `commit` records the snapshot head SHA its line anchors were
/// written against. Set by `draft <mr> -` at ingest time; a hand-editor may set
/// it explicitly. At submit, the comment is anchored to its own commit, so
/// different actions in one draft can target different snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
    Comment {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        file: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        line: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        side: Option<Side>,
        /// First line of a multi-line span (with `line` as the end). Defaults to
        /// the same side as `side` unless `start_side` overrides it.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        start_line: Option<u32>,
        /// Side of the multi-line `start_line`; defaults to `side` when absent.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        start_side: Option<Side>,
        body: String,
        /// The snapshot head SHA this comment's line anchors were written
        /// against. When set, `submit` anchors the comment to this commit
        /// (the forge may mark it outdated if the head has moved). When unset,
        /// `submit` falls back to the current snapshot's head.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        commit: Option<String>,
    },
    Summary {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        id: Option<String>,
        body: String,
    },
    Reply {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        id: Option<String>,
        thread: ThreadId,
        body: String,
    },
    Resolve {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        id: Option<String>,
        thread: ThreadId,
        resolved: bool,
    },
    Drop {
        id: String,
    },
}

impl Action {
    pub fn id(&self) -> Option<&str> {
        match self {
            Action::Comment { id, .. }
            | Action::Summary { id, .. }
            | Action::Reply { id, .. }
            | Action::Resolve { id, .. } => id.as_deref(),
            Action::Drop { id } => Some(id),
        }
    }

    pub fn id_mut(&mut self) -> Option<&mut Option<String>> {
        match self {
            Action::Comment { id, .. }
            | Action::Summary { id, .. }
            | Action::Reply { id, .. }
            | Action::Resolve { id, .. } => Some(id),
            Action::Drop { .. } => None,
        }
    }

    pub fn ensure_id(&mut self) {
        if let Some(id) = self.id_mut() {
            if id.is_none() {
                *id = Some(new_action_id());
            }
        }
    }

    /// The read-view anchor and the commit it was written against, for a comment
    /// action, via the shared [`comment_anchor`] inference. The commit is the
    /// action's own (per-comment snapshot), falling back to `head` for actions
    /// that predate per-comment stamping. Returns `(None, None)` for a non-comment
    /// action and for an MR-level comment (no code anchor).
    pub fn read_anchor(&self, head: Option<&str>) -> (Option<Anchor>, Option<String>) {
        match self {
            Action::Comment {
                file,
                line,
                side,
                start_line,
                start_side,
                commit,
                ..
            } => {
                let commit = commit.clone().or_else(|| head.map(str::to_owned));
                let anchor = comment_anchor(
                    file.as_deref(),
                    *line,
                    *side,
                    *start_line,
                    *start_side,
                    None,
                );
                (anchor, commit)
            }
            _ => (None, None),
        }
    }
}

/// `local.json` — your unsubmitted review: an optional verdict and an append-only
/// list of id-addressed actions. Edited by hand or an editor; `submit` compacts
/// and flushes it. This shape is the public write contract.
///
/// Each `Comment` action carries its own `commit` — the snapshot head SHA its
/// line anchors were written against. This makes cross-snapshot drafting safe:
/// different comments in one draft can target different snapshots, and `submit`
/// anchors each to its own commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Local {
    pub schema: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// Truncate a SHA to a display-friendly prefix (11 hex chars, like `git log`).
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(11)]
}

impl Default for Local {
    fn default() -> Self {
        Local {
            schema: SCHEMA,
            verdict: None,
            actions: Vec::new(),
        }
    }
}

impl Local {
    pub fn is_empty(&self) -> bool {
        self.verdict.is_none() && self.actions.is_empty()
    }

    pub fn summary(&self) -> Option<&str> {
        self.actions.iter().rev().find_map(|a| match a {
            Action::Summary { body, .. } => Some(body.as_str()),
            _ => None,
        })
    }

    /// Compact the append-only draft into the actions that should be submitted.
    ///
    /// All live actions get an id, later actions with the same id replace earlier
    /// ones, and `drop` removes the current live action with that id. Comments are
    /// also stamped with the current snapshot head when they do not already carry
    /// an explicit commit.
    pub fn normalize(&mut self, head: &str) {
        self.ensure_action_ids();
        self.stamp_comments(head);
        self.compact();
    }

    pub fn ensure_action_ids(&mut self) {
        for action in &mut self.actions {
            action.ensure_id();
        }
    }

    /// Anchor any unstamped comment to the current snapshot head, so hand-edited
    /// or pre-commit drafts get a `commit` before dedup and submission.
    fn stamp_comments(&mut self, head: &str) {
        if head.is_empty() {
            return;
        }
        for a in &mut self.actions {
            if let Action::Comment { commit, .. } = a {
                if commit.is_none() {
                    *commit = Some(head.to_owned());
                }
            }
        }
    }

    fn compact(&mut self) {
        let mut out: Vec<Action> = Vec::new();
        for a in self.actions.drain(..) {
            match a {
                Action::Drop { id } => {
                    out.retain(|existing| existing.id() != Some(id.as_str()));
                }
                action => {
                    if let Some(id) = action.id() {
                        let id = id.to_owned();
                        out.retain(|existing| existing.id() != Some(id.as_str()));
                    }
                    out.push(action);
                }
            }
        }
        self.actions = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(file: &str, line: u32, commit: Option<&str>) -> Action {
        Action::Comment {
            id: None,
            file: Some(file.into()),
            line: Some(line),
            side: None,
            start_line: None,
            start_side: None,
            body: "x".into(),
            commit: commit.map(str::to_owned),
        }
    }

    #[test]
    fn normalize_stamps_unstamped_comments_with_current_head() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![comment("a.c", 1, None)],
        };
        local.normalize("deadbeef");
        match &local.actions[0] {
            Action::Comment { commit, .. } => assert_eq!(commit.as_deref(), Some("deadbeef")),
            _ => panic!("expected a comment"),
        }
    }

    #[test]
    fn normalize_leaves_explicit_commit_intact() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Comment {
                    id: Some("same".into()),
                    file: Some("a.c".into()),
                    line: Some(1),
                    side: None,
                    start_line: None,
                    start_side: None,
                    body: "old".into(),
                    commit: Some("older".into()),
                },
                Action::Comment {
                    id: Some("same".into()),
                    file: Some("a.c".into()),
                    line: Some(1),
                    side: None,
                    start_line: None,
                    start_side: None,
                    body: "new".into(),
                    commit: Some("older".into()),
                },
            ],
        };
        local.normalize("deadbeef");
        // The later action with the same id replaces the earlier one, and the
        // survivor keeps its own commit,
        // not the current head.
        assert_eq!(local.actions.len(), 1);
        match &local.actions[0] {
            Action::Comment { commit, .. } => assert_eq!(commit.as_deref(), Some("older")),
            _ => panic!("expected a comment"),
        }
    }

    #[test]
    fn normalize_dedup_keeps_distinct_commits() {
        // Same file/line but different snapshots are two distinct intents —
        // both survive dedup (cross-snapshot drafting).
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                comment("a.c", 1, Some("snapA")),
                comment("a.c", 1, Some("snapB")),
            ],
        };
        local.normalize("deadbeef");
        assert_eq!(local.actions.len(), 2);
    }

    #[test]
    fn normalize_collapses_repeated_resolves_to_the_last() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: true,
                },
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: false,
                },
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: true,
                },
            ],
        };
        local.normalize("deadbeef");
        assert_eq!(local.actions.len(), 1);
        match &local.actions[0] {
            Action::Resolve {
                thread, resolved, ..
            } => {
                assert_eq!(thread.as_str(), "42");
                assert!(*resolved, "last write wins");
            }
            _ => panic!("expected a resolve"),
        }
    }

    #[test]
    fn normalize_collapses_remote_prefix_to_bare_thread_id() {
        // A draft written with `remote:42` and one written with the bare `42`
        // refer to the same thread; normalize keys them together (last write
        // wins) and stores the canonical bare form, so neither show's fold nor
        // submit ever sees a `remote:` prefix leak into the forge URL.
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "remote:42".into(),
                    resolved: false,
                },
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: true,
                },
                Action::Reply {
                    id: Some("reply".into()),
                    thread: "remote:42".into(),
                    body: "x".into(),
                },
            ],
        };
        local.normalize("deadbeef");
        // The two resolves collapse to one bare-id action, last value wins.
        let resolves: Vec<_> = local
            .actions
            .iter()
            .filter(|a| matches!(a, Action::Resolve { .. }))
            .collect();
        assert_eq!(resolves.len(), 1);
        match &resolves[0] {
            Action::Resolve {
                thread, resolved, ..
            } => {
                assert_eq!(thread.as_str(), "42");
                assert!(*resolved, "last write wins");
            }
            _ => unreachable!(),
        }
        // The reply keeps its body; its thread is normalized to bare too.
        match &local
            .actions
            .iter()
            .find(|a| matches!(a, Action::Reply { .. }))
            .unwrap()
        {
            Action::Reply { thread, .. } => assert_eq!(thread.as_str(), "42"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn normalize_drops_live_action_by_id() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Comment {
                    id: Some("c".into()),
                    file: Some("a.c".into()),
                    line: Some(1),
                    side: None,
                    start_line: None,
                    start_side: None,
                    body: "x".into(),
                    commit: None,
                },
                Action::Drop { id: "c".into() },
            ],
        };
        local.normalize("deadbeef");
        assert!(local.actions.is_empty());
    }

    #[test]
    fn normalize_keeps_latest_summary_action() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Summary {
                    id: Some("s".into()),
                    body: "old".into(),
                },
                Action::Summary {
                    id: Some("s".into()),
                    body: "new".into(),
                },
            ],
        };
        local.normalize("deadbeef");
        assert_eq!(local.summary(), Some("new"));
        assert_eq!(local.actions.len(), 1);
    }

    #[test]
    fn thread_id_strips_remote_prefix_on_every_ingress() {
        use std::str::FromStr;
        assert_eq!(ThreadId::from("remote:9987").as_str(), "9987");
        assert_eq!(ThreadId::from("9987").as_str(), "9987");
        assert_eq!(ThreadId::from_str("remote:1").unwrap().as_str(), "1");
        // Deserialized from JSON, either spelling normalizes to bare.
        let bare: ThreadId = serde_json::from_str("\"remote:42\"").unwrap();
        assert_eq!(bare.as_str(), "42");
        // And the wire form is always bare, with the display facet on demand.
        assert_eq!(serde_json::to_string(&bare).unwrap(), "\"42\"");
        assert_eq!(bare.remote_ref(), "remote:42");
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut local = Local {
            schema: SCHEMA,
            verdict: None,
            actions: vec![
                Action::Comment {
                    id: Some("c".into()),
                    file: Some("a.c".into()),
                    line: Some(1),
                    side: None,
                    start_line: None,
                    start_side: None,
                    body: "x".into(),
                    commit: Some("snap".into()),
                },
                Action::Comment {
                    id: Some("c".into()),
                    file: Some("a.c".into()),
                    line: Some(1),
                    side: None,
                    start_line: None,
                    start_side: None,
                    body: "y".into(),
                    commit: Some("snap".into()),
                },
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: true,
                },
                Action::Resolve {
                    id: Some("r".into()),
                    thread: "42".into(),
                    resolved: false,
                },
            ],
        };
        local.normalize("deadbeef");
        let once = local.actions.clone();
        local.normalize("deadbeef");
        assert_eq!(local.actions, once, "a second normalize is a no-op");
    }
}
