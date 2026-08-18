//! `wits build` — resolve a plan, honour the branch strategy, run the backend's
//! steps.
//!
//! Its own top-level command (§1.3 of `docs/project/design.md`), but entirely
//! built on `project`'s read-only core: `project` is the component that knows
//! what a project *is*; this module only knows how to *build* one.
//!
//! The build systems live in [`wits_util::build_system`], beside the
//! read-only core they build on: they are purely a build-time concern, so
//! `project` need not expose them (§1.4). The one thing the core resolver still
//! needs — translating a toolchain into native env/definitions at L0 (§5.4) —
//! it gets through the `ToolchainInjector` seam, which each backend implements
//! and `build` hands to `resolve::plan`.
//!
//! Under worktree/hybrid the target worktree must already exist — hybrid first
//! discovers it from Git's inventory — and `build` never creates one
//! implicitly. Under in-place, building a non-current branch switches the
//! focus's own-git repo behind a [`RestoreGuard`], so the working tree is always
//! returned to where it started, even on failure.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_util::process::Command;

use crate::cmd::project::ProfileArgs;
use wits_util::build_system::{backend_for, Backend, BuildMode, EmitContext};
use wits_util::git::{Repository, RestoreGuard};
use wits_util::project::model::{BranchStrategy, Profile};
use wits_util::project::resolve::{self, Plan, PlanInput, ToolchainInjector};
use wits_util::project::resolve_target;
use wits_util::project::workspace::{ProjectData, Workspace};

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Project name or path (default: the project owning the current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    #[command(flatten)]
    pub profile: ProfileArgs,

    /// Configure only; do not compile.
    #[arg(long = "config-only", conflicts_with_all = ["build_only", "reconfig", "uninstall"])]
    pub config_only: bool,
    /// Compile only; assume already configured.
    #[arg(long = "build-only", conflicts_with_all = ["reconfig", "uninstall"])]
    pub build_only: bool,
    /// Delete the build dir and configure fresh.
    #[arg(long, conflicts_with = "uninstall")]
    pub reconfig: bool,
    /// Reverse an install (backend-driven).
    #[arg(long)]
    pub uninstall: bool,
    /// Install after building.
    #[arg(long)]
    pub install: bool,
    /// Override the install prefix, ignoring the project's configured
    /// `install_dir` (the backend's install-prefix, e.g. cmake's
    /// `CMAKE_INSTALL_PREFIX`). Affects configure as well as install.
    #[arg(long = "install-dir", value_name = "DIR")]
    pub install_dir: Option<PathBuf>,
    /// Override the resolved build directory, ignoring the project's `build_dir`
    /// template — e.g. to build a `review checkout` in an isolated dir without
    /// touching config. The symmetric partner of `--install-dir`; highest
    /// priority, verbatim (§5.5).
    #[arg(long = "build-dir", value_name = "DIR")]
    pub build_dir: Option<PathBuf>,
    /// Build a specific target.
    #[arg(short = 't', long = "target")]
    pub build_target: Option<String>,

    /// Raw args appended to the configure command (verbatim).
    #[arg(long = "extra-config-args", num_args = 1.., value_name = "ARG")]
    pub extra_config_args: Vec<String>,
    /// Raw args appended to the build command (verbatim).
    #[arg(long = "extra-build-args", num_args = 1.., value_name = "ARG")]
    pub extra_build_args: Vec<String>,
    /// Raw args appended to the install command (verbatim).
    #[arg(long = "extra-install-args", num_args = 1.., value_name = "ARG")]
    pub extra_install_args: Vec<String>,
    /// Shorthand: -Xconfig,ARG / -Xbuild,ARG / -Xinstall,ARG (repeatable).
    #[arg(short = 'X', value_name = "SCOPE,ARG")]
    pub extra: Vec<String>,
}

/// What a build *does*, not where it resolves to (that is `project::Profile`).
/// Extra args are verbatim and applied last, at the highest priority (§5.5).
/// Lives here, not in `project::model`, because nothing outside this module
/// reads it — `resolve::plan` only ever needs the three extra-args lists,
/// passed separately so `project` doesn't need to know this type exists.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub mode: BuildMode,
    pub install: bool,
    /// A command-line override of the resolved install prefix (§5.5); `None`
    /// leaves the project's configured `install_dir` in force.
    pub install_dir: Option<PathBuf>,
    /// A command-line override of the resolved build dir (§5.5); `None` leaves
    /// the project's `build_dir` template in force.
    pub build_dir: Option<PathBuf>,
    pub target: Option<String>,
    pub extra_config_args: Vec<String>,
    pub extra_build_args: Vec<String>,
    pub extra_install_args: Vec<String>,
}

