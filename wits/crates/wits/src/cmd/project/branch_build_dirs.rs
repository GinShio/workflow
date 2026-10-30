//! `project branch-build-dirs` — every existing build directory one checkout's
//! branch identifies, across all projects.

use anyhow::{Context, Result};
use clap::Args;

use wits_project::{layout, resolve_target, Workspace};

use super::common::{query_branch, CheckoutArgs};

#[derive(Debug, Args)]
pub struct BranchBuildDirsArgs {
    /// Project name, or a path inside a checkout (default: the current dir).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    #[command(flatten)]
    pub at: CheckoutArgs,
}

/// `branch-build-dirs`: every existing build directory that `--branch` of the
/// target's checkout identifies, one `<project>\t<path>` per line.
///
/// The cross-project shape is the point. A branch deletion has to reach the build
/// trees of every project that keys its build on that checkout's branch, and a
/// borrowed component is keyed on by more than one — so a per-project query
/// silently leaves the borrowers' trees behind. The judgement about *which*
/// projects qualify is [`wits_project::branch_build_dirs`]'s, derived from the
/// registry rather than declared in it.
///
/// This stays a query: it prints what a deletion would orphan and removes
/// nothing, because `project` is the read-only half of the tool ("The read/act
/// split") and the caller that deletes wants its own confirmation and dry-run
/// anyway.
///
/// It resolves paths only ([`wits_project::resolve_paths`]), and a `build_dir`
/// cannot read a preset, an org, or the project's own build tables — paths are
/// resolved before any of those apply. So a project whose presets do not
/// resolve still has its branch's trees found: their location is known exactly
/// even where its build configuration is broken.
pub(super) fn run(ws: &Workspace, args: &BranchBuildDirsArgs) -> Result<()> {
    let target = resolve_target(ws, args.target.as_deref())?;
    let repo = target.checkout_focus(args.at.focus.as_deref())?;
    let branch = query_branch(ws, &target, &repo, args.at.branch.as_deref())?;
    // Ask by the branch's *checkout*, not the repository. Two repos of a project
    // may share one git dir and be told apart only by which checkout they place
    // (a bare-backed component and a review checkout of it), so collapsing to the
    // repository path would discard the one thing that distinguishes them.
    let path = layout::work_dir(ws, target.project, &repo, &branch)
        .with_context(|| format!("cannot resolve the checkout of repo '{repo}'"))?;
    for found in wits_project::branch_build_dirs(ws, &path, &branch) {
        println!("{}\t{}", found.project, found.path.display());
    }
    Ok(())
}
