//! `wits review checkout` — put an MR's code somewhere runnable.
//!
//! Materialization is what lets a reviewer build, run, and fuzz an MR locally.
//! There is **one** review worktree (a sibling `../<main>.review`), reused for
//! every MR: `checkout <mr>` and `--next`/`--prev` just switch its HEAD to the
//! target snapshot. A stack therefore costs one worktree, not one per member,
//! and pruning a merged member never disturbs it. `--in-place` instead moves
//! HEAD in the current working tree (one at a time, hard-guarding a dirty tree
//! so reviewing someone else's work never buries yours).
//!
//! The worktree mechanics themselves — where it goes, how `git worktree add` is
//! driven, how submodules are materialised — are [`wits_util::worktree`]'s, shared
//! with `wits worktree`. What stays here is review's own policy: *one* worktree
//! for the whole store, pointed at whichever snapshot you are reading.
//!
//! `--submodules` materialises the checkout's submodules. The **first** time it
//! borrows objects from your primary checkout (so even large submodules cost no
//! re-download); on a later HEAD switch it just **updates** the already-present
//! submodules to the new snapshot's pins — the borrow is a one-time
//! materialisation concern, not something to redo on every switch.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use wits_util::git::Repository;
use wits_util::worktree;

use super::model::short;
use super::{default_worktree_dir, local, CheckoutArgs, ReviewCtx};

pub fn run(repo: &Repository, args: &CheckoutArgs) -> Result<()> {
    let ctx = local(repo)?;
    let id = resolve_target(&ctx, args)?;

    let info = ctx
        .store
        .load_info(&id)
        .with_context(|| format!("MR {id} isn't fetched — run `wits review fetch {id}` first"))?;
    let Some(head) = info.head().map(str::to_owned) else {
        bail!("MR {id} has no fetched snapshot — run `wits review fetch {id}` for full detail");
    };

    // Materialise the checkout, idempotently — so a second run (e.g. to add
    // `--submodules`) reuses what the first made rather than erroring.
    let checkout = if args.in_place {
        // In-place operates on the *current* working tree — whichever worktree
        // the command was invoked from — since that is precisely what "check out
        // here" means.
        let current = ctx.repo.toplevel().unwrap_or_else(|| PathBuf::from("."));
        let git = Repository::new(&current);
        if git.is_dirty() {
            bail!(
                "working tree has uncommitted changes; commit or stash them first \
                 (in-place checkout moves HEAD and would bury your work)"
            );
        }
        git.checkout(&head)
            .with_context(|| format!("checking out MR {id} in place"))?;
        log::info!("checked out MR {id} at {} (detached HEAD)", short(&head));
        current
    } else {
        // One review worktree, re-pointed at each MR. An explicit `--worktree` is
        // resolved against this cwd, so the exists-check and the worktree that
        // gets made agree on which directory is meant.
        let dir = match &args.worktree {
            Some(dir) => std::path::absolute(dir)
                .with_context(|| format!("resolving --worktree {}", dir.display()))?,
            None => default_worktree_dir(&ctx.repo),
        };
        if dir.exists() {
            // Re-point the existing review worktree at this MR by switching HEAD.
            // A snapshot is a commit, never a branch, so this can't collide with
            // a branch checked out elsewhere.
            worktree::repoint(&dir, &head)
                .with_context(|| format!("switching the review worktree to MR {id}"))?;
            log::info!(
                "MR {id}: switched review worktree {} to {}",
                dir.display(),
                short(&head)
            );
        } else {
            worktree::create(&ctx.repo, &dir, &head)
                .with_context(|| format!("checking out MR {id}"))?;
            log::info!("MR {id} checked out into worktree {}", dir.display());
        }
        dir
    };

    if args.submodules {
        let synced = worktree::sync_submodules(&checkout)
            .with_context(|| format!("syncing submodules for MR {id}"))?;
        if synced == 0 {
            log::info!("MR {id}: no submodules present");
        } else {
            log::info!("MR {id}: synced {synced} submodule(s)");
        }
    }

    ctx.store.set_current(&id)?;
    Ok(())
}

/// The MR to materialise: explicit, or the neighbour of the current checkout.
fn resolve_target(ctx: &ReviewCtx, args: &CheckoutArgs) -> Result<String> {
    if let Some(handle) = &args.mr {
        return super::parse_mr_handle(handle);
    }
    if !args.next && !args.prev {
        bail!("give an MR to check out, or use --next/--prev");
    }

    let current = ctx
        .store
        .current()
        .context("no current review to navigate from; check out an MR first")?;
    let infos = ctx.store.list_infos();
    let (chain, pos) = super::stack_chain(&infos, &current);
    let target = if args.next {
        chain.get(pos + 1)
    } else {
        pos.checked_sub(1).and_then(|i| chain.get(i))
    };
    target.cloned().with_context(|| {
        let edge = if args.next { "top" } else { "bottom" };
        format!("already at the {edge} of the stack (current MR {current})")
    })
}