/// `wits build` — its own top-level command, over the shared `project` core.
pub fn run(args: &BuildArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let project = resolve_target(&ws, args.target.as_deref())?;
    execute(
        &ws,
        project,
        &args.profile.to_profile(),
        &build_options(args)?,
    )
}

fn build_options(a: &BuildArgs) -> Result<BuildOptions> {
    let mode = if a.config_only {
        BuildMode::ConfigOnly
    } else if a.build_only {
        BuildMode::BuildOnly
    } else if a.reconfig {
        BuildMode::Reconfig
    } else if a.uninstall {
        BuildMode::Uninstall
    } else {
        BuildMode::Auto
    };

    let (mut cfg, mut build, mut install) = (
        a.extra_config_args.clone(),
        a.extra_build_args.clone(),
        a.extra_install_args.clone(),
    );
    for x in &a.extra {
        let (scope, arg) = x
            .split_once(',')
            .with_context(|| format!("-X expects SCOPE,ARG (got '{x}')"))?;
        match scope {
            "config" => cfg.push(arg.to_owned()),
            "build" => build.push(arg.to_owned()),
            "install" => install.push(arg.to_owned()),
            other => bail!("-X scope must be config|build|install (got '{other}')"),
        }
    }

    Ok(BuildOptions {
        mode,
        install: a.install,
        install_dir: a.install_dir.clone(),
        build_dir: a.build_dir.clone(),
        target: a.build_target.clone(),
        extra_config_args: cfg,
        extra_build_args: build,
        extra_install_args: install,
    })
}

