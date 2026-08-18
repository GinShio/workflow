//! The resolution pipeline (§5): turn a project + a [`Profile`] into concrete
//! paths and one accumulated [`LogicalConfig`], in a single left-to-right pass.
//!
//! The pass is strictly one-directional — toolchain → org → project → presets → CLI —
//! and no later layer can overwrite a toolchain's compiler identity, so nothing
//! is ever re-asserted or recomputed. Context values may reference each other
//! (`env.BIN` from `env.TOOLS`); the template engine resolves those lazily, so
//! the order entries appear in a map never matters.
//!
//! Everything here is read-only. Most resolution is input-pure; hybrid workdir
//! resolution additionally reads Git's live worktree inventory so it can follow
//! a branch checked out outside the declared directory. It never mutates Git or
//! the filesystem, which is what lets `info` report without running a build.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::git::Repository;
use crate::template::{Engine, Value};

use super::context::{self, apply_def_map, apply_env_map, fold_env, resolve_args, Ctx};
use super::model::{infer_kind, BranchStrategy, BuildSystem, LogicalConfig, Profile, Toolchain};
use super::presets::{applied_presets, resolve_preset_into};
use super::toolchain::{resolve_toolchain, select_toolchain};
use super::workspace::{expand_tilde, ProjectData, Workspace};

// The context construction, preset, toolchain, and host-fact machinery lives in
// sibling modules; this file is just the pipeline that composes them. The public
// context builders are re-exported so existing `resolve::…` call sites (and the
// `build_system` backends) don't have to learn the new module layout.
pub use super::context::{context_for_repo, path_context, path_slug, repo_value, system_facts};

/// A fully resolved build plan: everything `build` executes and `info` reports.
pub struct Plan {
    pub focus: String,
    /// The repo the build sources from (the focus's `anchor`, or the focus).
    pub build_repo: String,
    /// The nearest own-git repo from the focus — what carries branch identity and
    /// is switched to the target branch.
    pub identity_repo: Option<String>,
    pub strategy: BranchStrategy,
    pub branch_raw: String,
    pub branch_slug: String,
    pub build_type: String,
    pub generator: Option<String>,
    /// The resolved build system. Part of the read-only query surface (§11);
    /// `build` reads it from project config pre-planning (to pick a backend
    /// before it has a plan), so this copy is currently for consumers/`info`.
    #[allow(dead_code)]
    pub build_system: Option<BuildSystem>,
    pub toolchain: Option<Toolchain>,
    /// The resolved checkout of `build_repo`, also exposed to templates as
    /// `repos.<build_repo>.workdir`.
    pub work_dir: PathBuf,
    /// Every repo's resolved checkout, by name — the same values bound as
    /// `repos.<name>.workdir`, kept typed.
    ///
    /// This is what a caller that has to **run git somewhere** reads. A repo's
    /// `path` is its repository, which for a bare-backed repo is a git-dir with no
    /// working tree; its `workdir` is a checkout. The distinction is invisible for
    /// an in-place repo, which is exactly why it gets lost — and a project may mix
    /// the two, taking its branch identity from a bare-backed component it borrows
    /// while building an in-place checkout of its own.
    pub work_dirs: std::collections::BTreeMap<String, PathBuf>,
    /// Where the backend configures from: the build repo's `source_dir` template,
    /// or the build repo's workdir when unset. Distinct from `work_dir`, which
    /// stays the checkout root that carries branch identity and anchors paths.
    pub source_dir: PathBuf,
    /// The build repo's resolved `build_dir` template, if it declared one.
    pub build_dir: Option<PathBuf>,
    /// The build repo's resolved `install_dir` template, if it declared one.
    pub install_dir: Option<PathBuf>,
    pub logical: LogicalConfig,
    /// The final context, so callers can resolve arbitrary templates or inspect.
    /// Part of the read-only query surface (§11); not consumed in-tree yet.
    #[allow(dead_code)]
    pub context: Value,
}

impl Plan {
    /// One repo's resolved checkout — see [`work_dirs`](Plan::work_dirs).
    pub fn work_dir_of(&self, repo: &str) -> Option<&std::path::Path> {
        self.work_dirs.get(repo).map(PathBuf::as_path)
    }
}

/// The one build-system responsibility the pipeline needs: translate a selected
/// [`Toolchain`]'s canonical fields into a backend's native env/definitions at
/// L0 (§5.4). This is the *only* seam between the read-only core and the build
/// systems — the core owns the trait, but the concrete backends that implement
/// it live entirely in `crate::build_system` (§1.4). The core never names
/// a backend, and
/// callers that only resolve *paths* (the `*-dir` queries, `info`) inject nothing.
pub trait ToolchainInjector {
    /// Merge the toolchain's native env/definitions into `cfg`. Runs at L0, so a
    /// later preset or CLI override of the same key wins.
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig);
}

