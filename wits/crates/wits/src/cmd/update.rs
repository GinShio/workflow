//! `wits update` — refresh every repo of a project.
//!
//! The default action never switches branches. A conventional feature checkout
//! fast-forwards the `main_branch` ref without touching its tree. A bare-backed
//! repo fast-forwards the linked worktree already holding main, or updates only
//! the bare ref when no such worktree remains. Remote reconciliation is
//! additive: missing remotes, mirror push-URLs, and a missing fetch refspec are
//! added, existing ones never touched.
//!
//! A submodule is just a nested repo, so it gets the same treatment; undeclared
//! nested submodules are refreshed to their recorded commit (never `--init`,
//! which belongs to a fresh checkout — clone or worktree creation).
//!
//! Two things a repo may say about *whose* checkout it is shape what happens
//! here. A **borrowed** repo (`from`) belongs to the project that declares it, so
//! `update` leaves it alone unless `--with-borrowed` asks otherwise — otherwise
//! one shared component would be fetched once per project that consumes it. A
//! repo's **`skip`** list is *applied* by `clone` (a tree we are still building)
//! and only *verified* by update, which never touches a working tree.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_util::project::context::Ctx;

use wits_util::git::{self, Repository, RestoreGuard};
use wits_util::project::model::{infer_kind, BranchStrategy, Kind, RawRepo};
use wits_util::project::resolve;
use wits_util::project::resolve_target;
use wits_util::project::skip;
use wits_util::project::workspace::{ProjectData, Workspace};
use wits_util::worktree;

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Project name or path (default: the project owning the current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// Also refresh repos borrowed from another project (`from`). Off by default:
    /// the project that owns a shared component updates it, so a component used
    /// by five projects is fetched once, not five times.
    #[arg(long = "with-borrowed")]
    pub with_borrowed: bool,
}

/// `wits update` — its own top-level command, over the shared `project` core.
pub fn run(args: &UpdateArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let project = resolve_target(&ws, args.target.as_deref())?;
    execute(&ws, project, args.with_borrowed)
}

fn execute(ws: &Workspace, project: &ProjectData, with_borrowed: bool) -> Result<()> {
    for name in repo_order(project) {
        let repo = &project.repos[&name];
        if infer_kind(&name, repo) == Kind::Subtree {
            continue; // shares its anchor's git; no work of its own
        }
        if project.is_borrowed(&name) && !with_borrowed {
            log::debug!("repo '{name}': borrowed, left to its owning project");
            continue;
        }
        // A nested repo lives *inside* main's checkout, so there has to be one. Under
        // a bare-backed main that is a worktree, which may legitimately not exist yet
        // — and then the nested lifecycle has nowhere to happen, so it is skipped
        // rather than failed. Asked of the very root `repo_primary_path` nests under,
        // so the guard and the path it guards cannot disagree.
        if infer_kind(&name, repo) == Kind::Submodule {
            let root = resolve::nesting_root(ws, project, "main")?;
            if !root.exists() {
                log::debug!(
                    "repo '{name}': main has no checkout at {}; nested lifecycle skipped",
                    root.display()
                );
                continue;
            }
        }
        let path = resolve::repo_primary_path(ws, project, &name)
            .with_context(|| format!("cannot resolve path of repo '{name}'"))?;
        let git = Repository::new(path);
        if !git.exists() {
            clone_repo(ws, project, &name, &git)
                .with_context(|| format!("cloning repo '{name}' of project '{}'", project.name))?;
        } else {
            update_repo(ws, project, &name, &git)
                .with_context(|| format!("updating repo '{name}' of project '{}'", project.name))?;
        }
    }
    Ok(())
}

