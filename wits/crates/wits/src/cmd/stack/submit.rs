//! `wits stack submit` — make the MRs match the stack.
//!
//! Everything MR-shaped lives here and only here: open the ones that are
//! missing, and correct the base of the ones the topology moved. It never
//! pushes — a branch is expected to already be on `origin` (via `push`), and if
//! it isn't the forge will refuse to open the MR, which is the honest failure.
//!
//! The two-phase shape (read all state, then apply) exists so that base
//! corrections can fan out in parallel while creation stays serialized: several
//! forges race on their own duplicate detection when sibling MRs are opened at
//! the same instant, and a serial create side-steps that for no real cost.

use wits_util::forge::{HeadRepo, NewMr};
use wits_util::git::Repository;
use wits_util::log as wits_log;
use wits_util::project::remotes::Declared;

use super::{
    fail_if_any, map_parallel, resolution, BranchMrs, ForgeSession, SubmitArgs, TitleSource,
};

/// What a branch needs, decided from its current remote MR state.
enum Decision {
    AlreadyOpen(String),
    FixBase { id: String, display: String },
    Create,
    SkipClosed(String),
}

pub fn run(repo: &Repository, declared: &Declared, args: &SubmitArgs) -> anyhow::Result<()> {
    let plan = resolution::plan_scoped(repo, declared, &args.scope)?;
    if plan.selected.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo, &declared.roles)?;
    let (forge, noun) = (&session.forge, session.noun);
    let tips = repo.branch_tips();

    // Phase 1 — read existing MR state for every branch, in parallel.
    let found = map_parallel(&plan.selected, |branch| {
        (
            branch.clone(),
            plan.base_for(branch),
            forge.mrs_for_branch(HeadRepo::Origin, branch),
        )
    });

    // Phase 2 — decide, and sort into the two execution lanes.
    let mut fixes = Vec::new();
    let mut creates = Vec::new();
    let mut failures = 0usize;
    for (branch, base, mrs) in found {
        let mrs = match mrs {
            Ok(mrs) => BranchMrs::sort(mrs, Some(&base)),
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e}");
                continue;
            }
        };
        mrs.warn_other_open(noun, &branch);
        let local_tip = tips.get(&branch).map(String::as_str);
        match decide(mrs, &base, local_tip, args.force) {
            Decision::AlreadyOpen(display) => {
                log::info!("{noun} {display} already targets {base} ({branch})")
            }
            Decision::FixBase { id, display } => fixes.push((branch, base, id, display)),
            Decision::Create => creates.push((branch, base)),
            Decision::SkipClosed(display) => log::info!(
                "{branch}: a closed {noun} {display} sits at this commit; not reopening (use --force)"
            ),
        }
    }

    // Base corrections — independent, so fan out.
    let fix_results = map_parallel(&fixes, |(branch, base, id, display)| {
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!("retarget {noun} {display} ({branch}) -> {base}"));
            return Ok(());
        }
        forge.set_base(id, base)
    });
    for ((branch, base, _, display), result) in fixes.iter().zip(fix_results) {
        match result {
            Ok(()) => log::info!("retargeted {noun} {display} ({branch}) -> {base}"),
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e}");
            }
        }
    }

    // Creation — serialized on purpose (see module note).
    for (branch, base) in &creates {
        let draft = *base != plan.base_branch && !args.no_draft;
        let (title, body) = title_body(repo, base, branch, args.title_source);
        if wits_log::is_dry_run() {
            let tag = if draft { " (draft)" } else { "" };
            wits_log::dry_run(&format!("create {noun} for {branch} -> {base}{tag}"));
            continue;
        }
        let req = NewMr {
            branch: branch.clone(),
            base: base.clone(),
            title,
            body,
            draft,
        };
        match forge.create(&req) {
            Ok(mr) => log::info!("created {noun} {} ({branch}): {}", mr.display, mr.web_url),
            Err(e) => {
                failures += 1;
                log::warn!("failed to create {noun} for {branch}: {e}");
            }
        }
    }

    fail_if_any(failures)
}