/// Inputs that vary per invocation but are not part of the file model.
///
/// Deliberately *not* `build::BuildOptions`: the pipeline only ever needs the
/// verbatim L3 overrides (§5.5), not the build action's `mode`/`install`/
/// `target`, so callers that just resolve paths (the `*-dir` queries, `--check`)
/// can leave those empty instead of fabricating a whole `BuildOptions`.
pub struct PlanInput<'a> {
    pub profile: &'a Profile,
    /// The target branch (from `--branch` or the caller's git read).
    pub branch: &'a str,
    /// Whether to inject the toolchain's env/definitions (skipped when trusting
    /// an already-configured build dir; §5.3). Selection still happens.
    pub inject_toolchain: bool,
    /// The build system's toolchain translator (§5.4). `None` for path-only
    /// resolves; L0 is skipped when it is absent even if `inject_toolchain`.
    pub injector: Option<&'a dyn ToolchainInjector>,
    /// L3 — verbatim overrides, applied last, at the highest priority.
    pub extra_config_args: &'a [String],
    pub extra_build_args: &'a [String],
    pub extra_install_args: &'a [String],
}

impl<'a> PlanInput<'a> {
    /// A path-only resolve: select a toolchain but don't inject it, and apply no
    /// L3 overrides. This is exactly what the read-only consumers want — `info`,
    /// the `*-dir` path queries, and `--check` validation all resolve paths
    /// without a backend — so they no longer spell out the four empty/false fields
    /// (and can't drift on them). `build` still uses the full struct, since it
    /// alone supplies an injector and real L3 args.
    pub fn paths_only(profile: &'a Profile, branch: &'a str) -> Self {
        PlanInput {
            profile,
            branch,
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        }
    }
}

pub fn plan(ws: &Workspace, project: &ProjectData, input: &PlanInput<'_>) -> Result<Plan> {
    let profile = input.profile;
    let focus = project.focus_name(profile.focus.as_deref()).to_owned();
    if !project.repos.contains_key(&focus) {
        bail!(
            "focus repo '{focus}' is not defined in project '{}'",
            project.name
        );
    }
    let build_repo = anchor_of(project, &focus);
    if !project.repos.contains_key(&build_repo) {
        bail!("anchor repo '{build_repo}' (of focus '{focus}') is not defined");
    }
    let identity_repo = identity_repo(project, &focus);
    let strategy = BranchStrategy::parse(project.repos[&build_repo].branch_strategy.as_deref())?;

    let branch_raw = input.branch.to_owned();
    let branch_slug = path_slug(&branch_raw);
    let build_type = profile
        .build_type
        .clone()
        .unwrap_or_else(|| "debug".to_owned());
    let generator = profile
        .generator
        .clone()
        .or_else(|| project.project.generator.clone());
    let build_system = project.project.build_system;

    // Toolchain *selection* always happens (path templates depend on the name).
    let toolchain = select_toolchain(ws, project, profile)?;

    // --- Base context ------------------------------------------------------
    // The shared base (project.*, repos.*, repo.* = focus, org.*, system.*,
    // env.*) is built in `context`; the pipeline layers the Profile-specific
    // bindings on top.
    let mut ctx = context::plan_base(ws, project, &focus);
    // The Profile-free context initially carries configured repository paths.
    // Once a full plan is available, nested repos under a bare-backed main must
    // instead point into main's bootstrap checkout.
    for name in project.repos.keys() {
        let primary = repo_primary_path(ws, project, name)?;
        ctx.set(
            &format!("repos.{name}.path"),
            Value::str(primary.display().to_string()),
        );
        if name == &focus {
            ctx.set("repo.path", Value::str(primary.display().to_string()));
        }
    }
    ctx.set("branch.raw", Value::str(&branch_raw));
    ctx.set("branch.slug", Value::str(&branch_slug));
    ctx.set("build_type", Value::str(&build_type));
    if let Some(gen) = &generator {
        ctx.set("generator", Value::str(gen));
    }
    // CLI-registered `--spec K=V` values, as the `spec.*` namespace. Only what
    // was passed is bound, so a template that references an unsupplied
    // `{{spec.X}}` fails loudly (the engine errors on an unknown path, §6.1) —
    // the "must be specified to be used" contract, enforced rather than guessed.
    for (key, value) in &profile.specs {
        ctx.set(&format!("spec.{key}"), Value::str(value));
    }

    // Resolve the toolchain against the base context and expose it as toolchain.*.
    let toolchain = match toolchain {
        Some((name, raw)) => Some(resolve_toolchain(&mut ctx, name, &raw)?),
        None => None,
    };

    // --- Paths -------------------------------------------------------------
    // A `--work-dir` override wins over the strategy (§5.5, highest priority,
    // verbatim): the caller has a checkout in hand and wants the build sourced
    // from it, so we neither resolve `worktree_dir` nor assume the in-place
    // clone. The resolved checkout is stored under the build repo's namespace,
    // so a bare repository's `path` remains the Git common directory rather than
    // being mistaken for a source tree.
    // Resolve a workdir for every named repo. `path` remains the repository
    // identity/common directory (and can therefore be a bare repository);
    // `workdir` is the checkout that a branch-specific template should use.
    // The explicit `--work-dir` override applies only to the build repo, while
    // the other repos use their own declared strategy.
    let mut work_dirs = std::collections::BTreeMap::new();
    for name in project.repos.keys() {
        let repo_strategy = if name == &build_repo {
            strategy
        } else {
            BranchStrategy::parse(project.repos[name].branch_strategy.as_deref())?
        };
        let dir = match (&profile.work_dir, name == &build_repo) {
            (Some(dir), true) => dir.clone(),
            _ => resolve_work_dir(ws, project, &ctx, name, repo_strategy, &branch_raw)?,
        };
        ctx.set(
            &format!("repos.{name}.workdir"),
            Value::str(dir.display().to_string()),
        );
        work_dirs.insert(name.clone(), dir);
    }
    let work_dir = work_dirs[&build_repo].clone();

    // The configure source and the output dirs live on the **build repo**
    // (the focus's anchor): that checkout owns work / source / build / install
    // together. `source_dir` defaults to the checkout root; the other two are
    // optional, so a repo that is never built simply omits them.
    let build = &project.repos[&build_repo];
    let source_dir = match &build.source_dir {
        Some(tpl) => PathBuf::from(ctx.render(tpl)?),
        None => work_dir.clone(),
    };
    let build_dir = match &build.build_dir {
        Some(tpl) => Some(PathBuf::from(ctx.render(tpl)?)),
        None => None,
    };
    let install_dir = match &build.install_dir {
        Some(tpl) => Some(PathBuf::from(ctx.render(tpl)?)),
        None => None,
    };

    // --- Pipeline ----------------------------------------------------------
    let mut logical = LogicalConfig::default();

    // L0 — toolchain injection. The build system's translator (§5.4) is supplied
    // by the caller; a path-only resolve has none, and simply skips this layer.
    if input.inject_toolchain {
        if let (Some(tc), Some(inj)) = (&toolchain, input.injector) {
            inj.apply_toolchain(tc, &mut logical);
            fold_env(&mut ctx, &logical);
        }
    }

    // L0.5 — org config. An org's environment/definitions are its unconditional
    // contribution to every project that joins it (§5.6), the way a project's own
    // are to its build; only its presets (L2) have to be named to take effect.
    // Applied below the project so any inherited key can be overridden simply by
    // declaring it. An org that was never declared contributes nothing, exactly
    // as it contributes no namespace and no presets.
    if let Some(org) = project.org.as_deref() {
        if let Some(org_data) = ws.org_base(org) {
            apply_env_map(
                &mut ctx,
                &mut logical,
                "org.environment",
                &org_data.environment,
            )?;
            apply_def_map(
                &mut ctx,
                &mut logical,
                "org.definitions",
                &org_data.definitions,
            )?;
        }
    }

    // L1 — project config.
    apply_env_map(
        &mut ctx,
        &mut logical,
        "project.environment",
        &project.project.environment,
    )?;
    apply_def_map(
        &mut ctx,
        &mut logical,
        "project.definitions",
        &project.project.definitions,
    )?;
    resolve_args(
        &ctx,
        &project.project.extra_config_args,
        &mut logical.extra_config_args,
    )?;
    resolve_args(
        &ctx,
        &project.project.extra_build_args,
        &mut logical.extra_build_args,
    )?;
    resolve_args(
        &ctx,
        &project.project.extra_install_args,
        &mut logical.extra_install_args,
    )?;

    // L2 — presets.
    let names = applied_presets(
        ws,
        project,
        &focus,
        profile,
        &toolchain,
        &build_type,
        &generator,
    );
    for name in &names {
        let mut seen = Vec::new();
        resolve_preset_into(&mut ctx, &mut logical, ws, project, &focus, name, &mut seen)?;
    }

    // L3 — CLI extra args (verbatim, highest priority).
    logical
        .extra_config_args
        .extend(input.extra_config_args.iter().cloned());
    logical
        .extra_build_args
        .extend(input.extra_build_args.iter().cloned());
    logical
        .extra_install_args
        .extend(input.extra_install_args.iter().cloned());

    Ok(Plan {
        focus,
        build_repo,
        identity_repo,
        strategy,
        branch_raw,
        branch_slug,
        build_type,
        generator,
        build_system,
        toolchain,
        work_dir,
        work_dirs,
        source_dir,
        build_dir,
        install_dir,
        logical,
        context: ctx.into_value(),
    })
}

