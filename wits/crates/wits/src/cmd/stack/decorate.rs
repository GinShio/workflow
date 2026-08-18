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

use super::{fail_if_any, find_open_mrs, map_parallel, resolution, DecorateArgs, ForgeSession};

pub fn run(repo: &Repository, args: &DecorateArgs) -> anyhow::Result<()> {
    let attrs = Attributes {
        labels: args.labels.clone(),
        assignees: args.assignees.clone(),
        reviewers: args.reviewers.clone(),
    };
    if attrs.is_empty() {
        anyhow::bail!("nothing to set: pass at least one --label / --assignee / --reviewer");
    }
    let branches = target_branches(repo, args)?;
    if branches.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo)?;
    let noun = session.noun;

    // Find the open MRs (shared with `anno`), then apply attributes to each in
    // parallel — independent per MR, so a slow forge call for one doesn't stall
    // the rest.
    let (mrs, mut failures) = find_open_mrs(&session, &branches);
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

/// One branch (the named one, or the current) by default; the whole in-scope
/// stack under `--all`.
fn target_branches(repo: &Repository, args: &DecorateArgs) -> anyhow::Result<Vec<String>> {
    if args.all {
        let current = repo.current_branch();
        return Ok(resolution::plan(repo, current.as_deref(), true)?.selected);
    }
    let branch = match &args.branch {
        Some(b) => b.clone(),
        None => repo
            .current_branch()
            .ok_or_else(|| anyhow::anyhow!("detached HEAD: name a branch to decorate"))?,
    };
    Ok(vec![branch])
}
