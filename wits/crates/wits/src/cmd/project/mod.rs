//! `wits project` — the CLI shell over the read-only project core.
//!
//! Describes projects (the default), validates their configuration (`--check`),
//! and answers machine-readable status, path, or identity queries.
//! Everything about what a project *is* — the model, the workspace registry,
//! resolution, and the project-shaped git surface — lives in the read-only core
//! at [`wits_util::project`]; this module is one of its consumers, alongside the
//! separate `wits build` and `wits update` commands. See
//! `docs/reference/project-design.rst`, "Library shape — core plus actions";
//! quoted section names below are from the same document.
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
use wits_util::project::context::value_to_string;
use wits_util::project::model::{Kind, Profile};
use wits_util::project::skip;
use wits_util::project::workspace::{expand_tilde, looks_like_path, ProjectData, Workspace};
use wits_util::project::{resolve, resolve_target};

/// `wits project` — the read-only half of the tool: one verb per shape of
/// question it answers.
#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectSub,
    /// The profile axes (branch / build-type / toolchain / …) that shape
    /// resolution. Declared once here as **global** flags, so every `project`
    /// subcommand accepts them uniformly — the way `-v`/`-n` are inherited from
    /// the process layer ("CLI surface") — and so a machine-readable path query
    /// resolves the *same* dir a build would (the one shared `Profile`, "Profile
    /// vs BuildOptions"). Being global, they may be written on either side of
    /// the subcommand.
    #[command(flatten)]
    pub profile: ProfileArgs,
}

/// The verbs, and why each is its own rather than a key of `info --get`.
///
/// `info` projects one *value* out of a resolved plan, which is a shape three of
/// these do not have: `exists` answers with an exit status and must work on a
/// project that is not cloned (so it never resolves a plan at all);
/// `branch-build-dirs` is a search across the registry returning 0..N rows
/// spanning projects, not a field of this project; `hash` carries its own flags
/// and walks git objects rather than resolving config. Folding any of them into
/// `--get` would cost it the single-line-scalar contract that makes `--get`
/// repeatable and safe to read back positionally.
#[derive(Debug, Subcommand)]
pub enum ProjectSub {
    /// Summarise every registered project, one line each.
    List,
    /// Describe one project in full — or, with `--get`, print one resolved value.
    Info(InfoArgs),
    /// Validate configuration legality (no target: every project, for CI).
    Check(TargetArgs),
    /// Exit successfully when a named project's main repository is cloned.
    Exists(ExistsArgs),
    /// Print every existing build directory that a branch of one checkout
    /// identifies, across all projects — what a branch deletion orphans.
    BranchBuildDirs(TargetArgs),
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
/// until then only the three named modes are exposed, per "do less" ("We are a
/// mechanism, not a policy engine").
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
    /// Project name, or a path inside one (default: the project owning the
    /// current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// Print one resolved value instead of the description: a dotted path into
    /// the same template context the config files are written against, so
    /// `build_dir`, `repos.main.workdir`, `toolchain.linker` and
    /// `repo.main_branch` are all spelled here exactly as a template spells
    /// them. Repeatable — each path prints one line, in the order given, so a
    /// script reads several answers out of one resolve instead of paying for a
    /// registry load per question.
    ///
    /// The value must be a scalar: a table or a list is not one line, and
    /// rendering it as several would break reading the lines back positionally.
    /// Use the description for those.
    #[arg(long = "get", value_name = "PATH")]
    pub get: Vec<String>,
}