/// Decide a branch's fate from its MRs. An open MR is either correct or needs
/// its base moved; otherwise the newest closed/merged leftover decides, and the
/// branch is recreated only when our local tip has moved past it (or `--force`),
/// so a branch that was merged and is being reused doesn't spawn a duplicate.
fn decide(mrs: BranchMrs, base: &str, local_tip: Option<&str>, force: bool) -> Decision {
    if let Some(mr) = mrs.open {
        if mr.base == base {
            return Decision::AlreadyOpen(mr.display);
        }
        return Decision::FixBase {
            id: mr.id,
            display: mr.display,
        };
    }
    let Some(mr) = mrs.newest_closed else {
        return Decision::Create;
    };
    let moved_on = match (mr.head_sha.as_deref(), local_tip) {
        (Some(remote), Some(local)) => remote != local,
        // Can't compare — assume it moved rather than silently skip.
        _ => true,
    };
    if force || moved_on {
        Decision::Create
    } else {
        Decision::SkipClosed(mr.display)
    }
}

/// Seed a new MR's title and body from one of the branch's commits — the latest
/// by default, since that is usually the change's final framing.
fn title_body(
    repo: &Repository,
    base: &str,
    branch: &str,
    source: TitleSource,
) -> (String, String) {
    let commits = repo.commits(&format!("{base}..{branch}"));
    let chosen = match source {
        TitleSource::First => commits.first(),
        TitleSource::Last => commits.last(),
    };
    match chosen {
        Some(c) if !c.subject.is_empty() => (c.subject.clone(), c.body.clone()),
        _ => (branch.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wits_util::forge::{MergeRequest, MrState};

    fn mr(id: &str, base: &str, sha: Option<&str>, state: MrState) -> MergeRequest {
        MergeRequest {
            id: id.into(),
            display: format!("#{id}"),
            state,
            base: base.into(),
            source: String::new(),
            head_sha: sha.map(Into::into),
            body: String::new(),
            web_url: String::new(),
        }
    }

    /// A branch's MRs, newest first as the forge returns them, sorted for `base`.
    fn mrs(base: &str, list: Vec<MergeRequest>) -> BranchMrs {
        BranchMrs::sort(list, Some(base))
    }

    // The regression that motivated matching by head only: an open MR whose base
    // no longer matches the topology must be detected and scheduled for a
    // retarget, not missed (which would have created a duplicate).
    #[test]
    fn open_mr_with_drifted_base_is_retargeted() {
        let open = vec![mr("1", "stale-base", None, MrState::Open)];
        let d = decide(mrs("wanted-base", open), "wanted-base", None, false);
        assert!(matches!(d, Decision::FixBase { .. }));
    }

    #[test]
    fn open_mr_with_correct_base_is_a_noop() {
        let open = vec![mr("1", "base", None, MrState::Open)];
        let d = decide(mrs("base", open), "base", None, false);
        assert!(matches!(d, Decision::AlreadyOpen(_)));
    }

    #[test]
    fn no_mr_means_create() {
        let d = decide(mrs("base", Vec::new()), "base", None, false);
        assert!(matches!(d, Decision::Create));
    }

    #[test]
    fn closed_mr_at_current_tip_is_skipped_unless_forced() {
        let merged = || vec![mr("1", "base", Some("abc"), MrState::Merged)];
        assert!(matches!(
            decide(mrs("base", merged()), "base", Some("abc"), false),
            Decision::SkipClosed(_)
        ));
        // --force overrides the guard.
        assert!(matches!(
            decide(mrs("base", merged()), "base", Some("abc"), true),
            Decision::Create
        ));
    }

    #[test]
    fn closed_mr_left_behind_by_new_commits_is_recreated() {
        let closed = vec![mr("1", "base", Some("old-sha"), MrState::Closed)];
        assert!(matches!(
            decide(mrs("base", closed), "base", Some("new-sha"), false),
            Decision::Create
        ));
    }

    // Two open MRs from one branch, the more recently updated one into another
    // base: the one already on the planned base is left alone rather than its
    // stray twin being retargeted onto it.
    #[test]
    fn an_open_mr_on_the_base_is_kept_over_a_newer_stray() {
        let open = vec![
            mr("9", "other", None, MrState::Open),
            mr("4", "main", None, MrState::Open),
        ];
        let d = decide(mrs("main", open), "main", None, false);
        assert!(matches!(d, Decision::AlreadyOpen(ref display) if display == "#4"));
    }

    #[test]
    fn an_open_mr_outranks_a_newer_closed_one() {
        let list = vec![
            mr("5", "main", Some("abc"), MrState::Merged),
            mr("4", "old", None, MrState::Open),
        ];
        let d = decide(mrs("main", list), "main", Some("abc"), false);
        assert!(matches!(d, Decision::FixBase { ref id, .. } if id == "4"));
    }
}