// --- focus / anchor / identity ------------------------------------------------

/// The repo the build sources from: the focus's `anchor`, or the focus itself.
pub fn anchor_of(project: &ProjectData, focus: &str) -> String {
    project
        .repos
        .get(focus)
        .and_then(|r| r.anchor.clone())
        .unwrap_or_else(|| focus.to_owned())
}

/// The nearest own-git repo starting from the focus: the focus itself if it has
/// its own git, otherwise its anchor (a subtree shares its anchor's git).
pub fn identity_repo(project: &ProjectData, focus: &str) -> Option<String> {
    let mut name = focus.to_owned();
    for _ in 0..project.repos.len() + 1 {
        let repo = project.repos.get(&name)?;
        if infer_kind(&name, repo).has_own_git() {
            return Some(name);
        }
        name = repo.anchor.clone()?;
    }
    None
}

// --- paths --------------------------------------------------------------------

fn resolve_work_dir(
    ws: &Workspace,
    project: &ProjectData,
    ctx: &Ctx,
    repo_name: &str,
    strategy: BranchStrategy,
    branch: &str,
) -> Result<PathBuf> {
    match strategy {
        BranchStrategy::InPlace => repo_primary_path(ws, project, repo_name)
            .with_context(|| format!("cannot resolve path of repo '{repo_name}'")),
        BranchStrategy::Worktree | BranchStrategy::Hybrid => {
            if strategy == BranchStrategy::Hybrid {
                if let Some(path) = checkout_holding(ws, project, repo_name, branch)? {
                    return Ok(path);
                }
            }
            let tpl = project.repos[repo_name]
                .worktree_dir
                .as_deref()
                .with_context(|| {
                    format!(
                        "repo '{repo_name}' uses {} strategy but has no worktree_dir",
                        project.repos[repo_name]
                            .branch_strategy
                            .as_deref()
                            .unwrap_or("in-place")
                    )
                })?;
            // `worktree_dir` is a field of this repo, so scope `repo` to it — and
            // render it in the namespace of the project that *owns* the repo. For a
            // repo this project declares itself those are the same thing, and the
            // plan's own richer context is used. For a **borrowed** one they are not:
            // the template was written by the owning project, where `{{project.name}}`
            // means the component, so rendering it here would relocate every worktree
            // of a shared component under the borrower's name. (`path` is spared
            // because the loader makes it absolute up front; a branch-keyed template
            // cannot be, so it carries its owner instead.)
            let (owner, owner_repo) = template_owner(ws, project, repo_name)?;
            let mut scoped = match std::ptr::eq(owner, project) {
                true => ctx.clone(),
                false => Ctx::new(branch_root(ws, owner, &owner_repo, branch)),
            };
            scoped.set("repo", repo_value(owner, &owner_repo));
            let primary = repo_primary_path(ws, owner, &owner_repo)?;
            scoped.set("repo.path", Value::str(primary.display().to_string()));
            anchor_worktree_path(ws, owner, &owner_repo, expand_tilde(&scoped.render(tpl)?))
        }
    }
}