pub fn run(args: &ProjectArgs) -> Result<()> {
    let ws = Workspace::load()?;
    // The profile axes live on the parent as global flags, so they are read here
    // once and handed to whichever subcommand ran — the value lands on the parent
    // regardless of which side of the subcommand it was written on.
    let profile = &args.profile;
    match &args.command {
        ProjectSub::List => list(&ws),
        ProjectSub::Info(a) => info(&ws, a, profile),
        ProjectSub::Check(a) => check(&ws, a.target.as_deref()),
        ProjectSub::Exists(a) => exists(&ws, a),
        ProjectSub::BranchBuildDirs(a) => branch_build_dirs(&ws, a, profile),
        ProjectSub::Hash(a) => hash(&ws, a, profile),
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

/// Resolve a branch's build [`Plan`](resolve::Plan) for a query, anchored like
/// [`resolve_repo`] with the branch defaulting to the anchored repo's current
/// one. Shared by `info` and `--get`, so a description and a projection out of
/// it can never disagree.
fn resolve_plan<'a>(
    ws: &'a Workspace,
    target: Option<&str>,
    profile: &ProfileArgs,
) -> Result<(&'a ProjectData, resolve::Plan)> {
    let (project, repo) = resolve_repo(ws, target, profile.focus.as_deref())?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    // Carry the *whole* profile (build_type / toolchain / generator / presets),
    // not just focus+branch: a `build_dir`/`install_dir` template may embed any
    // of them ("Context variables"), so dropping them would print a dir that no
    // build ever uses.
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
/// way `build` does ("Branch identity") — through the one
/// [`resolve::current_branch`], so a query and the build it describes can never
/// disagree about which branch is meant.
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
        .context("no own-git repo to take a branch from; pass --branch")?;
    resolve::current_branch(ws, project, &identity)?
        .context("could not determine a branch; pass --branch")
}

/// `info --get <path>…`: one resolved value per path, one line each, in the
/// order given.
///
/// The path is a dotted lookup into the plan's own template context — the same
/// namespace the config files are written against — so there is no second
/// vocabulary to define, document, or keep from drifting: `build_dir`,
/// `repos.main.workdir` and `repo.main_branch` mean here exactly what they mean
/// in a `[repos.*]` table. That also makes every value a *resolved* one, since
/// the context resolves a binding that is itself a template on the way out.
///
/// Two rules, both in service of the single-line contract that lets a caller
/// read N lines back positionally:
///
///   - a non-scalar is refused rather than flattened. A table or a list is not
///     one line, and joining one would hand back a string that no longer
///     round-trips to the value it came from;
///   - one failing path fails the whole invocation. Printing a blank line for it
///     would keep the *positions* right while leaving the caller unable to tell
///     an empty value from an absent one, which is worse than not answering.
fn get_paths(ws: &Workspace, args: &InfoArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, plan) = resolve_plan(ws, args.target.as_deref(), profile)?;
    // Resolve every path before printing any: a later failure must not leave a
    // caller holding a partial, positionally-misread answer on stdout.
    let mut values = Vec::with_capacity(args.get.len());
    for path in &args.get {
        let value = plan
            .ctx
            .get_scalar(path)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("project '{}': --get {path}", project.key()))?;
        values.push(value);
    }
    for value in values {
        println!("{value}");
    }
    Ok(())
}

/// `branch-build-dirs`: every existing build directory that `--branch` of the
/// anchored checkout identifies, one `<project>\t<path>` per line.
///
/// The cross-project shape is the point. A branch deletion has to reach the build
/// trees of every project that keys its build on that checkout's branch, and a
/// borrowed component is keyed on by more than one — so a per-project query
/// silently leaves the borrowers' trees behind. The judgement about *which*
/// projects qualify is [`resolve::branch_build_dirs`]'s, derived from the
/// registry rather than declared in it.
///
/// This stays a query: it prints what a deletion would orphan and removes
/// nothing, because `project` is the read-only half of the tool ("The read/act
/// split") and the caller that deletes wants its own confirmation and dry-run
/// anyway.
fn branch_build_dirs(ws: &Workspace, args: &TargetArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    let branch = branch_or_current(ws, project, &repo, profile.branch.as_deref())?;
    // Ask by the branch's *checkout*, not the repository. Two repos of a project
    // may share one git dir and be told apart only by which checkout they place
    // (a bare-backed component and a review checkout of it), so collapsing to the
    // repository path would discard the one thing that distinguishes them.
    let path = resolve::work_dir(ws, project, &repo, &branch)
        .with_context(|| format!("cannot resolve the checkout of repo '{repo}'"))?;
    for found in resolve::branch_build_dirs(ws, &path, &branch) {
        println!("{}\t{}", found.project, found.path.display());
    }
    Ok(())
}

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
    // The sha comes off a *ref*, so the repository answers it for any `--branch`
    // without a checkout. The walk below reads working trees (a gitlink is only
    // reported where it is materialised), so it runs in that branch's checkout —
    // which for a bare-backed repo is a worktree, and never the git-dir, where
    // nothing would ever be found.
    let repository = resolve::repo_primary_path(ws, project, &identity)
        .with_context(|| format!("cannot resolve path of repo '{identity}'"))?;
    let sha = git::Repository::new(&repository)
        .rev_parse(&branch)
        .with_context(|| format!("branch '{branch}' does not exist in repo '{identity}'"))?;
    let path = resolve::work_dir(ws, project, &identity, &branch)
        .with_context(|| format!("cannot resolve the checkout of repo '{identity}'"))?;
    let git = git::Repository::new(&path);

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

/// `list`: one summary line per registered project. Its own verb rather than
/// `info` with no target, so `info` describes exactly one project on every path
/// and its omitted positional means what it means for `build`/`update` — the
/// project owning the current directory.
fn list(ws: &Workspace) -> Result<()> {
    for project in ws.projects() {
        println!("{}", summary_line(project));
    }
    Ok(())
}

