//! `wits stack sync` — push the in-scope branches to the `origin` role's remote,
//! and nothing else.
//!
//! Sync is intentionally the dumbest verb: it makes the remote branch tips match
//! the local ones. No forge, no MR. Keeping it that narrow is what lets the
//! other verbs assume the remote is current without entangling push failures
//! with MR logic.

use anyhow::Context;
use wits_util::git::Repository;
use wits_util::remote::RemoteRoles;

use super::{fail_if_any, map_parallel, resolution, ScopeArgs};

pub fn run(repo: &Repository, roles: &RemoteRoles, scope: &ScopeArgs) -> anyhow::Result<()> {
    let target = push_target(roles, None)?;
    let plan = resolution::plan_scoped(repo, roles, scope)?;

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
        let outcome = repo.push_force_with_lease(target, branch);
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

/// The remote a push goes to: `requested` when the caller names one, else the
/// `origin` role's holder.
///
/// Whatever the source, the result may not be the `upstream` holder. Pushing a
/// stack of feature branches at the repository we merge *into* is wrong even where
/// we have the rights: `submit` would then open every MR from the target onto
/// itself. That is why there is no fallback from `origin` to the merge target
/// here, unlike everywhere else in the toolset.
///
/// `requested` is always `None` today, and with it the rejection cannot fire —
/// a remote declares at most one `role`, and `remotes::validate` refuses a second
/// holder for either, so the `origin` holder is never the `upstream` holder. The
/// check is written here rather than omitted because this is the parameter a
/// user-supplied remote name will arrive through, and then it is the only thing
/// standing between a typo and an MR from `main` onto `main`.
fn push_target<'a>(roles: &'a RemoteRoles, requested: Option<&'a str>) -> anyhow::Result<&'a str> {
    let target = match requested {
        Some(name) => name,
        None => roles.origin().context(
            "no remote holds the origin role, so there is nowhere to push; declare one, or name \
             a remote 'origin'",
        )?,
    };
    if roles.upstream() == Some(target) {
        anyhow::bail!(
            "'{target}' holds the upstream role — that is the repository we merge into, not one \
             to push a stack at"
        );
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_target_defaults_to_the_origin_holder_under_any_name() {
        let roles = RemoteRoles::new(Some("myfork".into()), Some("fdo".into()));
        assert_eq!(push_target(&roles, None).unwrap(), "myfork");
    }

    /// The merge target is never a push destination, which is the one place the
    /// toolset deliberately has no fallback from `origin`.
    #[test]
    fn a_read_only_checkout_has_nowhere_to_push() {
        let read_only = RemoteRoles::new(None, Some("fdo".into()));
        let err = push_target(&read_only, None).unwrap_err().to_string();
        assert!(err.contains("nowhere to push"), "{err}");
    }

    /// The rejection a future `--remote` makes reachable: naming the upstream
    /// holder explicitly must fail rather than push at the merge target.
    #[test]
    fn naming_the_upstream_holder_is_refused() {
        let roles = RemoteRoles::new(Some("myfork".into()), Some("fdo".into()));
        let err = push_target(&roles, Some("fdo")).unwrap_err().to_string();
        assert!(err.contains("holds the upstream role"), "{err}");
        // A free name that holds no role at all is a legitimate push target.
        assert_eq!(push_target(&roles, Some("rocm")).unwrap(), "rocm");
    }
}
