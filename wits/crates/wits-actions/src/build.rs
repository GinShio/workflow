//! The `wits build` action — resolve a plan, honour the branch strategy, run
//! the backend's steps.
//!
//! Entirely built on `project`'s read-only core
//! (`docs/reference/project-design.rst`, "Library shape — core plus actions";
//! quoted section names below are from the same document): `project` is the
//! component that knows what a project *is*; this module only knows how to
//! *build* one. The CLI parses flags and hands the resolved [`BuildRequest`]
//! here, so an in-process orchestrator can run the same build without a command
//! line.
//!
//! The build systems live in [`wits_build`], beside the read-only core they
//! build on: they are purely a build-time concern, so `project` need not
//! expose them. The one thing the core resolver still needs — translating a
//! toolchain into native env/definitions at L0 ("Single source of truth for
//! compilers, realised by the backend") — it gets through the
//! `ToolchainInjector` seam, which each backend implements and `build` hands
//! to `resolve_plan`.
//!
//! Under worktree/hybrid the target worktree must already exist — hybrid first
//! discovers it from Git's inventory — and `build` never creates one
//! implicitly. Under in-place, building a non-current branch switches the
//! focus's own-git repo behind a [`RestoreGuard`], so the working tree is always
//! returned to where it started, even on failure. An explicit detached build
//! ([`Identity::branch`] is `None`) is the exception to branch-driven planning:
//! it consumes the selected detached checkout as-is and never binds `branch.*`
//! or switches anything.

use std::path::Path;

use anyhow::{bail, Context, Result};

use wits_build::{backend_for, Backend, BuildMode, EmitContext};
use wits_core::git::{Repository, RestoreGuard};
use wits_core::process::Command;
use wits_project::layout;
use wits_project::{
    resolve_plan, BranchStrategy, BuildInput, BuildPlan, PathInput, Project, ToolchainInjector,
    Workspace,
};

use crate::Identity;

/// What a build *does*, not which build it is (that is [`Identity`]). Extra
/// args are verbatim and applied last, at the highest priority (L3 in "The
/// pipeline"). The CLI constructs one from its flags; `resolve_plan` receives
/// only its extra-argument fields, so `wits-project` never learns this type.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub mode: BuildMode,
    pub install: bool,
    pub target: Option<String>,
    pub extra_config_args: Vec<String>,
    pub extra_build_args: Vec<String>,
    pub extra_install_args: Vec<String>,
}

/// One build request: the resolved identity plus what the build *does*.
pub struct BuildRequest {
    pub identity: Identity,
    pub options: BuildOptions,
}

