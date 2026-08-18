//! Worktree policy: where a worktree belongs, how it is materialised, and when
//! it may be reclaimed.
//!
//! [`crate::git`] is the porcelain — one call, one `git` invocation. The
//! decisions sit here, because they are decisions rather than plumbing, and
//! because two callers make the same ones: `wits worktree` drives them from the
//! command line, and `wits review checkout` materialises an MR into a worktree
//! the same way.
//!
//! Three things are worth knowing before reading:
//!
//! **Sparse checkout is git's job, not ours.** Since git 2.36 (`worktree: copy
//! sparse-checkout patterns and config on add`) `git worktree add` copies the
//! sparse-checkout pattern file and `config.worktree` into the new worktree
//! itself — in cone *and* non-cone mode, and even under `--no-checkout`. So
//! nothing here reads or replays patterns: git's copy is the file verbatim, where
//! anything we replayed would be a round-trip through `sparse-checkout
//! list`/`set` that can only lose fidelity.
//!
//! **Which worktree drives the add decides what it inherits.** git copies the
//! patterns of the worktree the `add` ran *from*. A normal repo uses its main
//! worktree; a bare repo uses the live worktree holding its symbolic HEAD branch
//! (normally the project bootstrap), then any live linked worktree, falling back
//! to the bare common dir only for the first add. It never depends on the
//! caller's cwd.
//!
//! **Submodules are the part git does not do.** A linked worktree shares no
//! submodule object store with the primary, so a naive `submodule update --init`
//! re-clones every submodule from scratch. [`sync_submodules`] instead borrows
//! objects from a store the *repository* owns on first materialisation, and
//! afterwards just follows the pins. A conventional clone already has such a
//! store (git's `<common>/modules/<name>`, filled by the main worktree); a bare
//! repository has no primary to fill it, so [`shared_store`] creates one at the
//! same path the first time a worktree needs it — *before* that worktree
//! materialises anything, so the first checkout borrows on the same terms as
//! every later one and the repository never holds two copies of one submodule.
//!
//! That ordering is why the walk over a nested submodule tree is **ours** rather
//! than `--recursive`'s: each level borrows from a different store, and a level
//! git materialises before its store exists downloads a full copy that nothing
//! afterwards can reclaim.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::git::{self, BranchStatus, Repository, StatusCounts, Submodule};

/// A worktree plus the facts that decide whether it can be reclaimed.
///
/// Gathered together by [`Inventory::gather`] rather than queried per-field, so a
/// listing and the sweep that follows it judge the same snapshot.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    /// The checked-out branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    pub head: Option<String>,
    /// HEAD abbreviated the way git would (`core.abbrev`), so it matches what
    /// `git log --oneline` prints in this repo and stays copy-pasteable.
    pub short_head: Option<String>,
    /// The repository's own working tree, as opposed to a linked worktree.
    pub is_main: bool,
    /// A bare repository — it has no working tree, so it is not a checkout you
    /// could have work in. Distinct from a detached HEAD, which also has no branch.
    pub bare: bool,
    /// The worktree this command is running *from*. Removing it would delete the
    /// ground under the caller's shell, and git refuses anyway.
    pub is_current: bool,
    /// Locked against automatic pruning (`git worktree lock`).
    pub locked: bool,
    pub lock_reason: Option<String>,
    /// The directory is gone; only git's administrative record remains.
    pub prunable: bool,
    /// What is uncommitted here. Always empty for a [`prunable`](Self::prunable)
    /// entry, whose working tree cannot be read at all — its *commits* still can,
    /// so every other field below is still answered.
    pub changes: StatusCounts,
    /// Commits this worktree's HEAD has that the trunk lacks, and the reverse.
    /// `None` when no trunk was found, so an unanswerable question never reads as
    /// "safe to discard".
    pub trunk_ahead: Option<u32>,
    pub trunk_behind: Option<u32>,
    /// The remote-tracking branch this one follows, and the distance to it.
    pub tracking: Option<String>,
    pub tracking_ahead: u32,
    pub tracking_behind: u32,
    /// The branch had an upstream and the remote-tracking ref is gone, which is
    /// how a merged-and-deleted branch looks after `git fetch --prune`.
    pub upstream_gone: bool,
    /// HEAD's commit time, the signal for how long this work has sat still.
    pub head_time: Option<i64>,
}

impl Entry {
    /// A short, stable handle for this worktree: its directory name, which is
    /// also what git files its administrative record under.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Has this work already landed on the trunk? True only when the distance is
    /// known *and* zero — an unknown trunk leaves this `false`.
    pub fn merged(&self) -> bool {
        self.trunk_ahead == Some(0)
    }

    /// Would removing this destroy anything? Only uncommitted changes and
    /// untracked files can be lost; commits and the branch always survive.
    pub fn dirty(&self) -> bool {
        !self.changes.is_clean()
    }
}

/// The extra facts a **panel** shows, each costing its own read, so they are not
/// gathered for a listing that would throw most of them away.
#[derive(Debug, Clone, Default)]
pub struct Details {
    /// The active sparse-checkout cone; empty when the worktree is not sparse.
    pub sparse: Vec<String>,
    /// Submodules present in this (possibly sparse) checkout, and how many of
    /// them have actually been initialised.
    pub submodules: usize,
    pub submodules_initialised: usize,
}

/// Read the panel-only facts for one worktree.
///
/// A worktree with no working tree to inspect — bare, or whose directory is
/// gone — has none of these, and is answered with an empty [`Details`] rather
/// than by asking git about a path that isn't there.
pub fn details(entry: &Entry) -> Details {
    if entry.bare || entry.prunable {
        return Details::default();
    }
    let wt = Repository::new(&entry.path);
    let submodules = wt.materialised_submodules();
    Details {
        sparse: if wt.is_sparse() {
            wt.sparse_list()
        } else {
            Vec::new()
        },
        submodules_initialised: submodules
            .iter()
            .filter(|sub| entry.path.join(&sub.path).join(".git").exists())
            .count(),
        submodules: submodules.len(),
    }
}

/// Every worktree of one repository, with the trunk they are judged against.
pub struct Inventory {
    entries: Vec<Entry>,
    trunk: Option<String>,
}