fn info(ws: &Workspace, args: &InfoArgs, profile: &ProfileArgs) -> Result<()> {
    if !args.get.is_empty() {
        return get_paths(ws, args, profile);
    }
    let project = resolve_target(ws, args.target.as_deref())?;
    describe(ws, project, profile)
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

/// `info`: everything known about one project, in four sections — what it is,
/// its repos and their git state, the path *templates* it declares, and the
/// resolution of those templates for one branch.
///
/// Templates and their resolution are both shown, rather than one or the other.
/// They answer different questions ("what did I write" versus "what does it come
/// out as"), and a mismatch between them is exactly the bug this output exists to
/// make visible; showing only the resolved form hides the declaration that
/// produced it, and only the template hides everything the profile contributes.
/// Resolution still needs a branch, so that section is what degrades when none
/// can be discovered — the templates are always printed.
fn describe(ws: &Workspace, project: &ProjectData, profile: &ProfileArgs) -> Result<()> {
    println!("project: {}", project.key());
    println!("  source:       {}", project.source.display());
    if let Some(org) = &project.org {
        println!("  org:          {org}");
    }
    println!(
        "  focus:        {}",
        project.focus_name(profile.focus.as_deref())
    );
    if let Some(bs) = project.project.build_system {
        println!("  build_system: {}", bs.as_str());
    }
    if let Some(gen) = &project.project.generator {
        println!("  generator:    {gen}");
    }
    if let Some(tc) = &project.project.toolchain {
        println!("  toolchain:    {tc}");
    }
    if !project.project.default_presets.is_empty() {
        println!(
            "  default_presets: {}",
            project.project.default_presets.join(" ")
        );
    }

    println!();
    println!("repos:");
    for (name, repo) in &project.repos {
        let kind = project.kind_of(name).map(|k| k.as_str()).unwrap_or("?");
        // A path template that fails to resolve is a real config error; surface
        // it inline rather than letting `unwrap_or_default()` render an empty
        // path that then masquerades as a plain "<not cloned>" repo.
        let path = match resolve::repo_primary_path(ws, project, name) {
            Ok(path) => path,
            Err(e) => {
                println!("  {name:<12} {kind:<10} <path error: {e}>");
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
        println!("  {name:<12} {kind:<10} {state:<24} {}", path.display());
        // Where a repo's identity came from, and what its checkout leaves out —
        // both invisible in the path alone, and both change what you are looking
        // at (a borrowed repo is someone else's to update).
        if let Some(from) = &repo.from {
            println!("      borrowed from {from}");
        }
        if let Some(anchor) = &repo.anchor {
            println!("      anchor        {anchor}");
        }
        if let Some(mb) = &repo.main_branch {
            println!("      main_branch   {mb}");
        }
        if let Some(strategy) = &repo.branch_strategy {
            println!("      strategy      {strategy}");
        }
        // Remotes decide where `update` fetches from and where a stack pushes, and
        // a role held by a free name is invisible in the URL alone.
        for (remote, spec) in &repo.remotes {
            let role = spec.role.map(|r| r.as_str()).unwrap_or("");
            let role = if role.is_empty() {
                String::new()
            } else {
                format!("  [{role}]")
            };
            println!("      remote        {remote:<10} {}{role}", spec.url);
            for mirror in &spec.mirrors {
                println!("        mirror      {mirror}");
            }
        }
        if !repo.skip.is_empty() {
            println!("      skip          {}", repo.skip.join(" "));
        }
        for wt in git.worktrees() {
            if wt.path != path {
                let b = wt.branch.as_deref().unwrap_or("-");
                println!("      worktree      {b:<16} {}", wt.path.display());
            }
        }
    }

    // The declared path templates, always — resolution may be impossible (no
    // branch), but what the file says never is. Read with the same
    // focus-over-anchor precedence `resolve` applies, so what is printed is the
    // template that would actually be used and not merely the one nearest to hand.
    let focus = project.focus_name(profile.focus.as_deref());
    let build_repo = resolve::anchor_of(project, focus);
    let anchor = &project.repos[&build_repo];
    let focused = &project.repos[focus];
    let templates = [
        ("source_dir", anchor.source_dir.as_ref()),
        (
            "build_dir",
            focused.build_dir.as_ref().or(anchor.build_dir.as_ref()),
        ),
        (
            "install_dir",
            focused.install_dir.as_ref().or(anchor.install_dir.as_ref()),
        ),
    ];
    if templates.iter().any(|(_, t)| t.is_some()) {
        println!();
        println!("templates:");
        for (name, template) in templates {
            if let Some(template) = template {
                println!("  {name:<12} {template}");
            }
        }
    }

    // Resolution needs a branch, so this is the section that degrades: an
    // unresolvable branch says so rather than silently dropping the section,
    // since "no resolved paths" and "this project has none" are different facts.
    let branch = branch_or_current(ws, project, focus, profile.branch.as_deref()).ok();
    println!();
    let Some(branch) = branch else {
        println!("resolved: <no branch discovered — pass --branch to resolve>");
        return Ok(());
    };
    let plan = resolve::plan(
        ws,
        project,
        &resolve::PlanInput::paths_only(&profile.to_profile(), &branch),
    )?;
    let identity = plan
        .branch
        .as_ref()
        .context("branch path query resolved as detached")?;
    println!(
        "resolved (branch {}, build_type {}):",
        identity.raw, plan.build_type
    );
    // focus / build_repo / identity_repo are three roles one repo usually fills
    // at once, which is exactly why they are printed separately: when they differ
    // (a borrowed focus, an anchored subtree) every path below follows a
    // different one of them.
    println!("  focus:         {}", plan.focus);
    println!("  build_repo:    {}", plan.build_repo);
    println!(
        "  identity_repo: {}",
        plan.identity_repo.as_deref().unwrap_or("-")
    );
    println!("  strategy:      {}", plan.strategy.as_str());
    println!("  branch.slug:   {}", identity.slug);
    if let Some(bs) = plan.build_system {
        println!("  build_system:  {}", bs.as_str());
    }
    if let Some(gen) = &plan.generator {
        println!("  generator:     {gen}");
    }
    if let Some(tc) = &plan.toolchain {
        println!("  toolchain:     {}", tc.name);
    }
    if !plan.presets.is_empty() {
        println!("  presets:       {}", plan.presets.join(" "));
    }
    println!("  source_dir:    {}", plan.source_dir.display());
    if let Some(dir) = &plan.build_dir {
        println!("  build_dir:     {}", dir.display());
    }
    if let Some(dir) = &plan.install_dir {
        println!("  install_dir:   {}", dir.display());
    }
    for (name, dir) in &plan.work_dirs {
        println!("  repos.{name}.workdir: {}", dir.display());
    }

    // The accumulated pipeline result. A path-only resolve skips L0 (there is no
    // backend to translate a toolchain for), so what shows here is the org,
    // project, and preset layers — which is what answers "where did this
    // definition come from", read together with `presets` above.
    let logical = &plan.logical;
    if !logical.definitions.is_empty() {
        println!();
        println!("definitions:");
        for (key, value) in &logical.definitions {
            // The backend's own spelling, so what is reported is what the build
            // receives — not minijinja's Jinja/Python rendering, in which a
            // boolean reads `True`.
            println!("  {key} = {}", value_to_string(value));
        }
    }
    if !logical.environment.is_empty() {
        println!();
        println!("environment:");
        for (key, value) in &logical.environment {
            println!("  {key} = {value}");
        }
    }
    for (label, args) in [
        ("extra_config_args", &logical.extra_config_args),
        ("extra_build_args", &logical.extra_build_args),
        ("extra_install_args", &logical.extra_install_args),
    ] {
        if !args.is_empty() {
            println!();
            println!("{label}:");
            for arg in args {
                println!("  {arg}");
            }
        }
    }
    Ok(())
}

// --- check --------------------------------------------------------------------

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
        // A `developed_as` that resolves to nothing is silent everywhere else:
        // path resolution keeps the structural answer rather than failing, so
        // that one registry file deployed without the project it names cannot
        // break every query. Judged here instead, where the whole registry is in
        // hand and the verdict is the caller's to act on.
        if let Some(spec) = &repo.developed_as {
            if ws.project(spec).is_err() {
                issues.push(format!(
                    "repo '{name}': developed_as '{spec}' names no known project"
                ));
            } else if ws.developed_as(project, name).is_none() {
                issues.push(format!(
                    "repo '{name}': developed_as '{spec}' does not borrow this repo"
                ));
            }
        }
        // A declared `skip` that is not in force is the one config fact whose
        // truth lives on disk rather than in the file, so it is checked here
        // rather than validated at load. Only a cloned checkout can answer.
        // Judged in the repo's own checkout, whatever its shape — the same one
        // `update` verifies, so a `--check` that passes is a promise `update` will
        // not then contradict. Which checkout that is belongs to
        // `resolve::primary_checkout`, not to a strategy test spelled out here.
        if !repo.skip.is_empty() {
            if let Some(checkout) = resolve::primary_checkout(ws, project, name)
                .ok()
                .flatten()
                .map(git::Repository::new)
            {
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

    let p = &project.project;
    let has_build_dir = project.repos.values().any(|r| r.build_dir.is_some());
    if has_build_dir && p.build_system.is_none() {
        issues.push("build_dir is set but build_system is not".into());
    }
    // Whether a declared `build_system` actually has a backend is `wits build`'s
    // concern (it errors at run time); the core neither knows nor validates the
    // set of supported build systems ("The backend abstraction — the only
    // extension axis"). Here we only cross-check the *declared* facts: a
    // toolchain's own `supports` list against `build_system`.
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
