//! Turning "the file on disk and where I'm standing" into "the work to do".
//!
//! This is the single seam every verb shares, and that is the whole point: if
//! `sync`, `submit`, and `anno` each decided scope for themselves they would
//! inevitably drift apart. Instead they all consume one [`StackPlan`] — the same
//! ordered set of operable branches and the same base for each — so the
//! fork-point rule and the base mapping live in exactly one place.

use std::collections::HashSet;
use std::fs;
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

fn machete_path(repo: &Repository) -> Option<PathBuf> {
    repo.git_dir().map(|dir| dir.join("machete"))
}

/// Load the machete forest. An *absent* file is a legitimately empty stack
/// (`Ok(default)`); a file that exists but can't be read (permissions, a
/// transient I/O error) is *not* the same as "no stack" — silently scoping to
/// empty would drop every branch from every stack verb, so that is a hard error
/// the caller surfaces rather than a warning it might miss. Parsing itself never
/// fails (indentation always yields a forest).
pub fn load_topology(repo: &Repository) -> anyhow::Result<Topology> {
    let Some(path) = machete_path(repo).filter(|p| p.exists()) else {
        return Ok(Topology::default());
    };
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {} (the machete stack file)", path.display()))?;
    Ok(Topology::parse(&text))
}

/// Persist the forest back to `.git/machete`. A local-state mutation, so it
/// honours dry-run rather than silently rewriting the file underneath a `-n`.
pub fn save_topology(repo: &Repository, topology: &Topology) -> anyhow::Result<()> {
    let path = machete_path(repo).ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?;
    if wits_log::is_dry_run() {
        wits_log::dry_run(&format!("write {}", path.display()));
        return Ok(());
    }
    fs::write(&path, topology.render())?;
    Ok(())
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
}
