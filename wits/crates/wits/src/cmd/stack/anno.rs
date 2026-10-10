//! `wits stack anno` — keep each MR's navigation comment pointing at its
//! neighbours.
//!
//! A reviewer landing on one MR should be able to see the whole stack and where
//! this change sits in it. So for every in-scope MR we (re)generate a
//! navigation block and keep it in one conversation comment of that MR,
//! written by the token's user and edited in place. A comment, never the
//! description: in a squash-merging repository the description becomes the
//! landed commit's message (LLVM's, for one), while a comment is part of no
//! commit under any merge method — so wits needs to know none of them.
//!
//! The comment is found again on every run by the marker its body starts with,
//! among the user's own comments. Its id is cached nowhere: whether to edit it
//! depends on its current text, and reading that costs the same request as
//! finding it. Nothing else on the MR is written, except that a block an older
//! wits put into the description is stripped out once the comment holds it;
//! nothing on the forge is ever deleted.

use std::collections::HashMap;

use anyhow::Context;
use wits_util::forge::{Forge, MergeRequest, MrComment};
use wits_util::git::Repository;
use wits_util::log as wits_log;
use wits_util::project::remotes::Declared;

use super::topology::Topology;
use super::{fail_if_any, find_open_mrs, map_parallel, resolution, store, ForgeSession, ScopeArgs};

/// The first line of every navigation comment, and how one is recognised. It
/// must never change: the comments already posted are found by it, and a new
/// marker would leave each of them behind as a stale duplicate. It also opened
/// the block older versions wrote into descriptions, which is how that block
/// is found to strip it.
const HEADER: &str = "<!-- wits stack: generated navigation, do not edit below -->";
const FOOTER: &str = "<!-- wits stack: end navigation -->";

pub fn run(repo: &Repository, declared: &Declared, scope: &ScopeArgs) -> anyhow::Result<()> {
    let plan = resolution::plan_scoped(repo, declared, scope)?;

    if plan.standalone {
        log::info!("standalone branch: a lone MR has nothing to navigate, skipping");
        return Ok(());
    }
    if plan.selected.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo, &declared.roles)?;
    let noun = session.noun;

    // Discover the open MR for each branch up front; rendering is local.
    let (found, mut failures) = find_open_mrs(&session, &plan.selected, |branch| {
        Some(plan.base_for(branch))
    });
    let mrs: HashMap<String, MergeRequest> = found.into_iter().collect();
    if mrs.is_empty() {
        log::info!("no open {noun}s to annotate");
        return fail_if_any(failures);
    }

    // Cache discovered numbers in each branch's config, so later runs and
    // `wits stack` can show them without another round-trip. The plan was built
    // before the network round-trip, so the update applies to a fresh load:
    // whatever else changed in the meantime is carried through, not overwritten
    // with a stale snapshot.
    let stored = store::load(repo, &plan.base_branch);
    let mut topology = stored.topology.clone();
    let mut annotations_changed = false;
    for (branch, mr) in &mrs {
        let annotation = format!("{noun} {}", mr.display);
        if topology.contains(branch) && topology.annotation(branch) != Some(annotation.as_str()) {
            topology.set_annotation(branch, annotation);
            annotations_changed = true;
        }
    }
    if annotations_changed {
        store::save(repo, &stored, &topology)?;
    }

    let jobs: Vec<(&String, &MergeRequest, String)> = plan
        .selected
        .iter()
        .filter_map(|branch| {
            let mr = mrs.get(branch)?;
            let block = render_navigation(&plan.topology, branch, &mrs, noun, &plan.base_branch)?;
            Some((branch, mr, block))
        })
        .collect();

    let results = map_parallel(&jobs, |(branch, mr, block)| {
        annotate(session.forge.as_ref(), noun, branch, mr, block)
    });
    let mut changed = 0usize;
    for ((branch, mr, _), result) in jobs.iter().zip(results) {
        match result {
            Ok(done) => {
                changed += usize::from(done.changed());
                report(noun, branch, mr, &done);
            }
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e:#}");
            }
        }
    }
    if changed == 0 && failures == 0 {
        log::info!("navigation already up to date");
    }
    fail_if_any(failures)
}