/// The project whose namespace a repo's **own** path templates belong to, and the
/// name it goes by there: itself, or the project a `from` borrow points at.
fn template_owner<'a>(
    ws: &'a Workspace,
    project: &'a ProjectData,
    repo_name: &str,
) -> Result<(&'a ProjectData, String)> {
    let Some(spec) = project.repos.get(repo_name).and_then(|r| r.from.as_deref()) else {
        return Ok((project, repo_name.to_owned()));
    };
    let reference = super::model::parse_borrow(spec).map_err(anyhow::Error::msg)?;
    Ok((ws.project(reference.project)?, reference.repo.to_owned()))
}

/// A context able to resolve one repo's branch-keyed path templates: its project's
/// per-repo namespace plus the `branch.*` bindings a `worktree_dir` needs.
fn branch_root(ws: &Workspace, project: &ProjectData, repo_name: &str, branch: &str) -> Value {
    let mut root = context_for_repo(ws, project, repo_name);
    root.insert_path("branch.raw", Value::str(branch));
    root.insert_path("branch.slug", Value::str(path_slug(branch)));
    root
}

fn anchor_worktree_path(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
    path: PathBuf,
) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    let repository = repo_primary_path(ws, project, repo_name)?;
    let parent = repository.parent().with_context(|| {
        format!(
            "repo '{repo_name}' path {} has no parent for relative worktree path {}",
            repository.display(),
            path.display()
        )
    })?;
    Ok(parent.join(path))
}

// --- where a repo's git actually runs -----------------------------------------
//
// A repo has two locations, and confusing them is the single mistake this section
// exists to make impossible. Its **`path`** is the repository — a git-dir, and for
// a bare-backed repo a git-dir with no working tree at all. Its **`workdir`** is a
// checkout. Anything that touches a working tree (a branch switch, a merge, a
// submodule update, a sparse mask, "which branch am I on") has to run in the
// latter, and the four functions below are the only sanctioned ways to name one.
//
// For an in-place repo the two coincide, which is why the distinction gets lost —
// and a project may freely mix the strategies, building an in-place checkout of its
// own while taking branch identity from a bare-backed component it borrows.

/// The checkout of `repo_name` for `branch`, following **that repo's own** declared
/// strategy — the standalone form of what [`plan`] binds as
/// `repos.<name>.workdir`.
///
/// Callers with a [`Plan`] in hand should read [`Plan::work_dir_of`] instead; this
/// is for the lifecycle commands that have no plan. The path is not promised to
/// exist: under worktree/hybrid an absent checkout resolves to the location
/// `worktree_dir` names, which is what makes an error message able to say where the
/// worktree *should* be.
pub fn work_dir(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
    branch: &str,
) -> Result<PathBuf> {
    let repo = project
        .repos
        .get(repo_name)
        .with_context(|| format!("repo '{repo_name}' not found"))?;
    let strategy = BranchStrategy::parse(repo.branch_strategy.as_deref())?;
    let ctx = Ctx::new(branch_root(ws, project, repo_name, branch));
    resolve_work_dir(ws, project, &ctx, repo_name, strategy, branch)
}

/// The checkout of `repo_name` that currently has `branch` checked out, whatever
/// shape the repository is: a conventional clone when that is the branch it sits
/// on, or the linked worktree holding it in a bare-backed one. `None` when no
/// checkout has it.
///
/// Git's live inventory is the authority rather than the `worktree_dir` template:
/// a worktree may have been created or moved elsewhere, and hybrid is specifically
/// the strategy that follows it.
pub fn checkout_holding(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
    branch: &str,
) -> Result<Option<PathBuf>> {
    Ok(repository_worktree_for_branch(
        &repository_of(ws, project, repo_name)?,
        branch,
    ))
}