impl Inventory {
    /// Read the repository's worktrees and derive each one's reclaim facts.
    ///
    /// One pass, because a `prune` must act on exactly the state its `info`
    /// showed. Costs a handful of `git` invocations per worktree, which is
    /// nothing next to the human deciding what to delete.
    pub fn gather(repo: &Repository) -> Inventory {
        let trunk = trunk_rev(repo);
        // One query answers every branch's position. A worktree on a branch — the
        // overwhelming majority — then needs no per-worktree ref reads at all;
        // only a detached HEAD, which is in no ref, is looked up individually.
        let statuses = repo.branch_statuses(trunk.as_deref());
        // Through the filesystem, because git spells the same directory two ways
        // (a symlinked parent, `/tmp` vs `/private/tmp`) depending on how it was
        // reached. Both directions of a mismatch here are safe — a missed match
        // means git refuses the removal, an extra one means a worktree is left
        // alone — so unlike `is_main` this can rest on a path comparison.
        let current = repo.toplevel().and_then(|p| std::fs::canonicalize(p).ok());
        // The corrected path for the main entry. `git worktree list` reports the
        // main worktree's *git-dir* rather than its working tree when this
        // repository is itself a **submodule** (`<super>/.git/modules/<name>`),
        // which would otherwise surface as the repo's path here and send anyone
        // following it into `.git`. `None` only for a bare repository, which has
        // no working tree and whose git-dir git already reports correctly.
        let main_tree = repo.main_worktree();

        let entries = repo
            .worktrees()
            .into_iter()
            .enumerate()
            .map(|(index, wt)| {
                // git lists the main worktree first, then the linked ones
                // (git-worktree(1), "list"). Position is the reliable test:
                // comparing `main_worktree()` against this path would compare two
                // strings git derived by different routes — `--show-toplevel` here
                // versus the common git-dir's parent from inside a linked
                // worktree — and a symlinked parent makes those differ textually
                // for the same directory. That mismatch would clear `is_main`,
                // and the immunity in `Filter::reason` with it.
                let is_main = index == 0;
                let path = match (is_main, &main_tree) {
                    (true, Some(tree)) => tree.clone(),
                    _ => wt.path,
                };
                let is_current = match (&current, std::fs::canonicalize(&path)) {
                    (Some(here), Ok(there)) => *here == there,
                    _ => false,
                };
                // Only the *working tree* is unreadable for a prunable entry; its
                // commits are still in the repository, so every position fact
                // below is still answered. `Filter::reason` never selects such an
                // entry regardless — this is about not lying in a listing.
                let changes = if wt.prunable {
                    StatusCounts::default()
                } else {
                    Repository::new(&path).status_counts()
                };

                // Judge the *branch* where there is one — it may have moved on
                // since this worktree's HEAD was recorded.
                let status = wt.branch.as_deref().and_then(|b| statuses.get(b));
                let position = match (status, &wt.head) {
                    (Some(status), _) => status.clone(),
                    // Detached: not in any ref, so ask about the commit directly.
                    (None, Some(head)) => detached_status(repo, head, trunk.as_deref()),
                    (None, None) => BranchStatus {
                        short_head: String::new(),
                        upstream: None,
                        upstream_ahead: 0,
                        upstream_behind: 0,
                        upstream_gone: false,
                        trunk_ahead: None,
                        trunk_behind: None,
                        commit_time: 0,
                    },
                };

                Entry {
                    is_main,
                    is_current,
                    changes,
                    short_head: (!position.short_head.is_empty()).then_some(position.short_head),
                    trunk_ahead: position.trunk_ahead,
                    trunk_behind: position.trunk_behind,
                    tracking: position.upstream,
                    tracking_ahead: position.upstream_ahead,
                    tracking_behind: position.upstream_behind,
                    upstream_gone: position.upstream_gone,
                    head_time: (position.commit_time > 0).then_some(position.commit_time),
                    path,
                    branch: wt.branch,
                    head: wt.head,
                    bare: wt.bare,
                    locked: wt.locked,
                    lock_reason: wt.lock_reason,
                    prunable: wt.prunable,
                }
            })
            .collect();

        Inventory { entries, trunk }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The revision worktrees are judged merged against, when one was found.
    pub fn trunk(&self) -> Option<&str> {
        self.trunk.as_deref()
    }

    /// Find the worktree a user named. A target may be its **path**, its
    /// **branch**, or its **directory name** — all three are how one naturally
    /// refers to a worktree, and accepting only one of them would mean
    /// remembering which.
    ///
    /// An ambiguous target is an error rather than a guess: silently picking one
    /// of two worktrees is the wrong failure mode for a command that deletes.
    pub fn resolve(&self, target: &str) -> Result<&Entry> {
        // Compare paths through the filesystem so a relative path, a symlinked
        // parent, or a trailing slash still matches git's absolute answer. A
        // target that isn't a path at all simply fails to canonicalise and falls
        // through to the name comparisons.
        let as_path = std::fs::canonicalize(target).ok();
        let matches: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| {
                if let Some(path) = &as_path {
                    if std::fs::canonicalize(&entry.path).is_ok_and(|real| &real == path) {
                        return true;
                    }
                }
                entry.branch.as_deref() == Some(target) || entry.name() == target
            })
            .collect();

        match matches.as_slice() {
            [one] => Ok(one),
            [] => bail!("no worktree matches '{target}' (try its path, branch, or directory name)"),
            many => {
                let paths: Vec<String> =
                    many.iter().map(|e| e.path.display().to_string()).collect();
                bail!(
                    "'{target}' is ambiguous — it matches {}: name one by path",
                    paths.join(", ")
                )
            }
        }
    }
}

/// Which worktrees a listing shows, or a sweep reclaims.
///
/// One type for both so that `info --merged` is a faithful preview of what
/// `prune --merged` would remove, instead of two predicates that can drift.
/// Every field is opt-in; an all-false filter selects nothing, and it is the
/// caller's job to decide what a bare `prune` should default to.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter {
    /// Already landed on the trunk.
    pub merged: bool,
    /// The branch's upstream is gone from the remote.
    pub gone: bool,
    /// HEAD's commit predates this Unix instant.
    pub dormant_before: Option<i64>,
}

impl Filter {
    /// Does this filter ask for anything?
    pub fn is_empty(&self) -> bool {
        !self.merged && !self.gone && self.dormant_before.is_none()
    }

