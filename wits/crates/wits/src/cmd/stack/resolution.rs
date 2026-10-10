//! Turning "the file on disk and where I'm standing" into "the work to do".
//!
//! This is the single seam every verb shares, and that is the whole point: if
//! `sync`, `submit`, and `anno` each decided scope for themselves they would
//! inevitably drift apart. Instead they all consume one [`StackPlan`] — the same
//! ordered set of operable branches and the same base for each — so the
//! fork-point rule and the base mapping live in exactly one place.

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::Context;
use wits_util::git::Repository;
use wits_util::log as wits_log;
use wits_util::project::remotes::Declared;

use super::topology::Topology;

/// The resolved scope of one invocation.
pub struct StackPlan {
    pub topology: Topology,
    pub base_branch: String,
    /// Branches to operate on, in traversal order, never including the base.
    pub selected: Vec<String>,
    /// The current branch wasn't in the file, so this is a synthesized one-node
    /// stack. `anno` skips these (a lone MR has nothing to navigate to).
    pub standalone: bool,
}

impl StackPlan {
    /// The base a branch's MR should target: its parent in the tree, or the base
    /// branch itself when the branch is a root.
    pub fn base_for(&self, branch: &str) -> String {
        self.topology
            .parent(branch)
            .map(str::to_owned)
            .unwrap_or_else(|| self.base_branch.clone())
    }
}

/// The machete file: `<common-git-dir>/machete`, one per **repository**.
///
/// The common dir, not the plain git dir, and for the same reason the review store
/// and the submodule object stores use it. A stack is a set of branches, which is a
/// repository-wide fact; inside a linked worktree the plain git dir is that
/// worktree's *private* administrative directory
/// (`<common>/worktrees/<id>`), so writing there gives every worktree its own
/// invisible forest and hands the file to `git worktree remove` to delete. In a
/// bare-style layout that is the whole file, since no checkout is the main worktree.
///
/// For a conventional clone the two are the same directory, which is why this went
/// unnoticed — and `$GIT_COMMON_DIR/machete` is already what the
/// `reference-transaction` hook prunes, so this is the location the toolset had
/// settled on everywhere but here.
fn machete_path(repo: &Repository) -> Option<PathBuf> {
    repo.git_common_dir()
        .or_else(|| repo.git_dir())
        .map(|dir| dir.join("machete"))
}

/// A forest left in a *worktree-private* git dir by an earlier version. Read as a
/// fallback so a stack does not silently vanish, and only while the shared file does
/// not exist — the first [`save_topology`] writes the shared one and this stops being
/// consulted.
fn stale_private_path(repo: &Repository) -> Option<PathBuf> {
    let private = repo.git_dir()?.join("machete");
    let shared = machete_path(repo)?;
    (private != shared && private.exists()).then_some(private)
}

/// Load the machete forest. An *absent* file is a legitimately empty stack
/// (`Ok(default)`); a file that exists but can't be read (permissions, a
/// transient I/O error) is *not* the same as "no stack" — silently scoping to
/// empty would drop every branch from every stack verb, so that is a hard error
/// the caller surfaces rather than a warning it might miss. Parsing itself never
/// fails (indentation always yields a forest).
pub fn load_topology(repo: &Repository) -> anyhow::Result<Topology> {
    let shared = machete_path(repo).filter(|p| p.exists());
    let path = match shared {
        Some(path) => path,
        None => match stale_private_path(repo) {
            Some(private) => {
                log::warn!(
                    "reading the stack from {}, which belongs to this worktree alone and goes \
                     with it when the worktree is removed; the next structure edit writes {} \
                     instead, after which the old file can be deleted",
                    private.display(),
                    machete_path(repo)
                        .expect("a path exists to read one")
                        .display()
                );
                private
            }
            None => return Ok(Topology::default()),
        },
    };
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {} (the machete stack file)", path.display()))?;
    Ok(Topology::parse(&text))
}

