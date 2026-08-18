//! `wits project` — the CLI shell over the read-only project core.
//!
//! Describes projects (the default), validates their configuration (`--check`),
//! and answers machine-readable status, path, or identity queries.
//! Everything about what a project *is* — the model, the workspace registry,
//! resolution, and the project-shaped git surface — lives in the read-only core
//! at [`wits_util::project`]; this module is one of its consumers, alongside the
//! separate `wits build` and `wits update` commands. See
//! `docs/project/design.md` §1.4.
//!
//! It deliberately owns **no** worktree management. That was once `project
//! context`, which created a branch's worktree and tore down its build dir;
//! worktrees are now [`wits worktree`](crate::cmd::worktree)'s, which does the
//! job for any repository rather than only a registered one. `project` and
//! worktrees meet at a path and nowhere else — see `build`'s `--work-dir`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::{Args, Subcommand, ValueEnum};

use anyhow::Context;

use wits_util::git;
use wits_util::project::model::{Kind, Profile};
use wits_util::project::skip;
use wits_util::project::workspace::{expand_tilde, looks_like_path, ProjectData, Workspace};
use wits_util::project::{resolve, resolve_target};

/// `wits project` — describe projects (the default), or answer one script query.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: Option<ProjectSub>,
    #[command(flatten)]
    pub info: InfoArgs,
    /// The profile axes (branch / build-type / toolchain / …) that shape
    /// resolution. Declared once here as **global** flags, so every `project`
    /// subcommand accepts them uniformly — the way `-v`/`-n` are inherited from
    /// the process layer (§1.3) — and so a machine-readable path query resolves
    /// the *same* dir a build would (the one shared `Profile`, §6.3). Being
    /// global, they are exempt from `args_conflicts_with_subcommands` and may be
    /// written on either side of the subcommand.
    #[command(flatten)]
    pub profile: ProfileArgs,
}

#[derive(Debug, Subcommand)]
pub enum ProjectSub {
    /// Exit successfully when a named project's main repository is cloned.
    Exists(ExistsArgs),
    /// Print the main branch of the repo you are in (or a named project) —
    /// the machine-readable answer scripts and git hooks need.
    MainBranch(TargetArgs),
    /// Print the resolved build directory for a branch, one line, for scripts.
    BuildDir(TargetArgs),
    /// Print the resolved install prefix for a branch, one line, for scripts.
    InstallDir(TargetArgs),
    /// Print the resolved source directory (where the build configures from).
    SourceDir(TargetArgs),
    /// Print the branch's checkout root (`repos.<name>.workdir`).
    WorkDir(TargetArgs),
    /// Print a repo's commit hash for a branch, optionally with its submodules'
    /// pinned hashes — read from the tree, so no checkout or branch switch.
    Hash(HashArgs),
}

#[derive(Debug, Args)]
pub struct ExistsArgs {
    /// Project name, either bare or fully qualified as `org/name`.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// A target anchored by name or path (default: the current dir). The branch and
/// the rest of the resolution profile arrive via the global [`ProfileArgs`] on
/// the parent, so every query shares one shape.
#[derive(Debug, Args)]
pub struct TargetArgs {
    /// Project name, or a path inside a checkout (default: the current dir).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
}

/// `hash`: a target (like every query) plus how far to descend into submodules.
/// The branch and focus arrive via the global [`ProfileArgs`]; `--submodules` is
/// `hash`-only, so it stays local rather than polluting every subcommand.
#[derive(Debug, Args)]
pub struct HashArgs {
    /// Project name, or a path inside a checkout (default: the current dir).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// How far to descend into submodules, reading each level's pinned commit
    /// from the tree (never a checkout or branch switch).
    #[arg(long, default_value = "none")]
    pub submodules: SubmoduleScope,
    /// Declared submodule repos whose **live HEAD** overrides the pinned gitlink
    /// in the output (and drives recursion): the components you are actually
    /// working on, which a build takes at their checked-out commit rather than
    /// the stale commit the superproject records. Repeatable and/or
    /// comma-separated. Each name must be a submodule repo of the project, and
    /// this needs `--submodules direct|recursive` (there is a walk to override).
    #[arg(long, value_delimiter = ',', value_name = "NAME")]
    pub repos: Vec<String>,
}

/// How far `hash` walks the submodule tree. This is really one axis — *depth* —
/// so it is stored as one (`levels`): `none` = 0, `direct` = 1, `recursive` =
/// unbounded. Modelling it as a depth means a future `--depth N` (should a real
/// need for an exact intermediate depth appear) slots in without a redesign;
/// until then only the three named modes are exposed, per "do less" (§1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SubmoduleScope {
    /// This repo only.
    #[default]
    None,
    /// This repo plus its direct submodules.
    Direct,
    /// This repo and its submodules, recursively as far as their objects are
    /// present (an un-fetched submodule bounds the walk — never a checkout).
    Recursive,
}

