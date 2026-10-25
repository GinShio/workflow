//! CLI-only resolution shared by the `project` verbs: which repo a target names,
//! which branch its queries default to, and the plan assembly `info`/`--get`
//! share. The registry and its resolution are `wits-project`'s; nothing here is.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

use wits_project::{
    expand_tilde, looks_like_path, resolve_plan, target_branch, BranchAbsence, BuildInput,
    BuildPlan, PathInput, Profile, Project, Workspace,
};

use crate::cmd::identity::ProfileArgs;

/// A target anchored by name or path (default: the current dir). The branch and
/// the rest of the resolution profile arrive via the global [`ProfileArgs`] on
/// the parent, so every query shares one shape.
#[derive(Debug, Args)]
pub struct TargetArgs {
    /// Project name, or a path inside a checkout (default: the current dir).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
}

/// Resolve a target to `(project, anchor-repo)`: a path (or the current dir)
/// resolves to the *containing* repo, a name to the project's focus repo.
pub(super) fn resolve_repo<'a>(
    ws: &'a Workspace,
    target: Option<&str>,
    focus: Option<&str>,
) -> Result<(&'a Project, String)> {
    match target {
        None => {
            let cwd = std::env::current_dir()?;
            ws.repo_for_path(&cwd)
                .map(|(project, repo)| (project, repo.name().to_owned()))
                .context(
                    "not inside any known project; pass a name or run from inside a project's checkout",
                )
        }
        Some(t) if looks_like_path(t) => {
            let path = expand_tilde(t);
            ws.repo_for_path(&path)
                .map(|(project, repo)| (project, repo.name().to_owned()))
                .with_context(|| format!("no project owns the path {}", path.display()))
        }
        // A name resolves to the project's focus repo; `--focus` overrides which
        // repo that is (a path already names the repo, so the override is moot).
        Some(name) => {
            let project = ws.project(name)?;
            Ok((project, project.focus_name(focus).to_owned()))
        }
    }
}

/// Resolve a branch's build [`BuildPlan`] for a query, anchored like
/// [`resolve_repo`] with the branch defaulting to the anchored repo's current
/// one. Shared by `info` and `--get`, so a description and a projection out of
/// it can never disagree.
pub(super) fn plan_for_query<'a>(
    ws: &'a Workspace,
    target: Option<&str>,
    profile: &ProfileArgs,
) -> Result<(&'a Project, BuildPlan)> {
    let (project, repo) = resolve_repo(ws, target, profile.focus.as_deref())?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    // Carry the *whole* profile (build_type / toolchain / generator / presets),
    // not just focus+branch: a `build_dir`/`install_dir` template may embed any
    // of them ("Context variables"), so dropping them would print a dir that no
    // build ever uses.
    let mut resolved = profile.to_profile();
    resolved.focus = Some(repo);
    let plan = resolve_plan(
        ws,
        project,
        &paths_input(&resolved, Some(&branch), profile.work_dir.as_deref()),
    )?;
    Ok((project, plan))
}

/// A path-only plan input for the read-only queries: no injection, no L3 args.
///
/// `--work-dir` is an axis of [`PathInput`], not [`Profile`], so the CLI has to
/// pass it here explicitly; a helper that dropped it would silently make every
/// read query ignore the override.
pub(super) fn paths_input<'a>(
    profile: &'a Profile,
    branch: Option<&'a str>,
    work_dir: Option<&'a Path>,
) -> BuildInput<'a> {
    BuildInput {
        paths: PathInput {
            profile,
            branch,
            work_dir,
            build_dir: None,
            install_dir: None,
        },
        inject_toolchain: false,
        injector: None,
        extra_config_args: &[],
        extra_build_args: &[],
        extra_install_args: &[],
    }
}

/// The branch to resolve for: the explicit `--branch`, else the identity repo's
/// current branch. Shared by the path queries and `hash` so they default the same
/// way `build` does ("Branch identity") — through the one
/// [`target_branch`](wits_project::target_branch) funnel, so a query and the
/// build it describes can never disagree about which branch is meant.
pub(super) fn branch_or_current(
    ws: &Workspace,
    project: &Project,
    repo: &str,
    explicit: Option<&str>,
) -> Result<String> {
    Ok(
        target_branch(ws, project, repo, explicit, false, BranchAbsence::Query)?
            .expect("a non-detached target_branch is an error or a branch"),
    )
}

/// Best-effort canonical path for keying/looking up overrides: both the map keys
/// and the walk's on-disk paths pass through this, so `..`/symlink spellings
/// still compare equal. Falls back to the path as-is when it can't be resolved.
pub(super) fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