/// Fail unless the repo's declared `skip` is actually in force.
///
/// Config is the truth here: we do not repair a checkout that contradicts it,
/// because doing so means deleting content on a tree we did not build. Under
/// `-v` the exact commands are printed, so the destructive step stays an explicit
/// act of yours.
fn verify_skip(git: &Repository, name: &str, repo: &RawRepo) -> Result<()> {
    if repo.skip.is_empty() {
        return Ok(());
    }
    let violations = skip::violations(git, &repo.skip);
    if violations.is_empty() {
        return Ok(());
    }
    for v in &violations {
        log::error!("repo '{name}': {v}");
    }
    if wits_util::log::is_verbose() {
        log::debug!("bring it in line with:");
        for cmd in skip::remedy(git, &repo.skip) {
            log::debug!("  (cd {} && {cmd})", git.path().display());
        }
    } else {
        log::info!("re-run with -v to see the commands that fix this");
    }
    bail!(
        "repo '{name}': declared 'skip' is not in force ({} problem(s))",
        violations.len()
    )
}

/// Realise a repo's `skip` on a checkout we just built: deinit the submodules it
/// covers, then write the sparse patterns — in that order, because sparse alone
/// cannot mask a materialised submodule (see [`skip`]). Then verify, so residue
/// git cannot remove (a hook's untracked files) is an error rather than a mask
/// that silently did not take.
///
/// The one thing this refuses to do is **overwrite somebody else's sparse
/// patterns**. `sparse-checkout set` replaces the whole list, so a checkout a
/// `clone` hook narrowed to a sparse cone would be silently *widened* to the
/// entire tree while we masked one path out of it. Cone mode cannot express an
/// exclusion at all, so there is no safe write to make there either.
fn apply_skip(git: &Repository, name: &str, repo: &RawRepo) -> Result<()> {
    if repo.skip.is_empty() {
        return Ok(());
    }
    let subs = skip::materialised_skipped_submodules(git, &repo.skip);
    git.submodule_deinit(&subs)?;

    if git.is_sparse() && !skip::sparse_already_ours(git, &repo.skip) {
        if wits_util::log::is_verbose() {
            log::debug!("existing patterns: {}", git.sparse_list().join(" "));
            log::debug!("declared 'skip' wants to add exclusions for:");
            for entry in &repo.skip {
                log::debug!("  {entry}");
            }
        }
        bail!(
            "repo '{name}': the checkout already has sparse-checkout patterns that wits did not \
             write, and applying 'skip' would replace them (widening the checkout). Fold the \
             exclusions into those patterns yourself, or drop 'skip' for this repo"
        );
    }
    git.sparse_set(&skip::sparse_patterns(&repo.skip))?;
    verify_skip(git, name, repo)
}

/// `repos.main` first (nested repos are cloned through it), then the rest.
fn repo_order(project: &ProjectData) -> Vec<String> {
    let mut order = Vec::new();
    if project.repos.contains_key("main") {
        order.push("main".to_owned());
    }
    for name in project.repos.keys() {
        if name != "main" {
            order.push(name.clone());
        }
    }
    order
}

fn clone_repo(ws: &Workspace, project: &ProjectData, name: &str, git: &Repository) -> Result<()> {
    let repo = &project.repos[name];
    let ctx = Ctx::new(resolve::context_for_repo(ws, project, name));
    let strategy = BranchStrategy::parse(repo.branch_strategy.as_deref())?;

    // A clone override owns repository creation. The default clone shape follows
    // the branch strategy: in-place creates a conventional checkout, while
    // worktree/hybrid create a bare common repository and an explicit bootstrap
    // checkout for main.
    if let Some(action) = repo.hooks.clone.as_ref() {
        run_hook(&ctx, None, action, "clone")?;
    } else if strategy.is_bare_backed() {
        clone_bare_repo(ws, project, name, git)?;
    } else {
        clone_in_place_repo(repo, git)?;
    }

    let checkout = if strategy.is_bare_backed() {
        let dir = resolve::bootstrap_worktree_dir(ws, project, name)?;
        if !wits_util::log::is_dry_run() && !dir.exists() {
            bail!(
                "repo '{name}': bare clone has no bootstrap worktree at {}",
                dir.display()
            );
        }
        Repository::new(dir)
    } else {
        git.clone()
    };

    // Both default clone shapes install the mask themselves, ahead of the
    // materialisation it has to precede. What is left for here is the `clone`
    // override, which built the tree its own way and may well have left the mask
    // off; for the other two this is idempotent.
    apply_skip(&checkout, name, repo)?;
    run_hook_opt(
        &ctx,
        Some(checkout.path()),
        repo.hooks.post_clone.as_ref(),
        "post_clone",
    )?;
    // A post hook can write into excluded paths. Verify again rather than
    // silently leaving a bootstrap checkout that contradicts its declaration.
    verify_skip(&checkout, name, repo)?;
    Ok(())
}

