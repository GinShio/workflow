//! The read/ref floor: config, branches, commits, ranges, and the ref plumbing
//! (pushes, branch deletes, and the `review` object-fetch/pin refs). Reads run
//! even under a dry-run; the ref/push mutations are captured so a failure keeps
//! git's own message. See the [module overview](super).

use std::collections::HashMap;
use std::path::PathBuf;

use super::{GitError, Repository};

/// A commit's identity and its message pre-split into subject (first line) and
/// body. The split lives here because the two are almost always wanted
/// separately — a short summary versus the detail — and doing it once avoids
/// every caller re-deriving the same boundary.
#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    pub body: String,
}

/// Where one local branch stands, as [`Repository::branch_statuses`] reports it.
#[derive(Debug, Clone)]
pub struct BranchStatus {
    /// The tip commit, abbreviated the way git would.
    pub short_head: String,
    /// The configured upstream in short form (`origin/feat`), if any.
    pub upstream: Option<String>,
    /// Distance to that upstream. Both zero when in sync, when the upstream is
    /// gone, or when there is none.
    pub upstream_ahead: u32,
    pub upstream_behind: u32,
    /// An upstream is configured but its remote-tracking ref no longer exists —
    /// how a merged-and-deleted branch looks after `git fetch --prune`. Distinct
    /// from having no upstream at all: never having had one is not losing one.
    pub upstream_gone: bool,
    /// Distance to the trunk that was asked about; `None` when none was passed,
    /// or when this git is too old for `%(ahead-behind:)`.
    pub trunk_ahead: Option<u32>,
    pub trunk_behind: Option<u32>,
    pub commit_time: i64,
}

/// What is uncommitted in a working tree, per [`Repository::status_counts`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusCounts {
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
}

impl StatusCounts {
    /// Nothing uncommitted — the tree is clean (ignored files aside).
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.modified == 0 && self.untracked == 0
    }
}

/// Read `%(upstream:track,nobracket)` — `""`, `gone`, `ahead 1`, `behind 2`, or
/// `ahead 1, behind 2` — into a distance pair. `gone` and the empty case are both
/// zero: neither describes a distance.
fn parse_track(track: &str) -> (u32, u32) {
    let read = |key: &str| -> u32 {
        track
            .split(',')
            .filter_map(|part| part.trim().strip_prefix(key))
            .filter_map(|n| n.trim().parse().ok())
            .next()
            .unwrap_or(0)
    };
    (read("ahead "), read("behind "))
}

/// Read `%(ahead-behind:<ref>)`, which is `"<ahead> <behind>"`.
fn parse_ahead_behind(field: &str) -> Option<(u32, u32)> {
    let mut parts = field.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// One entry of a `diff --name-status` over a range: a file the MR touched, its
/// change kind, and its former path when the change was a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    /// The porcelain status letter — `A`dded, `M`odified, `D`eleted, `R`enamed,
    /// `C`opied. Kept as a char because that is exactly git's own vocabulary.
    pub status: char,
}

impl Repository {
    // -- reads ----------------------------------------------------------------

    /// Read a config value, or `None` when the key is unset.
    pub fn get_config(&self, key: &str) -> Result<Option<String>, GitError> {
        Ok(self.query(&["config", "--get", key]))
    }

