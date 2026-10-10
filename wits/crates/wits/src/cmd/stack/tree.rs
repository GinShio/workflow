//! `wits stack tree` — editing the stack's structure directly.
//!
//! These are the manual overrides to the forest that `slice` normally builds:
//! drop branches, move a line onto a new base, or rewrite the whole forest as
//! text. They are separated from the workflow verbs (push/submit/anno) because
//! they change *what the stack is* rather than *acting on it*.
//!
//! The one rule running through all of them: removing a branch never throws away
//! the work stacked above it. `Topology::remove` splices a node's children up
//! into its place, so a mid-stack deletion leaves the downstream line intact (and
//! `submit` then retargets its base). The base branch is protected from removal,
//! and moves are refused if they would form a cycle.
//!
//! There is no prune and no rename: the stack lives in each branch's own config,
//! which git deletes with the branch and moves with a rename (see `store`).

use std::collections::HashSet;
use std::fs;
use std::io::Read as _;

use anyhow::Context;
use wits_util::git::Repository;
use wits_util::project::remotes::Declared;

use super::topology::Topology;
use super::{fail_if_any, resolution, store, EditArgs, MvArgs, RmArgs, TreeAction};

pub fn run(repo: &Repository, declared: &Declared, action: &TreeAction) -> anyhow::Result<()> {
    match action {
        TreeAction::Rm(args) => rm(repo, declared, args),
        TreeAction::Mv(args) => mv(repo, declared, args),
        TreeAction::Edit(args) => edit(repo, declared, args),
    }
}

fn rm(repo: &Repository, declared: &Declared, args: &RmArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, declared)?;
    let stored = store::load(repo, &base);
    let mut topology = stored.topology.clone();
    let mut changed = false;
    let mut deletions = Vec::new();

    for branch in &args.branches {
        if *branch == base {
            log::warn!("refusing to remove the base branch '{branch}'");
            continue;
        }
        if !topology.contains(branch) {
            log::warn!("{branch}: not in the stack");
            continue;
        }
        let parent = topology.parent(branch).unwrap_or(base.as_str()).to_owned();
        if topology.remove(branch) {
            changed = true;
            log::info!("removed {branch} from the stack (children reattached to {parent})");
        }
        if args.delete {
            deletions.push(branch.clone());
        }
    }

    // The stack edit lands first: deleting a branch takes its config — and with
    // it the only record of where its children sat — so they are spliced up
    // before the branch goes.
    if changed {
        store::save(repo, &stored, &topology)?;
    }

    let mut failures = 0usize;
    for branch in &deletions {
        match repo.delete_branch(branch, args.force) {
            Ok(()) => log::info!("deleted branch {branch}"),
            // A requested branch delete that failed is a real failure, not a
            // warning to shrug off — count it so the command exits non-zero,
            // matching the other verbs' contract.
            Err(e) => {
                failures += 1;
                log::warn!("could not delete branch {branch}: {e}");
            }
        }
    }
    fail_if_any(failures)
}

fn mv(repo: &Repository, declared: &Declared, args: &MvArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, declared)?;
    let branch = &args.branch;
    let onto = &args.onto;

    if *branch == base {
        anyhow::bail!("cannot move the base branch '{branch}'");
    }
    if branch == onto {
        anyhow::bail!("a branch cannot be stacked on itself");
    }

    // The branch and its new parent must be real: you can only stack a branch
    // that exists, onto another branch (or the base). This is also what stops a
    // typo from minting a phantom node.
    let tips = repo.branch_tips();
    if !tips.contains_key(branch) {
        anyhow::bail!("branch '{branch}' does not exist locally");
    }
    if *onto != base && !tips.contains_key(onto) {
        anyhow::bail!("parent '{onto}' is neither the base branch nor an existing branch");
    }

    let stored = store::load(repo, &base);
    let mut topology = stored.topology.clone();
    topology.ensure(&base);
    topology.ensure(onto);
    topology.ensure(branch);
    if !topology.reparent(branch, onto) {
        anyhow::bail!(
            "cannot move '{branch}' onto '{onto}': that would place it beneath its own descendant"
        );
    }
    store::save(repo, &stored, &topology)?;

    log::info!("moved {branch} onto {onto} (its substack moved with it)");
    log::info!("note: this updates the stack's shape only — rebase {branch} onto {onto} for the code to match");
    Ok(())
}

/// What the editor buffer opens with, above the stack itself.
const EDIT_HELP: &str = "\
# The stack, one branch per line, each indented under the branch it sits on.
# The text after a name is the MR it was last seen with; delete it to forget
# it. Delete a line to take that branch out of the stack: what sat on it moves
# up to the line above. Lines starting with '#' are ignored.
";

/// Rewrite the whole forest as text — git-machete's format — in git's editor,
/// or from a file (`-` for stdin), which is also how a forest kept elsewhere is
/// brought in. Nothing is written unless the text parses into a forest of
/// existing branches.
fn edit(repo: &Repository, declared: &Declared, args: &EditArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, declared)?;
    let stored = store::load(repo, &base);
    let current = stored.topology.render();

    let text = match args.file.as_deref() {
        None => edit_in_editor(repo, &format!("{EDIT_HELP}{current}"))?,
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("reading the stack from stdin")?;
            text
        }
        Some(path) => fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
    };
    let edited = parse_forest(repo, &base, &text)?;
    if edited.render() == current && stored.rehomed.is_empty() {
        log::info!("the stack is unchanged");
        return Ok(());
    }
    store::save(repo, &stored, &edited)?;
    log::info!("rewrote the stack");
    Ok(())
}

/// Parse an edited forest, refusing what cannot be a stack: a name twice, a
/// name that is neither a local branch nor the base, or the base sitting on
/// another branch.
fn parse_forest(repo: &Repository, base: &str, text: &str) -> anyhow::Result<Topology> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();

    let tips = repo.branch_tips();
    let mut seen = HashSet::new();
    let mut repeated = Vec::new();
    let mut unknown = Vec::new();
    for line in &lines {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        if !seen.insert(name) {
            repeated.push(name);
        }
        if name != base && !tips.contains_key(name) {
            unknown.push(name);
        }
    }
    if !repeated.is_empty() {
        anyhow::bail!(
            "the stack names {} more than once; nothing was changed",
            repeated.join(", ")
        );
    }
    if !unknown.is_empty() {
        anyhow::bail!(
            "{} {} not a local branch; nothing was changed",
            unknown.join(", "),
            if unknown.len() == 1 { "is" } else { "are" }
        );
    }

    let topology = Topology::parse(&lines.join("\n"));
    if let Some(parent) = topology.parent(base) {
        anyhow::bail!("the base branch '{base}' cannot sit on '{parent}'; nothing was changed");
    }
    Ok(topology)
}

/// Open `seed` in git's editor and return what was saved, the way git runs an
/// editor: through the shell, so options in the setting still work. The editor
/// opens under `--dry-run` too — reading what to do is not a change; the save
/// that follows is what prints instead of writing.
fn edit_in_editor(repo: &Repository, seed: &str) -> anyhow::Result<String> {
    let file = tempfile::Builder::new()
        .prefix("wits-stack-")
        .suffix(".machete")
        .tempfile()
        .context("creating the stack edit file")?;
    fs::write(file.path(), seed)?;
    wits_util::editor::edit(repo, file.path()).context("nothing was changed")?;
    fs::read_to_string(file.path()).context("reading back the edited stack")
}