/// Run one build request: plan it, guard the branch it acts on, then run the
/// backend's steps.
pub fn build(ws: &Workspace, project: &Project, request: &BuildRequest) -> Result<()> {
    let identity = &request.identity;
    let opts = &request.options;

    // Resolve the backend once, from the project's declared build_system — it is
    // both the L0 toolchain injector for planning and the step emitter below.
    // (`build_system` is not profile-overridable, so this matches `plan`.) The
    // enum is total, so there is no "unsupported" error path here; an unknown
    // name was rejected when the project file was parsed.
    let backend = project.settings().build_system.map(backend_for);

    let plan = make_plan(ws, project, identity, opts, backend)?;

    let Some(build_dir) = plan.paths.build_dir.clone() else {
        log::warn!(
            "project '{}': no build_dir configured — nothing to build",
            project.name()
        );
        return Ok(());
    };
    let be = backend.context("build_dir is set but build_system is not")?;
    log::debug!("backend: {}", be.name());

    // The checkout the branch dance acts on: the identity repo's **workdir**, never
    // its `path`. Those differ for every bare-backed repo, where `path` is a git-dir
    // with no working tree — and the identity repo is not necessarily the build
    // repo, so it is not necessarily the one `plan.paths.strategy` describes. Reading
    // `path` here is what ran `git switch` inside a bare repository.
    //
    // Kept alive for the build scope so the restore guard can borrow it.
    let identity_git = plan
        .paths
        .identity_repo
        .as_deref()
        .and_then(|name| plan.paths.work_dir_of(name))
        .map(Repository::new);

    // An explicit work_dir (the CLI's `--work-dir`) means the caller already
    // materialised the checkout (e.g. a `review checkout` worktree of an MR);
    // build sources from it as-is and manages no branch or worktree of its own —
    // so neither the worktree-exists gate nor the in-place branch dance applies.
    // This is the whole point of the override: the two commands meet only at the
    // path.
    let _guard = if identity.work_dir.is_some() {
        if !plan.paths.work_dir.exists() {
            bail!(
                "--work-dir {} does not exist",
                plan.paths.work_dir.display()
            );
        }
        None
    } else if identity.branch.is_none() {
        // Resolution already verified that the identity checkout has a real
        // detached HEAD. Detached builds consume the selected checkouts as-is:
        // no branch worktree gate and no in-place branch dance.
        None
    } else {
        let branch = plan
            .paths
            .branch
            .as_ref()
            .context("attached build plan has no branch identity")?;
        // Two independent requirements, on two possibly different repos. The **build
        // repo's** checkout must exist where the plan says the sources are…
        match plan.paths.strategy {
            BranchStrategy::Worktree => require_worktree(&plan.paths.work_dir, &branch.raw)?,
            BranchStrategy::Hybrid => require_hybrid_worktree(ws, project, &plan, &branch.raw)?,
            BranchStrategy::InPlace => {}
        }
        // …and the **identity repo** must be on the branch being built.
        match (&identity_git, plan.paths.identity_repo.as_deref()) {
            (Some(git), Some(name)) => prepare_branch(project, git, &branch.raw, name)?,
            _ => None,
        }
    };

    let steps = be.steps(&EmitContext {
        source_dir: &plan.paths.source_dir,
        build_dir: &build_dir,
        install_dir: plan.paths.install_dir.as_deref(),
        build_type: &plan.paths.build_type,
        generator: plan.paths.generator.as_deref(),
        target: opts.target.as_deref(),
        logical: &plan.logical,
        mode: opts.mode,
        install: opts.install,
    })?;

    for step in &steps {
        log::info!("{}", step.description);
        let mut cmd = Command::new(&step.program);
        cmd.args(step.args.iter().cloned()).current_dir(&step.cwd);
        for (k, v) in &plan.logical.environment {
            cmd.env(k, v);
        }
        let code = cmd.status()?;
        if code != 0 {
            bail!("{} failed (exit {code})", step.description);
        }
    }
    Ok(())
}

/// One repo's declared branch strategy.
fn repo_strategy(project: &Project, name: &str) -> Result<BranchStrategy> {
    let repo = project.repo(name).with_context(|| {
        format!(
            "repo '{name}' is not defined in project '{}'",
            project.name()
        )
    })?;
    Ok(repo.decl().strategy)
}

/// A worktree that must already be there, on the branch being built. `build` never
/// creates one — that is `wits worktree create`'s act, so the error says so.
fn require_worktree(dir: &Path, branch: &str) -> Result<()> {
    if !dir.exists() {
        bail!(
            "worktree for branch '{branch}' does not exist at {} — create it with \
             `wits worktree create {branch} {}`",
            dir.display(),
            dir.display()
        );
    }
    let actual = Repository::new(dir)
        .current_branch()
        .with_context(|| format!("{} is not an attached Git worktree", dir.display()))?;
    if actual != branch {
        bail!(
            "worktree {} is on branch '{actual}', not '{branch}'",
            dir.display()
        );
    }
    Ok(())
}