fn clone_source(repo: &RawRepo) -> Result<(&str, &str)> {
    match (
        repo.remotes.upstream.as_deref(),
        repo.remotes.origin.as_deref(),
    ) {
        (Some(upstream), _) => Ok((upstream, "upstream")),
        (None, Some(origin)) => Ok((origin, "origin")),
        (None, None) => bail!("cannot clone: no [remotes] origin or upstream declared"),
    }
}

fn clone_in_place_repo(repo: &RawRepo, git: &Repository) -> Result<()> {
    let (clone_url, remote) = clone_source(repo)?;
    // Install sparse patterns before the first checkout so excluded paths never
    // materialise in a conventional clone.
    let deferred = !repo.skip.is_empty();
    git::clone(clone_url, remote, git.path(), deferred)?;
    ensure_remotes(git, repo)?;
    if deferred {
        git.sparse_set(&skip::sparse_patterns(&repo.skip))?;
    }
    let target = match (&repo.main_branch, deferred) {
        (Some(main), _) => Some(main.clone()),
        (None, true) => git.current_branch(),
        (None, false) => None,
    };
    if let Some(branch) = target {
        git.checkout(&branch)?;
    }
    let subs: Vec<String> = git
        .materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .collect();
    git.submodule_update(&subs, true)?;
    Ok(())
}

fn clone_bare_repo(
    ws: &Workspace,
    project: &ProjectData,
    name: &str,
    git: &Repository,
) -> Result<()> {
    let repo = &project.repos[name];
    let (clone_url, remote) = clone_source(repo)?;
    let main = repo
        .main_branch
        .as_deref()
        .context("bare-backed repo has no main_branch")?;
    let bootstrap = resolve::bootstrap_worktree_dir(ws, project, name)?;

    // A tracking bare host rather than `git clone --bare`: the remote's branches
    // belong in `refs/remotes/<remote>/*`, and `refs/heads` belongs to the
    // branches this repository works on. See `git::init_bare_host`.
    git::init_bare_host(clone_url, remote, git.path(), main)?;
    ensure_remotes(git, repo)?;
    // `create_known` deliberately skips a second ref lookup: during dry-run the
    // clone is only planned, so no branch exists on disk yet.
    worktree::create_known(git, &bootstrap, main)?;

    // The mask goes on *before* anything is materialised, which is the one thing
    // a bare host cannot get for free. `git worktree add` copies the sparse
    // patterns of the checkout it runs from, and a host that has just been
    // created has no checkout at all — so the bootstrap starts out full, and a
    // `skip`ped submodule would be cloned in its entirety and only then
    // deinitialised. For a component the project deliberately does not
    // materialise (an internal repo it borrows from elsewhere) that download is
    // not merely wasted: it is a clone that can fail and take the whole `update`
    // with it. The conventional clone below already has this ordering, through
    // `--no-checkout`.
    apply_skip(&Repository::new(&bootstrap), name, repo)?;

    // Through the worktree policy rather than a plain recursive init. The
    // bootstrap is a *linked* worktree, so git would file its submodule stores
    // under the bootstrap's own administrative directory, where `git worktree
    // remove` later takes them out along with the bootstrap. `sync_submodules`
    // publishes them into the repository instead, which both survives the
    // bootstrap and gives every later worktree something safe to borrow.
    worktree::sync_submodules(&bootstrap)
        .with_context(|| format!("materialising submodules in {}", bootstrap.display()))?;
    Ok(())
}

