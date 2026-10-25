//! Where a repo's git actually runs: the checkout-selection layer.
//!
//! A repo has two locations, and confusing them is the single mistake this
//! module exists to make impossible. Its **`path`** is the repository — a
//! git-dir, and for a bare-backed repo a git-dir with no working tree at all.
//! Its **`workdir`** is a checkout. Anything that touches a working tree (a
//! branch switch, a merge, a submodule update, a sparse mask, "which branch am I
//! on") has to run in the latter, and the functions below are the only
//! sanctioned ways to name one.
//!
//! For an in-place repo the two coincide, which is why the distinction gets
//! lost — and a project may freely mix the strategies, building an in-place
//! checkout of its own while taking branch identity from a bare-backed component
//! it borrows.
//!
//! Worktree templates render in the namespace of the project that **owns** the
//! repo ([`Repo::template_owner`]), never the borrower's: a borrowed component's
//! `worktree_dir` was written against its own project's names.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use minijinja::Value;

use wits_core::git::{Repository, Worktree};

use crate::context::{
    context_for_repo, insert_path, path_slug, repo_value, Bindings, Ctx, TemplateError,
};
use crate::decl::{BranchStrategy, Kind};
use crate::workspace::{expand_tilde, Project, Repo, Workspace};

/// The on-disk location of every repo's repository, in the sense `path` means it.
///
/// `main` and standalone repos use their declared path; nested repos (a
/// submodule or subtree) normally live under `repos.main`. When main is
/// bare-backed, that relative path belongs under main's checkout, not inside the
/// bare common directory — see [`nesting_root`].
pub fn primary_path(ws: &Workspace, project: &Project, repo: &Repo) -> Result<PathBuf> {
    let path = project.repo_abs_path(repo.name())?;
    if repo.name() == "main" || repo.kind() == Kind::Standalone {
        return Ok(path);
    }

    let main_path = project.repo_abs_path("main")?;
    let relative = path.strip_prefix(&main_path).with_context(|| {
        format!(
            "nested repo '{}' path {} is not under repos.main {}",
            repo.name(),
            path.display(),
            main_path.display()
        )
    })?;
    let main = project.repos.get("main").context("repo 'main' not found")?;
    Ok(nesting_root(ws, project, main)?.join(relative))
}

/// The checkout that a repo's **nested** repos live inside.
///
/// A conventional clone is its own answer. A bare-backed repo's is a *worktree*
/// — the one holding `main_branch`, else its bootstrap — because a git-dir has
/// no tree for anything to be nested in.
pub fn nesting_root(ws: &Workspace, project: &Project, main: &Repo) -> Result<PathBuf> {
    let path = project.repo_abs_path(main.name())?;
    if !main.decl.strategy.is_bare_backed() {
        return Ok(path);
    }
    if !path.exists() {
        return bootstrap_worktree_dir(ws, project, main);
    }
    let git = Repository::new(&path);
    // Declared bare-backed but on disk still a conventional checkout — a
    // migration in progress. Nest under what is actually there.
    if git.git_dir().is_none() || !git.is_bare() {
        return Ok(path);
    }
    match main
        .decl
        .main_branch
        .as_deref()
        .and_then(|branch| repository_worktree_for_branch(&git, branch))
    {
        Some(worktree) => Ok(worktree),
        None => bootstrap_worktree_dir(ws, project, main),
    }
}