impl SubmoduleScope {
    /// Levels of submodules to print *below* the repo itself. `None` = unbounded.
    fn levels(self) -> Option<usize> {
        match self {
            SubmoduleScope::None => Some(0),
            SubmoduleScope::Direct => Some(1),
            SubmoduleScope::Recursive => None,
        }
    }
}

/// The profile axes shared by `project` (all subcommands, via global flags) and
/// `build`. Each field is `global` so it propagates to every `project`
/// subcommand; positionals cannot be global, which is why the `NAME|PATH`
/// target stays per-subcommand on [`TargetArgs`]/[`InfoArgs`].
#[derive(Debug, Args, Default)]
pub struct ProfileArgs {
    /// Target branch (the build identity). Default: the focus repo's current branch.
    #[arg(short = 'b', long, global = true)]
    pub branch: Option<String>,
    /// Build type — lowercase, meson-aligned (debug, release, …).
    #[arg(short = 'B', long = "build-type", global = true)]
    pub build_type: Option<String>,
    /// Select a declared toolchain.
    #[arg(short = 'T', long, global = true)]
    pub toolchain: Option<String>,
    /// Build-system generator (e.g. Ninja).
    #[arg(short = 'G', long, global = true)]
    pub generator: Option<String>,
    /// Apply a preset (repeatable; accepts org/preset).
    #[arg(short = 'p', long = "preset", global = true)]
    pub presets: Vec<String>,
    /// Override which repo is the focus.
    #[arg(long, global = true)]
    pub focus: Option<String>,
    /// Build the base from this checkout verbatim, bypassing the branch
    /// strategy's `worktree_dir`/in-place resolution — e.g. a `wits review
    /// checkout` worktree. Everything else (`build_dir`/`source_dir`/…) still
    /// anchors on it.
    #[arg(long = "work-dir", value_name = "DIR", global = true)]
    pub work_dir: Option<PathBuf>,
    /// Register a template variable, exposed as `{{spec.KEY}}` (repeatable;
    /// `KEY=VALUE`). A project template that references `{{spec.KEY}}` requires
    /// it — this is how an out-of-band value (an MR number, a variant tag)
    /// enters resolution without being baked into the project file.
    #[arg(long = "spec", value_name = "KEY=VALUE", global = true, value_parser = parse_spec)]
    pub specs: Vec<(String, String)>,
}

/// Parse a `--spec KEY=VALUE` pair. Split on the *first* `=` so a value may
/// itself contain `=`; the key must be non-empty. Validated at parse time so a
/// malformed pair errors on the command line, not mid-resolve.
fn parse_spec(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, _)) if k.trim().is_empty() => Err(format!("empty key in spec '{s}'")),
        Some((k, v)) => Ok((k.trim().to_owned(), v.to_owned())),
        None => Err(format!("expected KEY=VALUE, got '{s}'")),
    }
}

