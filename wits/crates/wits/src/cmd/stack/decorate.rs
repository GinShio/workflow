//! `wits stack decorate` — add labels, assignees, and reviewers to MRs.
//!
//! Unlike the other verbs, attributes differ per MR (one branch wants one label,
//! its neighbour another), so this one is **single-MR by default**: it acts on
//! the named branch, or the current one. To set the *same* attributes across the
//! whole stack — a common `stacked` label, say — use `--all`. Per-branch
//! differences are expressed simply by running it once per branch with that
//! branch's flags (typically from a small per-repo script).
//!
//! It is additive and idempotent: it only adds what you list and never removes
//! anything, so a project's own label/reviewer automation is never clobbered, and
//! re-running is safe. Like `submit`, it leaves the work of *finding* the MR to
//! the forge and never pushes.

use wits_util::forge::Attributes;
use wits_util::git::Repository;
use wits_util::log as wits_log;
use wits_util::remote::RemoteRoles;

use super::resolution::StackPlan;
use super::{
    fail_if_any, find_open_mrs, map_parallel, resolution, DecorateArgs, ForgeSession, ScopeArgs,
};

pub fn run(repo: &Repository, roles: &RemoteRoles, args: &DecorateArgs) -> anyhow::Result<()> {
    let attrs = Attributes {
        labels: args.labels.clone(),
        assignees: args.assignees.clone(),
        reviewers: args.reviewers.clone(),
    };
    if attrs.is_empty() {
        anyhow::bail!("nothing to set: pass at least one --label / --assignee / --reviewer");
    }
    let (branches, plan) = target_branches(repo, roles, args)?;
    if branches.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo, roles)?;
    let noun = session.noun;

    // Find the open MRs (shared with `anno`), then apply attributes to each in
    // parallel — independent per MR, so a slow forge call for one doesn't stall
    // the rest.
    let (mrs, mut failures) = find_open_mrs(&session, &branches, |branch| {
        plan.as_ref().map(|plan| plan.base_for(branch))
    });
    let results = map_parallel(&mrs, |(branch, mr)| {
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!(
                "decorate {noun} {} ({branch}): {}",
                mr.display,
                attrs.summary()
            ));
            return Ok(());
        }
        session.forge.apply_attributes(&mr.id, &attrs)
    });
    for ((branch, mr), result) in mrs.iter().zip(results) {
        match result {
            // Keep the full `anyhow` chain in the log rather than flattening to a
            // bare string, so a forge error's cause survives.
            Ok(()) => log::info!("decorated {noun} {} ({branch})", mr.display),
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e:#}");
            }
        }
    }
    fail_if_any(failures)
}

/// One branch (the named one, or the current) by default; under `--all`, every
/// branch of that branch's whole stack, exactly as the other verbs' `--all`
/// scopes it — together with that plan, which knows the base each MR should
/// target.
fn target_branches(
    repo: &Repository,
    roles: &RemoteRoles,
    args: &DecorateArgs,
) -> anyhow::Result<(Vec<String>, Option<StackPlan>)> {
    if args.all {
        let scope = ScopeArgs {
            branch: args.branch.clone(),
            all: true,
        };
        let plan = resolution::plan_scoped(repo, roles, &scope)?;
        return Ok((plan.selected.clone(), Some(plan)));
    }
    let branch = match &args.branch {
        Some(b) => b.clone(),
        None => repo
            .current_branch()
            .ok_or_else(|| anyhow::anyhow!("detached HEAD: name a branch to decorate"))?,
    };
    Ok((vec![branch], None))
}
