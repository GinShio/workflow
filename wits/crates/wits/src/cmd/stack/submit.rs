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

use std::collections::HashMap;

use wits_util::forge::{HeadRepo, MergeRequest, NewMr};
use wits_util::git::Repository;
use wits_util::log as wits_log;
use wits_util::project::remotes::Declared;

use super::resolution::StackPlan;
use super::{
    fail_if_any, map_parallel, resolution, BranchMrs, ForgeSession, SubmitArgs, TitleSource,
};

/// What a branch needs, decided from its current remote MR state.
enum Decision {
    AlreadyOpen(MergeRequest),
    FixBase(MergeRequest),
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
    let (_, failures) = reconcile(repo, &session, &plan, args, |_| true);
    fail_if_any(failures)
}

/// Make the MRs of the plan's branches match it, acting only on the branches
/// `acts_on` admits: open each missing MR, and move each drifted base. Every
/// branch's MRs are looked up once, in parallel, whether acted on or not.
///
/// Returns each branch's open MR as it stands afterwards — a created one, a
/// retargeted one with its new base — and the number of failures. A branch with
/// no open MR (a create under `--dry-run`, a closed MR left alone, a failure) is
/// absent.
pub(super) fn reconcile(
    repo: &Repository,
    session: &ForgeSession,
    plan: &StackPlan,
    args: &SubmitArgs,
    acts_on: impl Fn(&str) -> bool,
) -> (HashMap<String, MergeRequest>, usize) {
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
    let mut open = HashMap::new();
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
        if !acts_on(&branch) {
            if let Some(mr) = mrs.open {
                open.insert(branch, mr);
            }
            continue;
        }
        let local_tip = tips.get(&branch).map(String::as_str);
        match decide(mrs, &base, local_tip, args.force) {
            Decision::AlreadyOpen(mr) => {
                log::info!("{noun} {} already targets {base} ({branch})", mr.display);
                open.insert(branch, mr);
            }
            Decision::FixBase(mr) => fixes.push((branch, base, mr)),
            Decision::Create => creates.push((branch, base)),
            Decision::SkipClosed(display) => log::info!(
                "{branch}: a closed {noun} {display} sits at this commit; not reopening (use --force)"
            ),
        }
    }

    // Base corrections — independent, so fan out.
    let fix_results = map_parallel(&fixes, |(branch, base, mr)| {
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!(
                "retarget {noun} {} ({branch}) -> {base}",
                mr.display
            ));
            return Ok(());
        }
        forge.set_base(&mr.id, base)
    });
    for ((branch, base, mut mr), result) in fixes.into_iter().zip(fix_results) {
        match result {
            Ok(()) => {
                log::info!("retargeted {noun} {} ({branch}) -> {base}", mr.display);
                mr.base = base;
                open.insert(branch, mr);
            }
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e}");
            }
        }
    }

    // Creation — serialized on purpose (see module note).
    for (branch, base) in creates {
        let draft = base != plan.base_branch && !args.no_draft;
        let (title, body) = title_body(repo, &base, &branch, args.title_source);
        if wits_log::is_dry_run() {
            let tag = if draft { " (draft)" } else { "" };
            wits_log::dry_run(&format!("create {noun} for {branch} -> {base}{tag}"));
            continue;
        }
        let req = NewMr {
            branch: branch.clone(),
            base,
            title,
            body,
            draft,
        };
        match forge.create(&req) {
            Ok(mr) => {
                log::info!("created {noun} {} ({branch}): {}", mr.display, mr.web_url);
                open.insert(branch, mr);
            }
            Err(e) => {
                failures += 1;
                log::warn!("failed to create {noun} for {branch}: {e}");
            }
        }
    }

    (open, failures)
}

