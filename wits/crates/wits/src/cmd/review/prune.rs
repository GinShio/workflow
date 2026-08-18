//! `wits review prune` — drop the store for MRs that are done or gone quiet.
//!
//! Terminal MRs (merged/closed) are always dropped; `--older-than` also catches
//! *dormant* ones — those not fetched within a day count or since an ISO date.
//! Pruning removes both the per-MR directory and the snapshot pins
//! (`refs/wits/review/*`), so git can finally collect the reviewed objects.

use anyhow::Result;

use wits_util::forge::MrState;
use wits_util::git::Repository;
use wits_util::time::parse_cutoff;

use super::model::state_word;
use super::store::refs;
use super::{default_worktree_dir, local, parse_mr_handle, PruneArgs, ReviewCtx};

pub fn run(repo: &Repository, args: &PruneArgs) -> Result<()> {
    let ctx = local(repo)?;
    let current = ctx.store.current();

    // A named MR is dropped whatever its state — the "I'm done with this one,
    // reclaim its store even though it hasn't merged" path.
    if let Some(handle) = &args.mr {
        let id = parse_mr_handle(handle)?;
        prune_one(&ctx, &id, "requested", &current)?;
        reclaim_worktree_if_empty(&ctx);
        return Ok(());
    }

    // Otherwise sweep: terminal MRs always, plus dormant ones under a cutoff.
    // `--older-than` is a day count or an ISO-8601 date, as a Unix instant.
    let cutoff = args.older_than.as_deref().map(parse_cutoff).transpose()?;
    let mut pruned = 0;
    for info in ctx.store.list_infos() {
        let terminal = matches!(info.mr.state, MrState::Merged | MrState::Closed);
        // Dormant iff we have a real last-sync time (a full fetch, `fetched_at`
        // > 0) that predates the cutoff. A feed-only entry (`0`) is never dormant.
        let stale = cutoff.is_some_and(|before| info.fetched_at > 0 && info.fetched_at < before);
        if !terminal && !stale {
            continue;
        }
        let why = if terminal {
            state_word(info.mr.state, info.mr.draft)
        } else {
            "dormant"
        };
        prune_one(&ctx, &info.mr.id, why, &current)?;
        pruned += 1;
    }

    if pruned == 0 {
        log::info!("nothing to prune");
    } else {
        log::info!("pruned {pruned} MR(s)");
        reclaim_worktree_if_empty(&ctx);
    }
    Ok(())
}

/// Drop one MR's local footprint: its snapshot pins (so git can GC the objects)
/// and its store directory — clearing the current-checkout pointer when it named
/// this MR. It does **not** touch the review worktree: there is a single one,
/// shared across a stack, so a merged member merging out must not disturb the
/// review of its still-open siblings. The worktree is reclaimed once the store
/// empties (see [`reclaim_worktree_if_empty`]).
fn prune_one(ctx: &ReviewCtx, id: &str, why: &str, current: &Option<String>) -> Result<()> {
    for (name, _) in ctx.repo.refs_under(&refs::mr_prefix(id)) {
        if let Err(e) = ctx.repo.delete_ref(&name) {
            log::warn!("MR {id}: could not delete {name}: {e}");
        }
    }
    ctx.store.delete_mr(id)?;
    // If we just pruned the checked-out MR, drop the dangling pointer so a
    // later `--next`/`--prev` doesn't navigate from a store that's gone. The
    // worktree stays put (still showing that snapshot until the next checkout,
    // which is a cheap HEAD switch away).
    if current.as_deref() == Some(id) {
        ctx.store.clear_current()?;
    }
    log::info!("pruned MR {id} ({why})");
    Ok(())
}

/// Reclaim the single review worktree once the store has no MRs left — it has
/// nothing more to show. Best-effort and forced (untracked build output is the
/// reviewer's, but an empty store means the review session is over). It is a
/// no-op while any MR remains, since a stack shares this one worktree.
fn reclaim_worktree_if_empty(ctx: &ReviewCtx) {
    if !ctx.store.list_infos().is_empty() {
        return;
    }
    let dir = default_worktree_dir(&ctx.repo);
    if !dir.exists() {
        return;
    }
    match wits_util::worktree::remove(&ctx.repo, &dir, true) {
        Ok(()) => log::info!("removed review worktree {}", dir.display()),
        Err(e) => log::warn!("could not remove review worktree {}: {e}", dir.display()),
    }
}