/// The checkout `repo_name`'s working-tree work runs in when no branch selects one:
/// its own working tree, or a bare-backed repo's stand-in for the main worktree it
/// does not have. `None` only when the repo has no checkout at all.
///
/// Independent of the caller's cwd, which is what a *lifecycle* command needs — a
/// verification whose target moved with your shell would report different things on
/// different runs. See [`crate::worktree::primary_checkout`].
pub fn primary_checkout(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
) -> Result<Option<PathBuf>> {
    Ok(crate::worktree::primary_checkout(&repository_of(
        ws, project, repo_name,
    )?))
}

/// The checkout of `repo_name` that carries its branch identity **right now**, with
/// no branch supplied to resolve against.
///
/// A repository with one working tree has only one answer. A bare-backed one has a
/// checkout per branch and so no inherent current one — the caller's cwd decides
/// when it is standing in one of them, since that is precisely what "the branch I
/// am working on" means, and [`primary_checkout`] answers otherwise. What must
/// *not* answer is the bare repository's own symbolic HEAD: that names
/// `main_branch`, whose worktree may not exist at all, so every path derived from
/// it would point at nothing.
pub fn current_checkout(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
) -> Result<Option<PathBuf>> {
    let repository = repository_of(ws, project, repo_name)?;
    if let Ok(cwd) = std::env::current_dir() {
        let here = Repository::new(cwd);
        if same_repository(&repository, &here) {
            if let Some(top) = here.toplevel() {
                return Ok(Some(top));
            }
        }
    }
    Ok(crate::worktree::primary_checkout(&repository))
}

/// The branch `repo_name` is on, read from [`current_checkout`]. `None` on a
/// detached HEAD, or when the repo has no checkout to read.
pub fn current_branch(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
) -> Result<Option<String>> {
    Ok(current_checkout(ws, project, repo_name)?
        .and_then(|dir| Repository::new(dir).current_branch()))
}

/// A handle on the repo *as a repository* — the one place `path` is the right
/// answer, because `git worktree list` and the ref plumbing are asked of the
/// repository rather than of a checkout.
fn repository_of(ws: &Workspace, project: &ProjectData, repo_name: &str) -> Result<Repository> {
    let path = repo_primary_path(ws, project, repo_name)
        .with_context(|| format!("cannot resolve path of repo '{repo_name}'"))?;
    Ok(Repository::new(path))
}

/// Do two handles name the same repository? Compared by common git-dir through the
/// filesystem, since git spells one directory several ways depending on how it was
/// reached (a symlinked parent, `/tmp` against `/private/tmp`).
fn same_repository(a: &Repository, b: &Repository) -> bool {
    a.git_common_dir()
        .zip(b.git_common_dir())
        .is_some_and(|(a, b)| {
            let real = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
            real(a) == real(b)
        })
}

fn repository_worktree_for_branch(git: &Repository, branch: &str) -> Option<PathBuf> {
    let main = git.main_worktree();
    git.worktrees()
        .into_iter()
        .enumerate()
        .find(|(_, wt)| {
            !wt.bare && !wt.prunable && wt.branch.as_deref() == Some(branch) && wt.path.exists()
        })
        .map(|(index, wt)| {
            if index == 0 {
                main.unwrap_or(wt.path)
            } else {
                wt.path
            }
        })
}

/// The stable checkout/repository path used for repo-scoped Git operations.
///
/// A nested repo normally lives under `repos.main`. When main is bare-backed,
/// that relative path belongs under main's checkout, not inside the bare common
/// directory — see [`nesting_root`]. Standalone and borrowed repos already carry an
/// absolute repository path and pass through unchanged.
pub fn repo_primary_path(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
) -> Result<PathBuf> {
    let path = project.repo_abs_path(repo_name)?;
    if repo_name == "main" || project.kind_of(repo_name) == Some(super::model::Kind::Standalone) {
        return Ok(path);
    }

    let main_path = project.repo_abs_path("main")?;
    let relative = path.strip_prefix(&main_path).with_context(|| {
        format!(
            "nested repo '{repo_name}' path {} is not under repos.main {}",
            path.display(),
            main_path.display()
        )
    })?;
    Ok(nesting_root(ws, project, "main")?.join(relative))
}

/// The checkout that `repo_name`'s **nested** repos live inside.
///
/// A conventional clone is its own answer. A bare-backed repo's is a *worktree* —
/// the one holding `main_branch`, else its bootstrap — because a git-dir has no tree
/// for anything to be nested in. Shared with the caller that has to decide whether
/// the nested lifecycle can run at all, so that check and the paths it guards can
/// never disagree about which directory is meant.
pub fn nesting_root(ws: &Workspace, project: &ProjectData, repo_name: &str) -> Result<PathBuf> {
    let path = project.repo_abs_path(repo_name)?;
    let repo = project
        .repos
        .get(repo_name)
        .with_context(|| format!("repo '{repo_name}' not found"))?;
    if !BranchStrategy::parse(repo.branch_strategy.as_deref())?.is_bare_backed() {
        return Ok(path);
    }
    if !path.exists() {
        return bootstrap_worktree_dir(ws, project, repo_name);
    }
    let git = Repository::new(&path);
    // Declared bare-backed but on disk still a conventional checkout — a migration
    // in progress. Nest under what is actually there.
    if git.git_dir().is_none() || !git.is_bare() {
        return Ok(path);
    }
    match repo
        .main_branch
        .as_deref()
        .and_then(|branch| repository_worktree_for_branch(&git, branch))
    {
        Some(worktree) => Ok(worktree),
        None => bootstrap_worktree_dir(ws, project, repo_name),
    }
}