fn require_hybrid_worktree(
    ws: &Workspace,
    project: &Project,
    plan: &BuildPlan,
    branch: &str,
) -> Result<()> {
    let build = project
        .repo(&plan.paths.build_repo)
        .context("build repo not found")?;
    let found = layout::checkout_holding(ws, project, build, branch)?;
    let Some(actual) = found else {
        bail!(
            "branch '{}' is not checked out in any worktree of repo '{}' — create it with \
             `wits worktree create {} {}`",
            branch,
            plan.paths.build_repo,
            branch,
            plan.paths.work_dir.display()
        );
    };
    if actual != plan.paths.work_dir {
        bail!(
            "worktree for branch '{}' moved from {} to {} while planning; retry the build",
            branch,
            plan.paths.work_dir.display(),
            actual.display()
        );
    }
    require_worktree(&plan.paths.work_dir, branch)
}

fn make_plan(
    ws: &Workspace,
    project: &Project,
    identity: &Identity,
    opts: &BuildOptions,
    be: Option<&dyn Backend>,
) -> Result<BuildPlan> {
    let profile = &identity.profile;
    // Select-vs-inject ("Selection vs injection, and trusting an existing
    // config"): in auto/build-only, an already-configured build dir with no
    // explicit toolchain request is *trusted* — we skip toolchain
    // injection so a rerun does not reconfigure. Injection only shapes the L0
    // env/definitions, never the paths, so the build dir is the same either way.
    //
    // Whether we're even *eligible* to trust is known without planning (mode +
    // explicit-toolchain); only "is it already configured?" needs the build dir.
    // So when eligible we plan the no-injection form first — the exact plan we
    // keep if we do trust — and re-plan with injection only when the dir turns
    // out to be unconfigured (a one-time first configure). Every subsequent
    // rerun of a configured tree, the frequent path, plans just once.
    let explicit_toolchain =
        profile.toolchain.is_some() || std::env::var_os("WITS_PROJECT_TOOLCHAIN").is_some();
    let trust_eligible =
        matches!(opts.mode, BuildMode::Auto | BuildMode::BuildOnly) && !explicit_toolchain;

    let plan_with = |inject_toolchain: bool| {
        resolve_plan(
            ws,
            project,
            &BuildInput {
                paths: PathInput {
                    profile,
                    branch: identity.branch.as_deref(),
                    work_dir: identity.work_dir.as_deref(),
                    build_dir: identity.build_dir.as_deref(),
                    install_dir: identity.install_dir.as_deref(),
                },
                inject_toolchain,
                injector: be.map(|b| b as &dyn ToolchainInjector),
                extra_config_args: &opts.extra_config_args,
                extra_build_args: &opts.extra_build_args,
                extra_install_args: &opts.extra_install_args,
            },
        )
    };

    if !trust_eligible {
        return plan_with(true);
    }

    let plan = plan_with(false)?;
    let configured = plan
        .paths
        .build_dir
        .as_ref()
        .zip(be)
        .is_some_and(|(bd, be)| be.is_configured(bd));
    if configured {
        Ok(plan)
    } else {
        plan_with(true)
    }
}

