//! CLI plumbing the `project` verbs share: the `--branch`/`--focus` pair a
//! query about one checkout takes, the branch a query defaults to, and the plan
//! `info` describes and `--get` projects from. Which project and which repo a
//! positional names is `wits-project`'s [`Target`].

use anyhow::Result;
use clap::Args;

use wits_project::{
    resolve_plan, target_branch, BranchAbsence, BuildInput, BuildPlan, PathInput, Target, Workspace,
};

use crate::cmd::identity::ProfileArgs;

/// The two axes a query about one checkout takes — `hash`, `branch-build-dirs`.
/// The rest of the profile shapes a build, which these queries do not resolve.
#[derive(Debug, Args, Default)]
pub struct CheckoutArgs {
    /// Branch to answer for. Default: the one checked out where the target
    /// points.
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    /// The repo to ask about. Default: the repo whose checkout holds the path,
    /// or the project's focus for a name.
    #[arg(long)]
    pub focus: Option<String>,
}

/// The branch a query resolves for `repo`: `--branch`, else the one checked
/// out where the target points — through the [`target_branch`] funnel `build`
/// uses, so a query and the build it describes agree.
pub(super) fn query_branch(
    ws: &Workspace,
    target: &Target,
    repo: &str,
    explicit: Option<&str>,
) -> Result<String> {
    Ok(target_branch(
        ws,
        target.project,
        repo,
        explicit,
        false,
        BranchAbsence::Query,
        &target.here,
    )?
    .expect("a non-detached target_branch is an error or a branch"))
}

/// The plan `info` describes and `--get` projects from: the build `wits build`
/// makes from the same target and flags, for `focus` and `branch`, without
/// toolchain injection or extra arguments. One helper serves both, so a
/// description and a projection out of it cannot disagree.
pub(super) fn query_plan(
    ws: &Workspace,
    target: &Target,
    focus: &str,
    branch: &str,
    profile: &ProfileArgs,
) -> Result<BuildPlan> {
    let resolved = profile.to_profile(focus);
    resolve_plan(
        ws,
        target.project,
        &BuildInput {
            paths: PathInput {
                profile: &resolved,
                branch: Some(branch),
                work_dir: profile.work_dir.as_deref(),
                build_dir: None,
                install_dir: None,
                here: Some(&target.here),
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        },
    )
}