/// Resolve the fixed worktree created immediately after a bare clone.
///
/// Hybrid requires an explicit, branch-independent template. Worktree may use
/// one too; when omitted its ordinary `worktree_dir` is rendered for
/// `main_branch`, preserving the deterministic branch-named layout.
pub fn bootstrap_worktree_dir(
    ws: &Workspace,
    project: &ProjectData,
    repo_name: &str,
) -> Result<PathBuf> {
    // Both templates are the repo's own, so both render in the namespace of the
    // project that owns it — see [`template_owner`].
    let (project, repo_name) = template_owner(ws, project, repo_name)?;
    let repo_name = repo_name.as_str();
    let repo = project
        .repos
        .get(repo_name)
        .with_context(|| format!("repo '{repo_name}' not found"))?;
    let branch = repo
        .main_branch
        .as_deref()
        .with_context(|| format!("repo '{repo_name}' has no main_branch"))?;
    let template = repo
        .worktree_dir
        .as_deref()
        .with_context(|| format!("repo '{repo_name}' has no worktree_dir"))?;
    let rendered =
        Engine::new(branch_root(ws, project, repo_name, branch)).resolve_str(template)?;
    let main_worktree = match rendered {
        Value::Str(path) => anchor_worktree_path(ws, project, repo_name, expand_tilde(&path))?,
        other => bail!("worktree_dir for repo '{repo_name}' resolved to a non-string: {other:?}"),
    };

    if let Some(template) = repo.bootstrap_worktree_dir.as_deref() {
        let engine = Engine::new(context_for_repo(ws, project, repo_name));
        let rendered = engine.resolve_str(template).with_context(|| {
            format!(
                "resolving bootstrap_worktree_dir for repo '{repo_name}' \
                 (it must not reference branch.*)"
            )
        })?;
        return match rendered {
            Value::Str(path) => {
                let path = expand_tilde(&path);
                if path.is_absolute() {
                    Ok(path)
                } else {
                    let parent = main_worktree.parent().with_context(|| {
                        format!(
                            "worktree_dir {} has no parent for relative \
                             bootstrap_worktree_dir {}",
                            main_worktree.display(),
                            path.display()
                        )
                    })?;
                    Ok(parent.join(path))
                }
            }
            other => bail!(
                "bootstrap_worktree_dir for repo '{repo_name}' resolved to a non-string: \
                 {other:?}"
            ),
        };
    }
    Ok(main_worktree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::Profile;

    fn ws_with(body: &str, stem: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{stem}.toml")), body).unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        (dir, ws)
    }

    /// A stand-in for a real backend, so the pipeline can be tested without any
    /// build-system dependency: it echoes the toolchain's `cc` into a definition
    /// (the way a backend would) so we can assert L0 ran. Real per-backend
    /// translation is tested in `crate::build_system`.
    struct MockInjector;
    impl ToolchainInjector for MockInjector {
        fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig) {
            if let Some(cc) = &tc.cc {
                cfg.set_definition("MOCK_CC", Value::Str(cc.clone()));
            }
        }
    }

    #[test]
    fn resolves_paths_and_injects_toolchain() {
        let body = r#"
            [project]
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{toolchain.name}}/{{build_type}}"

            [toolchains.clang]
            cc = "clang"
            cxx = "clang++"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile {
            build_type: Some("release".into()),
            ..Default::default()
        };
        let injector = MockInjector;
        let input = PlanInput {
            profile: &profile,
            branch: "main",
            inject_toolchain: true,
            injector: Some(&injector),
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = plan(&ws, project, &input).unwrap();
        assert_eq!(plan.work_dir, PathBuf::from("/src/hello"));
        assert_eq!(
            plan.build_dir.unwrap(),
            PathBuf::from("/src/hello/_build/clang/release")
        );
        // The injector ran at L0: the selected toolchain's `cc` was translated
        // into the backend-shaped definition the mock emits.
        assert!(plan.logical.definitions.iter().any(|(k, _)| k == "MOCK_CC"));
    }

    #[test]
    fn bare_repo_uses_a_named_workdir_for_worktree_builds() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello.git"
            main_branch = "main"
            build_dir = "{{repos.other.workdir}}/_build/{{branch.slug}}"
            branch_strategy = "worktree"
            worktree_dir = "{{repo.path}}.worktrees/{{branch.slug}}"

            [repos.other]
            path = "/src/other.git"
            main_branch = "main"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.worktrees/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.primary"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let plan = plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "feature/x"),
        )
        .unwrap();

        assert_eq!(
            plan.work_dir,
            PathBuf::from("/src/hello.git.worktrees/feature_x")
        );
        assert_eq!(
            plan.build_dir.unwrap(),
            PathBuf::from("/src/other.git.worktrees/feature_x/_build/feature_x")
        );
    }

    #[test]
    fn bootstrap_path_is_explicit_for_hybrid_and_main_branch_for_worktree() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/main.git"
            main_branch = "trunk"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.primary"

            [repos.other]
            path = "/src/other.git"
            main_branch = "stable/x"
            branch_strategy = "worktree"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"

            [repos.relative]
            path = "/src/relative.git"
            main_branch = "main"
            branch_strategy = "hybrid"
            worktree_dir = "/work/relative/{{branch.slug}}"
            bootstrap_worktree_dir = "main"

            [repos.anchored]
            path = "/src/anchored.git"
            main_branch = "main"
            branch_strategy = "worktree"
            worktree_dir = "worktrees/{{branch.slug}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, "main").unwrap(),
            PathBuf::from("/src/main.git.primary")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, "other").unwrap(),
            PathBuf::from("/src/other.git.wt/stable_x")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, "relative").unwrap(),
            PathBuf::from("/work/relative/main")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, "anchored").unwrap(),
            PathBuf::from("/src/worktrees/main")
        );
    }

    #[test]
    fn top_level_work_dir_is_not_a_template_variable() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/hello.git"
            main_branch = "main"
            build_dir = "{{work.dir}}/_build"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let err = match plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "main"),
        ) {
            Ok(_) => panic!("expected the removed top-level work.dir to fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("work.dir"));
    }

    #[test]
    fn spec_vars_and_work_dir_override_resolve() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/mr-{{spec.mr}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile {
            // A checkout materialised elsewhere (e.g. a review worktree).
            work_dir: Some(PathBuf::from("/tmp/hello.review/mr-42")),
            specs: [("mr".to_owned(), "42".to_owned())].into_iter().collect(),
            ..Default::default()
        };
        let plan = plan(&ws, project, &PlanInput::paths_only(&profile, "feature")).unwrap();
        // The named repo workdir is the verbatim override, not the strategy's
        // path…
        assert_eq!(plan.work_dir, PathBuf::from("/tmp/hello.review/mr-42"));
        // …and the build_dir template anchored on it and consumed `{{spec.mr}}`.
        assert_eq!(
            plan.build_dir.unwrap(),
            PathBuf::from("/tmp/hello.review/mr-42/_build/mr-42")
        );
    }

    #[test]
    fn a_referenced_but_unsupplied_spec_is_a_hard_error() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{spec.mr}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        // No `spec.mr` supplied — resolving the template must fail loudly.
        let err = match plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "main"),
        ) {
            Ok(_) => panic!("expected a hard error for the unsupplied spec"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("spec"),
            "error names the missing path: {err}"
        );
    }

    #[test]
    fn source_dir_defaults_to_repo_workdir_and_can_be_a_subdir() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            source_dir = "{{repos.main.workdir}}/subdir"

            [repos.other]
            path = "/src/other"
            main_branch = "main"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let base = Profile::default();
        let input = |profile: &Profile| -> Plan {
            plan(
                &ws,
                project,
                &PlanInput {
                    profile,
                    branch: "main",
                    inject_toolchain: false,
                    injector: None,
                    extra_config_args: &[],
                    extra_build_args: &[],
                    extra_install_args: &[],
                },
            )
            .unwrap()
        };
        // The named repo workdir stays the checkout root; source_dir is the
        // declared subdir.
        let plan = input(&base);
        assert_eq!(plan.work_dir, PathBuf::from("/src/hello"));
        assert_eq!(plan.source_dir, PathBuf::from("/src/hello/subdir"));
        // A repo without source_dir falls back to its workdir.
        let other = Profile {
            focus: Some("other".into()),
            ..Default::default()
        };
        let plan2 = input(&other);
        assert_eq!(plan2.source_dir, plan2.work_dir);
    }

    #[test]
    fn no_injector_skips_l0_but_still_resolves_paths() {
        let body = r#"
            [project]
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"

            [toolchains.clang]
            cc = "clang"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile::default();
        let input = PlanInput {
            profile: &profile,
            branch: "main",
            inject_toolchain: true, // requested, but no injector supplied
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = plan(&ws, project, &input).unwrap();
        // Toolchain *selection* still happened (paths need the name)…
        assert_eq!(plan.toolchain.as_ref().unwrap().name, "clang");
        // …but with no injector, L0 emitted nothing.
        assert!(plan.logical.definitions.is_empty());
    }

    #[test]
    fn org_config_is_inherited_and_the_project_overrides_it() {
        let body = r#"
            [org]
            name = "acme"
            [org.environment]
            SHARED_VAR = "from-org"
            UNTOUCHED_VAR = "org-only"
            OVERRIDDEN_VAR = "from-org"
            [org.definitions]
            ORG_LEVEL = 42
            ORG_FLAG = false

            [project]
            org = "acme"
            [project.environment]
            MY_ENV = "{{org.environment.SHARED_VAR}}"
            OVERRIDDEN_VAR = "from-project"
            [project.definitions]
            MY_DEF = "{{org.definitions.ORG_LEVEL}}"

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("acme/x").unwrap();
        let input = PlanInput {
            profile: &Profile::default(),
            branch: "main",
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = plan(&ws, project, &input).unwrap();
        // Every org entry is inherited, whether or not anything referenced it.
        assert_eq!(plan.logical.env_entry("UNTOUCHED_VAR"), Some("org-only"));
        assert_eq!(plan.logical.env_entry("SHARED_VAR"), Some("from-org"));
        assert!(plan.logical.has_definition("ORG_LEVEL"));
        assert!(plan.logical.has_definition("ORG_FLAG"));
        // The org.* namespace still resolves, so existing references keep working.
        assert_eq!(plan.logical.env_entry("MY_ENV"), Some("from-org"));
        assert!(plan.logical.has_definition("MY_DEF"));
        // The org is the lowest layer: declaring a key at project level wins.
        assert_eq!(
            plan.logical.env_entry("OVERRIDDEN_VAR"),
            Some("from-project")
        );
    }

    /// An org's definitions keep their TOML type through inheritance, so a
    /// backend can still spell a bool as a bool rather than the string "false".
    #[test]
    fn inherited_org_definitions_keep_their_type() {
        let body = r#"
            [org]
            name = "acme"
            [org.definitions]
            ORG_FLAG = false
            ORG_COUNT = 8

            [project]
            org = "acme"

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("acme/x").unwrap();
        let plan = plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "main"),
        )
        .unwrap();
        let def = |key: &str| {
            plan.logical
                .definitions
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert!(matches!(def("ORG_FLAG"), Value::Bool(false)));
        assert!(matches!(def("ORG_COUNT"), Value::Int(8)));
    }

    /// A project may name an org that no file declares. That is not fatal today
    /// (no validation rejects it), so inheritance must degrade to contributing
    /// nothing rather than panicking or erroring mid-plan.
    #[test]
    fn an_undeclared_org_contributes_nothing() {
        let body = r#"
            [project]
            org = "ghost"
            [project.definitions]
            OWN = true

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("ghost/x").unwrap();
        let plan = plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "main"),
        )
        .unwrap();
        assert!(plan.logical.has_definition("OWN"));
        assert_eq!(plan.logical.definitions.len(), 1);
    }

    /// The `org.*` namespace outlives inheritance: a preset that spells out
    /// `{{org.environment.X}}` binds it to a differently-named key, which is not
    /// something inheritance alone can express.
    #[test]
    fn org_namespace_referenceable_from_preset() {
        let body = r#"
            [org]
            name = "myorg"
            [org.environment]
            ORG_SETTING = "hello"

            [project]
            org = "myorg"
            default_presets = ["use-org"]

            [project.presets.use-org]
            environment = { FROM_ORG = "{{org.environment.ORG_SETTING}}" }

            [repos.main]
            path = "/src/y"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "y");
        let project = ws.project("myorg/y").unwrap();
        let input = PlanInput {
            profile: &Profile::default(),
            branch: "main",
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = plan(&ws, project, &input).unwrap();
        assert_eq!(plan.logical.env_entry("FROM_ORG"), Some("hello"));
        // …and the source entry is inherited under its own name as well.
        assert_eq!(plan.logical.env_entry("ORG_SETTING"), Some("hello"));
    }

    #[test]
    fn preset_applies_when_and_override() {
        let body = r#"
            [project]
            build_system = "cmake"
            default_presets = ["warn"]

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"

            [project.presets.warn]
            definitions = { WERROR = true }

            [project.presets.dbg]
            applies_when = { build_type = "debug" }
            definitions = { ASSERTS = true }
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("x").unwrap();
        let profile = Profile::default();
        let input = PlanInput {
            profile: &profile,
            branch: "main",
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = plan(&ws, project, &input).unwrap();
        assert!(plan.logical.has_definition("WERROR"));
        assert!(plan.logical.has_definition("ASSERTS")); // auto-applied for debug
    }

    /// `build_dir` / `install_dir` belong to the **build repo** (the focus's
    /// anchor), the same owner as `source_dir`. A focus that builds through
    /// another checkout uses that checkout's templates; a self-anchored focus
    /// uses its own. That is what lets two independently-built repos in one
    /// project keep separate output trees.
    #[test]
    fn build_and_install_dirs_come_from_the_build_repo() {
        let body = r#"
            [project]
            focus = "lib"

            [repos.main]
            path = "/src/root"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/root"
            install_dir = "{{repos.main.workdir}}/_install/root"

            [repos.lib]
            path = "/src/lib"
            main_branch = "main"
            anchor = "main"

            [repos.tool]
            path = "/src/tool"
            main_branch = "main"
            build_dir = "{{repos.tool.workdir}}/_build/tool"
            install_dir = "{{repos.tool.workdir}}/_install/tool"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        // Default focus is `lib`, which builds through `main`.
        let via_anchor = plan(
            &ws,
            project,
            &PlanInput::paths_only(&Profile::default(), "main"),
        )
        .unwrap();
        assert_eq!(via_anchor.build_repo, "main");
        assert_eq!(
            via_anchor.build_dir.unwrap(),
            PathBuf::from("/src/root/_build/root")
        );
        assert_eq!(
            via_anchor.install_dir.unwrap(),
            PathBuf::from("/src/root/_install/root")
        );

        let self_anchored = plan(
            &ws,
            project,
            &PlanInput::paths_only(
                &Profile {
                    focus: Some("tool".into()),
                    ..Default::default()
                },
                "main",
            ),
        )
        .unwrap();
        assert_eq!(self_anchored.build_repo, "tool");
        assert_eq!(
            self_anchored.build_dir.unwrap(),
            PathBuf::from("/src/tool/_build/tool")
        );
        assert_eq!(
            self_anchored.install_dir.unwrap(),
            PathBuf::from("/src/tool/_install/tool")
        );
    }
}
