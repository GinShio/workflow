//! `wits stack submit` — make the MRs match the stack.
//!
//! Everything MR-shaped lives here and only here: open the ones that are
//! missing, and correct the base of the ones the topology moved. It never
//! pushes — a branch is expected to already be on `origin` (via `sync`), and if
//! it isn't the forge will refuse to open the MR, which is the honest failure.
//!
//! The two-phase shape (read all state, then apply) exists so that base
//! corrections can fan out in parallel while creation stays serialized: several
//! forges race on their own duplicate detection when sibling MRs are opened at
//! the same instant, and a serial create side-steps that for no real cost.

use std::collections::HashMap;

use wits_util::forge::{Forge, MrState, NewMr};
use wits_util::git::Repository;
use wits_util::log as wits_log;

use super::{fail_if_any, map_parallel, resolution, ForgeSession, SubmitArgs, TitleSource};

/// What a branch needs, decided from its current remote MR state.
enum Decision {
    AlreadyOpen(String),
    FixBase { id: String, display: String },
    Create,
    SkipClosed(String),
}

pub fn run(repo: &Repository, args: &SubmitArgs) -> anyhow::Result<()> {
    let plan = resolution::plan_scoped(repo, &args.scope)?;
    if plan.selected.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo)?;
    let (forge, noun) = (&session.forge, session.noun);
    let tips = repo.branch_tips();

    // Phase 1 — read existing MR state for every branch, in parallel.
    let decisions = map_parallel(&plan.selected, |branch| {
        let base = plan.base_for(branch);
        let decision = decide(forge.as_ref(), branch, &base, &tips, args.force);
        (branch.clone(), base, decision)
    });

    // Phase 2 — sort into the two execution lanes.
    let mut fixes = Vec::new();
    let mut creates = Vec::new();
    let mut failures = 0usize;
    for (branch, base, decision) in decisions {
        match decision {
            Ok(Decision::AlreadyOpen(display)) => {
                log::info!("{noun} {display} already targets {base} ({branch})")
            }
            Ok(Decision::FixBase { id, display }) => fixes.push((branch, base, id, display)),
            Ok(Decision::Create) => creates.push((branch, base)),
            Ok(Decision::SkipClosed(display)) => log::info!(
                "{branch}: a closed {noun} {display} sits at this commit; not reopening (use --force)"
            ),
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e}");
            }
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