    /// Why `entry` is selected, as the word to show a user, or `None` when it
    /// isn't.
    ///
    /// Four kinds of worktree are never selected, however the filter is set.
    /// The **main** worktree is the repository itself. The **current** one is
    /// where the caller is standing. A **locked** one was pinned deliberately,
    /// which is exactly the instruction not to reclaim it. And a **prunable** one
    /// has no directory left to remove — its stale record is `git worktree
    /// prune`'s business, which [`prune_records`] handles separately.
    ///
    /// This is the single gate every sweep passes through, so immunity cannot be
    /// forgotten by one caller and remembered by another.
    pub fn reason(&self, entry: &Entry) -> Option<&'static str> {
        if entry.is_main || entry.is_current || entry.locked || entry.prunable {
            return None;
        }
        if self.merged && entry.merged() {
            return Some("merged");
        }
        if self.gone && entry.upstream_gone {
            return Some("upstream gone");
        }
        if let Some(before) = self.dormant_before {
            if entry.head_time.is_some_and(|time| time < before) {
                return Some("dormant");
            }
        }
        None
    }
}

/// The stable directory to drive git from, and to hang sibling worktrees off:
/// the main worktree's working tree where there is one.
///
/// For a **bare** repository there is none, and the common git-dir *is* the
/// repository — so that is the anchor, and sibling worktrees land beside
/// `<repo>.git`. This must come before the toplevel fallback: inside a linked
/// worktree of a bare repo there *is* a toplevel, but it is that worktree, not
/// the repository.
pub fn anchor(repo: &Repository) -> PathBuf {
    repo.main_worktree()
        .or_else(|| repo.git_common_dir())
        .or_else(|| repo.toplevel())
        .unwrap_or_else(|| repo.path().to_path_buf())
}

/// A worktree location beside the main one: `<parent>/<main-name>.<suffix>`.
///
/// Anchored to the *main* worktree rather than the caller's cwd so it names the
/// same directory from anywhere — and so linked worktrees can never nest inside
/// one another. `wits worktree` passes a branch slug as the suffix; `review
/// checkout` passes `review`.
pub fn sibling_dir(main_worktree: &Path, suffix: &str) -> PathBuf {
    let parent = main_worktree.parent().unwrap_or(main_worktree);
    let name = main_worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    parent.join(format!("{name}.{suffix}"))
}