/// Resolve the fixed worktree created immediately after a bare clone.
///
/// Hybrid requires an explicit, branch-independent template. Worktree may use
/// one too; when omitted its ordinary `worktree_dir` is rendered for
/// `main_branch`, preserving the deterministic branch-named layout.
pub fn bootstrap_worktree_dir(ws: &Workspace, project: &Project, repo: &Repo) -> Result<PathBuf> {
    // Both templates are the repo's own, so both render in the namespace of the
    // project that owns it.
    let (project, repo) = owner(ws, project, repo)?;
    let branch = repo
        .decl
        .main_branch
        .as_deref()
        .with_context(|| format!("repo '{}' has no main_branch", repo.name()))?;
    let template = repo
        .decl
        .worktree_dir
        .as_deref()
        .with_context(|| format!("repo '{}' has no worktree_dir", repo.name()))?;
    let rendered = Ctx::new(branch_root(ws, project, repo, branch))
        .render_path(template)
        .with_context(|| format!("resolving worktree_dir for repo '{}'", repo.name()))?;
    let main_worktree = anchor_worktree_path(ws, project, repo, expand_tilde(&rendered))?;

    if let Some(template) = repo.decl.bootstrap_worktree_dir.as_deref() {
        let rendered = Ctx::new(context_for_repo(ws, project, repo.name()))
            .render_path(template)
            .with_context(|| {
                format!(
                    "resolving bootstrap_worktree_dir for repo '{}' \
                     (it must not reference branch.*)",
                    repo.name()
                )
            })?;
        let path = expand_tilde(&rendered);
        if path.is_absolute() {
            return Ok(path);
        }
        let parent = main_worktree.parent().with_context(|| {
            format!(
                "worktree_dir {} has no parent for relative bootstrap_worktree_dir {}",
                main_worktree.display(),
                path.display()
            )
        })?;
        return Ok(parent.join(path));
    }
    Ok(main_worktree)
}

/// The checkout of `repo` for `branch`, following **that repo's own** declared
/// strategy — the standalone form of what [`crate::paths::resolve_paths`] binds
/// as `repos.<name>.workdir`.
pub fn work_dir(ws: &Workspace, project: &Project, repo: &Repo, branch: &str) -> Result<PathBuf> {
    let ctx = Ctx::new(branch_root(ws, project, repo, branch));
    resolve_work_dir(ws, project, repo, &ctx, repo.decl.strategy, branch)
}

/// Whether a live worktree belongs to `repo` rather than to a sibling repo that
/// shares its git dir.
///
/// Two repos may share one git repository and still be different checkouts of
/// it: a bare-backed component and a review checkout of the same component each
/// declare their own placement. `git worktree list` answers for the
/// *repository*, so it reports both to both, and attributing every worktree to
/// every repo that points at that git dir makes the two indistinguishable.
///
/// A repo with no `worktree_dir` accepts what git reported: it declares no
/// placement to compare, so its checkout *is* the repository. Otherwise the
/// declared placements decide — its primary path, its `worktree_dir` rendered for
/// the worktree's branch, the branch-independent form of that template (the fixed
/// selector a detached build accepts), and its bootstrap checkout. A hybrid repo
/// falls back further, to the live inventory its strategy searches by design, but
/// only for a worktree that holds a branch and that no sibling *places* itself:
/// an explicit placement outranks the inventory search, which is what keeps two
/// repos of one project distinguishable.
///
/// This is deliberately the same predicate for path lookup and checkout
/// selection; a sibling worktree must not be selected by one and rejected by the
/// other.
pub(crate) fn worktree_belongs_to(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    wt: &Worktree,
) -> bool {
    if repo.decl.worktree_dir.is_none() {
        return true;
    }
    if places(ws, project, repo, wt) {
        return true;
    }
    repo.decl.strategy == BranchStrategy::Hybrid
        && wt.branch.is_some()
        && !project.repos().any(|(name, sibling)| {
            name != repo.name()
                && sibling.decl.worktree_dir.is_some()
                && places(ws, project, sibling, wt)
        })
}

/// Does `repo`'s own declaration place a checkout at `wt`'s path?
///
/// Template- and path-based rather than a call to [`work_dir`]: for a hybrid repo
/// `work_dir` searches the live inventory, which is exactly the question being
/// asked here, and would recurse back through [`checkout_holding`].
fn places(ws: &Workspace, project: &Project, repo: &Repo, wt: &Worktree) -> bool {
    // The repo's own primary path is always its checkout — including a
    // bare-backed declaration still on disk as a conventional clone during a
    // strategy migration — and no sibling can claim it.
    if let Ok(primary) = primary_path(ws, project, repo) {
        if same_path(&primary, &wt.path) {
            return true;
        }
    }
    // A detached worktree names no branch; the repo's own main branch stands in,
    // which is enough for a strategy whose path does not vary by branch.
    let main_branch = repo.decl.main_branch.as_deref();
    let branch = wt.branch.as_deref().or(main_branch);
    if let Some(branch) = branch {
        if declared_placement(ws, project, repo, Some(branch))
            .is_some_and(|placed| same_path(&placed, &wt.path))
        {
            return true;
        }
    }
    // A branch-independent `worktree_dir` is a fixed checkout selector — the
    // detached review checkout is the motivating case — so the repo places that
    // path for a detached worktree too.
    if declared_placement(ws, project, repo, None)
        .is_some_and(|placed| same_path(&placed, &wt.path))
    {
        return true;
    }
    // A worktree repo may declare a bootstrap checkout distinct from its
    // ordinary main-branch worktree; that fixed checkout is still placed by the
    // repo's strategy and is the checkout `update` fast-forwards.
    if branch == main_branch && repo.decl.bootstrap_worktree_dir.is_some() {
        if let Ok(bootstrap) = bootstrap_worktree_dir(ws, project, repo) {
            return same_path(&bootstrap, &wt.path);
        }
    }
    false
}