/// Persist the forest back to the machete file. A local-state mutation, so it
/// honours dry-run rather than silently rewriting the file underneath a `-n`.
///
/// The write lands through a sibling temp file and an atomic rename, so a
/// reader that holds no lock (`cat`, an older hook) sees either the old forest
/// or the new one, never a truncated one. An existing file's mode is kept, so
/// a save can never tighten or loosen it.
pub fn save_topology(repo: &Repository, topology: &Topology) -> anyhow::Result<()> {
    let path = machete_path(repo).ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?;
    if wits_log::is_dry_run() {
        wits_log::dry_run(&format!("write {}", path.display()));
        return Ok(());
    }
    let dir = path.parent().expect("the machete path always has a parent");
    let mut tmp = tempfile::Builder::new()
        .prefix(".machete.")
        .rand_bytes(6)
        .tempfile_in(dir)
        .context("creating the machete temp file")?;
    tmp.write_all(topology.render().as_bytes())
        .context("writing the machete forest")?;
    let mode = fs::metadata(&path)
        .map(|meta| meta.permissions())
        .unwrap_or_else(|_| fs::Permissions::from_mode(0o644));
    tmp.as_file().set_permissions(mode)?;
    tmp.persist(&path)
        .map_err(|err| err.error)
        .context("moving the machete temp file into place")?;
    Ok(())
}

/// An exclusive advisory lock over the machete file, held across one
/// load-edit-save cycle.
///
/// The file has several writers — `tree rm`/`mv`/`prune`, `anno`, `slice` —
/// and the `reference-transaction` hook drives one of them from another
/// process, so two read-modify-write cycles can overlap and lose an edit. The
/// lock is a sidecar `<machete>.lock` file held under an exclusive `flock`
/// (std's `File::lock`): the kernel drops it if a holder dies, so it cannot go
/// stale, and the save itself lands atomically on top. Readers without the
/// lock stay safe — the atomic rename means they see the old forest or the new
/// one, never a torn one — so only the mutating verbs lock.
///
/// Guard the whole cycle, not just the save: locking the write alone would
/// still let a stale load overwrite the winner of a race.
///
/// The lock file is never removed, and must not be. Unlike git's own `*.lock`
/// files, whose *existence* is the lock, this one is only the inode every
/// writer `flock`s, so its presence means nothing. Unlinking it on release
/// would let a writer already waiting on the old inode proceed while a
/// newcomer creates a fresh file and locks that instead — two holders at once,
/// the lost edit this exists to prevent. The same reason rules out locking the
/// machete file itself: [`save_topology`] replaces it by rename, which unlinks
/// the inode a waiter is queued on.
pub struct MacheteLock {
    // Holding the open descriptor *is* holding the lock; dropping it — or the
    // process exiting — releases. Nothing reads or writes through it.
    _file: Option<fs::File>,
}

impl MacheteLock {
    /// Take the lock for one load-edit-save cycle. Under `--dry-run` nothing is
    /// written, so no lock is taken and no lock file is created.
    pub fn acquire(repo: &Repository) -> anyhow::Result<Self> {
        if wits_log::is_dry_run() {
            return Ok(Self { _file: None });
        }
        let path = machete_lock_path(repo)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {} (the machete lock)", path.display()))?;
        file.lock().with_context(|| {
            format!(
                "waiting for {} (another stack edit holds it)",
                path.display()
            )
        })?;
        Ok(Self { _file: Some(file) })
    }
}

/// The sidecar lock file: the machete path with `.lock` appended, so it lives
/// beside the forest in the same (common git) directory.
fn machete_lock_path(repo: &Repository) -> anyhow::Result<PathBuf> {
    let mut path = machete_path(repo)
        .ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?
        .into_os_string();
    path.push(".lock");
    Ok(PathBuf::from(path))
}