/// A branch name reduced to one path component, for [`sibling_dir`]'s suffix.
///
/// Maps anything outside `[A-Za-z0-9._-]` to `_`, matching how `project` slugs a
/// branch for its own path templates, so one branch reads the same across the
/// toolset.
pub fn slug(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Add a worktree at `dir` checked out on `rev`, creating any missing parent
/// directories.
///
/// `rev` may be a branch (the worktree tracks it) or any other revision (a
/// detached HEAD), but it **must already exist**. Sparse patterns come along on
/// their own; see the module docs for how the stable add source is chosen.
///
/// The existence check is the contract, not a convenience: handed a name that
/// resolves to nothing, `git worktree add` *invents* a local branch — from a
/// same-named remote branch if it finds one, otherwise from the target directory's
/// name. Creating a worktree and creating a branch are different acts, and a
/// caller that asked for one must not silently get both.
///
/// Submodules are *not* touched — a caller that wants them calls
/// [`sync_submodules`] next. Keeping them apart is what lets a caller materialise
/// a checkout cheaply now and fill in its submodules later, which is exactly what
/// `review checkout --submodules` does as a second pass.
pub fn create(repo: &Repository, dir: &Path, rev: &str) -> Result<()> {
    if !repo.rev_exists(rev) {
        bail!("'{rev}' does not name an existing commit, branch, or tag");
    }
    create_known(repo, dir, rev)
}

/// Add a worktree for a revision the caller has already established.
///
/// The project clone lifecycle needs this narrower entry point because under
/// dry-run the preceding bare clone is planned rather than executed, so the
/// branch cannot yet be queried. Ordinary callers should use [`create`], which
/// verifies the revision before reaching here.
pub fn create_known(repo: &Repository, dir: &Path, rev: &str) -> Result<()> {
    // The `git worktree add` below runs from a stable source rather than the
    // caller's cwd, so a relative `dir` could land somewhere the user did not
    // ask for, and somewhere
    // other than where the `create_dir_all` beneath (which resolves against the
    // process cwd) had put its parents. Absolutising first makes every path here
    // mean one thing. `absolute` is purely lexical, so it works for a directory
    // that does not exist yet.
    let dir = std::path::absolute(dir)
        .with_context(|| format!("resolving worktree path {}", dir.display()))?;

    // `git worktree add` will not create intermediate directories, so a target
    // two levels out needs them first. This is the one filesystem write here, so
    // it carries its own dry-run guard — `Repository`'s git calls have theirs.
    if let Some(parent) = dir.parent() {
        if crate::log::is_dry_run() {
            crate::log::dry_run(&format!("mkdir -p {}", parent.display()));
        } else {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    Repository::new(add_source(repo))
        .worktree_add(&dir, rev)
        .with_context(|| format!("adding worktree at {}", dir.display()))?;
    Ok(())
}

/// The checkout whose per-worktree sparse configuration a new worktree should
/// inherit. A normal repository uses its primary working tree. A bare-backed
/// repository has no primary, so prefer the worktree holding the bare symbolic
/// HEAD branch — normally the project bootstrap — before another live worktree
/// and finally the bare common directory itself.
fn add_source(repo: &Repository) -> PathBuf {
    if let Some(main) = repo.main_worktree() {
        return main;
    }
    let head = repo.current_branch();
    let worktrees = repo.worktrees();
    if let Some(worktree) = worktrees
        .iter()
        .find(|wt| {
            !wt.bare && !wt.prunable && wt.path.exists() && wt.branch.as_deref() == head.as_deref()
        })
        .or_else(|| {
            worktrees
                .iter()
                .find(|wt| !wt.bare && !wt.prunable && wt.path.exists())
        })
    {
        return worktree.path.clone();
    }
    repo.git_common_dir()
        .or_else(|| repo.toplevel())
        .unwrap_or_else(|| repo.path().to_path_buf())
}

/// Move an existing worktree's HEAD to `rev`.
///
/// Refuses a worktree with uncommitted tracked changes: moving HEAD would bury
/// them. Untracked files (build output, most often) are left alone, which is what
/// makes re-pointing one worktree cheaper than making another.
///
/// A `rev` naming a **branch already checked out in another worktree** is refused
/// by git, which names the worktree holding it. That message is better than one
/// we could compose, so there is no pre-check here.
///
/// As with [`create`], submodules are the caller's next step via
/// [`sync_submodules`].
pub fn repoint(dir: &Path, rev: &str) -> Result<()> {
    let wt = Repository::new(dir);
    if wt.is_dirty() {
        bail!(
            "worktree {} has uncommitted changes; commit or stash them before moving its HEAD",
            dir.display()
        );
    }
    wt.checkout(rev)
        .with_context(|| format!("checking out {rev} in {}", dir.display()))?;
    Ok(())
}

/// Bring a worktree's submodules — at every level of nesting — in line with its
/// current HEAD, and report how many were touched.
///
/// Two paths per submodule, because most switches within one worktree are
/// between commits that share the same submodules:
///
/// - a submodule **not yet materialised** (no `<sub>/.git`) is a *first*
///   materialisation — init it borrowing objects from the store the repository
///   owns for it, so even a large submodule costs no download of its own;
/// - a submodule **already materialised** only needs its working tree moved to
///   the new pin — a plain `git submodule update`, no `--init`, no
///   `--reference`. The borrow was a one-time concern; re-passing it on every
///   HEAD switch would be pure waste.
///
/// The nesting is walked here, one level at a time, rather than handed to
/// `--recursive`: see the module docs for why the store has to exist before the
/// level that borrows it is materialised.
pub fn sync_submodules(dir: &Path) -> Result<usize> {
    let wt = Repository::new(dir);
    let Some(common) = wt.git_common_dir() else {
        // Under dry-run the checkout these submodules belong to was described
        // rather than created, so there is genuinely nothing here to read — and
        // the `worktree add` that would have made it has already been printed.
        if crate::log::is_dry_run() && !dir.exists() {
            crate::log::dry_run(&format!("materialise submodules in {}", dir.display()));
            return Ok(0);
        }
        bail!("{} is not inside a git repository", dir.display());
    };
    let level = Level {
        // Keyed by *name*, and nested the way git nests it: git derives a nested
        // store's location as `<parent-store>/modules/<name>`, so mirroring that
        // layout is what makes one lookup serve both repository shapes, and what
        // keeps a `git submodule update --recursive` run by hand borrowing too.
        stores: common.join("modules"),
        rel: PathBuf::new(),
        // Asked of the *repository*, once. A submodule checkout is its own little
        // repository — never bare — so asking again at a deeper level would
        // answer about the wrong thing and quietly disable the borrowing from the
        // second level down.
        owned: wt.is_bare(),
        common,
    };
    materialise(&wt, &level)
}

/// One nesting level of a submodule walk: where this level's object stores
/// belong, and what the repository around them looks like.
struct Level {
    /// The repository's common git-dir — the one directory that outlives every
    /// worktree, and the handle through which the other worktrees are listed.
    common: PathBuf,
    /// Where this level's stores sit, keyed by submodule name.
    stores: PathBuf,
    /// This checkout's path relative to the worktree root, so the same submodule
    /// can be found inside a *different* worktree of the same repository.
    rel: PathBuf,
    /// Whether the repository owns the `modules/` slots these stores go in — see
    /// [`shared_store`], which is the only thing that reads it.
    owned: bool,
}

/// Materialise one level's submodules and then descend into each, giving every
/// level its own store.
fn materialise(at: &Repository, level: &Level) -> Result<usize> {
    let mut synced = 0;
    for sub in at.materialised_submodules() {
        let checkout = at.path().join(&sub.path);
        let store = level.stores.join(&sub.name);
        if checkout.join(".git").exists() {
            at.submodule_follow_pin(&sub.path)
                .with_context(|| format!("updating submodule '{}'", sub.path))?;
        } else {
            let reference = shared_store(at, &sub, &store, level);
            at.submodule_init_borrow(&sub.path, reference.as_deref())
                .with_context(|| format!("initialising submodule '{}'", sub.path))?;
        }
        synced += 1;
        synced += materialise(
            &Repository::new(&checkout),
            &Level {
                common: level.common.clone(),
                stores: store.join("modules"),
                rel: level.rel.join(&sub.path),
                owned: level.owned,
            },
        )?;
    }
    Ok(synced)
}

/// The object store every worktree of this repository borrows `sub` from,
/// **creating it** when the repository has none yet.
///
/// A conventional clone needs none of this: `<common>/modules/<name>` is git's
/// own slot, the main worktree fills it, and the main worktree cannot be
/// removed — so borrowing from it is safe indefinitely. That slot is also the
/// reason this returns `None` rather than creating anything for a conventional
/// clone: writing our own bare copy there would squat the directory git wants for
/// the primary's submodule git-dir.
///
/// A bare repository has no primary, so left alone it has no durable store at
/// all — git puts a *linked* worktree's submodule git-dir under that worktree's
/// own administrative directory (`<common>/worktrees/<id>/modules/<name>`),
/// which `git worktree remove` deletes along with everything borrowing from it.
/// The fix is to give the repository a store of its own at the same path a
/// conventional clone uses, which is free of that hazard because it belongs to
/// no worktree. Squatting is not a concern there: `git --git-dir=<bare>
/// submodule` refuses outright ("cannot be used without a working tree"), so git
/// itself never fills the slot in a bare repository.
///
/// Two ways to create it, cheapest first. A copy some **live worktree** already
/// downloaded is published for the price of hardlinks, which is also how a
/// repository set up before this existed is brought into line. With nothing on
/// disk anywhere, the store is **downloaded** — and it is downloaded *into the
/// store*, not into the worktree that happened to ask first: whoever asks first
/// then borrows like everybody else, instead of keeping a full second copy that
/// drifts from the shared one at the next fetch.
///
/// Both are best effort. A store that cannot be created costs a download into the
/// worktree, which is worth a warning and not worth failing a worktree over.
fn shared_store(at: &Repository, sub: &Submodule, store: &Path, level: &Level) -> Option<PathBuf> {
    if git::is_object_store(store) {
        return Some(store.to_owned());
    }
    if !level.owned {
        return None;
    }
    if let Some(source) = existing_store(&level.common, &level.rel.join(&sub.path)) {
        match git::create_reference_store(&source, store) {
            Ok(()) => return Some(store.to_owned()),
            Err(e) => log::warn!(
                "could not publish a shared object store for submodule '{}' at {}: {e}",
                sub.name,
                store.display()
            ),
        }
    }
    let url = submodule_url(at, sub)?;
    match git::clone_reference_store(&url, store) {
        Ok(()) => Some(store.to_owned()),
        Err(e) => {
            log::warn!(
                "could not create a shared object store for submodule '{}' at {}: {e}; \
                 it will be downloaded into this worktree instead",
                sub.name,
                store.display()
            );
            None
        }
    }
}

/// Where `sub` is cloned from, as git resolves it.
///
/// Read from config rather than `.gitmodules` because a recorded URL may be
/// **relative** to the superproject's own remote, and because a repository may
/// override the declared one. `git submodule init` performs exactly that
/// resolution, offline, and writes the answer to `submodule.<name>.url` — so this
/// asks it to when the key is not there yet.
fn submodule_url(at: &Repository, sub: &Submodule) -> Option<String> {
    let key = format!("submodule.{}.url", sub.name);
    if let Ok(Some(url)) = at.get_config(&key) {
        return Some(url);
    }
    if let Err(e) = at.submodule_register(&sub.path) {
        log::warn!("could not resolve the URL of submodule '{}': {e}", sub.name);
        return None;
    }
    at.get_config(&key).ok().flatten()
}

/// A store for the submodule at `rel` (relative to a worktree root) that some
/// live worktree of this repository already holds, as the source to publish a
/// shared one from.
///
/// Asked of each worktree through `git_dir` rather than by assembling the path
/// ourselves: a submodule checkout reaches its git-dir through a `.git` gitfile,
/// and git is the only thing that reliably resolves it. The whole nested tree
/// comes along, since git built it under this store in the layout the chaining
/// expects.
///
/// The `.git` check is not redundant with that. Asked inside a directory that is
/// *not* a repository, `rev-parse` answers for the nearest enclosing one — so an
/// uninitialised submodule directory would resolve to the **superproject's** own
/// git-dir, and handing that to `--reference` would aim a submodule at its
/// parent's objects.
fn existing_store(common: &Path, rel: &Path) -> Option<PathBuf> {
    Repository::new(common)
        .worktrees()
        .iter()
        .find_map(|entry| {
            if entry.bare || entry.prunable || !entry.path.exists() {
                return None;
            }
            let checkout = entry.path.join(rel);
            if !checkout.join(".git").exists() {
                return None;
            }
            let git_dir = Repository::new(&checkout).git_dir()?;
            git::is_object_store(&git_dir).then_some(git_dir)
        })
}

/// Remove a worktree's directory and git's record of it.
///
/// `force` is passed through to git, which needs it for a worktree that is
/// locked or holds submodules. Guarding *uncommitted* work is the caller's job
/// (see [`Entry::dirty`]) — by the time git refuses, the message is about the
/// wrong thing.
pub fn remove(repo: &Repository, dir: &Path, force: bool) -> Result<()> {
    Repository::new(anchor(repo))
        .worktree_remove(dir, force)
        .with_context(|| format!("removing worktree {}", dir.display()))?;
    Ok(())
}

/// Drop git's administrative records for worktrees whose directories are gone
/// (`git worktree prune`). Nothing on disk is deleted — that already happened,
/// by whatever removed the directory.
pub fn prune_records(repo: &Repository) -> Result<()> {
    Repository::new(anchor(repo))
        .worktree_prune()
        .context("pruning stale worktree records")?;
    Ok(())
}

/// The position of a **detached** worktree's HEAD, which no ref names and so
/// `branch_statuses` cannot report. Shaped as a [`BranchStatus`] with the
/// upstream fields empty — a commit tracks nothing.
fn detached_status(repo: &Repository, head: &str, trunk: Option<&str>) -> BranchStatus {
    let (trunk_ahead, trunk_behind) = match trunk.and_then(|t| repo.ahead_behind(head, t)) {
        Some((ahead, behind)) => (Some(ahead), Some(behind)),
        None => (None, None),
    };
    BranchStatus {
        short_head: repo.short_rev(head).unwrap_or_default(),
        upstream: None,
        upstream_ahead: 0,
        upstream_behind: 0,
        upstream_gone: false,
        trunk_ahead,
        trunk_behind,
        commit_time: repo.commit_time(head).unwrap_or(0),
    }
}

/// The revision worktrees are judged merged against: the trunk, preferring the
/// remote-tracking ref over the local branch because the remote is what actually
/// decides whether work has landed (a local `main` may be days behind).
///
/// `origin` is tried before `upstream`, matching how the rest of the toolset
/// names remotes (`review` reads the forge from `upstream`, falling back to
/// `origin`; for a *trunk* the fork's own `origin` is the better first guess).
/// `None` when neither remote publishes a default branch, which leaves every
/// worktree un-merged rather than guessing.
fn trunk_rev(repo: &Repository) -> Option<String> {
    for remote in ["origin", "upstream"] {
        let Some(default) = repo.remote_default_branch(remote) else {
            continue;
        };
        let tracking = format!("{remote}/{default}");
        if repo.rev_exists(&tracking) {
            return Some(tracking);
        }
        if repo.rev_exists(&default) {
            return Some(default);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_dir_names_a_peer_of_the_main_worktree() {
        let dir = sibling_dir(Path::new("/src/proj"), "review");
        assert_eq!(dir, Path::new("/src/proj.review"));
        // A branch slug is just another suffix, so both callers land side by side.
        assert_eq!(
            sibling_dir(Path::new("/src/proj"), &slug("feat/x")),
            Path::new("/src/proj.feat_x")
        );
    }

    #[test]
    fn slug_keeps_path_safe_characters_and_folds_the_rest() {
        assert_eq!(slug("main"), "main");
        assert_eq!(slug("feature/add-thing"), "feature_add-thing");
        assert_eq!(slug("v1.2.3"), "v1.2.3");
        assert_eq!(slug("weird name?"), "weird_name_");
    }

    /// A plain linked worktree that every predicate would select: merged (zero
    /// commits the trunk lacks), upstream gone, and old. The immunity and opt-in
    /// tests below vary one field at a time from here.
    fn selectable_entry(head_time: i64) -> Entry {
        Entry {
            path: PathBuf::from("/src/proj.feat"),
            branch: Some("feat".to_owned()),
            head: None,
            short_head: None,
            is_main: false,
            bare: false,
            is_current: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            changes: StatusCounts::default(),
            trunk_ahead: Some(0),
            trunk_behind: Some(0),
            tracking: None,
            tracking_ahead: 0,
            tracking_behind: 0,
            upstream_gone: true,
            head_time: Some(head_time),
        }
    }

    /// `merged` is derived, not stored: it is exactly "no commits the trunk
    /// lacks", and an unknown distance must never read as merged.
    #[test]
    fn merged_means_zero_commits_the_trunk_lacks() {
        let mut entry = selectable_entry(0);
        assert!(entry.merged(), "zero ahead is merged");
        entry.trunk_ahead = Some(3);
        assert!(!entry.merged(), "commits of its own are not merged");
        entry.trunk_ahead = None;
        assert!(
            !entry.merged(),
            "an unanswerable question must not read as safe to discard"
        );
    }

    /// `dirty` likewise derives from the change counts, and ignored files never
    /// reach them — that is what keeps build output from blocking a sweep.
    #[test]
    fn dirty_means_some_uncommitted_change() {
        let mut entry = selectable_entry(0);
        assert!(!entry.dirty(), "no counts is clean");
        entry.changes = StatusCounts {
            untracked: 1,
            ..StatusCounts::default()
        };
        assert!(entry.dirty(), "a new untracked file is work at risk");
    }

    /// The safety gate is the part a bug would silently delete work through, so
    /// it is pinned independently of any git state.
    #[test]
    fn immune_worktrees_are_never_selected() {
        let base = selectable_entry(0);
        let filter = Filter {
            merged: true,
            gone: true,
            dormant_before: Some(1),
        };
        // The plain case is selected, so the assertions below isolate immunity.
        assert_eq!(filter.reason(&base), Some("merged"));

        for immune in [
            Entry {
                is_main: true,
                ..base.clone()
            },
            Entry {
                is_current: true,
                ..base.clone()
            },
            Entry {
                locked: true,
                ..base.clone()
            },
            Entry {
                prunable: true,
                ..base.clone()
            },
        ] {
            assert_eq!(
                filter.reason(&immune),
                None,
                "the main, current, locked and prunable worktrees must never be swept"
            );
        }
    }

    #[test]
    fn each_predicate_is_opt_in() {
        let entry = selectable_entry(100);
        // An empty filter asks for nothing, so it selects nothing — dormancy in
        // particular is never implied.
        let empty = Filter::default();
        assert!(empty.is_empty());
        assert_eq!(empty.reason(&entry), None);

        let merged_only = Filter {
            merged: true,
            ..Filter::default()
        };
        assert_eq!(merged_only.reason(&entry), Some("merged"));

        let gone_only = Filter {
            gone: true,
            ..Filter::default()
        };
        assert_eq!(gone_only.reason(&entry), Some("upstream gone"));

        let dormant_only = Filter {
            dormant_before: Some(200),
            ..Filter::default()
        };
        assert_eq!(dormant_only.reason(&entry), Some("dormant"));
        // A cutoff the commit predates is what "dormant" means; a newer commit
        // is not.
        let not_yet = Filter {
            dormant_before: Some(50),
            ..Filter::default()
        };
        assert_eq!(not_yet.reason(&entry), None);
    }

    /// Creating a worktree must materialise submodules by *borrowing* objects,
    /// and re-pointing it must then only follow the pins. Driven end to end
    /// against a real repository, since that split is the whole reason this
    /// module exists.
    #[test]
    fn create_borrows_submodule_objects_and_repoint_follows_pins() {
        let _guard = crate::log::test_flag_guard();
        // The submodule clone is a *child* git process, which inherits repo
        // config only via the environment — so the file-protocol allowance
        // (needed for a local test submodule; real ones are https/ssh) and
        // identity go through `GIT_CONFIG_*`. Held under the flag guard so it
        // doesn't race other tests.
        std::env::set_var("GIT_CONFIG_COUNT", "4");
        std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
        std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
        std::env::set_var("GIT_CONFIG_VALUE_1", "t@e.com");
        std::env::set_var("GIT_CONFIG_KEY_2", "user.name");
        std::env::set_var("GIT_CONFIG_VALUE_2", "T");
        // Keep the test hermetic from any globally-installed hooks.
        std::env::set_var("GIT_CONFIG_KEY_3", "core.hooksPath");
        std::env::set_var("GIT_CONFIG_VALUE_3", "/nonexistent-wits-test-hooks");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &Path, args: &[&str]| {
            crate::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        let mk = |name: &str| {
            let d = root.join(name);
            run(root, &["init", "-q", "-b", "main", name]);
            std::fs::write(d.join("f"), "v1").unwrap();
            run(&d, &["add", "f"]);
            run(&d, &["commit", "-q", "-m", "c1"]);
            d
        };
        let sub = mk("sub");
        let sup = root.join("P");
        run(root, &["init", "-q", "-b", "main", "P"]);
        run(
            &sup,
            &[
                "submodule",
                "add",
                "-q",
                &format!("file://{}", sub.display()),
                "sub",
            ],
        );
        run(&sup, &["commit", "-q", "-m", "add sub"]);
        run(&sup, &["submodule", "update", "--init", "--recursive"]);
        run(&sup, &["branch", "feat"]);

        let repo = Repository::new(&sup);
        let wt = sibling_dir(&anchor(&repo), &slug("feat"));
        create(&repo, &wt, "feat").unwrap();
        assert_eq!(sync_submodules(&wt).unwrap(), 1);

        // The worktree landed beside the main one, on the branch asked for.
        assert_eq!(wt, root.join("P.feat"));
        assert!(wt.join("sub/f").exists(), "submodule was materialised");
        // …and it borrowed rather than re-downloaded: the clone's alternates
        // point at the primary's store for that submodule.
        let store = repo.git_common_dir().unwrap().join("modules/sub");
        let sub_gitdir = Repository::new(wt.join("sub")).git_dir().unwrap();
        let alternates =
            std::fs::read_to_string(sub_gitdir.join("objects/info/alternates")).unwrap_or_default();
        assert!(
            alternates.contains(store.to_str().unwrap()),
            "submodule should borrow from {}, got: {alternates}",
            store.display()
        );

        // The inventory sees both worktrees and can name the new one three ways.
        let inv = Inventory::gather(&repo);
        assert_eq!(inv.entries().len(), 2);
        for target in ["feat", "P.feat", wt.to_str().unwrap()] {
            let found = inv.resolve(target).unwrap();
            assert_eq!(found.path, wt, "resolving '{target}'");
            assert!(!found.is_main);
        }
        assert!(inv.resolve("nope").is_err());
        let main = inv
            .entries()
            .iter()
            .find(|e| e.is_main)
            .expect("main worktree is listed");
        assert_eq!(main.path, sup);

        // Re-pointing a clean worktree follows the new pin without re-cloning.
        // A *commit* is what `review checkout` moves to (a detached snapshot);
        // a branch already live in the main worktree is git's to refuse, below.
        let main_sha = repo.rev_parse("main").unwrap();
        repoint(&wt, &main_sha).unwrap();
        assert_eq!(sync_submodules(&wt).unwrap(), 1);
        assert!(wt.join("sub/f").exists());

        // git forbids one branch in two worktrees, and says where the other is.
        let err = repoint(&wt, "main").unwrap_err().to_string();
        assert!(
            err.contains("checking out main"),
            "git's own refusal surfaces: {err}"
        );

        // A dirty worktree refuses the move, so uncommitted work is never buried.
        std::fs::write(wt.join("f"), "dirty").unwrap();
        let err = repoint(&wt, "feat").unwrap_err().to_string();
        assert!(err.contains("uncommitted"), "got: {err}");
    }

    /// The bare-backed case, which git leaves with nowhere durable to put a
    /// submodule's objects: it files a *linked* worktree's submodule git-dir
    /// under that worktree's own administrative directory, so borrowing from one
    /// worktree and later removing it leaves the borrower with an unreadable
    /// alternate. Driven end to end because that damage is silent until someone
    /// reads an object.
    ///
    /// The load-bearing assertion is that **no checkout owns a copy** — not even
    /// the first one, which is the one that pays for the download. A store built
    /// out of that first checkout's own git-dir would leave two full copies of
    /// every submodule, hardlinked at birth and diverging from the next fetch on.
    ///
    /// Every submodule here is deliberately **named** differently from its path,
    /// and nested two deep, because those are the two ways the store layout can
    /// be got wrong while still appearing to work in the common repository.
    #[test]
    fn a_bare_repo_owns_a_submodule_store_that_outlives_its_worktrees() {
        let _guard = crate::log::test_flag_guard();
        // The submodule clone is a *child* git process, which inherits repo
        // config only via the environment — see the sibling test above.
        std::env::set_var("GIT_CONFIG_COUNT", "4");
        std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
        std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
        std::env::set_var("GIT_CONFIG_VALUE_1", "t@e.com");
        std::env::set_var("GIT_CONFIG_KEY_2", "user.name");
        std::env::set_var("GIT_CONFIG_VALUE_2", "T");
        std::env::set_var("GIT_CONFIG_KEY_3", "core.hooksPath");
        std::env::set_var("GIT_CONFIG_VALUE_3", "/nonexistent-wits-test-hooks");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &Path, args: &[&str]| {
            crate::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        let mk = |name: &str| {
            let d = root.join(name);
            run(root, &["init", "-q", "-b", "main", name]);
            std::fs::write(d.join("f"), "v1").unwrap();
            run(&d, &["add", "f"]);
            run(&d, &["commit", "-q", "-m", "c1"]);
            d
        };
        let add_sub = |super_dir: &Path, name: &str, path: &str, url: &Path| {
            run(
                super_dir,
                &[
                    "submodule",
                    "add",
                    "-q",
                    "--name",
                    name,
                    &format!("file://{}", url.display()),
                    path,
                ],
            );
            run(super_dir, &["commit", "-q", "-m", "add submodule"]);
        };

        let leaf = mk("leaf");
        let mid = mk("mid");
        add_sub(&mid, "leafmod", "vendor/leaf", &leaf);
        let sup = mk("P");
        add_sub(&sup, "midmod", "third_party/mid", &mid);
        run(&sup, &["branch", "feat"]);

        let bare = root.join("b.git");
        crate::git::init_bare_host(
            &format!("file://{}", sup.display()),
            "origin",
            &bare,
            "main",
        )
        .unwrap();
        let repo = Repository::new(&bare);
        // A tracking host, so the remote's branches are remote-tracking refs and
        // `refs/heads` holds only the branch the bootstrap will check out.
        assert_eq!(
            repo.branch_tips().keys().collect::<Vec<_>>(),
            vec!["main"],
            "a worktree host starts with one local branch, not one per remote branch"
        );
        assert!(repo.rev_exists("origin/feat"), "…and the rest are tracked");
        assert_eq!(
            repo.remote_default_branch("origin").as_deref(),
            Some("main"),
            "origin/HEAD is published, so a trunk can be found"
        );

        // The bootstrap has nothing to borrow from, so it downloads — into the
        // store the repository owns, and then borrows it like anyone else. Both
        // levels of nesting count as synced.
        let bootstrap = root.join("b.main");
        create(&repo, &bootstrap, "main").unwrap();
        assert_eq!(sync_submodules(&bootstrap).unwrap(), 2);

        let shared = bare.join("modules").join("midmod");
        let shared_leaf = shared.join("modules").join("leafmod");
        assert!(
            git::is_object_store(&shared),
            "the repository should own a store at {}",
            shared.display()
        );
        assert!(
            git::is_object_store(&shared_leaf),
            "the nested store must be published too — it is where the chaining looks"
        );
        assert!(
            !bare.join("modules").join("third_party").exists(),
            "the store is keyed by submodule name, never by its path"
        );
        assert_eq!(
            Repository::new(&shared)
                .get_config("extensions.preciousObjects")
                .unwrap()
                .as_deref(),
            Some("true"),
            "a store others borrow from must refuse to delete objects"
        );

        // A second worktree borrows it, at every level, without re-downloading.
        // Its branch has to be asked for — the host tracks the remote rather than
        // pre-creating a local branch per remote one, so `create` refuses a name
        // that exists only as `origin/feat`, exactly as it does anywhere else.
        assert!(create(&repo, &root.join("b.nope"), "feat").is_err());
        repo.create_tracking_branch("feat", "origin/feat").unwrap();
        let feat = root.join("b.feat");
        create(&repo, &feat, "feat").unwrap();
        assert_eq!(sync_submodules(&feat).unwrap(), 2);

        let gitdir_of = |checkout: PathBuf| Repository::new(checkout).git_dir().unwrap();
        let borrows_from = |gitdir: &Path, want: &Path| {
            let alternates =
                std::fs::read_to_string(gitdir.join("objects/info/alternates")).unwrap_or_default();
            assert!(
                alternates.contains(want.to_str().unwrap()),
                "{} should borrow from {}, got: {alternates}",
                gitdir.display(),
                want.display()
            );
        };
        // Every checkout borrows, at every level — the bootstrap included, which
        // is the whole reason the store is seeded before anything is materialised.
        for checkout in [&bootstrap, &feat] {
            borrows_from(&gitdir_of(checkout.join("third_party/mid")), &shared);
            borrows_from(
                &gitdir_of(checkout.join("third_party/mid/vendor/leaf")),
                &shared_leaf,
            );
        }

        // The point of all of it: the worktree that paid for the download is
        // removable, and the borrower does not notice.
        remove(&repo, &bootstrap, true).unwrap();
        let objects_readable = |dir: PathBuf| {
            crate::process::Command::new("git")
                .args(["cat-file", "-e", "HEAD^{tree}"])
                .current_dir(&dir)
                .force_run()
                .exec()
                .is_ok_and(|result| result.is_success())
        };
        assert!(
            objects_readable(feat.join("third_party/mid")),
            "removing the bootstrap must not break a borrower"
        );
        assert!(
            objects_readable(feat.join("third_party/mid/vendor/leaf")),
            "…nor the nested level below it"
        );
    }

    /// A repository that predates all of this: cloned with `git clone --bare`, its
    /// submodules downloaded into a worktree's own administrative directory, and
    /// its remote left with no fetch refspec at all. Both defects are repairable
    /// in place, and the repair must not re-transfer the objects — which is the
    /// only reason the "copy a store some worktree already holds" path still
    /// exists now that a fresh repository seeds its stores up front.
    ///
    /// "Not re-transferred" is asserted through the **inode**: a store published
    /// with `clone --local` shares its pack files with the copy it came from, so a
    /// store that had fetched them again would be a different file.
    #[test]
    fn an_existing_bare_clone_is_repaired_without_a_second_download() {
        let _guard = crate::log::test_flag_guard();
        std::env::set_var("GIT_CONFIG_COUNT", "4");
        std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
        std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
        std::env::set_var("GIT_CONFIG_VALUE_1", "t@e.com");
        std::env::set_var("GIT_CONFIG_KEY_2", "user.name");
        std::env::set_var("GIT_CONFIG_VALUE_2", "T");
        std::env::set_var("GIT_CONFIG_KEY_3", "core.hooksPath");
        std::env::set_var("GIT_CONFIG_VALUE_3", "/nonexistent-wits-test-hooks");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &Path, args: &[&str]| {
            crate::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        let mk = |name: &str| {
            let d = root.join(name);
            run(root, &["init", "-q", "-b", "main", name]);
            std::fs::write(d.join("f"), "v1").unwrap();
            run(&d, &["add", "f"]);
            run(&d, &["commit", "-q", "-m", "c1"]);
            d
        };
        let sub = mk("sub");
        let sup = mk("P");
        run(
            &sup,
            &[
                "submodule",
                "add",
                "-q",
                "--name",
                "submod",
                &format!("file://{}", sub.display()),
                "vendor/sub",
            ],
        );
        run(&sup, &["commit", "-q", "-m", "add sub"]);
        run(&sup, &["branch", "feat"]);

        // The old shape, made the old way.
        let bare = root.join("b.git");
        let url = format!("file://{}", sup.display());
        run(
            root,
            &["clone", "-q", "--bare", "--origin", "origin", &url, "b.git"],
        );
        let repo = Repository::new(&bare);
        assert!(
            repo.local_branch_exists("feat"),
            "a bare clone copies every remote branch into refs/heads"
        );
        assert!(
            repo.get_config_all("remote.origin.fetch").is_empty(),
            "…and writes no refspec, so a plain fetch would update nothing"
        );

        // The refspec is additive, so reconciling remotes repairs it, and a plain
        // fetch then answers with remote-tracking refs.
        repo.ensure_remote("origin", &url).unwrap();
        assert_eq!(
            repo.get_config_all("remote.origin.fetch"),
            vec!["+refs/heads/*:refs/remotes/origin/*".to_owned()]
        );
        repo.fetch(&["origin"]).unwrap();
        assert!(repo.rev_exists("refs/remotes/origin/feat"));

        // A worktree materialised the old way owns the only copy on disk.
        let old = root.join("b.main");
        create(&repo, &old, "main").unwrap();
        run(&old, &["submodule", "update", "--init", "--recursive"]);
        let store = bare.join("modules/submod");
        assert!(
            !git::is_object_store(&store),
            "the repository owns nothing yet"
        );
        let old_gitdir = Repository::new(old.join("vendor/sub")).git_dir().unwrap();

        let feat = root.join("b.feat");
        create(&repo, &feat, "feat").unwrap();
        assert_eq!(sync_submodules(&feat).unwrap(), 1);
        assert!(
            git::is_object_store(&store),
            "the store is published from the copy already on disk"
        );
        let packs = |gitdir: &Path| -> std::collections::BTreeSet<u64> {
            use std::os::unix::fs::MetadataExt;
            std::fs::read_dir(gitdir.join("objects/pack"))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
                .filter_map(|e| e.metadata().ok().map(|m| m.ino()))
                .collect()
        };
        let (published, downloaded) = (packs(&store), packs(&old_gitdir));
        assert!(!downloaded.is_empty(), "the old copy has a pack to share");
        assert_eq!(
            published, downloaded,
            "the store must hardlink the objects already on disk, not fetch them again"
        );
        let gitdir = Repository::new(feat.join("vendor/sub")).git_dir().unwrap();
        let alternates =
            std::fs::read_to_string(gitdir.join("objects/info/alternates")).unwrap_or_default();
        assert!(
            alternates.contains(store.to_str().unwrap()),
            "and the new worktree borrows it, got: {alternates}"
        );
        // The store outlives the worktree it was copied out of, hardlinks and all.
        remove(&repo, &old, true).unwrap();
        assert!(feat.join("vendor/sub/f").exists());
        run(&feat.join("vendor/sub"), &["fsck", "--no-dangling"]);
    }
}