/// What to do about an MR's navigation comment.
#[derive(Debug, PartialEq, Eq)]
enum NavAction {
    Create,
    Edit { id: String },
    Keep,
}

/// The decision for one MR, borrowing from the comments it was made from.
struct NavPlan<'a> {
    action: NavAction,
    /// The user's further navigation comments, beyond the one kept current.
    extra: Vec<&'a MrComment>,
    /// Navigation comments another account wrote.
    foreign: Vec<&'a MrComment>,
}

/// Decide what an MR's navigation needs, from its comments (oldest first) and
/// the block it should show. Only the user's own comments are candidates:
/// wits never edits what someone else wrote. Of several, the oldest is kept
/// current, since it sits highest in the conversation; the rest are only
/// reported, because wits deletes nothing on the forge.
fn decide<'a>(comments: &'a [MrComment], block: &str) -> NavPlan<'a> {
    let (mut own, foreign): (Vec<&MrComment>, Vec<&MrComment>) = comments
        .iter()
        .filter(|comment| is_navigation(&comment.body))
        .partition(|comment| comment.own);
    let action = match own.first() {
        None => NavAction::Create,
        Some(kept) if same_text(&kept.body, block) => NavAction::Keep,
        Some(kept) => NavAction::Edit {
            id: kept.id.clone(),
        },
    };
    let extra = if own.is_empty() {
        Vec::new()
    } else {
        own.split_off(1)
    };
    NavPlan {
        action,
        extra,
        foreign,
    }
}

/// Whether a comment is a navigation comment: one whose body starts with the
/// marker. Merely containing it is not enough — a reply quoting the comment
/// contains it too.
fn is_navigation(body: &str) -> bool {
    body.trim_start().starts_with(HEADER)
}

/// Whether two bodies say the same, ignoring the line endings and outer
/// whitespace a forge may rewrite when it stores a body.
fn same_text(stored: &str, wanted: &str) -> bool {
    let normalize = |text: &str| text.replace("\r\n", "\n").trim().to_owned();
    normalize(stored) == normalize(wanted)
}

/// What [`annotate`] did, or under `--dry-run` would do, to one MR.
#[derive(Debug)]
struct Annotated {
    action: NavAction,
    /// The old block was stripped out of the description.
    stripped: bool,
    /// Links to the user's further navigation comments, left in place.
    extra: Vec<String>,
    /// Links to navigation comments another account wrote, left alone.
    foreign: Vec<String>,
}

impl Annotated {
    fn changed(&self) -> bool {
        self.action != NavAction::Keep || self.stripped
    }
}

/// Bring one MR's navigation comment to `block`, then strip the block an older
/// wits left in its description.
fn annotate(
    forge: &dyn Forge,
    noun: &str,
    branch: &str,
    mr: &MergeRequest,
    block: &str,
) -> anyhow::Result<Annotated> {
    let display = &mr.display;
    // A failed listing stops this MR: taking it for "no comment yet" is how a
    // second navigation comment would get posted. A failed create is not
    // retried either, since it may have landed after all; the next run lists
    // the comments and finds it.
    let comments = forge
        .list_comments(&mr.id)
        .with_context(|| format!("listing the comments of {noun} {display}"))?;
    let plan = decide(&comments, block);
    match &plan.action {
        NavAction::Create if wits_log::is_dry_run() => wits_log::dry_run(&format!(
            "create {noun} {display} navigation comment ({branch})"
        )),
        NavAction::Create => forge
            .add_comment(&mr.id, block)
            .with_context(|| format!("posting the navigation comment on {noun} {display}"))?,
        NavAction::Edit { .. } if wits_log::is_dry_run() => wits_log::dry_run(&format!(
            "update {noun} {display} navigation comment ({branch})"
        )),
        NavAction::Edit { id } => forge
            .edit_comment(&mr.id, id, block)
            .with_context(|| format!("updating the navigation comment on {noun} {display}"))?,
        NavAction::Keep => {}
    }
    // Only now that the comment holds the navigation may the description give
    // up its copy, so a failure part-way never leaves the MR without one.
    let stripped = mr.body.contains(HEADER);
    if stripped {
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!(
                "strip navigation from {noun} {display} description ({branch})"
            ));
        } else {
            // The comment already holds the navigation, so say so: the step
            // that failed is only the clean-up the next run retries.
            forge
                .set_body(&mr.id, &strip_generated(&mr.body))
                .with_context(|| {
                    format!(
                        "the navigation comment is in place, but stripping the old block from \
                         the description of {noun} {display} failed"
                    )
                })?;
        }
    }
    Ok(Annotated {
        action: plan.action,
        stripped,
        extra: plan.extra.iter().map(|c| c.url.clone()).collect(),
        foreign: plan.foreign.iter().map(|c| c.url.clone()).collect(),
    })
}