/// Resolve the base branch: the checkout's trunk, by the rule every command
/// shares ([`wits_util::project::trunk`]). There is no config override on
/// purpose — the answer comes from project identity, not a hand-maintained
/// setting (`docs/reference/stack-design.rst`, "Base branch resolution").
pub fn base_branch(repo: &Repository, declared: &Declared) -> anyhow::Result<String> {
    declared.trunk(repo).map(|trunk| trunk.name).context(
        "could not determine the base branch: no declared main_branch, no remote HEAD on the \
         merge target, and no main/master/trunk",
    )
}

/// Build the plan for one invocation. `anchor` is the branch the scope is
/// computed from (`None` on a detached HEAD with no branch named); `all` widens
/// the scope from the anchor's line of work to its whole stack.
fn plan(
    repo: &Repository,
    declared: &Declared,
    anchor: Option<&str>,
    all: bool,
) -> anyhow::Result<StackPlan> {
    let base_branch = base_branch(repo, declared)?;
    let topology = load_topology(repo)?;
    select(topology, base_branch, anchor, all)
}

/// Build the plan from CLI scope args. The positional branch is a *scope
/// anchor*: it replaces the checked-out branch as the point the stack is
/// computed from, so a stack can be driven without checking it out (handy with
/// worktrees or a dirty tree). When given explicitly it must name a real branch
/// (a live local ref, or one recorded in the file) so a typo cannot masquerade
/// as an empty synthetic stack. `--all` widens the scope around whichever
/// anchor is in force to that anchor's whole stack.
pub fn plan_scoped(
    repo: &Repository,
    declared: &Declared,
    scope: &super::ScopeArgs,
) -> anyhow::Result<StackPlan> {
    let anchor = match scope.branch.as_deref() {
        Some(branch) => {
            let known = repo.rev_parse(branch).is_some() || load_topology(repo)?.contains(branch);
            if !known {
                anyhow::bail!(
                    "no such branch '{branch}': not a local branch and not recorded in the machete file"
                );
            }
            Some(branch.to_owned())
        }
        None => repo.current_branch(),
    };
    plan(repo, declared, anchor.as_deref(), scope.all)
}

