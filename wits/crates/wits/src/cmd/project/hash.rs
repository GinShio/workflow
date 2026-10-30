//! `project hash` — a repo's commit hash for a branch, optionally with its
//! submodules' pinned hashes.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};

use wits_core::git;
use wits_core::path::real;
use wits_project::{identity_repo, layout, Kind, Project, Workspace};

use super::common::{branch_or_current, resolve_repo};
use crate::cmd::identity::ProfileArgs;

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
pub(super) fn run(ws: &Workspace, args: &HashArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    // Hash the identity repo: a subtree has no own git and borrows its anchor's.
    let identity = identity_repo(project, &repo).with_context(|| {
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
    let repository = layout::primary_path(ws, project, &identity)
        .with_context(|| format!("cannot resolve path of repo '{identity}'"))?;
    let sha = git::Repository::new(&repository)
        .rev_parse(&branch)
        .with_context(|| format!("branch '{branch}' does not exist in repo '{identity}'"))?;
    let path = layout::work_dir(ws, project, &identity, &branch)
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
    project: &Project,
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
        let path = layout::primary_path(ws, project, name)
            .with_context(|| format!("cannot resolve path of repo '{name}'"))?;
        let head = git::Repository::new(&path)
            .rev_parse("HEAD")
            .with_context(|| format!("repo '{name}' has no HEAD to read (is it checked out?)"))?;
        map.insert(real(&path), head);
    }
    Ok(map)
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
        let effective = overrides.get(&real(&work)).cloned().unwrap_or(sub_sha);
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