/// Log what [`annotate`] did to one MR, and warn about the navigation comments
/// it leaves alone.
fn report(noun: &str, branch: &str, mr: &MergeRequest, done: &Annotated) {
    let display = &mr.display;
    if !wits_log::is_dry_run() {
        match done.action {
            NavAction::Create => {
                log::info!("created the navigation comment on {noun} {display} ({branch})")
            }
            NavAction::Edit { .. } => {
                log::info!("updated the navigation comment on {noun} {display} ({branch})")
            }
            NavAction::Keep => {}
        }
        if done.stripped {
            log::info!(
                "stripped the old navigation out of the description of {noun} {display} \
                 ({branch})"
            );
        }
    }
    if !done.extra.is_empty() {
        log::warn!(
            "{branch}: {noun} {display} has further navigation comments of yours ({}); only the \
             oldest is kept current, so delete the others",
            done.extra.join(", ")
        );
    }
    if !done.foreign.is_empty() {
        log::warn!(
            "{branch}: {noun} {display} also carries navigation another account wrote ({}); wits \
             leaves it alone",
            done.foreign.join(", ")
        );
    }
}

/// Render the full navigation block for one branch's MR: one "Stack List"
/// section per downstream chain, all inside a single marker pair.
///
/// No line may start with `/`: GitLab runs a note's quick actions on every edit
/// as well as on creation (`Notes::UpdateService`), so such a line would act as
/// a command instead of being shown.
fn render_navigation(
    topology: &Topology,
    branch: &str,
    mrs: &HashMap<String, MergeRequest>,
    noun: &str,
    base_branch: &str,
) -> Option<String> {
    let sections: Vec<String> = topology
        .anno_blocks(branch)
        .into_iter()
        .filter_map(|block| render_section(topology, &block, mrs, branch, noun, base_branch))
        .collect();
    if sections.is_empty() {
        return None;
    }
    Some(format!("{HEADER}\n\n{}\n\n{FOOTER}", sections.join("\n\n")))
}

/// One "Stack List" section. Only nodes that actually have an MR get a numbered
/// entry; an MR-less ancestor (the base branch) still shows up as the parent in
/// a flow line, so the lineage reads correctly without inventing entries for it.
fn render_section(
    topology: &Topology,
    block: &[String],
    mrs: &HashMap<String, MergeRequest>,
    current: &str,
    noun: &str,
    base_branch: &str,
) -> Option<String> {
    let items: Vec<&String> = block.iter().filter(|n| mrs.contains_key(*n)).collect();
    if items.is_empty() {
        return None;
    }

    let total = items.len();
    let mut lines = vec!["### Stack List".to_owned(), String::new()];
    for (i, name) in items.iter().enumerate() {
        let mr = &mrs[*name];
        let marker = if *name == current {
            "  ⬅️ **current**"
        } else {
            ""
        };
        // A root branch has no parent in the tree; its MR still targets the base
        // branch, so show that rather than a bare placeholder.
        let parent = topology.parent(name).unwrap_or(base_branch);
        lines.push(format!(
            "  * [{}/{}] {} {}{}",
            i + 1,
            total,
            noun,
            mr.display,
            marker
        ));
        lines.push(format!("    `{parent}` ← `{name}`"));
    }
    Some(lines.join("\n"))
}