impl ProfileArgs {
    pub fn to_profile(&self) -> Profile {
        Profile {
            build_type: self.build_type.clone(),
            toolchain: self.toolchain.clone(),
            generator: self.generator.clone(),
            branch: self.branch.clone(),
            presets: self.presets.clone(),
            focus: self.focus.clone(),
            work_dir: self.work_dir.clone(),
            specs: self.specs.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Project name, or a path inside one (default: list every project).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// Validate configuration legality instead of describing.
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: &ProjectArgs) -> Result<()> {
    let ws = Workspace::load()?;
    // The profile axes live on the parent as global flags, so they are read here
    // once and handed to whichever subcommand ran — the value lands on the parent
    // regardless of which side of the subcommand it was written on.
    let profile = &args.profile;
    match &args.command {
        Some(ProjectSub::Exists(a)) => exists(&ws, a),
        Some(ProjectSub::MainBranch(a)) => main_branch(&ws, a, profile),
        Some(ProjectSub::BuildDir(a)) => build_dir(&ws, a, profile),
        Some(ProjectSub::InstallDir(a)) => install_dir(&ws, a, profile),
        Some(ProjectSub::SourceDir(a)) => source_dir(&ws, a, profile),
        Some(ProjectSub::WorkDir(a)) => work_dir(&ws, a, profile),
        Some(ProjectSub::Hash(a)) => hash(&ws, a, profile),
        None => info(&ws, &args.info, profile),
    }
}

// --- machine-readable queries (for scripts / git hooks) -----------------------

/// A quiet checkout query: the name must resolve uniquely, then `repos.main`
/// must be the root of either a working-tree checkout or a bare clone.
fn exists(ws: &Workspace, args: &ExistsArgs) -> Result<()> {
    let project = ws.project(&args.name)?;
    let path = resolve::repo_primary_path(ws, project, "main")
        .context("cannot resolve path of repos.main")?;
    let repo = git::Repository::new(&path);
    let root = repo
        .toplevel()
        .or_else(|| repo.is_bare().then(|| repo.git_common_dir()).flatten());
    if root.is_some_and(|root| canonical(&root) == canonical(&path)) {
        Ok(())
    } else {
        bail!(
            "project '{}' is registered but repos.main is not cloned at {}",
            project.key(),
            path.display()
        )
    }
}

/// Resolve a target to `(project, anchor-repo)`: a path (or the current dir)
/// resolves to the *containing* repo, a name to the project's focus repo.
fn resolve_repo<'a>(
    ws: &'a Workspace,
    target: Option<&str>,
    focus: Option<&str>,
) -> Result<(&'a ProjectData, String)> {
    match target {
        None => {
            let cwd = std::env::current_dir()?;
            ws.repo_for_path(&cwd).context(
                "not inside any known project; pass a name or run from inside a project's checkout",
            )
        }
        Some(t) if looks_like_path(t) => {
            let path = expand_tilde(t);
            ws.repo_for_path(&path)
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

/// The main branch that governs the anchored repo: its identity repo's
/// `main_branch` (a subtree inherits its anchor's). One line to stdout.
fn main_branch(ws: &Workspace, args: &TargetArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    let identity = resolve::identity_repo(project, &repo).unwrap_or(repo);
    let mb = project
        .repos
        .get(&identity)
        .and_then(|r| r.main_branch.clone())
        .with_context(|| {
            format!(
                "repo '{identity}' of project '{}' has no main_branch",
                project.key()
            )
        })?;
    println!("{mb}");
    Ok(())
}

/// Resolve a branch's build [`Plan`](resolve::Plan) for a path query, anchored
/// like [`resolve_repo`] with the branch defaulting to the anchored repo's
/// current one. Shared by the `*-dir` queries below, which differ only in the
/// resolved path they print.
fn resolve_plan<'a>(
    ws: &'a Workspace,
    args: &TargetArgs,
    profile: &ProfileArgs,
) -> Result<(&'a ProjectData, resolve::Plan)> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    // Carry the *whole* profile (build_type / toolchain / generator / presets),
    // not just focus+branch: a `build_dir`/`install_dir` template may embed any
    // of them (§6.2), so dropping them would print a dir that no build ever uses.
    let mut resolved = profile.to_profile();
    resolved.focus = Some(repo);
    resolved.branch = Some(branch.clone());
    let plan = resolve::plan(
        ws,
        project,
        &resolve::PlanInput::paths_only(&resolved, &branch),
    )?;
    Ok((project, plan))
}

/// The branch to resolve for: the explicit `--branch`, else the identity repo's
/// current branch. Shared by the path queries and `hash` so they default the same
/// way `build` does (§6.4).
fn branch_or_current(
    ws: &Workspace,
    project: &ProjectData,
    repo: &str,
    explicit: Option<&str>,
) -> Result<String> {
    if let Some(branch) = explicit {
        return Ok(branch.to_owned());
    }
    let identity = resolve::identity_repo(project, repo)
        .and_then(|name| resolve::repo_primary_path(ws, project, &name).ok())
        .map(git::Repository::new);
    if let (Some(configured), Ok(cwd)) = (&identity, std::env::current_dir()) {
        let here = git::Repository::new(cwd);
        let same_repo = configured
            .git_common_dir()
            .zip(here.git_common_dir())
            .is_some_and(|(a, b)| {
                let a = std::fs::canonicalize(&a).unwrap_or(a);
                let b = std::fs::canonicalize(&b).unwrap_or(b);
                a == b
            });
        if same_repo {
            return here
                .current_branch()
                .context("current worktree has a detached HEAD; pass --branch");
        }
    }
    identity
        .and_then(|git| git.current_branch())
        .context("could not determine a branch; pass --branch")
}

/// Generate a one-line path query over the resolved [`Plan`](resolve::Plan).
/// The queries differ only in which field they print and whether it is optional
/// (a declared template that may be absent) or always resolvable, so they are
/// one macro rather than four near-identical functions.
macro_rules! path_query {
    // An optional path: print it, or bail with why it isn't there.
    ($name:ident, $field:ident, optional: $absent:literal) => {
        fn $name(ws: &Workspace, args: &TargetArgs, profile: &ProfileArgs) -> Result<()> {
            let (project, plan) = resolve_plan(ws, args, profile)?;
            match plan.$field {
                Some(dir) => {
                    println!("{}", dir.display());
                    Ok(())
                }
                None => bail!("project '{}' {}", project.key(), $absent),
            }
        }
    };
    // An always-resolvable path.
    ($name:ident, $field:ident) => {
        fn $name(ws: &Workspace, args: &TargetArgs, profile: &ProfileArgs) -> Result<()> {
            let (_project, plan) = resolve_plan(ws, args, profile)?;
            println!("{}", plan.$field.display());
            Ok(())
        }
    };
}

// `build-dir`: where a checkout hook points `compile_commands.json`.
path_query!(build_dir, build_dir, optional: "has no build_dir template to resolve");
// `install-dir`: the resolved install prefix.
path_query!(install_dir, install_dir, optional: "has no install_dir configured");
// `source-dir`: where the backend configures from (defaults to the build repo's
// namespaced `workdir`).
path_query!(source_dir, source_dir);
// `work-dir`: the branch's checkout root, the selected repo's `workdir`.
path_query!(work_dir, work_dir);

/// `hash`: the commit a branch points at in the anchored repo, and — per
/// `--submodules` — the commits it pins in its submodules. Everything is read
/// from tree objects (`rev-parse`/`ls-tree`), so it answers for any `--branch`
/// without touching the working tree. Output: the repo's own line is its full
/// sha and its **absolute path**; each submodule line is its sha and a path
/// **relative to that repo**, one `<sha>\t<path>` per line for scripts.
/// Submodules that aren't checked out (sparse-omitted or uninitialised) are
/// skipped — see [`walk_submodules`].
///
/// `--repos` overlays *live* HEADs onto the otherwise-pinned manifest: a
/// superproject records a submodule at whatever commit was last committed, but a
/// component you are actively working on sits at a different commit in your
/// checkout — the one a build uses. Naming it in `--repos` prints (and recurses
/// from) that live commit instead of the stale pin.
fn hash(ws: &Workspace, args: &HashArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    // Hash the identity repo: a subtree has no own git and borrows its anchor's.
    let identity = resolve::identity_repo(project, &repo).with_context(|| {
        format!(
            "repo '{repo}' of project '{}' has no own git to hash",
            project.key()
        )
    })?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    let path = resolve::repo_primary_path(ws, project, &identity)
        .with_context(|| format!("cannot resolve path of repo '{identity}'"))?;
    let git = git::Repository::new(&path);
    let sha = git
        .rev_parse(&branch)
        .with_context(|| format!("branch '{branch}' does not exist in repo '{identity}'"))?;

    // Resolve `--repos` before the scope check so a bad name errors either way.
    let overrides = live_overrides(ws, project, &args.repos)?;
    if !overrides.is_empty() && args.submodules == SubmoduleScope::None {
        bail!(
            "--repos needs --submodules direct|recursive — there is no submodule walk to override"
        );
    }

    if args.submodules == SubmoduleScope::None {
        println!("{sha}");
        return Ok(());
    }
    // The repo identifies itself by its absolute path; submodules hang off it by
    // relative path.
    println!("{sha}\t{}", path.display());
    walk_submodules(&git, &sha, "", args.submodules.levels(), &overrides);
    Ok(())
}

/// Resolve `--repos` names to `canonical-abs-path -> live HEAD`, the map
/// [`walk_submodules`] consults to swap a pinned gitlink for the commit the
/// component is actually on. Each name must be a declared **submodule** repo:
/// only a submodule has a pinned gitlink to override (a standalone sibling is
/// not in any superproject's tree; a subtree has no own git). The live HEAD is
/// read from its checkout — an error if it isn't checked out, since there is
/// then no live commit to stand in for the pin.
fn live_overrides(
    ws: &Workspace,
    project: &ProjectData,
    names: &[String],
) -> Result<HashMap<PathBuf, String>> {
    let mut map = HashMap::new();
    for name in names {
        match project.kind_of(name) {
            None => bail!("--repos '{name}': no such repo in project '{}'", project.key()),
            Some(Kind::Submodule) => {}
            Some(k) => bail!(
                "--repos '{name}': a {} repo has no pinned gitlink to override (only submodules do)",
                k.as_str()
            ),
        }
        let path = resolve::repo_primary_path(ws, project, name)
            .with_context(|| format!("cannot resolve path of repo '{name}'"))?;
        let head = git::Repository::new(&path)
            .rev_parse("HEAD")
            .with_context(|| format!("repo '{name}' has no HEAD to read (is it checked out?)"))?;
        map.insert(canonical(&path), head);
    }
    Ok(map)
}

/// Best-effort canonical path for keying/looking up overrides: both the map keys
/// and the walk's on-disk paths pass through this, so `..`/symlink spellings
/// still compare equal. Falls back to the path as-is when it can't be resolved.
fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Print a repo's submodule gitlinks at `rev`, then descend into each while
/// `levels` allows (`Some(0)` stops; `None` is unbounded). `prefix` accumulates
/// the path relative to the top repo, so a nested submodule reads as
/// `outer/inner`.
///
/// Only submodules that are actually **checked out** are reported: a sparse
/// checkout omits everything outside its cone, and a fresh clone leaves
/// submodules uninitialised, and in both cases the working tree isn't there.
/// `hash` describes the checkout that exists, not the full manifest the tree
/// records — so an un-checked-out submodule is skipped even though we could read
/// its pinned sha. A checked-out submodule has a `.git` (a gitlink file, or a
/// dir on older git); its absence is the reliable "not materialised" signal, and
/// it also bounds the recursion (there is nothing to descend into).
///
/// `overrides` maps a submodule's checkout path to its live HEAD (from
/// `--repos`): when a gitlink's path is in it, that live commit is printed and
/// recursed from instead of the pinned one, so a component you are working on
/// shows its actual state while everything else stays the recorded manifest.
fn walk_submodules(
    repo: &git::Repository,
    rev: &str,
    prefix: &str,
    levels: Option<usize>,
    overrides: &HashMap<PathBuf, String>,
) {
    if levels == Some(0) {
        return;
    }
    for (sub_sha, sub_path) in repo.gitlinks(rev) {
        let work = repo.path().join(&sub_path);
        if !work.join(".git").exists() {
            continue;
        }
        let rel = if prefix.is_empty() {
            sub_path.clone()
        } else {
            format!("{prefix}/{sub_path}")
        };
        // A named component shows the commit its checkout is on, not the pin.
        let effective = overrides.get(&canonical(&work)).cloned().unwrap_or(sub_sha);
        println!("{effective}\t{rel}");
        walk_submodules(
            &git::Repository::new(work),
            &effective,
            &rel,
            levels.map(|n| n - 1),
            overrides,
        );
    }
}

// --- info ---------------------------------------------------------------------

fn info(ws: &Workspace, args: &InfoArgs, profile: &ProfileArgs) -> Result<()> {
    if args.check {
        return check(ws, args.target.as_deref());
    }
    match &args.target {
        None => {
            for project in ws.projects() {
                println!("{}", summary_line(project));
            }
            Ok(())
        }
        Some(_) => {
            let project = resolve_target(ws, args.target.as_deref())?;
            describe(ws, project, profile)
        }
    }
}

fn summary_line(project: &ProjectData) -> String {
    let bs = project
        .project
        .build_system
        .map(|b| b.as_str())
        .unwrap_or("-");
    let focus = project.focus_name(None);
    format!("{:<24} focus={:<8} build={}", project.key(), focus, bs)
}

fn describe(ws: &Workspace, project: &ProjectData, profile: &ProfileArgs) -> Result<()> {
    println!("project: {}", project.key());
    println!("  source: {}", project.source.display());
    if let Some(org) = &project.org {
        println!("  org:   {org}");
    }
    println!("  focus: {}", project.focus_name(profile.focus.as_deref()));
    if let Some(bs) = project.project.build_system {
        println!("  build: {}", bs.as_str());
    }
    if let Some(tc) = &project.project.toolchain {
        println!("  toolchain: {tc}");
    }

    println!("  repos:");
    for (name, repo) in &project.repos {
        let kind = project.kind_of(name).map(|k| k.as_str()).unwrap_or("?");
        // A path template that fails to resolve is a real config error; surface
        // it inline rather than letting `unwrap_or_default()` render an empty
        // path that then masquerades as a plain "<not cloned>" repo.
        let path = match resolve::repo_primary_path(ws, project, name) {
            Ok(path) => path,
            Err(e) => {
                println!("    {name:<10} {kind:<10} <path error: {e}>");
                continue;
            }
        };
        let git = git::Repository::new(&path);
        let state = if git.git_dir().is_some() {
            let branch = git.current_branch().unwrap_or_else(|| "-".into());
            let commit = git.head_commit().unwrap_or_else(|| "-".into());
            if git.is_bare() {
                format!("bare ({branch} @ {commit})")
            } else {
                format!("{branch} @ {commit}")
            }
        } else {
            "<not cloned>".into()
        };
        println!("    {name:<10} {kind:<10} {state:<24} {}", path.display());
        // Where a repo's identity came from, and what its checkout leaves out —
        // both invisible in the path alone, and both change what you are looking
        // at (a borrowed repo is someone else's to update).
        if let Some(from) = &repo.from {
            println!("      borrowed from {from}");
        }
        if !repo.skip.is_empty() {
            println!("      skip     {}", repo.skip.join(" "));
        }
        for wt in git.worktrees() {
            if wt.path != path {
                let b = wt.branch.as_deref().unwrap_or("-");
                println!("      worktree {b:<16} {}", wt.path.display());
            }
        }
    }

    // Resolved paths when a profile is supplied (or a current branch is known);
    // otherwise show the raw templates, since resolution needs a branch.
    let focus = project.focus_name(profile.focus.as_deref());
    let branch = branch_or_current(ws, project, focus, profile.branch.as_deref()).ok();
    match branch {
        Some(branch) => {
            let plan = resolve::plan(
                ws,
                project,
                &resolve::PlanInput::paths_only(&profile.to_profile(), &branch),
            )?;
            println!(
                "  resolved (branch {}, {}):",
                plan.branch_slug, plan.build_type
            );
            println!("    focus:       {}", plan.focus);
            if let Some(tc) = &plan.toolchain {
                println!("    toolchain:   {}", tc.name);
            }
            println!(
                "    repos.{}.workdir: {}",
                plan.build_repo,
                plan.work_dir.display()
            );
            if plan.source_dir != plan.work_dir {
                println!("    source_dir:  {}", plan.source_dir.display());
            }
            if let Some(b) = &plan.build_dir {
                println!("    build_dir:   {}", b.display());
            }
            if let Some(i) = &plan.install_dir {
                println!("    install_dir: {}", i.display());
            }
        }
        _ => {
            let build_repo =
                resolve::anchor_of(project, project.focus_name(profile.focus.as_deref()));
            if let Some(t) = project
                .repos
                .get(&build_repo)
                .and_then(|r| r.source_dir.as_ref())
            {
                println!("  source_dir (template):  {t}");
            }
            if let Some(t) = &project.project.build_dir {
                println!("  build_dir (template):   {t}");
            }
            if let Some(t) = &project.project.install_dir {
                println!("  install_dir (template): {t}");
            }
        }
    }
    Ok(())
}

// --- info --check -------------------------------------------------------------

fn check(ws: &Workspace, target: Option<&str>) -> Result<()> {
    let projects: Vec<&ProjectData> = match target {
        Some(_) => vec![resolve_target(ws, target)?],
        None => ws.projects().collect(),
    };
    let mut problems = Vec::new();
    for project in projects {
        for issue in check_one(ws, project) {
            problems.push(format!("[{}] {issue}", project.key()));
        }
    }
    if problems.is_empty() {
        println!("ok");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("{p}");
        }
        bail!("{} configuration problem(s)", problems.len())
    }
}

fn check_one(ws: &Workspace, project: &ProjectData) -> Vec<String> {
    let mut issues = Vec::new();

    for (name, repo) in &project.repos {
        if project.kind_of(name).is_some_and(|k| k.has_own_git()) && repo.main_branch.is_none() {
            issues.push(format!("repo '{name}' has its own git but no main_branch"));
        }
        // A declared `skip` that is not in force is the one config fact whose
        // truth lives on disk rather than in the file, so it is checked here
        // rather than validated at load. Only a cloned checkout can answer.
        if !repo.skip.is_empty() {
            if let Ok(path) = resolve::repo_primary_path(ws, project, name) {
                let git = git::Repository::new(&path);
                let checkout = if git.is_bare() {
                    repo.main_branch
                        .as_deref()
                        .and_then(|branch| {
                            resolve::worktree_for_branch(ws, project, name, branch)
                                .ok()
                                .flatten()
                        })
                        .map(git::Repository::new)
                } else if git.is_repo() {
                    Some(git)
                } else {
                    None
                };
                if let Some(checkout) = checkout {
                    for v in skip::violations(&checkout, &repo.skip) {
                        issues.push(format!("repo '{name}': {v}"));
                    }
                    if wits_util::log::is_verbose() {
                        for cmd in skip::remedy(&checkout, &repo.skip) {
                            log::debug!(
                                "repo '{name}' fix: (cd {} && {cmd})",
                                checkout.path().display()
                            );
                        }
                    }
                }
            }
        }
    }

    let p = &project.project;
    if p.build_dir.is_some() && p.build_system.is_none() {
        issues.push("build_dir is set but build_system is not".into());
    }
    // Whether a declared `build_system` actually has a backend is `wits build`'s
    // concern (it errors at run time); the core neither knows nor validates the
    // set of supported build systems (§1.4). Here we only cross-check the
    // *declared* facts: a toolchain's own `supports` list against `build_system`.
    if let Some(bs) = p.build_system {
        if let Some(tc) = &p.toolchain {
            if let Some(def) = ws.toolchains().get(tc) {
                if !def.supports.is_empty() && !def.supports.iter().any(|s| s == bs.as_str()) {
                    issues.push(format!(
                        "toolchain '{tc}' does not support '{}'",
                        bs.as_str()
                    ));
                }
            }
        }
    }
    if let Some(tc) = &p.toolchain {
        if !ws.toolchains().contains_key(tc) {
            issues.push(format!("unknown toolchain '{tc}'"));
        }
    }

    // A dry resolve catches template errors, preset cycles, unknown presets.
    // Validation has no backend, so this is a path-only resolve — toolchain
    // selection runs (and can fail), but there is nothing to inject.
    let profile = Profile {
        toolchain: p.toolchain.clone(),
        ..Default::default()
    };
    if let Err(e) = resolve::plan(
        ws,
        project,
        &resolve::PlanInput::paths_only(&profile, "main"),
    ) {
        issues.push(format!("resolution: {e:#}"));
    }
    issues
}
