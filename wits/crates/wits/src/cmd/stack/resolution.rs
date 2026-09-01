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

/// Resolve the base branch. The authoritative source is the future `project`
/// subcommand; until it exists we fall back to the merge target's remote HEAD,
/// then to whichever conventional trunk name actually exists locally. There is
/// no config override on purpose — the answer should come from project identity,
/// not a hand-maintained setting (see the design doc, §5.1).
pub fn base_branch(repo: &Repository) -> anyhow::Result<String> {
    for remote in ["upstream", "origin"] {
        if let Some(branch) = repo.remote_default_branch(remote) {
            return Ok(branch);
        }
    }
    for candidate in ["main", "master", "trunk"] {
        if repo.rev_parse(candidate).is_some() {
            return Ok(candidate.to_owned());
        }
    }
    anyhow::bail!("could not determine the base branch: no remote HEAD and no main/master/trunk")
}

/// Build the plan for this invocation. `current` is the checked-out branch
/// (`None` on a detached HEAD); `all` widens the scope to every recorded stack.
pub fn plan(repo: &Repository, current: Option<&str>, all: bool) -> anyhow::Result<StackPlan> {
    let base_branch = base_branch(repo)?;
    let topology = load_topology(repo)?;
    select(topology, base_branch, current, all)
}

/// The parsed scope selector, after CLI parsing but before touching git. clap's
/// `conflicts_with` guarantees the positional branch and `--all` never co-occur,
/// so the fourth (illegal) combination is simply not representable here — the
/// exclusion is enforced at the parser and the domain sees only the three legal
/// states.
enum Scope<'a> {
    /// Neither given: anchor on the checked-out branch (`None` = detached HEAD).
    Current,
    /// A named anchor branch.
    Branch(&'a str),
    /// Every recorded stack.
    All,
}

impl<'a> Scope<'a> {
    fn from_args(args: &'a super::ScopeArgs) -> Self {
        match (args.branch.as_deref(), args.all) {
            (Some(branch), _) => Scope::Branch(branch),
            (None, true) => Scope::All,
            (None, false) => Scope::Current,
        }
    }
}

/// Build the plan from CLI scope args. The positional branch is a *scope
/// anchor*: it replaces the checked-out branch as the point the stack is
/// computed from, so a stack can be driven without checking it out (handy with
/// worktrees or a dirty tree). It is mutually exclusive with `--all` (enforced by
/// clap), and — when given explicitly — must name a real branch (a live local
/// ref, or one recorded in the file) so a typo cannot masquerade as an empty
/// synthetic stack.
pub fn plan_scoped(repo: &Repository, scope: &super::ScopeArgs) -> anyhow::Result<StackPlan> {
    match Scope::from_args(scope) {
        Scope::All => plan(repo, None, true),
        Scope::Branch(branch) => {
            let known = repo.rev_parse(branch).is_some() || load_topology(repo)?.contains(branch);
            if !known {
                anyhow::bail!(
                    "no such branch '{branch}': not a local branch and not recorded in .git/machete"
                );
            }
            plan(repo, Some(branch), false)
        }
        Scope::Current => plan(repo, repo.current_branch().as_deref(), false),
    }
}

/// The scope decision, factored out from git so it can be exercised on literal
/// forests. See the design doc, §2, for the rationale behind each branch.
fn select(
    topology: Topology,
    base_branch: String,
    current: Option<&str>,
    all: bool,
) -> anyhow::Result<StackPlan> {
    if all {
        if topology.is_empty() {
            anyhow::bail!("no .git/machete stacks to operate on");
        }
        let selected = topology
            .all()
            .iter()
            .filter(|n| **n != base_branch)
            .cloned()
            .collect();
        return Ok(StackPlan {
            topology,
            base_branch,
            selected,
            standalone: false,
        });
    }

    let current = current
        .ok_or_else(|| anyhow::anyhow!("detached HEAD: check out a stack branch or pass --all"))?;
    if current == base_branch {
        anyhow::bail!("on the base branch '{base_branch}': check out a stack branch first");
    }

    // A branch the file never mentions is treated as its own one-node stack on
    // the base branch — the zero-setup path for an ordinary single MR.
    if !topology.contains(current) {
        let topology = Topology::synthetic(&base_branch, current);
        return Ok(StackPlan {
            topology,
            base_branch,
            selected: vec![current.to_owned()],
            standalone: true,
        });
    }

    // Standing on a fork means "I manage this whole tree"; standing on a linear
    // node means "this one line of work" and siblings are left alone.
    let names = if topology.is_fork_point(current) {
        let mut names = topology.ancestors(current);
        names.extend(topology.subtree(current));
        names
    } else {
        topology.linear_stack(current)
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
        Topology::parse("main\n    A\n        B\n            C\n                E\n            D\n")
    }

    #[test]
    fn all_mode_takes_every_branch_but_the_base() {
        let plan = select(sample(), "main".into(), None, true).unwrap();
        assert_eq!(plan.selected, ["A", "B", "C", "E", "D"]);
        assert!(!plan.standalone);
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
        let probe = std::fs::OpenOptions::new()
            .create(true)
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
