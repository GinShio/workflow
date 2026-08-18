//! The working-tree porcelain: worktrees, stashes, submodules, branch switches,
//! sparse cones, and clone — the wide, mutation-heavy surface the
//! `project`/`build`/`update` actions drive. Its mutations stream to the
//! terminal so progress shows live; its reads answer even under dry-run. See the
//! [module overview](super).

use std::path::Path;

use super::{GitError, Repository};
use crate::process::Command;

/// One submodule as `.gitmodules` declares it.
///
/// Both fields are carried because they are **not** interchangeable. The working
/// tree lives at `path`, but git files the object store under `name`:
/// `submodule_name_to_gitdir()` in git's `submodule.c` builds
/// `<git-dir>/modules/<name>`, and the recursive alternate chaining
/// (`submodule.alternateLocation=superproject`) derives every nested level the
/// same way. `git submodule add` defaults the name to the path, so the two
/// coincide in most repositories — which is what makes a lookup keyed by the
/// wrong one fail only in the few that renamed, and fail silently, as a borrow
/// that quietly becomes a full download.
#[derive(Debug, Clone)]
pub struct Submodule {
    pub name: String,
    pub path: String,
}

/// One `git worktree list` entry, as far as the porcelain reports it.
#[derive(Debug, Default, Clone)]
pub struct Worktree {
    pub path: std::path::PathBuf,
    /// The checked-out branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// HEAD's commit.
    pub head: Option<String>,
    /// A bare repository, which has no working tree of its own. Distinct from a
    /// detached HEAD, which also reports no branch.
    pub bare: bool,
    /// Locked against automatic pruning (`git worktree lock`).
    pub locked: bool,
    /// The reason given to `git worktree lock --reason`, when there was one.
    pub lock_reason: Option<String>,
    /// git considers the entry stale — its directory is gone, so only the
    /// administrative record under `.git/worktrees/` remains.
    pub prunable: bool,
}

impl Repository {
    // -- working-tree reads ---------------------------------------------------

