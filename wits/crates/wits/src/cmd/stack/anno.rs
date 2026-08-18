//! `wits stack anno` — keep each MR's description pointing at its neighbours.
//!
//! A reviewer landing on one MR should be able to see the whole stack and where
//! this change sits in it. So for every in-scope MR we (re)generate a navigation
//! block and splice it into the description, leaving the human-written part
//! alone. The block is delimited by a fixed marker pair, and there is exactly
//! one pair per description — that single-pair rule is what makes replacing the
//! old block reliable instead of guesswork.

use std::collections::HashMap;

use wits_util::forge::MergeRequest;
use wits_util::git::Repository;
use wits_util::log as wits_log;

use super::topology::Topology;
use super::{fail_if_any, find_open_mrs, map_parallel, resolution, ForgeSession, ScopeArgs};

const HEADER: &str = "<!-- wits stack: generated navigation, do not edit below -->";
const FOOTER: &str = "<!-- wits stack: end navigation -->";

pub fn run(repo: &Repository, scope: &ScopeArgs) -> anyhow::Result<()> {
    let mut plan = resolution::plan_scoped(repo, scope)?;

    if plan.standalone {
        log::info!("standalone branch: a lone MR has nothing to navigate, skipping");
        return Ok(());
    }
    if plan.selected.is_empty() {
        log::info!("no branches in scope");
        return Ok(());
    }

    let session = ForgeSession::open(repo)?;
    let noun = session.noun;

    // Discover the open MR for each branch up front; everything else is local.
    let (found, mut failures) = find_open_mrs(&session, &plan.selected);
    let mrs: HashMap<String, MergeRequest> = found.into_iter().collect();
    if mrs.is_empty() {
        log::info!("no open {noun}s to annotate");
        return fail_if_any(failures);
    }

    // Cache discovered numbers into the machete annotations so later runs (and a
    // human reading the file) can see them without another round-trip.
    let mut annotations_changed = false;
    for (branch, mr) in &mrs {
        let annotation = format!("{noun} {}", mr.display);
        if plan.topology.annotation(branch) != Some(annotation.as_str()) {
            plan.topology.set_annotation(branch, annotation);
            annotations_changed = true;
        }
    }
    if annotations_changed {
        resolution::save_topology(repo, &plan.topology)?;
    }

    // Build the new description for each MR whose body actually changes.
    let mut updates: Vec<(String, String, String, String)> = Vec::new();
    for branch in &plan.selected {
        let Some(mr) = mrs.get(branch) else { continue };
        let Some(block) = render_navigation(&plan.topology, branch, &mrs, noun, &plan.base_branch)
        else {
            continue;
        };
        let new_body = splice(&mr.body, &block);
        if new_body.trim() != mr.body.trim() {
            updates.push((branch.clone(), mr.id.clone(), new_body, mr.display.clone()));
        }
    }

    if updates.is_empty() {
        log::info!("descriptions already up to date");
        return fail_if_any(failures);
    }

    let results = map_parallel(&updates, |item| {
        let (branch, id, body, display) = (&item.0, &item.1, &item.2, &item.3);
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!("update {noun} {display} description ({branch})"));
            return Ok(());
        }
        session.forge.set_body(id, body)
    });
    for (item, result) in updates.iter().zip(results) {
        match result {
            Ok(()) => log::info!("updated {noun} {} ({})", item.3, item.0),
            Err(e) => {
                failures += 1;
                log::warn!("{}: {e}", item.0);
            }
        }
    }

    fail_if_any(failures)
}

/// Render the full navigation block for one branch's MR: one "Stack List"
/// section per downstream chain, all inside a single marker pair.
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

/// Replace any existing generated block in `body` with `block`, preserving the
/// human-written remainder. Relies on the single-pair invariant: there is at
/// most one HEADER…FOOTER region.
fn splice(body: &str, block: &str) -> String {
    let stripped = strip_generated(body);
    if stripped.is_empty() {
        block.to_owned()
    } else {
        format!("{stripped}\n\n{block}")
    }
}

fn strip_generated(body: &str) -> String {
    let Some(start) = body.find(HEADER) else {
        return body.trim().to_owned();
    };
    // The generated block runs from the HEADER to the end of its FOOTER. Search
    // for the FOOTER *after* the header so a footer accidentally left earlier in
    // the prose can't be mistaken for ours. If the footer is intact, drop exactly
    // that span; if it was torn off by a hand-edit, the block is unterminated —
    // and since `anno` always appends the block at the tail, everything from the
    // header onward is ours to drop. Either way exactly one marker pair survives.
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
    use super::*;
    use wits_util::forge::MrState;

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

    #[test]
    fn splice_replaces_old_block_and_keeps_prose() {
        let block = format!("{HEADER}\n\nnew\n\n{FOOTER}");
        let old = format!("Intro text.\n\n{HEADER}\n\nstale\n\n{FOOTER}");
        let result = splice(&old, &block);
        assert!(result.starts_with("Intro text."));
        assert!(result.contains("new"));
        assert!(!result.contains("stale"));
        // Still exactly one marker pair.
        assert_eq!(result.matches(HEADER).count(), 1);
    }

    #[test]
    fn splice_into_empty_body_is_just_the_block() {
        let block = format!("{HEADER}\n\nnav\n\n{FOOTER}");
        assert_eq!(splice("", &block), block);
    }

    // Multi-round anno: re-splicing identical content must be a no-op, which is
    // what lets a second `anno` run decide "already up to date".
    #[test]
    fn splice_is_idempotent() {
        let block = format!("{HEADER}\n\nnav v1\n\n{FOOTER}");
        let once = splice("Hello.", &block);
        let twice = splice(&once, &block);
        assert_eq!(once, twice);
    }

    // A torn marker pair (footer hand-deleted) must not defeat stripping and
    // leave two HEADERs behind — the generated block always sits at the tail, so
    // everything from a lone HEADER onward is ours to drop.
    #[test]
    fn splice_recovers_from_a_torn_footer() {
        let block = format!("{HEADER}\n\nfresh\n\n{FOOTER}");
        // Body whose previous block lost its footer to a manual edit.
        let torn = format!("Prose.\n\n{HEADER}\n\nstale nav with no footer");
        let result = splice(&torn, &block);
        assert!(result.starts_with("Prose."));
        assert!(result.contains("fresh") && !result.contains("stale nav"));
        assert_eq!(result.matches(HEADER).count(), 1);
        assert_eq!(result.matches(FOOTER).count(), 1);
    }

    // Re-anno after the numbers change: the old block is replaced, not stacked.
    #[test]
    fn splice_swaps_a_regenerated_block_in_place() {
        let v1 = format!("{HEADER}\n\nold nav\n\n{FOOTER}");
        let v2 = format!("{HEADER}\n\nnew nav\n\n{FOOTER}");
        let body = splice("Body.", &v1);
        let updated = splice(&body, &v2);
        assert!(updated.contains("new nav") && !updated.contains("old nav"));
        assert_eq!(updated.matches(HEADER).count(), 1);
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
}
