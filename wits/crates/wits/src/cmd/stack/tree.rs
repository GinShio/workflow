//! `wits stack tree` — editing the stack's structure directly.
//!
//! These are the manual overrides to the forest that `slice` normally builds:
//! drop branches, move a line onto a new base, or reconcile the file to reality.
//! They are separated from the workflow verbs (sync/submit/anno) because they
//! change *what the stack is* rather than *acting on it*.
//!
//! The one rule running through all of them: removing a branch never throws away
//! the work stacked above it. `Topology::remove` splices a node's children up
//! into its place, so a mid-stack deletion leaves the downstream line intact (and
//! `submit` then retargets its base). The base branch is protected from removal,
//! and moves are refused if they would form a cycle.

use wits_util::git::Repository;
use wits_util::remote::RemoteRoles;

use super::{fail_if_any, resolution, MvArgs, RenameArgs, RmArgs, TreeAction};

pub fn run(repo: &Repository, roles: &RemoteRoles, action: &TreeAction) -> anyhow::Result<()> {
    match action {
        TreeAction::Prune => prune(repo, roles),
        TreeAction::Rm(args) => rm(repo, roles, args),
        TreeAction::Mv(args) => mv(repo, roles, args),
        TreeAction::Rename(args) => rename(repo, args),
    }
}

/// Reconcile the file to git: drop every node whose branch no longer exists
/// locally. This is the automation-friendly cleanup — it needs no branch names,
/// is idempotent, and is safe because a branch that still exists (a live fork
/// sibling included) keeps its node; only genuinely deleted refs are pruned.
fn prune(repo: &Repository, roles: &RemoteRoles) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, roles)?;
    let _lock = resolution::MacheteLock::acquire(repo)?;
    let mut topology = resolution::load_topology(repo)?;
    if topology.is_empty() {
        log::info!("no stack to prune");
        return Ok(());
    }

    let tips = repo.branch_tips();
    let dangling: Vec<String> = topology
        .all()
        .iter()
        .filter(|name| **name != base && !tips.contains_key(*name))
        .cloned()
        .collect();

    if dangling.is_empty() {
        log::info!("nothing to prune");
        return Ok(());
    }

    for name in &dangling {
        let parent = topology.parent(name).unwrap_or(base.as_str()).to_owned();
        if topology.remove(name) {
            log::info!("pruned {name} (children reattached to {parent})");
        }
    }
    resolution::save_topology(repo, &topology)
}

fn rm(repo: &Repository, roles: &RemoteRoles, args: &RmArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, roles)?;
    let mut failures = 0usize;
    let mut deletions = Vec::new();

    {
        let _lock = resolution::MacheteLock::acquire(repo)?;
        let mut topology = resolution::load_topology(repo)?;
        let mut changed = false;

        for branch in &args.branches {
            if *branch == base {
                log::warn!("refusing to remove the base branch '{branch}'");
                continue;
            }
            if !topology.contains(branch) {
                log::warn!("{branch}: not in the stack");
                continue;
            }
            let parent = topology.parent(branch).unwrap_or(base.as_str()).to_owned();
            if topology.remove(branch) {
                changed = true;
                log::info!("removed {branch} from the stack (children reattached to {parent})");
            }
            if args.delete {
                deletions.push(branch.clone());
            }
        }

        if changed {
            resolution::save_topology(repo, &topology)?;
        }
    }

    // The git branch deletions run with the lock released: they fire the
    // reference-transaction hook, whose own `tree rm` would otherwise block on
    // this process's lock until git — waiting on the hook — times out. Saving
    // the stack edit first also means a failed branch delete can never leave
    // the file stale behind gone refs.
    for branch in &deletions {
        match repo.delete_branch(branch, args.force) {
            Ok(()) => log::info!("deleted branch {branch}"),
            // A requested branch delete that failed is a real failure, not a
            // warning to shrug off — count it so the command exits non-zero,
            // matching the other verbs' contract.
            Err(e) => {
                failures += 1;
                log::warn!("could not delete branch {branch}: {e}");
            }
        }
    }
    fail_if_any(failures)
}

fn mv(repo: &Repository, roles: &RemoteRoles, args: &MvArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, roles)?;
    let branch = &args.branch;
    let onto = &args.onto;

    if *branch == base {
        anyhow::bail!("cannot move the base branch '{branch}'");
    }
    if branch == onto {
        anyhow::bail!("a branch cannot be stacked on itself");
    }

    // The branch and its new parent must be real: you can only stack a branch
    // that exists, onto another branch (or the base). This is also what stops a
    // typo from minting a phantom node.
    let tips = repo.branch_tips();
    if !tips.contains_key(branch) {
        anyhow::bail!("branch '{branch}' does not exist locally");
    }
    if *onto != base && !tips.contains_key(onto) {
        anyhow::bail!("parent '{onto}' is neither the base branch nor an existing branch");
    }

    let _lock = resolution::MacheteLock::acquire(repo)?;
    let mut topology = resolution::load_topology(repo)?;
    topology.ensure(onto);
    topology.ensure(branch);
    if !topology.reparent(branch, onto) {
        anyhow::bail!(
            "cannot move '{branch}' onto '{onto}': that would place it beneath its own descendant"
        );
    }
    resolution::save_topology(repo, &topology)?;

    log::info!("moved {branch} onto {onto} (its substack moved with it)");
    log::info!("note: this updates the stack's shape only — rebase {branch} onto {onto} for the code to match");
    Ok(())
}

/// Follow a branch that changed its name, keeping its entry otherwise untouched.
///
/// `git branch -m` leaves the forest naming a branch that no longer exists, and
/// the file is the only record of where that line of work sat. The
/// `reference-transaction` hook can recognise that a deletion may be a rename but
/// never what the new name is, so it leaves the entry and points the user here.
/// It is a pure file edit: the git rename has already happened, and this command
/// never performs one (see `docs/commands/stack.rst`).
///
/// Unlike `rm` and `mv`, the base branch is **not** protected. A renamed base is
/// exactly the case where refusing would leave the forest naming a dead branch,
/// which is the staleness this verb exists to prevent.
fn rename(repo: &Repository, args: &RenameArgs) -> anyhow::Result<()> {
    let from = &args.from;
    let to = &args.to;

    if from == to {
        anyhow::bail!("'{from}' is already the entry's name");
    }
    // The same guard `mv` applies, for the same reason: the new name has to be a
    // branch that exists, so a typo cannot mint a phantom entry.
    if !repo.branch_tips().contains_key(to) {
        anyhow::bail!("branch '{to}' does not exist locally");
    }

    let _lock = resolution::MacheteLock::acquire(repo)?;
    let mut topology = resolution::load_topology(repo)?;

    // Renaming a branch that was never stacked is the ordinary case, not a
    // failure: most branches are not in the forest at all.
    if !topology.contains(from) {
        log::info!("{from}: not in the stack");
        return Ok(());
    }
    if topology.contains(to) {
        anyhow::bail!(
            "'{to}' is already in the stack; it can only be a stale entry (git refuses a rename \
             onto a live branch) — run `wits stack tree prune` and retry"
        );
    }
    if !topology.rename(from, to) {
        anyhow::bail!("could not rename '{from}' to '{to}' in the stack");
    }
    resolution::save_topology(repo, &topology)?;

    log::info!("renamed {from} to {to} in the stack");
    Ok(())
}