/// The path one repo's `worktree_dir` template places, or `None` when it declares
/// none or the template cannot answer against the Profile-free per-repo context.
///
/// A fixed selector is defined as a template that resolves without `branch.*`
/// (the detached rule in `docs/reference/project-reference.rst`), which is
/// exactly what this context can render; a template needing a Profile binding
/// (`{{branch.slug}}`, `{{build_type}}`) has no branch-independent placement to
/// compare and answers `None`.
fn declared_placement(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    branch: Option<&str>,
) -> Option<PathBuf> {
    repo.decl.worktree_dir.as_deref()?;
    let root = match branch {
        Some(branch) => branch_root(ws, project, repo, branch),
        None => context_for_repo(ws, project, repo.name()),
    };
    let ctx = Ctx::new(root);
    resolve_declared_worktree_dir(ws, project, repo, &ctx, branch)
        .ok()
        .flatten()
}

/// Compare two filesystem paths the way a caller means them: symlinks and `.`/
/// `..` spellings do not create a second checkout. A path that cannot be
/// canonicalised (e.g. one that does not exist yet) is compared as written.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

/// The checkout of `repo` that currently has `branch` checked out, whatever
/// shape the repository is: a conventional clone when that is the branch it sits
/// on, or the linked worktree holding it in a bare-backed one. `None` when no
/// checkout has it.
pub fn checkout_holding(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    branch: &str,
) -> Result<Option<PathBuf>> {
    let git = repository_of(ws, project, repo)?;
    let main = git.main_worktree();
    for (index, wt) in git.worktrees().into_iter().enumerate() {
        if !wt.is_live() || wt.branch.as_deref() != Some(branch) {
            continue;
        }
        // `git worktree list` reports a submodule's main worktree as its
        // git-dir, so index 0 still needs the working-tree correction here.
        let path = if index == 0 {
            main.clone().unwrap_or_else(|| wt.path.clone())
        } else {
            wt.path.clone()
        };
        let candidate = Worktree {
            path: path.clone(),
            ..wt
        };
        if worktree_belongs_to(ws, project, repo, &candidate) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// The checkout `repo`'s working-tree work runs in when no branch selects one:
/// its own working tree, or a bare-backed repo's stand-in for the main worktree
/// it does not have. `None` only when the repo has no checkout at all.
pub fn primary_checkout(ws: &Workspace, project: &Project, repo: &Repo) -> Result<Option<PathBuf>> {
    Ok(wits_core::git::primary_checkout(&repository_of(
        ws, project, repo,
    )?))
}

/// The checkout of `repo` that carries its branch identity **right now**, with
/// no branch supplied to resolve against.
pub fn current_checkout(ws: &Workspace, project: &Project, repo: &Repo) -> Result<Option<PathBuf>> {
    let repository = repository_of(ws, project, repo)?;
    if let Ok(cwd) = std::env::current_dir() {
        let here = Repository::new(cwd);
        if same_repository(&repository, &here) {
            if let Some(top) = here.toplevel() {
                let candidate = Worktree {
                    path: top.clone(),
                    branch: here.current_branch(),
                    ..Default::default()
                };
                // A detached checkout has no branch to attribute, but it is
                // still the caller's identity checkout: returning it lets the
                // branch query fail honestly instead of borrowing a sibling's
                // branch. An attached one must be placed by this repo's strategy.
                if candidate.branch.is_none() || worktree_belongs_to(ws, project, repo, &candidate)
                {
                    return Ok(Some(top));
                }
                return Ok(None);
            }
        }
    }
    Ok(wits_core::git::primary_checkout(&repository))
}

/// The branch `repo` is on, read from [`current_checkout`]. `None` on a detached
/// HEAD, or when the repo has no checkout to read.
pub fn current_branch(ws: &Workspace, project: &Project, repo: &Repo) -> Result<Option<String>> {
    Ok(current_checkout(ws, project, repo)?.and_then(|dir| Repository::new(dir).current_branch()))
}

/// The checkout a branch-keyed `worktree_dir` would place for `branch`,
/// bypassing the caller's cwd and the live inventory.
fn resolve_work_dir(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    ctx: &Ctx,
    strategy: BranchStrategy,
    branch: &str,
) -> Result<PathBuf> {
    match strategy {
        BranchStrategy::InPlace => primary_path(ws, project, repo)
            .with_context(|| format!("cannot resolve path of repo '{}'", repo.name())),
        BranchStrategy::Worktree | BranchStrategy::Hybrid => {
            if strategy == BranchStrategy::Hybrid {
                if let Some(path) = checkout_holding(ws, project, repo, branch)? {
                    return Ok(path);
                }
            }
            resolve_declared_worktree_dir(ws, project, repo, ctx, Some(branch))?.with_context(
                || {
                    format!(
                        "repo '{}' uses {} strategy but has no worktree_dir",
                        repo.name(),
                        strategy.as_str()
                    )
                },
            )
        }
    }
}

/// Resolve the checkout that contributes a repo to a detached build.
///
/// A branch-independent `worktree_dir` is an explicit checkout selection (the
/// fixed `review` worktree is the motivating case), so it wins over whichever
/// checkout happens to contain the caller. A branch-keyed template cannot answer
/// without inventing an identity and falls through to the live checkout:
/// caller's checkout when it belongs to this repo, otherwise the primary.
pub(crate) fn detached_work_dir(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    ctx: &Ctx,
) -> Result<PathBuf> {
    if let Some(path) = resolve_declared_worktree_dir(ws, project, repo, ctx, None)? {
        return Ok(path);
    }
    if let Some(path) = current_checkout(ws, project, repo)? {
        return Ok(path);
    }
    if repo.decl.strategy == BranchStrategy::InPlace {
        return primary_path(ws, project, repo)
            .with_context(|| format!("cannot resolve path of repo '{}'", repo.name()));
    }
    bail!(
        "repo '{}' has no checkout to use for a detached build",
        repo.name()
    )
}

/// Render one repo's declared worktree path in the namespace of the project
/// that owns it. `None` for a detached build means a missing `branch.*` binding
/// is not an error: that template describes branch worktrees, not this checkout.
fn resolve_declared_worktree_dir(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    ctx: &Ctx,
    branch: Option<&str>,
) -> Result<Option<PathBuf>> {
    let Some(tpl) = repo.decl.worktree_dir.as_deref() else {
        return Ok(None);
    };
    let (owner_project, owner_repo) = owner(ws, project, repo)?;
    let mut scoped = if std::ptr::eq(owner_project, project) && std::ptr::eq(owner_repo, repo) {
        ctx.clone()
    } else {
        let root = match branch {
            Some(branch) => branch_root(ws, owner_project, owner_repo, branch),
            None => context_for_repo(ws, owner_project, owner_repo.name()),
        };
        Ctx::new(root)
    };
    scoped.set("repo", repo_value(owner_project, owner_repo.name()));
    let primary = primary_path(ws, owner_project, owner_repo)?;
    scoped.set("repo.path", Value::from(primary.display().to_string()));
    let rendered = match scoped.render_path(tpl) {
        Ok(path) => path,
        Err(error) if branch.is_none() && missing_branch_binding(&error) => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Some(anchor_worktree_path(
        ws,
        owner_project,
        owner_repo,
        expand_tilde(&rendered),
    )?))
}

/// Whether a failed render failed only because a detached build has no
/// `branch.*` binding — the one template error a detached selection tolerates.
pub(crate) fn missing_branch_binding(error: &TemplateError) -> bool {
    matches!(
        error,
        TemplateError::UnknownPath(path) if path == "branch" || path.starts_with("branch.")
    )
}

/// A detached plan is opt-in, so its identity checkout must contain a real,
/// detached `HEAD`. `current_branch() == None` alone is insufficient: an unborn
/// branch and a non-repository also have no symbolic branch.
pub(crate) fn validate_detached_head(
    project: &Project,
    identity_repo: Option<&str>,
    work_dirs: &std::collections::BTreeMap<String, PathBuf>,
) -> Result<()> {
    let name = identity_repo.context("project has no own-git repo to take HEAD from")?;
    let path = work_dirs.get(name).with_context(|| {
        format!(
            "repo '{name}' of project '{}' has no resolved checkout",
            project.name()
        )
    })?;
    let git = Repository::new(path);
    if !git.is_repo() {
        bail!(
            "repo '{name}' has no Git checkout at {} for --detach",
            path.display()
        );
    }
    if let Some(branch) = git.current_branch() {
        bail!("repo '{name}' is on branch '{branch}'; --detach requires a detached HEAD");
    }
    if git.rev_parse("HEAD").is_none() {
        bail!(
            "repo '{name}' has no commit checked out at {} for --detach",
            path.display()
        );
    }
    Ok(())
}

/// The project and repo a repo's **own** path templates belong to: itself, or
/// the project a `from` borrow points at.
pub(crate) fn owner<'a>(
    ws: &'a Workspace,
    _project: &'a Project,
    repo: &'a Repo,
) -> Result<(&'a Project, &'a Repo)> {
    let owner_project = ws
        .projects
        .get(&repo.template_owner.project)
        .with_context(|| format!("template owner '{}' not found", repo.template_owner.project))?;
    let owner_repo = owner_project
        .repos
        .get(&repo.template_owner.repo)
        .with_context(|| {
            format!(
                "template owner repo '{}' not found",
                repo.template_owner.repo
            )
        })?;
    Ok((owner_project, owner_repo))
}

/// A context able to resolve one repo's branch-keyed path templates: its
/// project's per-repo namespace plus the `branch.*` bindings a `worktree_dir`
/// needs.
fn branch_root(ws: &Workspace, project: &Project, repo: &Repo, branch: &str) -> Bindings {
    let mut root = context_for_repo(ws, project, repo.name());
    insert_path(&mut root, "branch.raw", Value::from(branch));
    insert_path(&mut root, "branch.slug", Value::from(path_slug(branch)));
    root
}

fn anchor_worktree_path(
    ws: &Workspace,
    project: &Project,
    repo: &Repo,
    path: PathBuf,
) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    let repository = primary_path(ws, project, repo)?;
    let parent = repository.parent().with_context(|| {
        format!(
            "repo '{}' path {} has no parent for relative worktree path {}",
            repo.name(),
            repository.display(),
            path.display()
        )
    })?;
    Ok(parent.join(path))
}

/// A handle on the repo *as a repository* — the one place `path` is the right
/// answer, because `git worktree list` and the ref plumbing are asked of the
/// repository rather than of a checkout.
fn repository_of(ws: &Workspace, project: &Project, repo: &Repo) -> Result<Repository> {
    let path = primary_path(ws, project, repo)
        .with_context(|| format!("cannot resolve path of repo '{}'", repo.name()))?;
    Ok(Repository::new(path))
}

/// Do two handles name the same repository? Compared by common git-dir through
/// the filesystem, since git spells one directory several ways.
fn same_repository(a: &Repository, b: &Repository) -> bool {
    a.git_common_dir()
        .zip(b.git_common_dir())
        .is_some_and(|(a, b)| same_path(&a, &b))
}

fn repository_worktree_for_branch(git: &Repository, branch: &str) -> Option<PathBuf> {
    let main = git.main_worktree();
    git.worktrees()
        .into_iter()
        .enumerate()
        .find(|(_, wt)| wt.is_live() && wt.branch.as_deref() == Some(branch))
        .map(|(index, wt)| {
            if index == 0 {
                main.unwrap_or(wt.path)
            } else {
                wt.path
            }
        })
}
