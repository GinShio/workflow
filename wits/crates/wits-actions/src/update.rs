//! The `wits update` action — refresh every repo of a project.
//!
//! The default action never switches branches. A conventional feature checkout
//! fast-forwards the `main_branch` ref without touching its tree. A bare-backed
//! repo fast-forwards the linked worktree already holding main, or updates only
//! the bare ref when no such worktree remains. Remote reconciliation converges
//! the remotes the config declares — a missing one is added, a drifted URL is
//! corrected, the push-URL set is replaced — repairs a missing fetch refspec,
//! and never touches a remote the config does not name.
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
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_core::git::{self, Repository, RestoreGuard};
use wits_core::worktree;
use wits_project::layout;
use wits_project::remotes;
use wits_project::skip;
use wits_project::{HookPhase, Project, RepoDecl, Workspace};

/// How far an update reaches: a repo borrowed from another project is left to
/// its owner unless this asks otherwise.
pub struct UpdateOptions {
    pub with_borrowed: bool,
}

/// Refresh every repo of `project` — clone what is missing, update what is
/// there — without ever switching a branch.
pub fn update(ws: &Workspace, project: &Project, options: &UpdateOptions) -> Result<()> {
    let with_borrowed = options.with_borrowed;
    for name in repo_order(project) {
        let repo = project
            .repo(&name)
            .with_context(|| format!("repo '{name}' not found"))?;
        if repo.kind() == wits_project::Kind::Subtree {
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
        if repo.kind() == wits_project::Kind::Submodule {
            let root = layout::nesting_root(ws, project, "main")?;
            if !root.exists() {
                log::debug!(
                    "repo '{name}': main has no checkout at {}; nested lifecycle skipped",
                    root.display()
                );
                continue;
            }
        }
        let path = layout::primary_path(ws, project, &name)
            .with_context(|| format!("cannot resolve path of repo '{name}'"))?;
        let git = Repository::new(path);
        if !git.exists() {
            clone_repo(ws, project, &name, &git).with_context(|| {
                format!("cloning repo '{name}' of project '{}'", project.name())
            })?;
        } else {
            update_repo(ws, project, &name, &git).with_context(|| {
                format!("updating repo '{name}' of project '{}'", project.name())
            })?;
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
fn verify_skip(git: &Repository, name: &str, repo: &RepoDecl) -> Result<()> {
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
    if wits_core::log::is_verbose() {
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
fn apply_skip(git: &Repository, name: &str, repo: &RepoDecl) -> Result<()> {
    if repo.skip.is_empty() {
        return Ok(());
    }
    let subs = skip::materialised_skipped_submodules(git, &repo.skip);
    git.submodule_deinit(&subs)?;

    if git.is_sparse() && !skip::sparse_already_ours(git, &repo.skip) {
        if wits_core::log::is_verbose() {
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
fn repo_order(project: &Project) -> Vec<String> {
    let mut order = Vec::new();
    if project.repo("main").is_some() {
        order.push("main".to_owned());
    }
    for (name, _) in project.repos() {
        if name != "main" {
            order.push(name.to_owned());
        }
    }
    order
}

fn clone_repo(ws: &Workspace, project: &Project, name: &str, git: &Repository) -> Result<()> {
    let repo = project
        .repo(name)
        .with_context(|| format!("repo '{name}' not found"))?;
    let strategy = repo.decl().strategy;

    // A clone override owns repository creation. The default clone shape follows
    // the branch strategy: in-place creates a conventional checkout, while
    // worktree/hybrid create a bare common repository and an explicit bootstrap
    // checkout for main.
    match project.render_hook(ws, name, HookPhase::Clone)? {
        Some(action) => run_script(None, &action, "clone")?,
        None if strategy.is_bare_backed() => clone_bare_repo(ws, project, name, git)?,
        None => clone_in_place_repo(repo.decl(), git)?,
    }

    let checkout = if strategy.is_bare_backed() {
        let dir = layout::bootstrap_worktree_dir(ws, project, name)?;
        if !wits_core::log::is_dry_run() && !dir.exists() {
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
    apply_skip(&checkout, name, repo.decl())?;
    run_hook(
        ws,
        project,
        name,
        HookPhase::PostClone,
        Some(checkout.path()),
    )?;
    // A post hook can write into excluded paths. Verify again rather than
    // silently leaving a bootstrap checkout that contradicts its declaration.
    verify_skip(&checkout, name, repo.decl())?;
    Ok(())
}

/// The URL to clone from and the remote name to file it under: the merge target.
///
/// A clone has to name exactly one repository, and the merge target is the one
/// certain to exist — a fork holding only the `origin` role may not have been
/// created on the server yet. The remaining declared remotes are added by
/// [`ensure_remotes`] straight afterwards, and their first fetch is best-effort
/// (see [`default_update`]), so the not-yet-created fork still costs nothing.
fn clone_source(repo: &RepoDecl) -> Result<(&str, &str)> {
    let roles = remotes::roles_of(repo);
    let target = roles
        .merge_target()
        .context("cannot clone: no [remotes] entry holds the origin or upstream role")?;
    // Re-borrowed from the map so both halves outlive the `RemoteRoles` temporary.
    // `remotes::roles_of` only ever names a key of this map, so the entry is there.
    let (name, remote) = repo
        .remotes
        .get_key_value(target)
        .expect("a resolved role names a declared remote");
    Ok((remote.url.as_str(), name.as_str()))
}

fn clone_in_place_repo(repo: &RepoDecl, git: &Repository) -> Result<()> {
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

fn clone_bare_repo(ws: &Workspace, project: &Project, name: &str, git: &Repository) -> Result<()> {
    let repo = project
        .repo(name)
        .with_context(|| format!("repo '{name}' not found"))?;
    let (clone_url, remote) = clone_source(repo.decl())?;
    let main = repo
        .decl()
        .main_branch
        .as_deref()
        .context("bare-backed repo has no main_branch")?;
    let bootstrap = layout::bootstrap_worktree_dir(ws, project, name)?;

    // A tracking bare host rather than `git clone --bare`: the remote's branches
    // belong in `refs/remotes/<remote>/*`, and `refs/heads` belongs to the
    // branches this repository works on. See `git::init_bare_host`.
    git::init_bare_host(clone_url, remote, git.path(), main)?;
    ensure_remotes(git, repo.decl())?;
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
    apply_skip(&Repository::new(&bootstrap), name, repo.decl())?;

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

fn update_repo(ws: &Workspace, project: &Project, name: &str, git: &Repository) -> Result<()> {
    let repo = project
        .repo(name)
        .with_context(|| format!("repo '{name}' not found"))?;

    // Two different questions, which used to be one and so were answered wrongly
    // for a bare-backed repo.
    //
    // The checkout **holding `main_branch`** is what a fast-forward may advance —
    // only that one, since `update` never moves a branch a checkout is not on. It
    // may genuinely be absent, and then the branch moves as a ref instead. Which
    // checkout holds the branch is asked of the repository, not of this repo's
    // placements: a worktree a sibling repo places, or one moved off the
    // `worktree_dir` template, still holds it, and moving the ref under it
    // would strand that checkout behind its own HEAD.
    let holding = repo
        .decl()
        .main_branch
        .as_deref()
        .map(|branch| layout::branch_checkout(ws, project, name, branch))
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
        None => layout::primary_checkout(ws, project, name)?.map(Repository::new),
    };

    // Before anything else: a checkout that contradicts its declared `skip` is a
    // config/reality conflict, and refreshing it would only entrench whichever
    // copy of a shared component should not be there.
    if let Some(checkout) = &workdir {
        verify_skip(checkout, name, repo.decl())?;
    }

    ensure_remotes(git, repo.decl())?;

    // Fail-fast with guaranteed restoration: if a hook or override switches the
    // branch, the guard returns us to where we started on any exit.
    let _guard = workdir.as_ref().map(RestoreGuard::capture);
    let hook_cwd = workdir
        .as_ref()
        .map(Repository::path)
        .unwrap_or_else(|| git.path());

    run_hook(ws, project, name, HookPhase::PreUpdate, Some(hook_cwd))?;

    if let Some(action) = project.render_hook(ws, name, HookPhase::Update)? {
        run_script(Some(hook_cwd), &action, "update")?;
    } else {
        default_update(
            ws,
            project,
            name,
            git,
            holding.as_ref(),
            workdir.as_ref(),
            repo.decl(),
        )?;
    }

    run_hook(ws, project, name, HookPhase::PostUpdate, Some(hook_cwd))?;
    Ok(())
}

fn default_update(
    ws: &Workspace,
    project: &Project,
    name: &str,
    git: &Repository,
    holding: Option<&Repository>,
    workdir: Option<&Repository>,
    repo: &RepoDecl,
) -> Result<()> {
    let mb = repo
        .main_branch
        .as_deref()
        .context("own-git repo has no main_branch")?;
    // The remote `main` advances on. Read from the checkout when the declaration
    // assigns no role at all; see `remotes::roles_in_force` for what that serves
    // and what it still papers over.
    let roles = remotes::roles_in_force(repo, git);
    let target = roles.merge_target().context(
        "no remote holds the origin or upstream role, and this checkout has none named after one",
    )?;

    // Plain fetches, because `ensure_remotes` has just guaranteed the refspec that
    // makes one meaningful: every branch a remote has lands under
    // `refs/remotes/<name>/*`, which is what tracking distances and trunk detection
    // read, and `refs/heads` is left to this repository's own branches.
    //
    // The merge target goes first and its failure is fatal: the fast-forward below
    // reads `<target>/<main_branch>`, so there is nothing to do without it, and
    // failing here costs nothing further.
    git.fetch(&[target])?;
    // Every other declared remote, so an extra remote is worth declaring at all —
    // a declared remote you can never `git log` against is just a line of config.
    // Best-effort, and that asymmetry is what keeps `update` safe to run
    // unattended: a fork holding only the `origin` role may not have been created
    // on the server yet, and a repo declaring a mirror that is down must still
    // update. Nothing below reads these refs, so a miss costs only freshness.
    //
    // No `--prune` on purpose. Pruning would start producing `%(upstream:track)` =
    // `gone` branches, which `wits worktree prune`'s default sweep reclaims
    // worktrees for — a real change in what `update` means, not a detail of this
    // one. `ensure_remotes` prunes the one case where the refs are certainly
    // stale.
    for name in repo.remotes.keys().filter(|n| n.as_str() != target) {
        if let Err(e) = git.fetch(&[name.as_str()]) {
            log::warn!("remote '{name}' could not be fetched ({e})");
        }
    }

    // How `main` advances turns on **whether any checkout has it**, not on whether
    // the repository is bare. Those were always the same question asked two ways: a
    // conventional clone is itself the checkout holding whichever branch it is on,
    // and a bare-backed repo keeps a checkout per branch.
    match holding {
        Some(checkout) => checkout.merge_ff_only(&format!("{target}/{mb}"))?,
        // Nothing has it, so the branch moves as a ref and no working tree is
        // touched — which is exactly how a feature checkout keeps its own branch,
        // and its sparse cone, while `main` catches up underneath.
        None => git.fast_forward_branch(mb, &format!("{target}/{mb}"))?,
    }

    // Submodules follow whichever checkout exists: `main`'s where that is what is
    // checked out, else the tree the work is actually on, whose recorded pins are
    // the ones its build reads. A repo with no checkout at all has none to align.
    match workdir {
        Some(checkout) => refresh_submodules(ws, project, name, checkout),
        None => Ok(()),
    }
}

/// Follow the pins of the submodules that no project repo manages, leaving the
/// declared nested repos to their own lifecycle.
///
/// The declaration is a template (`repos.*.path`), and nested repos are placed
/// under `repos.main`; identify them by their **resolved path relative to main's
/// checkout**, not by the raw string. That also makes the answer independent of
/// which worktree of main is being refreshed. Only main owns nested repos, so a
/// non-main checkout has none to exclude.
fn refresh_submodules(
    ws: &Workspace,
    project: &Project,
    name: &str,
    git: &Repository,
) -> Result<()> {
    let mut declared = BTreeSet::new();
    if name == "main" {
        let root = layout::nesting_root(ws, project, "main")?;
        for (repo_name, repo) in project.repos() {
            if repo_name == "main" || repo.kind() == wits_project::Kind::Standalone {
                continue;
            }
            let path = layout::primary_path(ws, project, repo_name).with_context(|| {
                format!("cannot resolve path of declared nested repo '{repo_name}'")
            })?;
            if let Ok(relative) = path.strip_prefix(&root) {
                if let Some(relative) = normalize_relative(relative) {
                    declared.insert(relative);
                }
            }
        }
    }

    let subs: Vec<String> = git
        .materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .filter(|p| normalize_relative(Path::new(p)).is_none_or(|p| !declared.contains(&p)))
        .collect();
    worktree::feed_submodule_stores(git.path(), &subs);
    git.submodule_update(&subs, false)?;
    Ok(())
}

/// Collapse `.` and `..` lexically, so a templated `"./sub"`/`"x/../sub"` names
/// the same submodule path git reports. This is not filesystem resolution; the
/// paths are already relative to the same checkout. `None` when `..` escapes
/// that root, in which case the path is not a nested repo under `repos.main`.
fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// Make every **declared** remote match its declaration, and touch nothing else.
///
/// Two halves of one rule. Over the names the config file mentions, the file wins
/// outright: a URL that has drifted is corrected, a push-URL set that has drifted
/// is replaced. Over every other remote the file says nothing, so neither do we —
/// a remote added by hand is left exactly as it was, not pruned for being
/// undeclared.
///
/// Convergence rather than addition is also what keeps repeated runs honest; the
/// reasoning is in [`Repository::reconcile_remote`], which does the writing.
///
/// A re-pointed remote is prune-fetched here rather than in [`default_update`],
/// because the repair belongs next to the thing that invalidated the refs: this
/// runs for every repo, while the default action can be replaced wholesale by a
/// `hooks.update` override. Best-effort, since the new URL may be unreachable and
/// a stale tracking namespace is not worth failing an update over — the refs that
/// remain are merely old, which is the state they were already in.
fn ensure_remotes(git: &Repository, repo: &RepoDecl) -> Result<()> {
    for (name, desired) in remotes::desired(repo) {
        if git.reconcile_remote(&name, &desired.url, &desired.push_urls)? {
            if let Err(e) = git.fetch(&["--prune", &name]) {
                log::warn!(
                    "remote '{name}' now points elsewhere but could not be re-fetched ({e}); \
                     its remote-tracking refs still describe the previous repository"
                );
            }
        }
    }
    Ok(())
}

fn run_hook(
    ws: &Workspace,
    project: &Project,
    repo: &str,
    phase: HookPhase,
    cwd: Option<&Path>,
) -> Result<()> {
    if let Some(script) = project.render_hook(ws, repo, phase)? {
        run_script(cwd, &script, phase.as_str())?;
    }
    Ok(())
}

/// Run a templated hook via `sh -c`. `cwd` selects the working directory;
/// `None` inherits the process's, which is where a `clone` override runs since
/// the repo path does not exist yet. `post_clone` and every update-phase hook
/// run in the repo path itself. A non-zero exit fails fast.
fn run_script(cwd: Option<&Path>, script: &str, phase: &str) -> Result<()> {
    log::info!("hook {phase}: {script}");
    let mut cmd = wits_core::process::Command::new("sh");
    cmd.args(["-c".to_string(), script.to_owned()]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let code = cmd.status()?;
    if code != 0 {
        anyhow::bail!("{phase} hook failed (exit {code})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A declared nested repo is identified by its resolved path. A templated
    /// `path` must still keep its submodule out of the undeclared-pin refresh;
    /// otherwise `update` detaches it back to the superproject's recorded commit.
    #[test]
    fn a_templated_nested_repo_is_not_refreshed_to_its_pin() {
        let root = tempfile::tempdir().unwrap();
        let subsrc = root.path().join("subsrc");
        std::fs::create_dir(&subsrc).unwrap();
        git(&subsrc, &["init", "-q", "-b", "main", "."]);
        std::fs::write(subsrc.join("file"), "one").unwrap();
        git(&subsrc, &["add", "file"]);
        git(&subsrc, &["commit", "-q", "-m", "c1"]);
        std::fs::write(subsrc.join("file"), "two").unwrap();
        git(&subsrc, &["commit", "-q", "-am", "c2"]);

        let main = root.path().join("main");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main", "."]);
        std::fs::write(main.join("top"), "x").unwrap();
        git(&main, &["add", "top"]);
        git(&main, &["commit", "-q", "-m", "top"]);
        git(
            &main,
            &["submodule", "add", "-q", subsrc.to_str().unwrap(), "p"],
        );
        git(
            &main,
            &["submodule", "add", "-q", subsrc.to_str().unwrap(), "u"],
        );
        git(&main, &["commit", "-q", "-am", "submodules"]);

        let pinned = git(&main.join("u"), &["rev-parse", "HEAD"]);
        git(&main.join("p"), &["checkout", "-q", "HEAD~1"]);
        git(&main.join("u"), &["checkout", "-q", "HEAD~1"]);
        let moved = git(&main.join("p"), &["rev-parse", "HEAD"]);
        assert_ne!(pinned, moved);

        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        std::fs::write(
            config.join("p.toml"),
            format!(
                r#"
[project]
[repos.main]
path = "{}"
main_branch = "main"
[repos.sub]
path = "{{{{project.name}}}}"
main_branch = "main"
anchor = "main"
"#,
                main.display()
            ),
        )
        .unwrap();
        let ws = Workspace::load_from(&config).unwrap();
        let project = ws.project("p").unwrap();

        refresh_submodules(&ws, project, "main", &Repository::new(&main)).unwrap();

        assert_eq!(
            git(&main.join("p"), &["rev-parse", "HEAD"]),
            moved,
            "the declared nested repo must keep its own checkout"
        );
        assert_eq!(
            git(&main.join("u"), &["rev-parse", "HEAD"]),
            pinned,
            "the undeclared submodule must follow its recorded pin"
        );
    }
}