    /// Submodules recorded in `.gitmodules`, restricted to those that are
    /// materialised on disk (a sparse checkout may omit some).
    pub fn materialised_submodules(&self) -> Vec<Submodule> {
        let Some(out) = self.query(&["config", "--file", ".gitmodules", "--get-regexp", "path"])
        else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let (key, path) = line.split_once(' ')?;
                // `submodule.<name>.path`, where the name may itself contain
                // dots and slashes (amdvlk declares `icd/api/compiler`), so the
                // fixed ends are trimmed rather than the key split on '.'.
                let name = key.strip_prefix("submodule.")?.strip_suffix(".path")?;
                Some(Submodule {
                    name: name.to_owned(),
                    path: path.trim().to_owned(),
                })
            })
            .filter(|sub| self.path().join(&sub.path).exists())
            .collect()
    }

    /// The main worktree's *working tree*, stable no matter which worktree we are
    /// invoked from, or `None` for a **bare** repository — which has no working
    /// tree at all, and whose caller should fall back to the common git-dir.
    ///
    /// `git worktree list` is no help here: for a repository that is itself a
    /// **submodule**, it reports that repo's main worktree as its *git-dir*
    /// (`<super>/.git/modules/<name>`), not its working tree — anchoring there
    /// would bury a sibling worktree inside `.git/modules`. Instead:
    ///
    /// - a **bare** repo is rejected up front, read from the *common* config
    ///   because `rev-parse --is-bare-repository` answers for the *current*
    ///   worktree and so reports `false` from inside a linked worktree of a bare
    ///   repo — where the fall-through below would otherwise hand back the
    ///   directory merely *containing* the repository;
    /// - in the **main** worktree (`--git-dir` == `--git-common-dir`) the working
    ///   tree is exactly `--show-toplevel`, correct for a normal repo *and* a
    ///   submodule (whose working tree lives outside its git-dir);
    /// - in a **linked** worktree the main worktree's working tree is derived
    ///   from the common git-dir: a submodule records it as `core.worktree`
    ///   (relative to the common dir), and a normal repo leaves it unset, where
    ///   the working tree is the parent of `<main>/.git`.
    pub fn main_worktree(&self) -> Option<std::path::PathBuf> {
        let common = self.git_common_dir()?;
        if self.is_bare_repo(&common) {
            return None;
        }
        if self.git_dir().as_ref() == Some(&common) {
            return self.toplevel();
        }
        // A linked worktree: never anchor off the *current* toplevel (that is
        // what makes review worktrees nest under one another), nor off git
        // worktree list (a git-dir for a submodule). The common config's
        // `core.worktree` points at the main working tree for a submodule; a
        // normal repo has none, so `<main>/.git` → `<main>`.
        match self.config_file_get(&common.join("config"), "core.worktree") {
            Some(worktree) => Some(normalize_path(common.join(worktree))),
            None => common.parent().map(std::path::Path::to_path_buf),
        }
    }

    /// Is the repository itself bare? Read from the **common** config, which is
    /// the repo-wide truth: a linked worktree of a bare repo does have a working
    /// tree of its own, so the per-worktree question answers `false` there.
    fn is_bare_repo(&self, common_dir: &std::path::Path) -> bool {
        // Enabling per-worktree config (which sparse-checkout does) migrates
        // `core.bare` from `config` to the common `config.worktree`. Read both:
        // the latter is still repository-wide for the bare administrative
        // entry, unlike `<common>/worktrees/<id>/config.worktree`.
        ["config", "config.worktree"].iter().any(|name| {
            self.config_file_get(&common_dir.join(name), "core.bare")
                .as_deref()
                == Some("true")
        })
    }

    /// Whether the shared repository is bare, even when queried from one of its
    /// linked worktrees (where Git's per-worktree predicate reports false).
    pub fn is_bare(&self) -> bool {
        self.git_common_dir()
            .is_some_and(|common| self.is_bare_repo(&common))
    }

    /// Read a single value from an *explicit* config file rather than the repo's
    /// resolved config — the way to reach the **common** config from inside a
    /// linked worktree (whose own resolved config may shadow it).
    fn config_file_get(&self, file: &std::path::Path, key: &str) -> Option<String> {
        let file = file.to_string_lossy();
        self.query(&["config", "--file", &file, "--get", key])
    }

    /// Every worktree of this repository, main first (git's own order).
    ///
    /// Parsed line-by-line rather than by splitting the porcelain into records:
    /// a `locked`/`prunable` *reason* is free text that plain `--porcelain` does
    /// not escape (only the `-z` form does — see git 2.36's
    /// `worktree list --porcelain -z`), so a reason containing a newline would
    /// break record splitting. Treating `worktree ` as the only line that starts
    /// an entry degrades that case to a stray line we ignore, rather than an
    /// entry attributed to the wrong path. We read the flags, never the reasons.
    pub fn worktrees(&self) -> Vec<Worktree> {
        let Some(out) = self.query(&["worktree", "list", "--porcelain"]) else {
            return Vec::new();
        };
        let mut result: Vec<Worktree> = Vec::new();
        for line in out.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                result.push(Worktree {
                    path: std::path::PathBuf::from(path),
                    ..Worktree::default()
                });
                continue;
            }
            let Some(current) = result.last_mut() else {
                continue;
            };
            if let Some(branch) = line.strip_prefix("branch ") {
                current.branch = Some(branch.trim_start_matches("refs/heads/").to_owned());
            } else if let Some(head) = line.strip_prefix("HEAD ") {
                current.head = Some(head.to_owned());
            } else if line == "bare" {
                current.bare = true;
            } else if let Some(reason) = line.strip_prefix("locked") {
                current.locked = true;
                let reason = reason.trim();
                current.lock_reason = (!reason.is_empty()).then(|| reason.to_owned());
            } else if line == "prunable" || line.starts_with("prunable ") {
                current.prunable = true;
            }
        }
        result
    }

    /// Is `sparse-checkout` active for this checkout?
    pub fn is_sparse(&self) -> bool {
        self.query(&["config", "--bool", "core.sparseCheckout"])
            .as_deref()
            == Some("true")
    }

    /// The active sparse-checkout patterns (empty if not sparse).
    pub fn sparse_list(&self) -> Vec<String> {
        self.query(&["sparse-checkout", "list"])
            .map(|s| s.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The index tag and path of every entry matching `pathspecs`, from `git
    /// ls-files -t`. The tag is what tells a sparse-excluded entry (`S`, for
    /// skip-worktree) from an ordinary cached one (`H`) — the only reliable way
    /// to ask whether a path is *meant* to be absent rather than merely missing.
    pub fn ls_files_status(&self, pathspecs: &[String]) -> Vec<(char, String)> {
        let mut args = vec!["ls-files", "-t", "--"];
        let specs: Vec<&str> = pathspecs.iter().map(String::as_str).collect();
        args.extend(specs);
        let Some(out) = self.query(&args) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let (tag, path) = line.split_once(' ')?;
                Some((tag.chars().next()?, path.to_owned()))
            })
            .collect()
    }

    // -- working-tree mutations (streamed) ------------------------------------

    pub fn switch(&self, branch: &str) -> Result<(), GitError> {
        self.stream(&format!("switch to {branch}"), &["switch", branch])
    }

    /// Stash the working tree (including untracked). Returns whether anything was
    /// stashed, so a caller only pops when it pushed.
    pub fn stash_push(&self, message: &str) -> Result<bool, GitError> {
        if !self.is_dirty() {
            return Ok(false);
        }
        self.stream(
            "stash",
            &["stash", "push", "--include-untracked", "--message", message],
        )?;
        Ok(true)
    }

    pub fn stash_pop(&self) -> Result<(), GitError> {
        self.stream("stash pop", &["stash", "pop"])
    }

    pub fn fetch(&self, args: &[&str]) -> Result<(), GitError> {
        let mut all = vec!["fetch"];
        all.extend_from_slice(args);
        self.stream("fetch", &all)
    }

    pub fn merge_ff_only(&self, rev: &str) -> Result<(), GitError> {
        self.stream(
            &format!("fast-forward to {rev}"),
            &["merge", "--ff-only", rev],
        )
    }

    pub fn ensure_remote(&self, name: &str, url: &str) -> Result<(), GitError> {
        if self.remote_url(name).is_none() {
            self.stream(&format!("add remote {name}"), &["remote", "add", name, url])?;
        }
        self.ensure_fetch_refspec(name)
    }

    /// Make sure `name` maps the remote's branches into remote-tracking refs.
    ///
    /// `git remote add` writes this refspec, so for most repositories the check
    /// is a formality. `git clone --bare` is the exception that makes it
    /// necessary: it writes **none** (git's `clone.c` writes the refspec only for
    /// a non-bare or `--mirror` clone), having already mapped the remote's
    /// `refs/heads/*` straight onto the local `refs/heads/*`. The result is a
    /// repository where `git fetch <remote>` updates nothing at all — it has no
    /// refspec to fetch by, so it can only report into `FETCH_HEAD` — while every
    /// branch the remote had masquerades as a local one.
    ///
    /// Adding the refspec is additive and idempotent, so it repairs such a
    /// repository in place: from here on a plain fetch answers with
    /// `refs/remotes/<name>/*`, which is what upstream-tracking, `origin/HEAD`,
    /// and therefore trunk detection all read. It cannot un-invent the local
    /// branches an earlier `clone --bare` created; see the migration note in
    /// `docs/worktree.md`.
    pub fn ensure_fetch_refspec(&self, name: &str) -> Result<(), GitError> {
        if !self
            .get_config_all(&format!("remote.{name}.fetch"))
            .is_empty()
        {
            return Ok(());
        }
        self.capture(
            format!("add fetch refspec to {name}"),
            &[
                "config",
                &format!("remote.{name}.fetch"),
                &format!("+refs/heads/*:refs/remotes/{name}/*"),
            ],
            false,
        )
    }

    pub fn ensure_push_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        // Compare against the *raw* configured push URLs (`git config`), never
        // `git remote get-url`, whose output is rewritten by `url.*.insteadOf`.
        // An exact-string guard on the rewritten form never matches the declared
        // URL, so every run re-`--add`s it — the runaway pile of push URLs.
        let configured = self.get_config_all(&format!("remote.{name}.pushurl"));
        if !configured.iter().any(|u| u == url) {
            self.stream(
                &format!("add push url to {name}"),
                &["remote", "set-url", "--add", "--push", name, url],
            )?;
        }
        Ok(())
    }

    pub fn submodule_update(&self, paths: &[String], init: bool) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec!["submodule", "update", "--recursive"];
        if init {
            args.push("--init");
        }
        args.push("--");
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        args.extend(path_refs);
        self.stream("submodule update", &args)
    }

    /// Move one already-materialised submodule to the commit this checkout's
    /// index pins, **without descending** into its own submodules.
    ///
    /// The shallow counterpart to [`submodule_update`], for a caller walking the
    /// nesting itself because each level needs its own decision — which is what
    /// [`crate::worktree::sync_submodules`] does, since every level borrows from a
    /// different object store.
    pub fn submodule_follow_pin(&self, path: &str) -> Result<(), GitError> {
        self.stream(
            &format!("submodule update {path}"),
            &["submodule", "update", "--", path],
        )
    }

    /// Register one submodule's URL in this repository's config
    /// (`git submodule init`), without cloning or checking anything out.
    ///
    /// The point is the *resolution*, not the registration: `.gitmodules` may
    /// record a **relative** URL (`../sibling.git`), and turning that into
    /// something clonable means resolving it against the superproject's own
    /// remote — with `.gitmodules` overridable per-repository along the way. git
    /// already does exactly that here, offline and instantly, and writes the
    /// answer to `submodule.<name>.url`. So a caller that needs the URL (to seed
    /// an object store from it, say) asks git rather than reimplementing a
    /// resolution it would get subtly wrong.
    pub fn submodule_register(&self, path: &str) -> Result<(), GitError> {
        self.stream(
            &format!("submodule init {path}"),
            &["submodule", "init", "--", path],
        )
    }

    /// Clear submodules' working trees and drop their `submodule.<name>.url`
    /// registration, leaving the gitlinks in the index and the tree clean.
    ///
    /// This deletes checked-out content, so only a caller that *built* the tree
    /// it is trimming has any business calling it — in practice the `clone`
    /// lifecycle applying a repo's `skip`. `-f` is unconditional for the same
    /// reason: a tree we just created cannot hold work worth refusing over.
    pub fn submodule_deinit(&self, paths: &[String]) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec!["submodule", "deinit", "-f", "--"];
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        args.extend(path_refs);
        self.stream("submodule deinit", &args)
    }

    /// Replace the sparse-checkout patterns and apply them to the working tree.
    ///
    /// Always `--no-cone`: cone mode cannot express an exclusion, which is the
    /// only thing these patterns are ever used for here.
    pub fn sparse_set(&self, patterns: &[String]) -> Result<(), GitError> {
        if patterns.is_empty() {
            return Ok(());
        }
        let mut args = vec!["sparse-checkout", "set", "--no-cone"];
        let refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
        args.extend(refs);
        self.stream("sparse-checkout set", &args)
    }

    /// Init-and-update **one** submodule, borrowing its objects from `reference`.
    ///
    /// The borrowing is unconditional and self-completing:
    /// - `--reference <store>` aims this submodule's clone at the store the
    ///   repository keeps for it;
    /// - `submodule.alternateLocation=superproject` **chains** the borrow down to
    ///   anything nested that git materialises under it, deriving each level as
    ///   `<parent-store>/modules/<name>` — which is why the store layout must be
    ///   keyed by [`Submodule::name`] and nested exactly the way git nests it;
    /// - `submodule.alternateErrorStrategy=info` degrades a *missing* store to a
    ///   note plus a normal fetch rather than an error, so a level no store covers
    ///   simply downloads — the graceful fallback, baked in.
    ///
    /// **One level only**, deliberately: `--reference` names a single repository,
    /// and a nested submodule's store is a different one. Recursing here would
    /// materialise the deeper levels before their stores exist, and each would
    /// then download a full copy of what the repository was about to own. The
    /// walk therefore belongs to the caller, which interleaves it with seeding —
    /// see [`crate::worktree::sync_submodules`].
    ///
    /// There is deliberately **no `--dissociate`**. Dissociating copies the
    /// borrowed objects into each new store, which is the whole cost borrowing
    /// exists to avoid; it was only ever needed when the reference could vanish.
    /// Callers must therefore pass a store the *repository* owns rather than one
    /// belonging to a removable worktree — borrowing from a worktree and then
    /// removing it leaves the borrower with an unreadable alternate and an
    /// unusable submodule.
    ///
    /// Never shallow: borrowing removes the size pressure that would motivate
    /// `--depth`, which only buys fragility (a shallow boundary, server-dependent
    /// arbitrary-SHA fetches, a broken `git describe`).
    pub fn submodule_init_borrow(
        &self,
        path: &str,
        reference: Option<&Path>,
    ) -> Result<(), GitError> {
        let mut args: Vec<String> = vec![
            "-c".into(),
            "submodule.alternateLocation=superproject".into(),
            "-c".into(),
            "submodule.alternateErrorStrategy=info".into(),
            "submodule".into(),
            "update".into(),
            "--init".into(),
        ];
        if let Some(r) = reference {
            args.push("--reference".into());
            args.push(r.display().to_string());
        }
        args.push("--".into());
        args.push(path.to_owned());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.stream(&format!("submodule init {path}"), &arg_refs)
    }

    /// Add a worktree at `dir` checked out on `rev`.
    ///
    /// There is no `--no-checkout` variant, because the reason to want one is
    /// gone: it existed to add a worktree empty, install sparse-checkout patterns,
    /// and only then populate. Since git 2.36 `worktree add` copies the patterns
    /// itself — before the checkout, and with or without `--no-checkout` — so the
    /// dance replaced a correct one-step operation with a lossier three-step one.
    /// See [`crate::worktree`].
    pub fn worktree_add(&self, dir: &Path, rev: &str) -> Result<(), GitError> {
        let dir_s = dir.display().to_string();
        self.stream(
            &format!("add worktree for {rev}"),
            &["worktree", "add", &dir_s, rev],
        )
    }

    /// Drop the administrative records of worktrees whose directories are gone.
    /// Touches nothing on disk — those directories are already absent.
    pub fn worktree_prune(&self) -> Result<(), GitError> {
        self.stream("prune worktree records", &["worktree", "prune"])
    }

    pub fn worktree_remove(&self, dir: &Path, force: bool) -> Result<(), GitError> {
        let dir_s = dir.display().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&dir_s);
        self.stream("remove worktree", &args)
    }

    pub fn checkout(&self, rev: &str) -> Result<(), GitError> {
        self.stream(&format!("checkout {rev}"), &["checkout", rev])
    }
}

