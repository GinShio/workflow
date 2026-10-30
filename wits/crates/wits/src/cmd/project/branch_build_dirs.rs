//! `project branch-build-dirs` — every existing build directory one checkout's
//! branch identifies, across all projects.

use anyhow::{Context, Result};

use wits_project::{layout, Workspace};

use super::common::{branch_or_current, resolve_repo, TargetArgs};
use crate::cmd::identity::ProfileArgs;

/// `branch-build-dirs`: every existing build directory that `--branch` of the
/// anchored checkout identifies, one `<project>\t<path>` per line.
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
/// It resolves paths only ([`wits_project::resolve_paths`]): org, project, and
/// preset configuration never reach this query, so a broken preset elsewhere in
/// the registry no longer fails a branch cleanup. The resolved `build_dir` is
/// unchanged — paths are resolved before any layer is applied.
pub(super) fn run(ws: &Workspace, args: &TargetArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    // Ask by the branch's *checkout*, not the repository. Two repos of a project
    // may share one git dir and be told apart only by which checkout they place
    // (a bare-backed component and a review checkout of it), so collapsing to the
    // repository path would discard the one thing that distinguishes them.
    let path = layout::work_dir(ws, project, &repo, &branch)
        .with_context(|| format!("cannot resolve the checkout of repo '{repo}'"))?;
    for found in wits_project::branch_build_dirs(ws, &path, &branch) {
        println!("{}\t{}", found.project, found.path.display());
    }
    Ok(())
}