fn update_repo(ws: &Workspace, project: &ProjectData, name: &str, git: &Repository) -> Result<()> {
    let repo = &project.repos[name];
    let ctx = Ctx::new(resolve::context_for_repo(ws, project, name));

    // Two different questions, which used to be one and so were answered wrongly
    // for a bare-backed repo.
    //
    // The checkout **holding `main_branch`** is what a fast-forward may advance —
    // only that one, since `update` never moves a branch a checkout is not on. It
    // may genuinely be absent, and then the branch moves as a ref instead.
    let holding = repo
        .main_branch
        .as_deref()
        .map(|branch| resolve::checkout_holding(ws, project, name, branch))
        .transpose()?
        .flatten()
        .map(Repository::new);
    // The repo's **working tree**: where hooks run, what the restore guard protects,
    // and what a `skip` mask is verified in. A hook needs *a* working tree, not that
    // particular branch — run in the repository path instead, `git submodule` and
    // friends refuse outright ("cannot be used without a working tree") on exactly
    // the repos that have a worktree sitting right there.
    //
    // Falling back past `holding` is what makes `skip` independent of the branch
    // strategy. Verifying only the `main_branch` worktree meant a bare-backed repo
    // whose `main_branch` happened to sit in no worktree had its declaration silently
    // unverified — while `skip` is a property of the repo and of nothing else.
    let workdir = match holding.clone() {
        Some(checkout) => Some(checkout),
        None => resolve::primary_checkout(ws, project, name)?.map(Repository::new),
    };

    // Before anything else: a checkout that contradicts its declared `skip` is a
    // config/reality conflict, and refreshing it would only entrench whichever
    // copy of a shared component should not be there.
    if let Some(checkout) = &workdir {
        verify_skip(checkout, name, repo)?;
    }

    ensure_remotes(git, repo)?;

    // Fail-fast with guaranteed restoration: if a hook or override switches the
    // branch, the guard returns us to where we started on any exit.
    let _guard = workdir.as_ref().map(RestoreGuard::capture);
    let hook_cwd = workdir
        .as_ref()
        .map(Repository::path)
        .unwrap_or_else(|| git.path());

    run_hook_opt(
        &ctx,
        Some(hook_cwd),
        repo.hooks.pre_update.as_ref(),
        "pre_update",
    )?;

    if let Some(action) = repo.hooks.update.as_ref() {
        run_hook(&ctx, Some(hook_cwd), action, "update")?;
    } else {
        default_update(project, name, git, holding.as_ref(), workdir.as_ref(), repo)?;
    }

    run_hook_opt(
        &ctx,
        Some(hook_cwd),
        repo.hooks.post_update.as_ref(),
        "post_update",
    )?;
    Ok(())
}

fn default_update(
    project: &ProjectData,
    name: &str,
    git: &Repository,
    holding: Option<&Repository>,
    workdir: Option<&Repository>,
    repo: &RawRepo,
) -> Result<()> {
    let mb = repo
        .main_branch
        .as_deref()
        .context("own-git repo has no main_branch")?;
    // The sync source that `main` advances on: `upstream` if declared, else
    // `origin`. A fork declared as `origin` need not exist — nothing fetches it.
    let sync = if repo.remotes.upstream.is_some() {
        "upstream"
    } else {
        "origin"
    };

    // A plain fetch, because `ensure_remotes` has just guaranteed the refspec that
    // makes one meaningful: every branch the remote has lands under
    // `refs/remotes/<sync>/*`, which is what tracking distances and trunk detection
    // read, and `refs/heads` is left to this repository's own branches.
    git.fetch(&[sync])?;

    // How `main` advances turns on **whether any checkout has it**, not on whether
    // the repository is bare. Those were always the same question asked two ways: a
    // conventional clone is itself the checkout holding whichever branch it is on,
    // and a bare-backed repo keeps a checkout per branch.
    match holding {
        Some(checkout) => checkout.merge_ff_only(&format!("{sync}/{mb}"))?,
        // Nothing has it, so the branch moves as a ref and no working tree is
        // touched — which is exactly how a feature checkout keeps its own branch,
        // and its sparse cone, while `main` catches up underneath.
        None => git.fast_forward_branch(mb, &format!("{sync}/{mb}"))?,
    }

    // Submodules follow whichever checkout exists: `main`'s where that is what is
    // checked out, else the tree the work is actually on, whose recorded pins are
    // the ones its build reads. A repo with no checkout at all has none to align.
    match workdir {
        Some(checkout) => refresh_submodules(project, name, checkout),
        None => Ok(()),
    }
}

