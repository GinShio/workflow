//! Turning "the stack as stored and where I'm standing" into "the work to do".
//!
//! This is the single seam every verb shares, and that is the whole point: if
//! `push`, `submit`, and `anno` each decided scope for themselves they would
//! inevitably drift apart. Instead they all consume one [`StackPlan`] — the same
//! ordered set of operable branches and the same base for each — so the
//! fork-point rule and the base mapping live in exactly one place.

use std::collections::HashSet;

use anyhow::Context;
use wits_util::git::Repository;
use wits_util::project::remotes::Declared;

use super::store;
use super::topology::Topology;

/// The resolved scope of one invocation.
pub struct StackPlan {
    pub topology: Topology,
    pub base_branch: String,
    /// Branches to operate on, in traversal order, never including the base.
    pub selected: Vec<String>,
    /// The anchor isn't in the stack, so this is a synthesized one-node
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
    let topology = store::load(repo, &base_branch).topology;
    select(topology, base_branch, anchor, all)
}

/// Build the plan from CLI scope args. The positional branch is a *scope
/// anchor*: it replaces the checked-out branch as the point the stack is
/// computed from, so a stack can be driven without checking it out (handy with
/// worktrees or a dirty tree). When given explicitly it must name a local
/// branch, so a typo cannot masquerade as an empty synthetic stack. `--all`
/// widens the scope around whichever anchor is in force to that anchor's whole
/// stack.
pub fn plan_scoped(
    repo: &Repository,
    declared: &Declared,
    scope: &super::ScopeArgs,
) -> anyhow::Result<StackPlan> {
    let anchor = match scope.branch.as_deref() {
        Some(branch) => {
            if !repo.local_branch_exists(branch) {
                anyhow::bail!("no such branch '{branch}': not a local branch");
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

    // A branch the stack never mentions is treated as its own one-node stack on
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
    fn all_leaves_a_branch_outside_the_stack_on_its_own() {
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
}
