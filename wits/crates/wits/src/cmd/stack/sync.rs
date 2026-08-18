//! `wits stack sync` — push the in-scope branches to `origin`, and nothing else.
//!
//! Sync is intentionally the dumbest verb: it makes the remote branch tips match
//! the local ones. No forge, no MR. Keeping it that narrow is what lets the
//! other verbs assume the remote is current without entangling push failures
//! with MR logic.

use wits_util::git::Repository;

use super::{fail_if_any, map_parallel, resolution, ScopeArgs};

pub fn run(repo: &Repository, scope: &ScopeArgs) -> anyhow::Result<()> {
    let plan = resolution::plan_scoped(repo, scope)?;

    // Only push branches that actually exist locally; a name in the file with no
    // ref is a stale entry, not something to push.
    let tips = repo.branch_tips();
    let branches: Vec<String> = plan
        .selected
        .iter()
        .filter(|b| tips.contains_key(*b))
        .cloned()
        .collect();

    if branches.is_empty() {
        log::info!("nothing to push");
        return Ok(());
    }

    let results = map_parallel(&branches, |branch| {
        let outcome = repo.push_force_with_lease("origin", branch);
        (branch.clone(), outcome)
    });

    let mut failures = 0;
    for (branch, outcome) in results {
        match outcome {
            Ok(()) => log::info!("pushed {branch}"),
            Err(e) => {
                failures += 1;
                log::warn!("failed to push {branch}: {e}");
            }
        }
    }

    // Same all-or-nothing exit contract as submit/anno/decorate: per-branch
    // failures are warned, and the command still exits non-zero if any occurred.
    fail_if_any(failures)
}