fn refresh_submodules(project: &ProjectData, name: &str, git: &Repository) -> Result<()> {
    // Undeclared nested submodules → recorded commit; declared ones are managed
    // as their own repos, so skip their paths here. A `skip`ped submodule needs no
    // mention: it is not on disk, and `materialised_submodules` only reports what
    // is.
    let declared: BTreeSet<String> = project
        .repos
        .iter()
        .filter(|(n, r)| *n != name && infer_kind(n, r) != Kind::Standalone)
        .filter_map(|(_, r)| r.path.clone())
        .collect();
    let subs: Vec<String> = git
        .materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .filter(|p| !declared.contains(p))
        .collect();
    git.submodule_update(&subs, false)?;
    Ok(())
}

/// Additive remote reconciliation (§3.1): add what's missing, never modify.
///
/// "Missing" includes a remote that exists but has no **fetch refspec**, which is
/// what `git clone --bare` leaves behind and what makes a plain `git fetch` there
/// update no ref at all. Adding it is how a bare host cloned before this repairs
/// itself; see [`Repository::ensure_fetch_refspec`].
fn ensure_remotes(git: &Repository, repo: &RawRepo) -> Result<()> {
    if let Some(origin) = &repo.remotes.origin {
        git.ensure_remote("origin", origin)?;
        // Only touch push URLs when there are mirrors. git pushes to the fetch URL
        // by default, so with no mirrors there is nothing to add — and adding
        // origin's own URL as an explicit pushurl would be pointless churn. Once a
        // mirror makes an explicit pushurl necessary, git stops defaulting push to
        // the fetch URL, so origin's own URL must be listed alongside the mirrors.
        if !repo.remotes.mirrors.is_empty() {
            git.ensure_push_url("origin", origin)?;
            for mirror in &repo.remotes.mirrors {
                git.ensure_push_url("origin", mirror)?;
            }
        }
    }
    if let Some(upstream) = &repo.remotes.upstream {
        git.ensure_remote("upstream", upstream)?;
    }
    Ok(())
}

fn run_hook_opt(ctx: &Ctx, cwd: Option<&Path>, hook: Option<&String>, phase: &str) -> Result<()> {
    match hook {
        Some(cmd) => run_hook(ctx, cwd, cmd, phase),
        None => Ok(()),
    }
}

/// Run a templated hook via `sh -c`. `cwd` selects the working directory;
/// `None` inherits the process's, which is where a `clone` override runs since
/// the repo path does not exist yet. `post_clone` and every update-phase hook
/// run in the repo path itself. A non-zero exit fails fast.
fn run_hook(ctx: &Ctx, cwd: Option<&Path>, command: &str, phase: &str) -> Result<()> {
    let rendered = ctx
        .resolve_str(command)
        .with_context(|| format!("resolving {phase} hook"))?;
    // A hook is a shell script, so whatever it resolved to has to become one line
    // of text; only a whole-expression hook could arrive as anything else.
    let script = rendered.to_string();
    log::info!("hook {phase}: {script}");
    let mut cmd = wits_util::process::Command::new("sh");
    cmd.args(["-c".to_string(), script]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let code = cmd.status()?;
    if code != 0 {
        anyhow::bail!("{phase} hook failed (exit {code})");
    }
    Ok(())
}
