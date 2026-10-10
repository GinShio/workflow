//! `wits stack publish` — push, submit and anno in one pass.
//!
//! The three verbs stay separate because each reconciles one facet of the
//! remote and can be re-run on its own when a step fails. `publish` is their
//! everyday composition, and more than running them in a row: the stack is
//! planned once, each branch's MRs are looked up once where `submit` and `anno`
//! each looked them all up, and the steps are gated per branch. A branch whose
//! push failed is not submitted — the forge would refuse an MR for a branch it
//! does not have, or open one at a stale tip — though its MR, if it has one,
//! still numbers in its neighbours' navigation; and navigation is written only
//! to MRs that exist.

use wits_util::git::Repository;
use wits_util::project::remotes::Declared;

use super::{anno, fail_if_any, push, resolution, submit, ForgeSession, SubmitArgs};

pub fn run(repo: &Repository, declared: &Declared, args: &SubmitArgs) -> anyhow::Result<()> {
    let target = push::push_target(&declared.roles, None)?;
    let plan = resolution::plan_scoped(repo, declared, &args.scope)?;
    if plan.selected.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let (pushed, mut failures) = push::push_branches(repo, target, &plan.selected);
    if pushed.is_empty() {
        return fail_if_any(failures);
    }

    let session = ForgeSession::open(repo, &declared.roles)?;
    let (mrs, submit_failures) = submit::reconcile(repo, &session, &plan, args, |branch| {
        pushed.iter().any(|p| p == branch)
    });
    failures += submit_failures;

    if plan.standalone {
        log::info!("standalone branch: a lone MR has nothing to navigate");
    } else {
        failures += anno::navigate(repo, &session, &plan, &mrs)?;
    }
    fail_if_any(failures)
}