/// Restores a repo to the branch (and stash) it was on when captured, on *any*
/// scope exit — success, `?`-propagated error, or panic. This is the RAII form
/// of the classic stash → switch → build → switch back → pop dance: correctness
/// no longer depends on remembering to restore on every path. Restore is
/// best-effort and logs (Drop cannot return errors), which is the right failure
/// mode — a failed restore should warn, not mask the original error.
pub struct RestoreGuard<'a> {
    repo: &'a Repository,
    original_branch: Option<String>,
    stashed: bool,
}

impl<'a> RestoreGuard<'a> {
    /// Capture the current branch as the state to return to.
    pub fn capture(repo: &'a Repository) -> Self {
        RestoreGuard {
            repo,
            original_branch: repo.current_branch(),
            stashed: false,
        }
    }

    pub fn mark_stashed(&mut self) {
        self.stashed = true;
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(orig) = &self.original_branch {
            if self.repo.current_branch().as_deref() != Some(orig.as_str()) {
                if let Err(e) = self.repo.switch(orig) {
                    log::warn!("could not restore branch {orig}: {e}");
                }
            }
        }
        if self.stashed {
            if let Err(e) = self.repo.stash_pop() {
                log::warn!("could not pop auto-stash: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Borrowing a submodule's objects from a reference store must leave the new
    /// clone with an `alternates` file pointing at that store — the "no
    /// re-download" guarantee `review checkout --submodules` relies on.
    ///
    /// Driven level by level, the way [`crate::worktree::sync_submodules`] drives
    /// it, because that is the contract: this call materialises **one** submodule,
    /// and the store each level borrows from is a different repository.
    #[test]
    fn submodule_init_borrows_objects_via_alternates() {
        let _guard = crate::log::test_flag_guard();
        // The submodule clone `submodule_init_borrow` triggers is a *child* git
        // process, which inherits repo config only via the environment — so the
        // file-protocol allowance (needed for a local test submodule; real ones
        // are https/ssh) and identity go through `GIT_CONFIG_*`, not `-c` on the
        // setup calls. Held under the flag guard so it doesn't race other tests.
        std::env::set_var("GIT_CONFIG_COUNT", "4");
        std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
        std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        std::env::set_var("GIT_CONFIG_KEY_1", "user.email");
        std::env::set_var("GIT_CONFIG_VALUE_1", "t@e.com");
        std::env::set_var("GIT_CONFIG_KEY_2", "user.name");
        std::env::set_var("GIT_CONFIG_VALUE_2", "T");
        // Keep the test hermetic from any globally-installed hooks (a
        // `core.hooksPath` in the user's config would otherwise fire on commits).
        std::env::set_var("GIT_CONFIG_KEY_3", "core.hooksPath");
        std::env::set_var("GIT_CONFIG_VALUE_3", "/nonexistent-wits-test-hooks");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run = |dir: &Path, args: &[&str]| {
            Command::new("git")
                .args(args.iter().copied())
                .current_dir(dir)
                .force_run()
                .exec()
                .unwrap();
        };
        // Two levels of nesting: P -> mid -> leaf, so the recursive borrow's
        // chaining (not just the top level) is exercised.
        let mk = |name: &str| {
            let d = root.join(name);
            run(root, &["init", "-q", "-b", "main", name]);
            std::fs::write(d.join("f"), "v1").unwrap();
            run(&d, &["add", "f"]);
            run(&d, &["commit", "-q", "-m", "c1"]);
            d
        };
        let leaf = mk("leaf");
        let mid = mk("mid");
        run(
            &mid,
            &[
                "submodule",
                "add",
                "-q",
                &format!("file://{}", leaf.display()),
                "leaf",
            ],
        );
        run(&mid, &["commit", "-q", "-m", "add leaf"]);
        let sup = root.join("P");
        run(root, &["init", "-q", "-b", "main", "P"]);
        run(
            &sup,
            &[
                "submodule",
                "add",
                "-q",
                &format!("file://{}", mid.display()),
                "mid",
            ],
        );
        run(&sup, &["commit", "-q", "-m", "add mid"]);
        // Primary initialises the whole tree, so every level's store exists to borrow.
        run(&sup, &["submodule", "update", "--init", "--recursive"]);

        // A linked worktree of the superproject — where a bare update would
        // re-clone every submodule from scratch.
        let wt = root.join("W");
        run(
            &sup,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feat",
                "HEAD",
            ],
        );

        let common = Repository::new(&wt).git_common_dir().unwrap();
        let mid_store = common.join("modules/mid");
        Repository::new(&wt)
            .submodule_init_borrow("mid", Some(&mid_store))
            .unwrap();

        let alternates_of = |sub_rel: &str| {
            let gitdir = Repository::new(wt.join(sub_rel)).git_dir().unwrap();
            std::fs::read_to_string(gitdir.join("objects/info/alternates")).unwrap_or_default()
        };
        let mid_alts = alternates_of("mid");
        assert!(
            mid_alts.contains(mid_store.to_str().unwrap()),
            "the submodule should borrow from {}, got: {mid_alts}",
            mid_store.display()
        );
        // One level only: the nested submodule is the caller's next step, not a
        // side effect of this one, so nothing has been checked out there yet.
        assert!(
            !wt.join("mid/leaf/.git").exists(),
            "the nesting is the caller's to walk"
        );

        // Driving that next level is the same call, from inside the level above
        // and against that level's own store — which is where the chaining would
        // have looked, so the alternates land in the same place either way.
        let leaf_store = mid_store.join("modules/leaf");
        Repository::new(wt.join("mid"))
            .submodule_init_borrow("leaf", Some(&leaf_store))
            .unwrap();
        let leaf_alts = alternates_of("mid/leaf");
        assert!(
            leaf_alts.contains(leaf_store.to_str().unwrap()),
            "the nested submodule should borrow from its own store {}, got: {leaf_alts}",
            leaf_store.display()
        );
    }
}

/// Logically normalize a path, collapsing `.` and `..` without touching the
/// filesystem — so a relative `core.worktree` joined onto the git-dir resolves
/// the same way git resolves it, regardless of symlinks or missing intermediates.
fn normalize_path(path: std::path::PathBuf) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Clone `url` into `dir`, naming the fetched remote `remote`. A free function
/// because there is no repository yet to hang it off. `--origin` lets a repo
/// tracked from `upstream` leave the `origin` name free for a fork that may not
/// exist on the server yet.
///
/// `defer_checkout` clones with `--no-checkout`, leaving the caller to write
/// sparse patterns before materialising anything. That ordering is the whole
/// point: a repo with a `skip` list would otherwise check out the very paths it
/// declares it does not want, only to have them removed again.
pub fn clone(url: &str, remote: &str, dir: &Path, defer_checkout: bool) -> Result<(), GitError> {
    // Inherit stdio so clone progress streams live and in colour.
    let dir_s = dir.display().to_string();
    let mut args = vec!["clone", "--origin", remote];
    if defer_checkout {
        args.push("--no-checkout");
    }
    args.push(url);
    args.push(&dir_s);
    let code = Command::new("git").args(args).status()?;
    if code == 0 {
        Ok(())
    } else {
        Err(GitError::Failed {
            operation: format!("clone {url}"),
            message: format!("exit {code}"),
        })
    }
}

/// Create the shared repository a worktree-backed project hangs its checkouts
/// off: no working tree of its own, tracking `url` under `remote`, with `branch`
/// as the one local branch and the repository's symbolic HEAD.
///
/// **Not `git clone --bare`**, and that is the whole point of this function. A
/// bare clone maps the remote's `refs/heads/*` onto the *local* `refs/heads/*`,
/// so a repository that exists to host worktrees starts life with one local
/// branch per branch anybody ever pushed — thousands, in a shared tree — and
/// nothing distinguishes the branch you are working on from the 2 395 you are
/// not. It also writes no fetch refspec at all, so `git fetch` afterwards
/// updates no ref, and no `origin/HEAD` is published, so nothing can tell what
/// the trunk is. Every one of those is load-bearing here: `refs/heads` is what
/// `git branch`, the worktree inventory, and upstream tracking all read.
///
/// `init` + `remote add` + `fetch` inverts it. The remote's branches land where
/// they belong, in `refs/remotes/<remote>/*`; `remote add` writes the standard
/// refspec, so a later plain `git fetch` keeps them current; and `refs/heads`
/// holds exactly the branches this repository chose to work on — starting with
/// the one the bootstrap worktree checks out, created here so it tracks
/// `<remote>/<branch>`.
pub fn init_bare_host(url: &str, remote: &str, dir: &Path, branch: &str) -> Result<(), GitError> {
    let dir_s = dir.display().to_string();
    let code = Command::new("git")
        .args(["init", "--quiet", "--bare", &dir_s])
        .status()?;
    if code != 0 {
        return Err(GitError::Failed {
            operation: format!("init --bare {dir_s}"),
            message: format!("exit {code}"),
        });
    }

    let repo = Repository::new(dir);
    repo.ensure_remote(remote, url)?;
    // Tags come along because they are how a source tree names its releases, and
    // a host that has the branches but not the tags cannot answer `git describe`.
    repo.fetch(&["--tags", remote])?;
    // git publishes `refs/remotes/<remote>/HEAD` on the first fetch, but only
    // since 2.46; ask explicitly when it is still missing rather than
    // unconditionally, which would print a "unchanged" note on every clone.
    if repo.remote_default_branch(remote).is_none() {
        if let Err(e) = repo.remote_set_head(remote) {
            log::warn!("could not record {remote}'s default branch: {e}");
        }
    }
    repo.create_tracking_branch(branch, &format!("{remote}/{branch}"))?;
    repo.set_head(branch)
}

/// Build a **reference store** at `dir` by downloading `url`: a durable,
/// never-checked-out copy of a submodule for every worktree — including the very
/// first one — to borrow from with `--reference`.
///
/// This is the store's *first* home rather than a copy of somebody else's. That
/// ordering is the point. Left to itself, a linked worktree's `submodule update
/// --init` downloads into `<common>/worktrees/<id>/modules/<name>`, and the only
/// way to give the repository a store afterwards is to copy from there — which
/// leaves the checkout that paid for the download owning a full store of its own,
/// borrowing from nothing, and drifting away from the shared copy the moment
/// either of them fetches. Seeding first means one download, one store, and every
/// checkout borrowing it on equal terms.
///
/// A free function because there is no repository at `dir` yet to hang it off.
pub fn clone_reference_store(url: &str, dir: &Path) -> Result<(), GitError> {
    if crate::log::is_dry_run() {
        crate::log::dry_run(&format!("git clone --bare {url} {}", dir.display()));
        return Ok(());
    }
    publish_store(dir, |staging| {
        let staging_s = staging.display().to_string();
        let code = Command::new("git")
            .args(["clone", "--bare", "--origin", "origin", url, &staging_s])
            .status()?;
        if code != 0 {
            return Err(GitError::Failed {
                operation: format!("clone --bare {url}"),
                message: format!("exit {code}"),
            });
        }
        seal_store(&Repository::new(staging), None)
    })
}

/// Build a reference store at `dir` from the store tree at `src` — the same
/// durable copy [`clone_reference_store`] downloads, when the objects are already
/// on disk somewhere the repository does not own.
///
/// The **whole nested tree** is copied, not just `src`. `clone` takes only
/// `objects` and `refs`, so a lone clone of the top store would leave
/// `<dir>/modules/<nested-name>` absent — and that is precisely where the
/// alternate chaining looks (see [`Repository::submodule_init_borrow`]), so every
/// borrower would silently re-download every level below the first. In the trees
/// this exists for, the deeper levels hold most of the bytes.
///
/// `--local` is what makes this near-free: git hardlinks the object files rather
/// than copying them, so publishing an 8 GiB tree costs inodes and no blocks.
/// `--bare` is then honest about the result, which has no working tree.
pub fn create_reference_store(src: &Path, dir: &Path) -> Result<(), GitError> {
    if crate::log::is_dry_run() {
        crate::log::dry_run(&format!(
            "git clone --bare --local {} {} (with its nested submodule stores)",
            src.display(),
            dir.display()
        ));
        return Ok(());
    }
    publish_store(dir, |staging| clone_store_tree(src, staging))
}

/// Assemble a store in a staging directory and publish it with a single rename,
/// so a half-built one — an interrupted run, or a second `wits` racing this one —
/// is never visible under `dir`.
///
/// That matters more than it looks: a truncated pack behind an alternate is the
/// one failure mode a borrower cannot recover from by fetching, and a *partial*
/// tree would satisfy the "is there a store?" check and so never be rebuilt.
fn publish_store<F>(dir: &Path, build: F) -> Result<(), GitError>
where
    F: FnOnce(&Path) -> Result<(), GitError>,
{
    let parent = dir.parent().ok_or_else(|| GitError::Failed {
        operation: "create reference store".to_owned(),
        message: format!("{} has no parent directory", dir.display()),
    })?;
    ensure_dir(parent)?;

    let leaf = dir
        .file_name()
        .map_or_else(|| "store".to_owned(), |n| n.to_string_lossy().into_owned());
    let staging = parent.join(format!(".{leaf}.{}.incoming", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);

    let built = build(&staging);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return built;
    }

    match std::fs::rename(&staging, dir) {
        Ok(()) => Ok(()),
        // Another process published the same tree first. Its copy is as good as
        // ours by construction — same objects, same layout — so the loser of the
        // race cleans up and reports success rather than failing a borrow that
        // can now go ahead.
        Err(_) if is_object_store(dir) => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(GitError::Failed {
                operation: format!("publishing reference store at {}", dir.display()),
                message: e.to_string(),
            })
        }
    }
}

/// Make a freshly-cloned bare copy fit to be borrowed from: refuse to delete
/// objects, and keep a way to refresh them.
///
/// **Precious objects** makes git refuse `prune` and `repack -d` inside the
/// store. A borrower holds nothing but an `objects/info/alternates` pointer, so
/// an object deleted here corrupts every worktree that borrowed it — the hazard
/// git warns about under `clone --reference`, and `extensions.preciousObjects` is
/// git's own answer to it. `fetch` still works, which is all a reference store
/// ever needs. Undo with `git config --unset extensions.preciousObjects`. The
/// format-version bump goes with it because git only promises to honour
/// `extensions.*` at version 1.
///
/// The **fetch refspec** is what makes that promise true: `clone --bare` writes
/// none, so without this a store could only ever hold the objects it was created
/// with, and a worktree pinned to a commit newer than the store would download a
/// full copy of its own. `upstream` overrides the cloned-from URL, for a store
/// copied out of a *worktree's* administrative directory — a path that will not
/// outlive the worktree, where the submodule's real remote will.
fn seal_store(store: &Repository, upstream: Option<&str>) -> Result<(), GitError> {
    let path = store.path().display().to_string();
    for (key, value) in [
        ("core.repositoryformatversion", "1"),
        ("extensions.preciousObjects", "true"),
    ] {
        store.capture(
            format!("marking {path} precious"),
            &["config", key, value],
            true,
        )?;
    }
    if let Some(url) = upstream {
        store.capture(
            format!("pointing {path} at its upstream"),
            &["remote", "set-url", "origin", url],
            true,
        )?;
    }
    store.ensure_fetch_refspec("origin")
}

/// Copy one store and, beneath it, every nested submodule store, preserving the
/// `modules/<name>` layout the alternate chaining derives.
fn clone_store_tree(src: &Path, dst: &Path) -> Result<(), GitError> {
    if let Some(parent) = dst.parent() {
        ensure_dir(parent)?;
    }
    let src_s = src.display().to_string();
    let dst_s = dst.display().to_string();
    let code = Command::new("git")
        .args(["clone", "--bare", "--local", &src_s, &dst_s])
        .status()?;
    if code != 0 {
        return Err(GitError::Failed {
            operation: format!("clone --bare --local {src_s}"),
            message: format!("exit {code}"),
        });
    }
    // `clone --local` aims the new store's origin at the *source* path — a
    // worktree's administrative directory, which will not outlive the worktree.
    // The source was itself cloned from the submodule's real upstream, so carry
    // that across. Serving a borrow never fetches, but a store whose origin is a
    // dead path cannot be refreshed either, and a wrong remote is worse than none.
    let upstream = Repository::new(src)
        .get_config("remote.origin.url")
        .ok()
        .flatten();
    seal_store(&Repository::new(dst), upstream.as_deref())?;

    let (src_modules, dst_modules) = (src.join("modules"), dst.join("modules"));
    for nested in nested_stores(&src_modules) {
        clone_store_tree(&src_modules.join(&nested), &dst_modules.join(&nested))?;
    }
    Ok(())
}

/// The submodule stores directly under a `modules` directory, as paths relative
/// to it.
///
/// A search rather than a directory listing because a submodule *name* may
/// contain slashes — amdvlk declares `icd/api/compiler`, so its store sits three
/// levels down and `icd` itself is only a shared prefix with no `objects` of its
/// own. The descent stops at the first store on each branch: what is inside one
/// is its own nesting, which the recursive caller handles, and descending further
/// here would also walk into `objects/` and `refs/`, where a ref namespace
/// happening to be called `objects` would look like a store.
fn nested_stores(modules: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![std::path::PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let dir = modules.join(&rel);
        if !rel.as_os_str().is_empty() && is_object_store(&dir) {
            found.push(rel);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(rel.join(entry.file_name()));
            }
        }
    }
    found
}

fn ensure_dir(dir: &Path) -> Result<(), GitError> {
    std::fs::create_dir_all(dir).map_err(|e| GitError::Failed {
        operation: format!("creating {}", dir.display()),
        message: e.to_string(),
    })
}

/// Whether `dir` is a git object store something can borrow from with
/// `--reference` — a repository carrying an `objects` directory. A plain
/// intermediate directory (a nested submodule name's parent, `icd` for
/// `icd/api/compiler`) is not, and must never reach `--reference`.
pub fn is_object_store(dir: &Path) -> bool {
    dir.join("objects").is_dir()
}