    /// Every value of a (possibly multi-valued) config key, exactly as written.
    /// Unlike `git remote get-url`, `git config` does **not** apply
    /// `url.*.insteadOf` rewrites, so this is the lens to use when an idempotent
    /// compare must match the literal declared string (e.g. push URLs).
    pub fn get_config_all(&self, key: &str) -> Vec<String> {
        self.query(&["config", "--get-all", key])
            .map(|s| s.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The branch currently checked out, or `None` on a detached HEAD. A
    /// detached HEAD has no name to push or build on, so the absence is
    /// meaningful rather than an error.
    pub fn current_branch(&self) -> Option<String> {
        self.query(&["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    /// Resolve a revision to a full commit hash, or `None` if it doesn't exist.
    pub fn rev_parse(&self, spec: &str) -> Option<String> {
        self.query(&["rev-parse", "--verify", "--quiet", spec])
    }

    /// Whether a revision exists — the boolean form of [`rev_parse`](Self::rev_parse).
    pub fn rev_exists(&self, spec: &str) -> bool {
        self.rev_parse(spec).is_some()
    }

    /// Does a **local branch** of this name exist?
    ///
    /// Deliberately narrower than [`rev_exists`](Self::rev_exists), which also
    /// answers yes for a tag or a raw commit: the distinction is what lets a
    /// caller insist on an attached checkout rather than silently detaching.
    pub fn local_branch_exists(&self, name: &str) -> bool {
        self.rev_exists(&format!("refs/heads/{name}"))
    }

    /// Remote-tracking branches whose last path segment is `name` — so a caller
    /// asking for a local `feat` that does not exist can be pointed at the
    /// `origin/feat` that does.
    ///
    /// Lists every remote-tracking ref and filters here rather than handing git a
    /// glob, because `refs/remotes/*/<name>` would also have to cope with a remote
    /// whose own name contains a slash. Only ever called on an error path, where
    /// the extra listing costs nothing.
    pub fn remote_tracking_candidates(&self, name: &str) -> Vec<String> {
        let Some(out) = self.query(&["for-each-ref", "--format=%(refname:short)", "refs/remotes"])
        else {
            return Vec::new();
        };
        let suffix = format!("/{name}");
        out.lines()
            .filter(|refname| refname.ends_with(&suffix))
            .map(str::to_owned)
            .collect()
    }

    /// Whether the path is a git repository (inside a work tree).
    pub fn is_repo(&self) -> bool {
        self.query(&["rev-parse", "--is-inside-work-tree"])
            .as_deref()
            == Some("true")
    }

    /// Whether the path exists on disk at all — the "is there a checkout here?"
    /// question `update` asks before deciding clone-vs-refresh.
    pub fn exists(&self) -> bool {
        self.path().exists()
    }

    /// The short HEAD commit, or `None` on an unborn branch.
    pub fn head_commit(&self) -> Option<String> {
        self.query(&["rev-parse", "--short", "HEAD"])
    }

    /// The submodule gitlinks a commit pins, as `(full_sha, path)` pairs read
    /// straight from that commit's **tree object**.
    ///
    /// Because `ls-tree` reads objects, not the index or working tree, this
    /// answers for *any* `rev` (a branch you are not on) **without a checkout or
    /// a branch switch** — exactly what enumerating a branch's pinned submodules
    /// needs. `-r` walks into subdirectories, so a gitlink nested under a path
    /// (e.g. `vendor/sub`) is found; git does not descend across the submodule
    /// boundary, so these are the *direct* submodules only (recursion is the
    /// caller's job, repo by repo). Paths are relative to this repo's root.
    /// Empty when the rev pins no submodules or its objects aren't present.
    pub fn gitlinks(&self, rev: &str) -> Vec<(String, String)> {
        let Some(out) = self.query(&["ls-tree", "-r", rev]) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                // `<mode> SP <type> SP <object> TAB <path>`; a gitlink is the
                // mode-160000 / `commit` entry.
                let (meta, path) = line.split_once('\t')?;
                let mut fields = meta.split_whitespace();
                let mode = fields.next()?;
                let _type = fields.next()?;
                let object = fields.next()?;
                (mode == "160000").then(|| (object.to_owned(), path.to_owned()))
            })
            .collect()
    }

    /// Whether the working tree has uncommitted changes (tracked or untracked),
    /// **ignoring submodules**. This is the "would a branch switch or checkout
    /// disturb my work?" question — and a superproject `switch`/`checkout` never
    /// touches a submodule's working tree, so a submodule merely sitting at a
    /// different commit is not work at risk. Counting it would stash (or block a
    /// checkout) on every switch in a repo whose submodules have drifted, for
    /// nothing — the `project`/`build` flow realigns submodules explicitly right
    /// after the switch regardless.
    pub fn is_dirty(&self) -> bool {
        self.query(&["status", "--porcelain", "--ignore-submodules=all"])
            .is_some()
    }

    /// The absolute path of the `.git` directory, the natural home for a tool's
    /// own per-repository state files.
    pub fn git_dir(&self) -> Option<PathBuf> {
        self.query(&["rev-parse", "--absolute-git-dir"])
            .map(PathBuf::from)
    }

    /// The absolute path of the *common* git directory — the main `.git` shared
    /// by every linked worktree. Unlike [`git_dir`](Self::git_dir), this is stable
    /// across worktrees, so per-clone state (the review store) lands in the same
    /// place whether you run from the main checkout or a `checkout` worktree.
    pub fn git_common_dir(&self) -> Option<PathBuf> {
        self.query(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map(PathBuf::from)
    }

    /// The working tree's top-level directory, or `None` outside a work tree
    /// (e.g. a bare repo). The natural anchor for deriving a sibling worktree.
    pub fn toplevel(&self) -> Option<PathBuf> {
        self.query(&["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
    }

    /// The (fetch) URL of a named remote, or `None` if the remote is absent.
    pub fn remote_url(&self, name: &str) -> Option<String> {
        self.query(&["remote", "get-url", name])
    }

    /// Every local branch mapped to its tip commit. This is the content
    /// source-of-truth: whatever a branch points at here is what gets pushed.
    pub fn branch_tips(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let Some(out) = self.query(&[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads",
        ]) else {
            return map;
        };
        for line in out.lines() {
            if let Some((name, oid)) = line.split_once(' ') {
                map.insert(name.to_owned(), oid.to_owned());
            }
        }
        map
    }

    /// Where every local branch stands, in **one** `for-each-ref` — its
    /// abbreviated tip, its upstream and the distance to it, the distance to
    /// `trunk`, and when it was last committed to.
    ///
    /// Batched because the alternative is four or five `git` invocations *per
    /// branch* for facts git will hand over together, and because everything here
    /// must describe one consistent moment.
    ///
    /// Two details are git's judgement rather than ours. The abbreviation length
    /// comes from `%(objectname:short)`, which honours `core.abbrev` — so a hash
    /// printed here is the same one `git log --oneline` prints in this repo, and
    /// stays copy-pasteable. And the `gone` verdict comes from
    /// `%(upstream:track)`; deriving it by hand would mean re-implementing refspec
    /// mapping to turn `branch.<n>.merge` into a remote-tracking ref, and getting
    /// that wrong reads as "gone" for a branch merely configured unusually.
    ///
    /// `trunk` must name a ref that **exists**: `%(ahead-behind:)` makes the whole
    /// invocation fatal for an unresolvable one, taking every other field with it.
    /// Pass `None` when no trunk was found. The atom also needs git ≥ 2.41, so a
    /// failed query is retried without it — on an older git the trunk distances
    /// are simply absent rather than the command breaking.
    pub fn branch_statuses(&self, trunk: Option<&str>) -> HashMap<String, BranchStatus> {
        let with_trunk = trunk.map(|t| format!("%09%(ahead-behind:{t})"));
        let base = "%(refname:short)%09%(objectname:short)%09%(upstream:short)\
                    %09%(upstream:track,nobracket)%09%(committerdate:unix)";

        let out = with_trunk
            .as_deref()
            .and_then(|tail| {
                self.query(&[
                    "for-each-ref",
                    &format!("--format={base}{tail}"),
                    "refs/heads",
                ])
            })
            .or_else(|| self.query(&["for-each-ref", &format!("--format={base}"), "refs/heads"]));
        let Some(out) = out else {
            return HashMap::new();
        };

        let mut map = HashMap::new();
        for line in out.lines() {
            let mut fields = line.split('\t');
            let (Some(name), Some(short_head), Some(upstream), Some(track), Some(when)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let (upstream_ahead, upstream_behind) = parse_track(track);
            let (trunk_ahead, trunk_behind) = match fields.next().map(parse_ahead_behind) {
                Some(Some((ahead, behind))) => (Some(ahead), Some(behind)),
                _ => (None, None),
            };
            map.insert(
                name.to_owned(),
                BranchStatus {
                    short_head: short_head.to_owned(),
                    upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
                    upstream_ahead,
                    upstream_behind,
                    upstream_gone: track.trim() == "gone",
                    trunk_ahead,
                    trunk_behind,
                    commit_time: when.trim().parse().unwrap_or(0),
                },
            );
        }
        map
    }

    /// Commits `rev` has that `base` lacks, and the reverse — for a **detached**
    /// worktree, whose HEAD is not a branch and so never appears in
    /// [`branch_statuses`](Self::branch_statuses).
    pub fn ahead_behind(&self, rev: &str, base: &str) -> Option<(u32, u32)> {
        // `A...B` with `--left-right` counts each side's exclusive commits: left
        // is `base`-only (how far `rev` is behind), right is `rev`-only (ahead).
        let out = self.query(&[
            "rev-list",
            "--count",
            "--left-right",
            &format!("{base}...{rev}"),
        ])?;
        let mut parts = out.split_whitespace();
        let behind = parts.next()?.parse().ok()?;
        let ahead = parts.next()?.parse().ok()?;
        Some((ahead, behind))
    }

    /// The abbreviated form of `rev`, as git would print it (`core.abbrev`).
    pub fn short_rev(&self, rev: &str) -> Option<String> {
        self.query(&["rev-parse", "--short", rev])
    }

    /// What is uncommitted in the working tree, split the way a person would
    /// describe it. Submodules are excluded for the same reason
    /// [`is_dirty`](Self::is_dirty) excludes them.
    ///
    /// Ignored files are absent (git omits them without `--ignored`), which is
    /// what makes build output not count as work in progress.
    pub fn status_counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();
        let Some(out) = self.query(&["status", "--porcelain", "--ignore-submodules=all"]) else {
            return counts;
        };
        for line in out.lines() {
            // Porcelain v1: two status columns, `X` for the index and `Y` for the
            // working tree, and the special `??` for untracked. One file can be
            // both staged and modified (`MM`), and is counted in both.
            let mut chars = line.chars();
            let (Some(index), Some(worktree)) = (chars.next(), chars.next()) else {
                continue;
            };
            if index == '?' && worktree == '?' {
                counts.untracked += 1;
                continue;
            }
            if index != ' ' {
                counts.staged += 1;
            }
            if worktree != ' ' {
                counts.modified += 1;
            }
        }
        counts
    }

    /// Is `ancestor` reachable from `descendant` — i.e. has that work already
    /// landed there? A revision that doesn't resolve answers `false`, since an
    /// unanswerable question must not read as "yes, safe to discard".
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.succeeds(&["merge-base", "--is-ancestor", ancestor, descendant])
    }

    /// The commit time of `rev` as a Unix instant.
    pub fn commit_time(&self, rev: &str) -> Option<i64> {
        self.query(&["log", "-1", "--format=%ct", rev])?
            .trim()
            .parse()
            .ok()
    }

    /// The default branch a remote points its HEAD at (e.g. `main`), read from
    /// the locally-tracked `refs/remotes/<remote>/HEAD` symref. Returns `None`
    /// when that symref hasn't been established (a fresh clone may lack it until
    /// `git remote set-head`), letting the caller fall through to its next guess.
    pub fn remote_default_branch(&self, remote: &str) -> Option<String> {
        let symref = format!("refs/remotes/{remote}/HEAD");
        let target = self.query(&["symbolic-ref", "--quiet", &symref])?;
        let prefix = format!("refs/remotes/{remote}/");
        target.strip_prefix(&prefix).map(str::to_owned)
    }

    /// Commits in `range` (e.g. `main..feature`), oldest first.
    ///
    /// Subject and body are separated with control characters rather than
    /// newlines because a commit body is itself multi-line; the unit/record
    /// separators (`0x1f`/`0x1e`) can't occur in a message, so parsing stays
    /// unambiguous no matter how the author formatted things.
    pub fn commits(&self, range: &str) -> Vec<Commit> {
        let Some(out) = self.query(&[
            "log",
            "--reverse",
            "--pretty=format:%H%x1f%s%x1f%b%x1e",
            range,
        ]) else {
            return Vec::new();
        };

        out.split('\u{1e}')
            .map(str::trim)
            .filter(|record| !record.is_empty())
            .filter_map(|record| {
                let mut fields = record.splitn(3, '\u{1f}');
                let hash = fields.next()?.trim().to_owned();
                let subject = fields.next().unwrap_or("").trim().to_owned();
                let body = fields.next().unwrap_or("").trim().to_owned();
                Some(Commit {
                    hash,
                    subject,
                    body,
                })
            })
            .collect()
    }

    /// The files a range (`base..head`) touched, rename-aware. Empty when the
    /// range can't be computed (e.g. the base object isn't present locally),
    /// which the caller treats as "unknown" rather than "nothing changed".
    pub fn changed_files(&self, range: &str) -> Vec<FileChange> {
        let Some(out) = self.query(&["diff", "--name-status", "-M", range]) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let status = fields.next()?.chars().next()?;
                match status {
                    'R' | 'C' => {
                        let old = fields.next()?.to_owned();
                        let new = fields.next()?.to_owned();
                        Some(FileChange {
                            path: new,
                            old_path: Some(old),
                            status,
                        })
                    }
                    _ => Some(FileChange {
                        path: fields.next()?.to_owned(),
                        old_path: None,
                        status,
                    }),
                }
            })
            .collect()
    }

    /// A textual diff for a range, optionally narrowed to one path — the
    /// `diff --patch` convenience for a terminal or for debugging, never the
    /// editor's render path.
    pub fn diff_patch(&self, range: &str, path: Option<&str>) -> Option<String> {
        let mut args = vec!["diff", range];
        if let Some(p) = path {
            args.push("--");
            args.push(p);
        }
        self.query(&args)
    }

    // -- ref & history mutations (captured) -----------------------------------

    /// Force-push a branch to a remote, but refuse to overwrite commits the
    /// remote has that we don't (`--force-with-lease`). History-editing
    /// workflows make non-fast-forward pushes routine, so a plain force is the
    /// reflex — yet plain force will happily discard a push someone else made.
    /// The lease keeps the legitimate "I rewrote my own branch" case working
    /// while failing closed when the remote moved underneath us. Mutating, so
    /// dry-run prints rather than pushes.
    pub fn push_force_with_lease(&self, remote: &str, branch: &str) -> Result<(), GitError> {
        self.capture(
            format!("push {branch} -> {remote}"),
            &["push", remote, branch, "--force-with-lease"],
            false,
        )
    }

    /// Create local branch `name` at `start`, tracking it.
    ///
    /// `start` is normally a remote-tracking ref, which is what makes `--track`
    /// worth asking for: the branch then knows its upstream, so `%(upstream:track)`
    /// — and everything built on it, from `worktree info` to `stack` — can say how
    /// far it has drifted. A branch that already exists is left exactly as it is;
    /// this is how a repository *acquires* a branch, never how one is moved.
    pub fn create_tracking_branch(&self, name: &str, start: &str) -> Result<(), GitError> {
        if self.local_branch_exists(name) {
            return Ok(());
        }
        self.capture(
            format!("create branch {name} at {start}"),
            &["branch", "--track", name, start],
            false,
        )
    }

    /// Point HEAD at a local branch without touching a working tree.
    ///
    /// For a **bare** repository this is the only way to set HEAD at all, and it
    /// matters more there than it looks: the symbolic HEAD is the repository's own
    /// branch as far as `git worktree add` is concerned, and it is what the
    /// worktree policy reads to decide which live checkout a new worktree
    /// inherits its sparse patterns from.
    pub fn set_head(&self, branch: &str) -> Result<(), GitError> {
        self.capture(
            format!("point HEAD at {branch}"),
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
            false,
        )
    }

    /// Record which branch a remote's HEAD points at
    /// (`git remote set-head --auto`) — the symref
    /// [`remote_default_branch`](Self::remote_default_branch) reads, and so what
    /// trunk detection ultimately rests on.
    pub fn remote_set_head(&self, remote: &str) -> Result<(), GitError> {
        self.capture(
            format!("record {remote}'s default branch"),
            &["remote", "set-head", remote, "--auto"],
            false,
        )
    }

    /// Advance local branch `branch` to `target` **without a checkout**, refusing
    /// anything that is not a fast-forward.
    ///
    /// The ref-only counterpart to `merge --ff-only`, for a branch no worktree
    /// holds — a bare repository whose main worktree has been reclaimed, most
    /// often. Refusing a non-fast-forward is the same guarantee the merge gives:
    /// commits are only ever added, so an `update` cannot discard local work, and
    /// a branch that has genuinely diverged says so instead of being silently
    /// reset.
    pub fn fast_forward_branch(&self, branch: &str, target: &str) -> Result<(), GitError> {
        let refname = format!("refs/heads/{branch}");
        let Some(target_oid) = self.rev_parse(target) else {
            return Err(GitError::Failed {
                operation: format!("fast-forward {branch}"),
                message: format!("'{target}' does not name a commit"),
            });
        };
        if self.rev_exists(&refname) && !self.is_ancestor(&refname, &target_oid) {
            return Err(GitError::Failed {
                operation: format!("fast-forward {branch}"),
                message: format!(
                    "{branch} holds commits {target} does not; advancing it would discard them"
                ),
            });
        }
        self.capture(
            format!("advance {branch} to {target}"),
            &["update-ref", &refname, &target_oid],
            false,
        )
    }

    /// Delete a local branch. Without `force` this is `git branch -d`, which
    /// refuses to drop a branch that isn't merged — the safety we want by
    /// default; `force` escalates to `-D`. Mutating, so dry-run prints.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError> {
        let flag = if force { "-D" } else { "-d" };
        self.capture(
            format!("delete branch {name}"),
            &["branch", flag, name],
            false,
        )
    }

    // The `review` acquisition refs: fetch an MR's objects and hold them alive
    // with our own `refs/wits/review/*` pins. These run even under dry-run (like
    // every other read) — pinning a ref is local bookkeeping, not a change to the
    // remote or the user's branches.

    /// Fetch a remote ref into a local ref, forcing the update. Used to pull an
    /// MR head (`refs/pull/<n>/head`) into a `refs/wits/review/*` pin.
    pub fn fetch_ref(
        &self,
        remote: &str,
        remote_ref: &str,
        local_ref: &str,
    ) -> Result<(), GitError> {
        self.capture(
            format!("fetch {remote_ref} from {remote}"),
            &[
                "fetch",
                "--no-tags",
                remote,
                &format!("+{remote_ref}:{local_ref}"),
            ],
            true,
        )
    }

    /// Best-effort fetch of a bare object (e.g. an MR's base SHA, which may not
    /// be an ancestor of the head we already pulled) into a local ref. Servers
    /// that forbid fetching an arbitrary SHA make this fail; that is fine — the
    /// caller treats the object as simply unavailable.
    pub fn try_fetch_object(&self, remote: &str, sha: &str, local_ref: &str) -> bool {
        self.capture(
            format!("fetch object {sha}"),
            &["fetch", "--no-tags", remote, &format!("+{sha}:{local_ref}")],
            true,
        )
        .is_ok()
    }

    /// Point a ref at an object (our own `refs/wits/review/*` bookkeeping).
    pub fn update_ref(&self, name: &str, target: &str) -> Result<(), GitError> {
        self.capture(
            format!("update-ref {name}"),
            &["update-ref", name, target],
            true,
        )
    }

    /// Delete a ref. Mutating on purpose — this is `prune`'s cleanup, which a
    /// `-n` run should preview rather than perform.
    pub fn delete_ref(&self, name: &str) -> Result<(), GitError> {
        self.capture(
            format!("delete ref {name}"),
            &["update-ref", "-d", name],
            false,
        )
    }

    /// Every ref under `prefix` (e.g. `refs/wits/review/`) mapped to its target
    /// object id. The record of which snapshots we have pinned.
    pub fn refs_under(&self, prefix: &str) -> Vec<(String, String)> {
        let Some(out) = self.query(&["for-each-ref", "--format=%(refname) %(objectname)", prefix])
        else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(name, oid)| (name.to_owned(), oid.to_owned()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Command;

    /// Spin up a throwaway repo with one commit on `main` so ref/commit reads
    /// have something real to look at. `force_run` because tests share the
    /// global dry-run flag and run in parallel.
    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir.path())
                .force_run()
                .exec()
                .unwrap();
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "root"]);
        dir
    }