/// Decide a branch's fate from its remote MR state. An open MR is either correct
/// or needs its base moved; otherwise we look for a closed/merged leftover and
/// only recreate when our local tip has moved past it (or `--force`), so a branch
/// that was merged and is being reused doesn't spawn a duplicate.
fn decide(
    forge: &dyn Forge,
    branch: &str,
    base: &str,
    tips: &HashMap<String, String>,
    force: bool,
) -> anyhow::Result<Decision> {
    // One fetch (open-preferring) rather than a paired find(Open)+find(NotOpen):
    // the branch has at most one relevant MR, so we pull it once and branch on
    // its state locally.
    let Some(mr) = forge.find_any(branch)? else {
        return Ok(Decision::Create);
    };

    if mr.state == MrState::Open {
        if mr.base == base {
            return Ok(Decision::AlreadyOpen(mr.display));
        }
        return Ok(Decision::FixBase {
            id: mr.id,
            display: mr.display,
        });
    }

    // A merged/closed leftover: recreate only when our local tip has moved past
    // it (or `--force`), so reusing a merged branch doesn't spawn a duplicate.
    let local_tip = tips.get(branch).map(String::as_str);
    let moved_on = match (mr.head_sha.as_deref(), local_tip) {
        (Some(remote), Some(local)) => remote != local,
        // Can't compare — assume it moved rather than silently skip.
        _ => true,
    };
    if force || moved_on {
        Ok(Decision::Create)
    } else {
        Ok(Decision::SkipClosed(mr.display))
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

    /// A stand-in forge that returns canned answers, so the decision logic — the
    /// part that was once subtly wrong about a drifted base — can be tested
    /// without a network.
    struct MockForge {
        open: Option<MergeRequest>,
        closed: Option<MergeRequest>,
    }

    impl Forge for MockForge {
        fn noun(&self) -> &'static str {
            "PR"
        }
        fn find(
            &self,
            _branch: &str,
            state: wits_util::forge::StateFilter,
        ) -> anyhow::Result<Option<MergeRequest>> {
            use wits_util::forge::StateFilter;
            Ok(match state {
                StateFilter::Open => self.open.clone(),
                StateFilter::NotOpen => self.closed.clone(),
            })
        }
        fn find_any(&self, _branch: &str) -> anyhow::Result<Option<MergeRequest>> {
            // Open-preferring, mirroring the real backends' `pick_any`.
            Ok(self.open.clone().or_else(|| self.closed.clone()))
        }
        fn create(&self, _req: &NewMr) -> anyhow::Result<MergeRequest> {
            unreachable!("decide never creates")
        }
        fn set_base(&self, _id: &str, _base: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn set_body(&self, _id: &str, _body: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn apply_attributes(
            &self,
            _id: &str,
            _attrs: &wits_util::forge::Attributes,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn mr(base: &str, sha: Option<&str>, state: MrState) -> MergeRequest {
        MergeRequest {
            id: "1".into(),
            display: "#1".into(),
            state,
            base: base.into(),
            source: String::new(),
            head_sha: sha.map(Into::into),
            body: String::new(),
            web_url: String::new(),
        }
    }

    fn tips_of(branch: &str, sha: &str) -> HashMap<String, String> {
        HashMap::from([(branch.to_owned(), sha.to_owned())])
    }

    // The regression that motivated the find() fix: an open MR whose base no
    // longer matches the topology must be detected and scheduled for a retarget,
    // not missed (which would have created a duplicate).
    #[test]
    fn open_mr_with_drifted_base_is_retargeted() {
        let forge = MockForge {
            open: Some(mr("stale-base", None, MrState::Open)),
            closed: None,
        };
        let d = decide(&forge, "b", "wanted-base", &HashMap::new(), false).unwrap();
        assert!(matches!(d, Decision::FixBase { .. }));
    }

    #[test]
    fn open_mr_with_correct_base_is_a_noop() {
        let forge = MockForge {
            open: Some(mr("base", None, MrState::Open)),
            closed: None,
        };
        let d = decide(&forge, "b", "base", &HashMap::new(), false).unwrap();
        assert!(matches!(d, Decision::AlreadyOpen(_)));
    }

    #[test]
    fn no_mr_means_create() {
        let forge = MockForge {
            open: None,
            closed: None,
        };
        let d = decide(&forge, "b", "base", &HashMap::new(), false).unwrap();
        assert!(matches!(d, Decision::Create));
    }

    #[test]
    fn closed_mr_at_current_tip_is_skipped_unless_forced() {
        let forge = MockForge {
            open: None,
            closed: Some(mr("base", Some("abc"), MrState::Merged)),
        };
        let tips = tips_of("b", "abc");
        assert!(matches!(
            decide(&forge, "b", "base", &tips, false).unwrap(),
            Decision::SkipClosed(_)
        ));
        // --force overrides the guard.
        assert!(matches!(
            decide(&forge, "b", "base", &tips, true).unwrap(),
            Decision::Create
        ));
    }

    #[test]
    fn closed_mr_left_behind_by_new_commits_is_recreated() {
        let forge = MockForge {
            open: None,
            closed: Some(mr("base", Some("old-sha"), MrState::Closed)),
        };
        let tips = tips_of("b", "new-sha");
        assert!(matches!(
            decide(&forge, "b", "base", &tips, false).unwrap(),
            Decision::Create
        ));
    }
}