/// `body` without the navigation block older versions kept in descriptions,
/// the rest of the text untouched.
fn strip_generated(body: &str) -> String {
    let Some(start) = body.find(HEADER) else {
        return body.trim().to_owned();
    };
    // The generated block runs from the HEADER to the end of its FOOTER. Search
    // for the FOOTER *after* the header so a footer accidentally left earlier in
    // the prose can't be mistaken for ours. If the footer is intact, drop exactly
    // that span; if it was torn off by a hand-edit, the block is unterminated —
    // and since it was always appended at the tail, everything from the header
    // onward is ours to drop.
    let after_header = &body[start + HEADER.len()..];
    let tail = match after_header.find(FOOTER) {
        Some(rel) => &after_header[rel + FOOTER.len()..],
        None => "",
    };
    let mut kept = String::with_capacity(start + tail.len());
    kept.push_str(&body[..start]);
    kept.push_str(tail);
    kept.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use wits_util::forge::{Attributes, HeadRepo, MrState, NewMr};

    fn mr(display: &str) -> MergeRequest {
        MergeRequest {
            id: "1".into(),
            display: display.into(),
            state: MrState::Open,
            base: "x".into(),
            source: String::new(),
            head_sha: None,
            body: String::new(),
            web_url: String::new(),
        }
    }

    fn comment(id: &str, body: &str, own: bool) -> MrComment {
        MrComment {
            id: id.into(),
            body: body.into(),
            url: format!("u/{id}"),
            own,
        }
    }

    fn block(nav: &str) -> String {
        format!("{HEADER}\n\n{nav}\n\n{FOOTER}")
    }

    #[test]
    fn section_numbers_only_mr_bearing_nodes() {
        let topo = Topology::parse("main\n    a\n        b\n");
        let mut mrs = HashMap::new();
        mrs.insert("a".to_owned(), mr("#1"));
        mrs.insert("b".to_owned(), mr("#2"));
        // The block [main, a, b] has no MR for main, so it is shown only as a's
        // parent and the numbering starts at a.
        let section = render_section(
            &topo,
            &["main".into(), "a".into(), "b".into()],
            &mrs,
            "b",
            "PR",
            "main",
        )
        .unwrap();
        assert!(section.contains("[1/2] PR #1"));
        assert!(section.contains("`main` ← `a`"));
        assert!(section.contains("[2/2] PR #2  ⬅️ **current**"));
    }

    // A fork yields one section per downstream chain, all under one wrapper.
    #[test]
    fn fork_node_gets_one_section_per_downstream() {
        let topo = Topology::parse("main\n    A\n        B\n            C\n            D\n");
        let mut mrs = HashMap::new();
        for b in ["A", "B", "C", "D"] {
            mrs.insert(b.to_owned(), mr("#1"));
        }
        let nav = render_navigation(&topo, "B", &mrs, "PR", "main").unwrap();
        assert_eq!(nav.matches("### Stack List").count(), 2);
        assert_eq!(nav.matches(HEADER).count(), 1);
    }

    // A merged/closed middle node (no open MR) drops out of the numbering but
    // still shows as the parent in its child's flow line.
    #[test]
    fn a_node_without_an_mr_is_skipped_in_numbering() {
        let topo = Topology::parse("main\n    A\n        B\n            C\n");
        let mut mrs = HashMap::new();
        mrs.insert("A".to_owned(), mr("#1"));
        mrs.insert("C".to_owned(), mr("#3"));
        let section = render_section(
            &topo,
            &["main".into(), "A".into(), "B".into(), "C".into()],
            &mrs,
            "C",
            "PR",
            "main",
        )
        .unwrap();
        assert!(section.contains("[1/2] PR #1"));
        assert!(section.contains("[2/2] PR #3"));
        assert!(section.contains("`B` ← `C`"));
    }

    #[test]
    fn stripping_keeps_the_prose_around_the_old_block() {
        let body = format!("Intro text.\n\n{}\n\nA later note.", block("stale"));
        let stripped = strip_generated(&body);
        assert!(stripped.starts_with("Intro text."));
        assert!(stripped.ends_with("A later note."));
        assert!(!stripped.contains(HEADER) && !stripped.contains("stale"));
    }

    // A torn marker pair (footer hand-deleted) must not defeat stripping: the
    // generated block always sat at the tail, so everything from a lone HEADER
    // onward is ours to drop.
    #[test]
    fn stripping_recovers_from_a_torn_footer() {
        let torn = format!("Prose.\n\n{HEADER}\n\nstale nav with no footer");
        assert_eq!(strip_generated(&torn), "Prose.");
    }

    #[test]
    fn with_no_navigation_comment_one_is_created() {
        // Comments that are not navigation do not count, whoever wrote them.
        let comments = [comment("1", "LGTM", false), comment("2", "thanks", true)];
        assert_eq!(decide(&comments, &block("nav")).action, NavAction::Create);
    }

    #[test]
    fn an_identical_comment_is_left_alone() {
        // As a forge may hand it back: CRLF line endings and a trailing newline.
        let stored = block("nav").replace('\n', "\r\n") + "\r\n";
        let comments = [comment("1", &stored, true)];
        assert_eq!(decide(&comments, &block("nav")).action, NavAction::Keep);
    }

    #[test]
    fn the_oldest_navigation_comment_is_edited_and_the_rest_reported() {
        let comments = [
            comment("1", "hello", false),
            comment("2", &block("old"), true),
            comment("3", &block("a second copy"), true),
        ];
        let plan = decide(&comments, &block("new"));
        assert_eq!(plan.action, NavAction::Edit { id: "2".into() });
        let extra: Vec<&str> = plan.extra.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(extra, ["3"]);
        assert!(plan.foreign.is_empty());
    }

    #[test]
    fn a_quoted_marker_is_not_a_navigation_comment() {
        let quoted = format!("> {}\n\nWhy is this here?", block("nav"));
        let comments = [comment("1", &quoted, true)];
        assert_eq!(decide(&comments, &block("nav")).action, NavAction::Create);
    }

    #[test]
    fn another_accounts_navigation_is_reported_but_never_taken_over() {
        let comments = [comment("1", &block("nav"), false)];
        let plan = decide(&comments, &block("nav"));
        assert_eq!(plan.action, NavAction::Create);
        assert_eq!(plan.foreign.len(), 1);
    }

    /// A forge that serves canned comments and records every write it is asked
    /// for — successful or not — so the order of the writes, and the ones that
    /// must not happen, can be checked. The tests using it rely on the
    /// process-wide dry-run flag being off, as no test in this binary sets it.
    struct FakeForge {
        comments: Result<Vec<MrComment>, String>,
        /// The writes, by name, that fail.
        refuse: &'static [&'static str],
        writes: Mutex<Vec<String>>,
    }

    impl FakeForge {
        fn new(comments: Result<Vec<MrComment>, String>) -> Self {
            Self {
                comments,
                refuse: &[],
                writes: Mutex::new(Vec::new()),
            }
        }

        fn write(&self, name: &str, details: &str) -> anyhow::Result<()> {
            self.writes
                .lock()
                .unwrap()
                .push(format!("{name} {details}"));
            if self.refuse.contains(&name) {
                anyhow::bail!("refused");
            }
            Ok(())
        }

        fn writes(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl Forge for FakeForge {
        fn noun(&self) -> &'static str {
            "PR"
        }
        fn mrs_for_branch(
            &self,
            _head: HeadRepo,
            _branch: &str,
        ) -> anyhow::Result<Vec<MergeRequest>> {
            unreachable!("annotate looks up no MR")
        }
        fn create(&self, _req: &NewMr) -> anyhow::Result<MergeRequest> {
            unreachable!("annotate opens no MR")
        }
        fn set_base(&self, _id: &str, _base: &str) -> anyhow::Result<()> {
            unreachable!("annotate moves no base")
        }
        fn set_body(&self, _id: &str, body: &str) -> anyhow::Result<()> {
            self.write("set_body", body)
        }
        fn apply_attributes(&self, _id: &str, _attrs: &Attributes) -> anyhow::Result<()> {
            unreachable!("annotate sets no attributes")
        }
        fn list_comments(&self, _mr: &str) -> anyhow::Result<Vec<MrComment>> {
            self.comments.clone().map_err(anyhow::Error::msg)
        }
        fn add_comment(&self, _mr: &str, body: &str) -> anyhow::Result<()> {
            self.write("add_comment", body)
        }
        fn edit_comment(&self, _mr: &str, comment: &str, body: &str) -> anyhow::Result<()> {
            self.write("edit_comment", &format!("{comment} {body}"))
        }
    }

    fn with_body(body: &str) -> MergeRequest {
        MergeRequest {
            body: body.into(),
            ..mr("#1")
        }
    }

    #[test]
    fn an_old_description_block_moves_into_a_comment_first() {
        let forge = FakeForge::new(Ok(Vec::new()));
        let mr = with_body(&format!("Prose.\n\n{}", block("old")));
        let done = annotate(&forge, "PR", "b", &mr, &block("new")).unwrap();
        assert!(done.changed() && done.stripped);
        assert_eq!(
            forge.writes(),
            [
                format!("add_comment {}", block("new")),
                "set_body Prose.".to_owned()
            ]
        );
    }

    #[test]
    fn a_failed_listing_writes_nothing() {
        let forge = FakeForge::new(Err("timed out".into()));
        let mr = with_body(&block("old"));
        assert!(annotate(&forge, "PR", "b", &mr, &block("new")).is_err());
        assert!(forge.writes().is_empty());
    }

    #[test]
    fn a_failed_comment_leaves_the_description_alone() {
        let mut forge = FakeForge::new(Ok(Vec::new()));
        forge.refuse = &["add_comment"];
        let mr = with_body(&format!("Prose.\n\n{}", block("old")));
        let err = annotate(&forge, "PR", "b", &mr, &block("new")).unwrap_err();
        assert!(format!("{err:#}").contains("posting the navigation comment"));
        // The comment was attempted and nothing after it.
        assert_eq!(forge.writes(), [format!("add_comment {}", block("new"))]);
    }

    #[test]
    fn a_failed_strip_says_the_comment_is_in_place() {
        let mut forge = FakeForge::new(Ok(Vec::new()));
        forge.refuse = &["set_body"];
        let mr = with_body(&format!("Prose.\n\n{}", block("old")));
        let err = annotate(&forge, "PR", "b", &mr, &block("new")).unwrap_err();
        assert!(format!("{err:#}").contains("the navigation comment is in place"));
        assert_eq!(forge.writes().len(), 2);
    }

    #[test]
    fn an_up_to_date_mr_is_not_written() {
        let forge = FakeForge::new(Ok(vec![comment("7", &block("nav"), true)]));
        let done = annotate(&forge, "PR", "b", &with_body("Prose."), &block("nav")).unwrap();
        assert!(!done.changed());
        assert!(forge.writes().is_empty());
    }

    #[test]
    fn a_changed_navigation_edits_the_kept_comment() {
        let forge = FakeForge::new(Ok(vec![comment("7", &block("old"), true)]));
        annotate(&forge, "PR", "b", &with_body(""), &block("new")).unwrap();
        assert_eq!(forge.writes(), [format!("edit_comment 7 {}", block("new"))]);
    }
}