fn execute(
    ws: &Workspace,
    project: &ProjectData,
    profile: &Profile,
    opts: &BuildOptions,
) -> Result<()> {
    let focus = project.focus_name(profile.focus.as_deref()).to_owned();
    let identity = resolve::identity_repo(project, &focus);

    // The target branch: --branch, else the identity repo's current branch.
    let branch = match &profile.branch {
        Some(b) => b.clone(),
        None => current_branch(ws, project, identity.as_deref())?,
    };

    // Resolve the backend once, from the project's declared build_system — it is
    // both the L0 toolchain injector for planning and the step emitter below.
    // (`build_system` is not profile-overridable, so this matches `plan`.) The
    // enum is total, so there is no "unsupported" error path here; an unknown
    // name was rejected when the project file was parsed.
    let backend = project.project.build_system.map(backend_for);

    let mut plan = make_plan(ws, project, profile, opts, &branch, backend.as_deref())?;

    // A `--install-dir`/`--build-dir` on the command line overrides the
    // project's resolved value (§5.5, highest priority). Each only feeds a
    // backend step (install prefix / build dir), so patching the final plan
    // value is sufficient — no re-plan needed.
    if let Some(dir) = &opts.install_dir {
        plan.install_dir = Some(dir.clone());
    }
    if let Some(dir) = &opts.build_dir {
        plan.build_dir = Some(dir.clone());
    }

    let Some(build_dir) = plan.build_dir.clone() else {
        log::warn!(
            "project '{}': no build_dir configured — nothing to build",
            project.name
        );
        return Ok(());
    };
    let be = backend.context("build_dir is set but build_system is not")?;
    log::debug!("backend: {}", be.name());

    // The checkout the branch dance acts on: the identity repo's **workdir**, never
    // its `path`. Those differ for every bare-backed repo, where `path` is a git-dir
    // with no working tree — and the identity repo is not necessarily the build
    // repo, so it is not necessarily the one `plan.strategy` describes. Reading
    // `path` here is what ran `git switch` inside a bare repository.
    //
    // Kept alive for the build scope so the restore guard can borrow it.
    let identity_git = plan
        .identity_repo
        .as_deref()
        .and_then(|name| plan.work_dir_of(name))
        .map(Repository::new);

    // An explicit `--work-dir` means the caller already materialised the
    // checkout (e.g. a `review checkout` worktree of an MR); build sources from
    // it as-is and manages no branch or worktree of its own — so neither the
    // worktree-exists gate nor the in-place branch dance applies. This is the
    // whole point of the override: the two commands meet only at the path.
    let _guard = if profile.work_dir.is_some() {
        if !plan.work_dir.exists() {
            bail!("--work-dir {} does not exist", plan.work_dir.display());
        }
        None
    } else {
        // Two independent requirements, on two possibly different repos. The **build
        // repo's** checkout must exist where the plan says the sources are…
        match plan.strategy {
            BranchStrategy::Worktree => require_worktree(&plan.work_dir, &plan.branch_raw)?,
            BranchStrategy::Hybrid => require_hybrid_worktree(ws, project, &plan)?,
            BranchStrategy::InPlace => {}
        }
        // …and the **identity repo** must be on the branch being built.
        match (&identity_git, plan.identity_repo.as_deref()) {
            (Some(git), Some(name)) => prepare_branch(project, git, &plan, name)?,
            _ => None,
        }
    };

    let steps = be.steps(&EmitContext {
        source_dir: &plan.source_dir,
        build_dir: &build_dir,
        install_dir: plan.install_dir.as_deref(),
        build_type: &plan.build_type,
        generator: plan.generator.as_deref(),
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
fn repo_strategy(project: &ProjectData, name: &str) -> Result<BranchStrategy> {
    let repo = project
        .repos
        .get(name)
        .with_context(|| format!("repo '{name}' is not defined in project '{}'", project.name))?;
    BranchStrategy::parse(repo.branch_strategy.as_deref())
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

fn require_hybrid_worktree(ws: &Workspace, project: &ProjectData, plan: &Plan) -> Result<()> {
    let found = resolve::checkout_holding(ws, project, &plan.build_repo, &plan.branch_raw)?;
    let Some(actual) = found else {
        bail!(
            "branch '{}' is not checked out in any worktree of repo '{}' — create it with \
             `wits worktree create {} {}`",
            plan.branch_raw,
            plan.build_repo,
            plan.branch_raw,
            plan.work_dir.display()
        );
    };
    if actual != plan.work_dir {
        bail!(
            "worktree for branch '{}' moved from {} to {} while planning; retry the build",
            plan.branch_raw,
            plan.work_dir.display(),
            actual.display()
        );
    }
    require_worktree(&plan.work_dir, &plan.branch_raw)
}

fn make_plan(
    ws: &Workspace,
    project: &ProjectData,
    profile: &Profile,
    opts: &BuildOptions,
    branch: &str,
    be: Option<&dyn Backend>,
) -> Result<Plan> {
    // Select-vs-inject (§5.3): in auto/build-only, an already-configured build
    // dir with no explicit toolchain request is *trusted* — we skip toolchain
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

    if !trust_eligible {
        return plan_with(ws, project, profile, opts, branch, true, be);
    }

    let plan = plan_with(ws, project, profile, opts, branch, false, be)?;
    let configured = plan
        .build_dir
        .as_ref()
        .zip(be)
        .is_some_and(|(bd, be)| be.is_configured(bd));
    if configured {
        Ok(plan)
    } else {
        plan_with(ws, project, profile, opts, branch, true, be)
    }
}

fn plan_with(
    ws: &Workspace,
    project: &ProjectData,
    profile: &Profile,
    opts: &BuildOptions,
    branch: &str,
    inject_toolchain: bool,
    be: Option<&dyn Backend>,
) -> Result<Plan> {
    resolve::plan(
        ws,
        project,
        &PlanInput {
            profile,
            branch,
            inject_toolchain,
            injector: be.map(|b| b as &dyn ToolchainInjector),
            extra_config_args: &opts.extra_config_args,
            extra_build_args: &opts.extra_build_args,
            extra_install_args: &opts.extra_install_args,
        },
    )
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
///   created, which `build` never does implicitly (§3.4). Switching would be wrong
///   twice over — there may be no working tree to switch, and moving a worktree onto
///   another branch pulls it out from under whoever else is in it.
fn prepare_branch<'a>(
    project: &ProjectData,
    git: &'a Repository,
    plan: &Plan,
    name: &str,
) -> Result<Option<RestoreGuard<'a>>> {
    if repo_strategy(project, name)?.is_bare_backed() {
        require_worktree(git.path(), &plan.branch_raw)
            .with_context(|| format!("repo '{name}' carries this build's branch identity"))?;
        return Ok(None);
    }
    let current = git.current_branch().with_context(|| {
        format!("repo '{name}' is in a detached HEAD; pass --branch and check out a branch")
    })?;
    if current == plan.branch_raw {
        return Ok(None);
    }
    if !git.rev_exists(&plan.branch_raw) {
        bail!(
            "branch '{}' does not exist in repo '{name}'",
            plan.branch_raw
        );
    }

    let mut guard = RestoreGuard::capture(git);
    if git.stash_push("wits project auto-stash")? {
        guard.mark_stashed();
    }
    git.switch(&plan.branch_raw)?;
    // Align that checkout's own submodules to the target's recorded state.
    let subs: Vec<String> = git
        .materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .collect();
    git.submodule_update(&subs, false)?;
    Ok(Some(guard))
}

/// The branch a bare `wits build` is for: whatever the identity repo is currently
/// on. Read from that repo's *checkout* — see [`resolve::current_branch`], which is
/// also what the `project` path queries default through, so the two cannot disagree.
fn current_branch(ws: &Workspace, project: &ProjectData, identity: Option<&str>) -> Result<String> {
    let name = identity.context("project has no own-git repo to take a branch from")?;
    resolve::current_branch(ws, project, name)?.with_context(|| {
        format!("repo '{name}' has no branch checked out (a detached HEAD?); pass --branch")
    })
}
