//! The trunk of a checkout: the branch its work merges into.
//!
//! Every tool that asks gets this one answer, so that `stack` (the base a root MR
//! targets), `worktree` (what "merged" is measured against) and the git hooks
//! (git-branchless's main branch, `absorb-stack`'s floor) cannot disagree:
//!
//! - the **name** is the `main_branch` the owning repo declares, for a checkout a
//!   project owns; else the merge target's default branch, read from its remote
//!   HEAD; else the first of `main`, `master`, `trunk` that exists, locally or as
//!   one of the merge target's tracking refs;
//! - the **revision** is the merge target's tracking ref of that name when it is
//!   fetched, else the local branch.
//!
//! The declaration comes first because it is the user's statement of the
//! project's trunk, while a remote HEAD is only the server's default; the two
//! part ways for a project developed on a release branch. The tracking ref beats
//! the local branch because the remote decides what has landed, and a local
//! trunk may be days behind. Only the merge target is asked: the push side's
//! default branch answers a different question, and a fork's own trunk is
//! usually as old as the fork.

use crate::git::Repository;
use crate::remote::RemoteRoles;

/// Where a checkout's work merges into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trunk {
    /// The branch name: what an MR at the root of a stack targets on the forge,
    /// and what git-branchless records as the main branch.
    pub name: String,
    /// The revision that says where the trunk is, for ancestry questions:
    /// `<merge target>/<name>` when fetched, else the local branch. `None` when
    /// neither exists here — a declared trunk not fetched yet.
    pub rev: Option<String>,
}

/// Resolve a checkout's trunk from its roles and the `main_branch` its owning
/// repo declares (module docs). `None` when no rule names a branch.
pub fn resolve(repo: &Repository, roles: &RemoteRoles, declared: Option<&str>) -> Option<Trunk> {
    let target = roles.merge_target();
    let tracking = |name: &str| {
        target
            .map(|remote| format!("{remote}/{name}"))
            .filter(|rev| repo.rev_exists(rev))
    };
    let name = declared
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| target.and_then(|remote| repo.remote_default_branch(remote)))
        .or_else(|| {
            ["main", "master", "trunk"]
                .into_iter()
                .find(|name| repo.local_branch_exists(name) || tracking(name).is_some())
                .map(str::to_owned)
        })?;
    let rev = tracking(&name).or_else(|| repo.local_branch_exists(&name).then(|| name.clone()));
    Some(Trunk { name, rev })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Run git in `dir` with an identity and no hooks, so the test depends on
    /// nothing in the machine's own git config.
    fn git(dir: &Path, args: &[&str]) {
        let mut all = vec![
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e.com",
            "-c",
            "core.hooksPath=/nonexistent-wits-test-hooks",
        ];
        all.extend_from_slice(args);
        crate::process::Command::new("git")
            .args(all)
            .current_dir(dir)
            .force_run()
            .exec()
            .unwrap();
    }

    /// A repository on `main` with one commit, and the given remotes, whose
    /// tracking refs are made by hand rather than fetched.
    fn repo(remotes: &[&str]) -> (tempfile::TempDir, Repository) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main", "."]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "c"]);
        for remote in remotes {
            git(dir, &["remote", "add", remote, "https://example.com/r.git"]);
        }
        let repo = Repository::new(dir);
        (tmp, repo)
    }

    fn track(dir: &Path, remote: &str, branch: &str) {
        git(
            dir,
            &[
                "update-ref",
                &format!("refs/remotes/{remote}/{branch}"),
                "HEAD",
            ],
        );
    }

    fn set_head(dir: &Path, remote: &str, branch: &str) {
        git(
            dir,
            &[
                "symbolic-ref",
                &format!("refs/remotes/{remote}/HEAD"),
                &format!("refs/remotes/{remote}/{branch}"),
            ],
        );
    }

    fn trunk(name: &str, rev: Option<&str>) -> Option<Trunk> {
        Some(Trunk {
            name: name.into(),
            rev: rev.map(str::to_owned),
        })
    }

    #[test]
    fn a_declared_trunk_wins_over_the_remote_head() {
        let (tmp, repo) = repo(&["upstream"]);
        track(tmp.path(), "upstream", "main");
        track(tmp.path(), "upstream", "stable");
        set_head(tmp.path(), "upstream", "main");
        let roles = RemoteRoles::from_git(&repo);
        assert_eq!(
            resolve(&repo, &roles, Some("stable")),
            trunk("stable", Some("upstream/stable"))
        );
    }

    #[test]
    fn undeclared_it_is_the_merge_targets_remote_head_and_its_tracking_ref() {
        let (tmp, repo) = repo(&["origin", "upstream"]);
        track(tmp.path(), "origin", "dev");
        set_head(tmp.path(), "origin", "dev");
        track(tmp.path(), "upstream", "main");
        set_head(tmp.path(), "upstream", "main");
        let roles = RemoteRoles::from_git(&repo);
        assert_eq!(
            resolve(&repo, &roles, None),
            trunk("main", Some("upstream/main"))
        );
    }

    #[test]
    fn the_push_sides_head_is_never_asked() {
        // A fork: origin's HEAD names its own branch, the merge target has none.
        let (tmp, repo) = repo(&["origin", "upstream"]);
        track(tmp.path(), "origin", "dev");
        set_head(tmp.path(), "origin", "dev");
        let roles = RemoteRoles::from_git(&repo);
        assert_eq!(resolve(&repo, &roles, None), trunk("main", Some("main")));
    }

    #[test]
    fn a_conventional_name_counts_when_only_the_merge_target_tracks_it() {
        let (tmp, repo) = repo(&["upstream"]);
        git(tmp.path(), &["branch", "-q", "-m", "work"]);
        track(tmp.path(), "upstream", "master");
        let roles = RemoteRoles::from_git(&repo);
        assert_eq!(
            resolve(&repo, &roles, None),
            trunk("master", Some("upstream/master"))
        );
    }

    #[test]
    fn a_declared_trunk_not_fetched_yet_has_a_name_and_no_revision() {
        let (_tmp, repo) = repo(&["upstream"]);
        let roles = RemoteRoles::from_git(&repo);
        assert_eq!(
            resolve(&repo, &roles, Some("release")),
            trunk("release", None)
        );
    }

    #[test]
    fn nothing_to_go_on_resolves_to_none() {
        let (tmp, repo) = repo(&[]);
        git(tmp.path(), &["branch", "-q", "-m", "work"]);
        assert_eq!(resolve(&repo, &RemoteRoles::default(), None), None);
        // An empty declaration — a subtree's — is no declaration.
        assert_eq!(resolve(&repo, &RemoteRoles::default(), Some("")), None);
    }
}