/// Put the identity repo's checkout on the branch being built, returning a guard
/// that restores it. `None` when nothing had to move.
///
/// `git` is that repo's resolved workdir, so the two strategies differ only in what
/// to do when it is *not* already on the branch, and that difference is real policy
/// rather than a special case:
///
/// - **in-place** owns a single checkout, so it is switched there and back —
///   the classic stash → switch → build → restore dance.
/// - **worktree/hybrid** keeps one checkout per branch, so the resolved workdir
///   already *is* the answer: either it holds the branch, or the worktree has to be
///   created, which `build` never does implicitly ("What the strategies still
///   decide"). Switching would be wrong twice over — there may be no working
///   tree to switch, and moving a worktree onto another branch pulls it out from
///   under whoever else is in it.
fn prepare_branch<'a>(
    project: &Project,
    git: &'a Repository,
    branch: &str,
    name: &str,
) -> Result<Option<RestoreGuard<'a>>> {
    if repo_strategy(project, name)?.is_bare_backed() {
        require_worktree(git.path(), branch)
            .with_context(|| format!("repo '{name}' carries this build's branch identity"))?;
        return Ok(None);
    }
    let current = git.current_branch().with_context(|| {
        format!(
            "repo '{name}' is in a detached HEAD; pass --detach to build HEAD as-is, \
             or check out the requested branch"
        )
    })?;
    if current == branch {
        return Ok(None);
    }
    if !git.rev_exists(branch) {
        bail!("branch '{branch}' does not exist in repo '{name}'");
    }

    let mut guard = RestoreGuard::capture(git);
    if git.stash_push("wits project auto-stash")? {
        guard.mark_stashed();
    }
    git.switch(branch)?;
    // Align that checkout's own submodules to the target's recorded state.
    let subs: Vec<String> = git
        .materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .collect();
    git.submodule_update(&subs, false)?;
    Ok(Some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wits_project::{BuildSystem, Profile, Workspace};

    /// A workspace with one meson project that selects a toolchain whose `cc`
    /// the meson backend translates to `CC`.
    fn ws_with(src: &Path) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            format!(
                r#"
[project]
build_system = "meson"
toolchain = "t"

[repos.main]
path = "{}"
main_branch = "main"
build_dir = "{}/_build"

[toolchains.t]
cc = "cc-from-toolchain"
"#,
                src.display(),
                src.display()
            ),
        )
        .unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        (dir, ws)
    }

    fn identity(profile: Profile) -> Identity {
        Identity {
            profile,
            branch: Some("main".into()),
            work_dir: None,
            build_dir: None,
            install_dir: None,
        }
    }

    fn plan(ws: &Workspace, profile: Profile, options: &BuildOptions) -> BuildPlan {
        let project = ws.project("app").unwrap();
        make_plan(
            ws,
            project,
            &identity(profile),
            options,
            Some(backend_for(BuildSystem::Meson)),
        )
        .unwrap()
    }

    fn mark_configured(src: &Path) {
        std::fs::create_dir_all(src.join("_build/meson-private")).unwrap();
        std::fs::write(src.join("_build/meson-private/coredata.dat"), "").unwrap();
    }

    /// Select-vs-inject: in auto mode with no explicit toolchain request, a
    /// configured build dir is *trusted* — the plan carries no injected L0
    /// values, so a rerun does not silently reconfigure. An unconfigured one
    /// replans with injection, the one-time first configure.
    #[test]
    fn a_configured_build_dir_without_an_explicit_toolchain_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("app");
        let (_cfg, ws) = ws_with(&src);

        let unconfigured = plan(&ws, Profile::default(), &BuildOptions::default());
        assert_eq!(
            unconfigured.logical.env_entry("CC"),
            Some("cc-from-toolchain"),
            "an unconfigured tree must be planned with the toolchain injected"
        );

        mark_configured(&src);
        let trusted = plan(&ws, Profile::default(), &BuildOptions::default());
        assert_eq!(
            trusted.logical.env_entry("CC"),
            None,
            "a configured tree must be trusted, not reconfigured"
        );
    }

    /// An explicit toolchain request is never trusted away: the caller asked
    /// for that toolchain, so injection happens even over a configured tree.
    #[test]
    fn an_explicit_toolchain_request_is_injected_even_into_a_configured_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("app");
        let (_cfg, ws) = ws_with(&src);
        mark_configured(&src);
        let profile = Profile {
            toolchain: Some("t".into()),
            ..Default::default()
        };
        let planned = plan(&ws, profile, &BuildOptions::default());
        assert_eq!(planned.logical.env_entry("CC"), Some("cc-from-toolchain"));
    }

    /// Modes that are not a plain (re)build are never trust-eligible either:
    /// a reconfig is exactly the case where the injected values must be present.
    #[test]
    fn a_reconfig_is_planned_with_the_toolchain_injected() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("app");
        let (_cfg, ws) = ws_with(&src);
        mark_configured(&src);
        let options = BuildOptions {
            mode: BuildMode::Reconfig,
            ..Default::default()
        };
        let planned = plan(&ws, Profile::default(), &options);
        assert_eq!(planned.logical.env_entry("CC"), Some("cc-from-toolchain"));
    }
}