    #[test]
    fn reads_a_set_value_and_reports_missing_as_none() {
        let _guard = crate::log::test_flag_guard();
        let dir = init_repo();
        Command::new("git")
            .args(["config", "wits.transcrypt.password", "hunter2"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();

        let repo = Repository::new(dir.path());
        assert_eq!(
            repo.get_config("wits.transcrypt.password").unwrap(),
            Some("hunter2".to_owned())
        );
        assert_eq!(repo.get_config("wits.transcrypt.absent").unwrap(), None);
    }

    #[test]
    fn reports_current_branch_and_branch_tips() {
        let _guard = crate::log::test_flag_guard();
        let dir = init_repo();
        let repo = Repository::new(dir.path());

        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        let tips = repo.branch_tips();
        assert!(tips.contains_key("main"));
        assert_eq!(tips["main"], repo.rev_parse("main").unwrap());
    }

    #[test]
    fn commits_split_subject_and_body_oldest_first() {
        let _guard = crate::log::test_flag_guard();
        let dir = init_repo();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir.path())
                .force_run()
                .exec()
                .unwrap();
        };
        run(&[
            "commit",
            "--allow-empty",
            "-m",
            "first subject\n\nfirst body line",
        ]);
        run(&["commit", "--allow-empty", "-m", "second subject"]);

        let repo = Repository::new(dir.path());
        let commits = repo.commits("main~2..main");
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "first subject");
        assert_eq!(commits[0].body, "first body line");
        assert_eq!(commits[1].subject, "second subject");
        assert_eq!(commits[1].body, "");
    }

    #[test]
    fn gitlinks_reads_pinned_submodules_from_the_tree() {
        let _guard = crate::log::test_flag_guard();
        let sub = init_repo(); // the submodule's source repo
        let sup = init_repo(); // the superproject
        let run_in = |dir: &std::path::Path, args: &[&str]| {
            Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        // Adding a submodule from a local path needs the file protocol, which
        // modern git disables by default (CVE-2022-39253).
        run_in(
            sup.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub.path().to_str().unwrap(),
                "vendor/sub",
            ],
        );
        run_in(sup.path(), &["commit", "-m", "add sub"]);

        let repo = Repository::new(sup.path());
        // Read from a ref while HEAD stays put — no checkout, no switch.
        let links = repo.gitlinks("HEAD");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].1, "vendor/sub",
            "path is relative to the super root"
        );
        assert_eq!(
            links[0].0,
            Repository::new(sub.path()).rev_parse("HEAD").unwrap(),
            "the pinned sha is the submodule's own HEAD"
        );

        // A commit with no submodules yields nothing (not an error).
        assert!(repo.gitlinks("HEAD~1").is_empty());
    }

    #[test]
    fn dirty_tracks_superproject_changes() {
        let _guard = crate::log::test_flag_guard();
        let dir = init_repo();
        let repo = Repository::new(dir.path());
        assert!(!repo.is_dirty(), "a fresh committed tree is clean");
        std::fs::write(dir.path().join("scratch.txt"), "x").unwrap();
        assert!(repo.is_dirty(), "an untracked file makes it dirty");
    }
}