/// Decide a branch's fate from its MRs. An open MR is either correct or needs
/// its base moved; otherwise the newest closed/merged leftover decides, and the
/// branch is recreated only when our local tip has moved past it (or `--force`),
/// so a branch that was merged and is being reused doesn't spawn a duplicate.
fn decide(mrs: BranchMrs, base: &str, local_tip: Option<&str>, force: bool) -> Decision {
    if let Some(mr) = mrs.open {
        if mr.base == base {
            return Decision::AlreadyOpen(mr);
        }
        return Decision::FixBase(mr);
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
        assert!(matches!(d, Decision::FixBase(_)));
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
        assert!(matches!(d, Decision::AlreadyOpen(ref mr) if mr.display == "#4"));
    }

    #[test]
    fn an_open_mr_outranks_a_newer_closed_one() {
        let list = vec![
            mr("5", "main", Some("abc"), MrState::Merged),
            mr("4", "old", None, MrState::Open),
        ];
        let d = decide(mrs("main", list), "main", Some("abc"), false);
        assert!(matches!(d, Decision::FixBase(ref mr) if mr.id == "4"));
    }

    mod gating {
        use std::collections::HashMap;
        use std::path::Path;
        use std::sync::{Arc, Mutex};

        use wits_util::forge::{
            Attributes, Forge, HeadRepo, MergeRequest, MrComment, MrState, NewMr,
        };
        use wits_util::git::Repository;

        use super::super::reconcile;
        use crate::cmd::stack::resolution::StackPlan;
        use crate::cmd::stack::topology::Topology;
        use crate::cmd::stack::{ForgeSession, ScopeArgs, SubmitArgs, TitleSource};

        /// A forge holding one open MR per named branch, recording the writes.
        struct Fake {
            open: HashMap<&'static str, &'static str>,
            writes: Arc<Mutex<Vec<String>>>,
        }

        impl Forge for Fake {
            fn noun(&self) -> &'static str {
                "PR"
            }
            fn mrs_for_branch(
                &self,
                _: HeadRepo,
                branch: &str,
            ) -> anyhow::Result<Vec<MergeRequest>> {
                Ok(self
                    .open
                    .get(branch)
                    .map(|base| vec![mr(branch, base)])
                    .unwrap_or_default())
            }
            fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest> {
                self.writes
                    .lock()
                    .unwrap()
                    .push(format!("create {}", req.branch));
                Ok(mr(&req.branch, &req.base))
            }
            fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()> {
                self.writes
                    .lock()
                    .unwrap()
                    .push(format!("set_base {id} {base}"));
                Ok(())
            }
            fn set_body(&self, _: &str, _: &str) -> anyhow::Result<()> {
                unreachable!()
            }
            fn apply_attributes(&self, _: &str, _: &Attributes) -> anyhow::Result<()> {
                unreachable!()
            }
            fn list_comments(&self, _: &str) -> anyhow::Result<Vec<MrComment>> {
                unreachable!()
            }
            fn add_comment(&self, _: &str, _: &str) -> anyhow::Result<()> {
                unreachable!()
            }
            fn edit_comment(&self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
                unreachable!()
            }
        }

        fn mr(branch: &str, base: &str) -> MergeRequest {
            MergeRequest {
                id: format!("id-{branch}"),
                display: format!("#{branch}"),
                state: MrState::Open,
                base: base.into(),
                source: branch.into(),
                head_sha: None,
                body: String::new(),
                web_url: String::new(),
            }
        }

        fn git(dir: &Path, args: &[&str]) {
            let mut all = vec!["-c", "user.name=T", "-c", "user.email=t@e.com"];
            all.extend_from_slice(args);
            wits_util::process::Command::new("git")
                .args(all)
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        }

        /// `main` → `a` → `b` as branches with a commit each, `a`'s MR open
        /// against `stale`, and `b` without one.
        type Writes = Arc<Mutex<Vec<String>>>;

        fn setup(
            open_a_on: &'static str,
        ) -> (
            tempfile::TempDir,
            Repository,
            ForgeSession,
            StackPlan,
            Writes,
        ) {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            git(dir, &["init", "-q", "-b", "main", "."]);
            git(dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
            git(dir, &["switch", "-q", "-c", "a"]);
            git(dir, &["commit", "-q", "--allow-empty", "-m", "a"]);
            git(dir, &["switch", "-q", "-c", "b"]);
            git(dir, &["commit", "-q", "--allow-empty", "-m", "b"]);
            let writes = Writes::default();
            let session = ForgeSession {
                forge: Box::new(Fake {
                    open: HashMap::from([("a", open_a_on)]),
                    writes: Arc::clone(&writes),
                }),
                noun: "PR",
            };
            let plan = StackPlan {
                topology: Topology::parse("main\n    a\n        b\n"),
                base_branch: "main".into(),
                selected: vec!["a".into(), "b".into()],
                standalone: false,
            };
            let repo = Repository::new(dir);
            (tmp, repo, session, plan, writes)
        }

        fn args() -> SubmitArgs {
            SubmitArgs {
                scope: ScopeArgs {
                    branch: None,
                    all: false,
                },
                no_draft: false,
                force: false,
                title_source: TitleSource::Last,
            }
        }

        #[test]
        fn a_branch_not_acted_on_is_not_opened_but_its_mr_still_counts() {
            let (_tmp, repo, session, plan, writes) = setup("main");
            let (open, failures) = reconcile(&repo, &session, &plan, &args(), |b| b == "a");
            assert_eq!(failures, 0);
            assert!(writes.lock().unwrap().is_empty(), "b's MR is not opened");
            assert_eq!(open.keys().collect::<Vec<_>>(), ["a"]);
        }

        #[test]
        fn acted_on_a_missing_mr_is_opened_and_a_drifted_base_moved() {
            let (_tmp, repo, session, plan, writes) = setup("stale");
            let (open, failures) = reconcile(&repo, &session, &plan, &args(), |_| true);
            assert_eq!(failures, 0);
            assert_eq!(*writes.lock().unwrap(), ["set_base id-a main", "create b"]);
            assert_eq!(
                open["a"].base, "main",
                "the retargeted MR carries its new base"
            );
            assert_eq!(open["b"].base, "a");
        }
    }
}