/// The scope decision, factored out from git so it can be exercised on literal
/// forests. The rationale behind each branch is in
/// `docs/reference/stack-design.rst`, "Scope: which branches a verb touches".
fn select(
    topology: Topology,
    base_branch: String,
    anchor: Option<&str>,
    all: bool,
) -> anyhow::Result<StackPlan> {
    let anchor = anchor
        .ok_or_else(|| anyhow::anyhow!("detached HEAD: check out a stack branch or name one"))?;
    if anchor == base_branch {
        anyhow::bail!("on the base branch '{base_branch}': check out a stack branch first");
    }

    // A branch the file never mentions is treated as its own one-node stack on
    // the base branch — the zero-setup path for an ordinary single MR.
    if !topology.contains(anchor) {
        let topology = Topology::synthetic(&base_branch, anchor);
        return Ok(StackPlan {
            topology,
            base_branch,
            selected: vec![anchor.to_owned()],
            standalone: true,
        });
    }

    // `--all` takes every line of the anchor's stack, but no other stack on the
    // base. Otherwise, standing on a fork means "I manage this whole tree";
    // standing on a linear node means "this one line of work" and siblings are
    // left alone.
    let names = if all {
        topology.whole_stack(anchor, &base_branch)
    } else if topology.is_fork_point(anchor) {
        let mut names = topology.ancestors(anchor);
        names.extend(topology.subtree(anchor));
        names
    } else {
        topology.linear_stack(anchor)
    };

    let mut seen = HashSet::new();
    let selected = names
        .into_iter()
        .filter(|n| *n != base_branch && seen.insert(n.clone()))
        .collect();

    Ok(StackPlan {
        topology,
        base_branch,
        selected,
        standalone: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Topology {
        // main → A → B(fork) → C → E
        //                        → D
        //      → X → Y          (a second stack on the same base)
        Topology::parse(
            "main\n    A\n        B\n            C\n                E\n            D\n    X\n        Y\n",
        )
    }

    #[test]
    fn all_takes_the_whole_stack_around_the_anchor() {
        // From a leaf below the fork, from the stack's root, or from the fork's
        // other side: always every line of A's stack, never the X stack.
        for anchor in ["E", "A", "D"] {
            let plan = select(sample(), "main".into(), Some(anchor), true).unwrap();
            assert_eq!(
                plan.selected,
                ["A", "B", "C", "E", "D"],
                "anchored on {anchor}"
            );
            assert!(!plan.standalone);
        }
        let plan = select(sample(), "main".into(), Some("Y"), true).unwrap();
        assert_eq!(plan.selected, ["X", "Y"]);
    }

    #[test]
    fn all_leaves_a_branch_outside_the_file_on_its_own() {
        let plan = select(sample(), "main".into(), Some("hotfix"), true).unwrap();
        assert!(plan.standalone);
        assert_eq!(plan.selected, ["hotfix"]);
    }

    #[test]
    fn all_still_needs_a_stack_branch_to_anchor_on() {
        // The whole stack is relative to the anchor, so neither a detached HEAD
        // nor the base branch, which every stack shares, can choose one.
        assert!(select(sample(), "main".into(), None, true).is_err());
        assert!(select(sample(), "main".into(), Some("main"), true).is_err());
    }

    #[test]
    fn linear_node_takes_its_line_only() {
        // Standing on C (linear): main is dropped as base, D (sibling of nothing
        // here) isn't on C's first-child line.
        let plan = select(sample(), "main".into(), Some("C"), false).unwrap();
        assert_eq!(plan.selected, ["A", "B", "C", "E"]);
    }

    #[test]
    fn fork_point_takes_ancestors_plus_whole_subtree() {
        let plan = select(sample(), "main".into(), Some("B"), false).unwrap();
        assert_eq!(plan.selected, ["A", "B", "C", "E", "D"]);
    }

    #[test]
    fn unknown_branch_becomes_a_standalone_node() {
        let plan = select(sample(), "main".into(), Some("hotfix"), false).unwrap();
        assert!(plan.standalone);
        assert_eq!(plan.selected, ["hotfix"]);
        assert_eq!(plan.base_for("hotfix"), "main");
    }

    #[test]
    fn base_for_maps_to_parent() {
        let plan = select(sample(), "main".into(), Some("B"), false).unwrap();
        assert_eq!(plan.base_for("C"), "B");
        assert_eq!(plan.base_for("A"), "main");
    }

    #[test]
    fn standing_on_base_is_an_error() {
        assert!(select(sample(), "main".into(), Some("main"), false).is_err());
    }

    #[test]
    fn load_topology_defaults_when_absent_and_parses_when_present() {
        let dir = tempfile::tempdir().unwrap();
        wits_util::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();
        let repo = Repository::new(dir.path());

        // No machete file yet: a legitimately empty stack, never an error.
        assert!(load_topology(&repo).unwrap().is_empty());

        // A present file parses into the forest.
        let git_dir = repo.git_dir().unwrap();
        std::fs::write(git_dir.join("machete"), "main\n    feat\n").unwrap();
        let topo = load_topology(&repo).unwrap();
        assert_eq!(topo.parent("feat"), Some("main"));
    }

    /// One forest per **repository**, so a linked worktree reads and writes the very
    /// file the repository holds. Writing to the plain git dir instead gives every
    /// worktree its own invisible stack and loses it with `git worktree remove` — and
    /// for a bare-backed repo that is the whole file.
    /// The save is an atomic rename: the file's mode survives it, and no temp
    /// file is left behind for a reader to stumble over.
    #[test]
    fn a_save_preserves_the_file_mode_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        wits_util::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();
        let repo = Repository::new(dir.path());
        let git_dir = repo.git_dir().unwrap();
        let path = git_dir.join("machete");

        save_topology(&repo, &Topology::parse("main\n    feat\n")).unwrap();
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o644);

        // A hand-set mode survives the next save, and the content is the new one.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        save_topology(&repo, &Topology::parse("main\n")).unwrap();
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_topology(&repo).unwrap().parent("feat"), None);

        let leftovers = std::fs::read_dir(git_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".machete.")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    /// Two handles on the same repository cannot hold the machete lock at once:
    /// a second descriptor's `try_lock` is denied while the guard lives, and
    /// granted once it drops. (flock is per open file description, so this is
    /// exactly the exclusion another *process* would face.)
    #[test]
    fn the_machete_lock_is_exclusive_and_self_releasing() {
        let dir = tempfile::tempdir().unwrap();
        wits_util::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();
        let repo = Repository::new(dir.path());
        let lock_path = repo.git_dir().unwrap().join("machete.lock");

        let guard = MacheteLock::acquire(&repo).unwrap();
        // Opened exactly as `acquire` opens it, so the probe is the descriptor
        // another process would hold rather than one that truncates the file
        // under a live holder.
        let probe = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap();
        // Held by the guard: a second descriptor's try is denied (WouldBlock)…
        assert!(probe.try_lock().is_err());
        drop(guard);
        // …and granted once it drops.
        assert!(probe.try_lock().is_ok());
        probe.unlock().unwrap();
    }

    #[test]
    fn the_forest_is_shared_by_every_worktree_of_a_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &std::path::Path, args: &[&str]| {
            wits_util::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        run(root, &["init", "-q", "-b", "main", "src"]);
        let main = root.join("src");
        for (key, value) in [("user.email", "t@e.com"), ("user.name", "T")] {
            run(&main, &["config", key, value]);
        }
        run(&main, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        run(&main, &["branch", "feat"]);
        let linked = root.join("wt");
        run(
            &main,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "feat"],
        );

        // Saved from the linked worktree, the forest lands in the repository's own
        // git dir — not in `<common>/worktrees/<id>`, which belongs to that worktree.
        let from_worktree = Repository::new(&linked);
        save_topology(&from_worktree, &Topology::parse("main\n    feat\n")).unwrap();
        assert!(main.join(".git/machete").exists());
        assert!(!main.join(".git/worktrees/wt/machete").exists());

        // And every handle on the repository sees the same one.
        for repo in [&from_worktree, &Repository::new(&main)] {
            assert_eq!(load_topology(repo).unwrap().parent("feat"), Some("main"));
        }
    }

    /// A forest an older version left in a worktree's private git dir is still read
    /// while no shared one exists, so a stack does not silently vanish — and the next
    /// save moves it, after which the stale file is ignored.
    #[test]
    fn a_worktree_private_forest_is_still_read_until_one_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &std::path::Path, args: &[&str]| {
            wits_util::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        run(root, &["init", "-q", "-b", "main", "src"]);
        let main = root.join("src");
        for (key, value) in [("user.email", "t@e.com"), ("user.name", "T")] {
            run(&main, &["config", key, value]);
        }
        run(&main, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        run(&main, &["branch", "feat"]);
        let linked = root.join("wt");
        run(
            &main,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "feat"],
        );

        let repo = Repository::new(&linked);
        let private = repo.git_dir().unwrap().join("machete");
        std::fs::write(&private, "main\n    feat\n").unwrap();
        assert_eq!(load_topology(&repo).unwrap().parent("feat"), Some("main"));

        // Once a shared forest exists it is the only one consulted, stale file or not.
        save_topology(&repo, &Topology::parse("main\n")).unwrap();
        std::fs::write(&private, "main\n    stale\n").unwrap();
        assert_eq!(load_topology(&repo).unwrap().parent("stale"), None);
    }
}
