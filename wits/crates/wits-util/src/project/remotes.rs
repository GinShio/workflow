//! Turning a repo's declared remotes into roles, and answering the role question
//! for any checkout at all.
//!
//! The vocabulary itself lives in [`crate::remote`]; this is where a *user's
//! declaration* of it is read, checked, and — for the write side — turned into the
//! state a checkout should reach.
//!
//! # Declaration is the single source of truth
//!
//! Nothing is written into a repository to record which remote holds which role.
//! That is deliberate: a materialised copy of the mapping is a second source of
//! truth, and the drift between the two would have to be adjudicated on every
//! read. Instead the config file answers directly, and a checkout that no project
//! declares answers by the privileged names alone
//! ([`RemoteRoles::from_git`](crate::remote::RemoteRoles::from_git)).
//!
//! [`for_checkout`] routes between those two, which is what lets `stack` and
//! `review` serve a hand-cloned repo and a declared project through one call.
//! The routing has **three** outcomes, and collapsing the last two would be a
//! correctness bug rather than a shortcut:
//!
//! - the registry loads and owns this path → the declaration decides;
//! - the registry loads and does not own it → the privileged names decide;
//! - the registry cannot be read → an error, because a broken file belonging to
//!   some unrelated project must not silently change which remote we push to in a
//!   checkout that is perfectly fine.
//!
//! The first outcome has one sub-case, in [`roles_in_force`]: a declaration that
//! assigns no role at all falls through to the privileged names, since it has not
//! answered the question. That is all-or-nothing by role-set rather than per role,
//! and the reasoning — along with what it still papers over — is on that function.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use super::model::{RawRemote, RawRepo};
use super::workspace::Workspace;
use crate::git::Repository;
use crate::remote::{RemoteRoles, Role};

/// Resolve the roles a repo's declaration assigns.
///
/// A remote holds a role either by declaring one or by carrying that role's
/// privileged name. Validation belongs to [`validate`], which the loader runs, so
/// this is free to take the first holder it finds for each role.
pub fn roles_of(repo: &RawRepo) -> RemoteRoles {
    let holder = |role: Role| {
        repo.remotes
            .iter()
            .find(|(name, remote)| match remote.role {
                Some(declared) => declared == role,
                None => name.as_str() == role.as_str(),
            })
            .map(|(name, _)| name.clone())
    };
    RemoteRoles::new(holder(Role::Origin), holder(Role::Upstream))
}

/// Check a repo's declared remotes, as the loader does for every repo it ingests.
///
/// Three rules, each closing a way the name/role overlap could otherwise go wrong.
/// They are checked here rather than in `--check` because all three are facts of
/// the file, needing nothing from disk to settle.
pub fn validate(repo_name: &str, repo: &RawRepo) -> Result<()> {
    for (name, remote) in &repo.remotes {
        if remote.url.trim().is_empty() {
            bail!("repo '{repo_name}', remote '{name}': url is empty");
        }
        // A remote named after one role but claiming the other is legal to write
        // and impossible to read: git and every other tool would take the name at
        // face value while we took the declaration. Rejecting it costs nothing,
        // since a remote may always simply be named after the role it holds.
        if let (Some(declared), Some(by_name)) = (remote.role, role_named(name)) {
            if declared != by_name {
                bail!(
                    "repo '{repo_name}', remote '{name}': named after the '{}' role but declares \
                     role '{}' — rename the remote or drop the declaration",
                    by_name.as_str(),
                    declared.as_str()
                );
            }
        }
    }

    for role in Role::ALL {
        let holders: Vec<&str> = repo
            .remotes
            .iter()
            .filter(|(name, remote)| match remote.role {
                Some(declared) => declared == role,
                None => name.as_str() == role.as_str(),
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if holders.len() > 1 {
            bail!(
                "repo '{repo_name}': remotes {} all hold the '{}' role, which admits one",
                holders.join(", "),
                role.as_str()
            );
        }
    }
    Ok(())
}

/// The role a remote's *name* claims, if it is one of the privileged names.
fn role_named(name: &str) -> Option<Role> {
    Role::ALL.into_iter().find(|r| r.as_str() == name)
}

/// The roles in force for a checkout whose declaration is already in hand.
///
/// **All-or-nothing, by role-set rather than per role.** A declaration that names
/// any holder has answered for both roles, so a repo declaring only `origin` has
/// no `upstream` — not even if the checkout has a remote called that. Merging the
/// two authorities per role would mean a hand-added remote could silently become
/// the merge target of a declared project, which is the drift the whole
/// declaration-is-truth rule exists to prevent.
///
/// The fallback serves the declaration that assigns *no* role, which is not the
/// same as a repo with no remotes: a repo may be declared purely for its path and
/// build settings (amdllpc's `repos.review`), and then there are real remotes and
/// nothing declared to read. Those answer by the checkout's own privileged names —
/// the same rule that serves a repo no project declares.
///
/// Where that fires today it gets the right answer only because the repo shares a
/// checkout with one that *does* declare remotes, and that sibling's reconcile
/// created a remote literally named `origin`. Rename such a project's remotes to
/// free names and the shared checkout has no privileged name left to read. The
/// real missing concept is "two declared repos resolving to one git repository",
/// which nothing models yet — see the `update` design notes.
pub fn roles_in_force(repo: &RawRepo, checkout: &Repository) -> RemoteRoles {
    let declared = roles_of(repo);
    if declared == RemoteRoles::default() {
        RemoteRoles::from_git(checkout)
    } else {
        declared
    }
}

/// The roles in force for a checkout, found by asking whichever authority owns it.
///
/// See the module docs for why the three outcomes stay distinct.
pub fn for_checkout(checkout: &Repository) -> Result<RemoteRoles> {
    // A registry that cannot be read is not the same answer as a registry that
    // does not mention this path, so the error propagates instead of falling back.
    // No registry *at all*, though, is the second answer and not the third: on a
    // machine where no project has been declared these commands must still work.
    // The registry is read before we know whether it owns this path, so no choice
    // of checkout avoids this error — the advice has to be about the registry or
    // about which registry we are pointed at.
    let ws = Workspace::load_optional().context(
        "cannot read the project registry, so a declared remote role may be going unseen; \
         fix the reported file, or point WITS_PROJECT_CONFIG at a tree that loads",
    )?;
    let declared = ws.as_ref().and_then(|ws| ws.repo_for_path(checkout.path()));
    Ok(match declared {
        Some((project, repo_name)) => roles_in_force(&project.repos[&repo_name], checkout),
        None => RemoteRoles::from_git(checkout),
    })
}

/// The git remote state a declaration asks for: every declared remote's name
/// mapped to its fetch URL and the full set of URLs a push should reach.
///
/// Computed apart from any git call so the set arithmetic — the part that once let
/// push URLs multiply on every run — is settled and testable before anything is
/// written. [`push_urls`](DesiredRemote::push_urls) is the whole intended set, not
/// a delta, because that is the only shape an idempotent apply can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredRemote {
    pub url: String,
    pub push_urls: Vec<String>,
}

/// Plan the remote state for one repo. Only the declared names appear: a remote
/// the config never mentions is none of its business, so reconciliation leaves it
/// alone entirely.
pub fn desired(repo: &RawRepo) -> BTreeMap<String, DesiredRemote> {
    repo.remotes
        .iter()
        .map(|(name, remote)| (name.clone(), desired_one(remote)))
        .collect()
}

fn desired_one(remote: &RawRemote) -> DesiredRemote {
    // Git stops defaulting `push` to the fetch URL the moment any push URL exists,
    // so a fan-out set must name the remote's own URL too or pushing would reach
    // only the mirrors. With no mirrors there is no push URL at all: an explicit
    // one equal to the fetch URL would be churn that changes nothing.
    let push_urls = if remote.mirrors.is_empty() {
        Vec::new()
    } else {
        let mut urls = Vec::with_capacity(remote.mirrors.len() + 1);
        urls.push(remote.url.clone());
        for mirror in &remote.mirrors {
            if !urls.contains(mirror) {
                urls.push(mirror.clone());
            }
        }
        urls
    };
    DesiredRemote {
        url: remote.url.clone(),
        push_urls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(toml: &str) -> RawRepo {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn privileged_names_hold_their_role_without_declaring_it() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            origin = "https://example.com/fork.git"
            upstream = "https://example.com/canonical.git"
            "#,
        );
        let roles = roles_of(&r);
        assert_eq!(roles.origin(), Some("origin"));
        assert_eq!(roles.merge_target(), Some("upstream"));
        validate("x", &r).unwrap();
    }

    #[test]
    fn a_free_name_holds_a_role_only_by_declaring_it() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.myfork]
            url = "https://example.com/fork.git"
            role = "origin"
            [remotes.fdo]
            url = "https://example.com/canonical.git"
            role = "upstream"
            [remotes.rocm]
            url = "https://example.com/rocm.git"
            "#,
        );
        let roles = roles_of(&r);
        assert_eq!(roles.origin(), Some("myfork"));
        assert_eq!(roles.merge_target(), Some("fdo"));
        validate("x", &r).unwrap();
    }

    #[test]
    fn a_role_less_remote_is_kept_but_holds_nothing() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            origin = "https://example.com/fork.git"
            rocm = "https://example.com/rocm.git"
            "#,
        );
        assert_eq!(roles_of(&r).origin(), Some("origin"));
        assert!(desired(&r).contains_key("rocm"));
    }

    #[test]
    fn a_single_remote_serves_as_the_sync_source() {
        // The shape most projects have: one remote, no fork.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            origin = "https://example.com/canonical.git"
            "#,
        );
        let roles = roles_of(&r);
        assert_eq!(roles.merge_target(), Some("origin"));
        assert_eq!(roles.upstream(), None);
    }

    #[test]
    fn a_privileged_name_may_not_claim_the_other_role() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.origin]
            url = "https://example.com/canonical.git"
            role = "upstream"
            "#,
        );
        let err = validate("x", &r).unwrap_err().to_string();
        assert!(err.contains("named after the 'origin' role"), "{err}");
    }

    #[test]
    fn a_privileged_name_may_restate_its_own_role() {
        // Redundant but honest, and rejecting it would buy nothing.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.upstream]
            url = "https://example.com/canonical.git"
            role = "upstream"
            "#,
        );
        validate("x", &r).unwrap();
        assert_eq!(roles_of(&r).merge_target(), Some("upstream"));
    }

    #[test]
    fn two_remotes_may_not_hold_one_role() {
        // The collision that matters: an implicit holder and an explicit one.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            upstream = "https://example.com/a.git"
            [remotes.fdo]
            url = "https://example.com/b.git"
            role = "upstream"
            "#,
        );
        let err = validate("x", &r).unwrap_err().to_string();
        assert!(err.contains("hold the 'upstream' role"), "{err}");
    }

    #[test]
    fn an_unknown_role_fails_at_parse_time() {
        let err = toml::from_str::<RawRepo>(
            r#"
            path = "~/src/x"
            [remotes.m]
            url = "https://example.com/a.git"
            role = "mirrors"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("mirrors"), "{err}");
    }

    #[test]
    fn a_mistyped_remote_field_fails_at_parse_time() {
        let err = toml::from_str::<RawRepo>(
            r#"
            path = "~/src/x"
            [remotes.origin]
            url = "https://example.com/a.git"
            mirror = ["https://example.com/b.git"]
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("mirror"), "{err}");
    }

    #[test]
    fn an_empty_url_is_rejected() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            origin = ""
            "#,
        );
        let err = validate("x", &r).unwrap_err().to_string();
        assert!(err.contains("url is empty"), "{err}");
    }

    #[test]
    fn a_fan_out_set_includes_the_remotes_own_url() {
        // Git would otherwise push only to the mirrors; see `desired_one`.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.origin]
            url = "https://example.com/fork.git"
            mirrors = ["https://example.com/m1.git", "https://example.com/m2.git"]
            "#,
        );
        let plan = desired(&r);
        assert_eq!(
            plan["origin"].push_urls,
            vec![
                "https://example.com/fork.git",
                "https://example.com/m1.git",
                "https://example.com/m2.git",
            ]
        );
    }

    #[test]
    fn no_mirrors_means_no_push_urls_at_all() {
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes]
            origin = "https://example.com/fork.git"
            "#,
        );
        assert!(desired(&r)["origin"].push_urls.is_empty());
    }

    #[test]
    fn a_mirror_equal_to_the_fetch_url_is_not_listed_twice() {
        // Declaring it is redundant, but a duplicate push URL would make git send
        // the same push twice.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.origin]
            url = "https://example.com/fork.git"
            mirrors = ["https://example.com/fork.git"]
            "#,
        );
        assert_eq!(
            desired(&r)["origin"].push_urls,
            vec!["https://example.com/fork.git"]
        );
    }

    /// A machine with no project config at all must still resolve roles, from the
    /// privileged names. Getting this wrong makes every `stack` and `review`
    /// command fail on a fresh install, which is the opposite of the point.
    #[test]
    fn a_checkout_with_no_registry_falls_back_to_privileged_names() {
        let _guard = crate::log::test_flag_guard();
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        let run = |args: &[&str]| {
            crate::process::Command::new("git")
                .args(args.iter().copied())
                .current_dir(&checkout)
                .force_run()
                .exec()
                .unwrap();
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["remote", "add", "origin", "https://example.com/a.git"]);

        // An empty directory *is* a config tree, just one declaring nothing — which
        // is the "registry does not own this path" branch. The "no config tree at
        // all" branch is `find_root` returning `None`, covered in `config`.
        let empty_root = tmp.path().join("cfg");
        std::fs::create_dir_all(&empty_root).unwrap();
        std::env::set_var("WITS_PROJECT_CONFIG", &empty_root);
        let roles = for_checkout(&Repository::new(&checkout)).unwrap();
        std::env::remove_var("WITS_PROJECT_CONFIG");

        assert_eq!(roles.origin(), Some("origin"));
        assert_eq!(roles.merge_target(), Some("origin"));
    }

    /// A config root that was *named* and cannot be honoured stays an error: an
    /// explicit instruction we cannot carry out must not be silently downgraded to
    /// "no projects declared".
    #[test]
    fn a_named_but_missing_config_root_is_an_error() {
        let _guard = crate::log::test_flag_guard();
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();

        std::env::set_var("WITS_PROJECT_CONFIG", tmp.path().join("does-not-exist"));
        let err = for_checkout(&Repository::new(&checkout)).unwrap_err();
        std::env::remove_var("WITS_PROJECT_CONFIG");

        assert!(
            err.to_string().contains("project registry"),
            "should name the registry: {err}"
        );
    }

    #[test]
    fn the_plan_is_stable_across_repeated_computation() {
        // The property that keeps an apply idempotent: the plan is a set, so
        // computing it again cannot grow it.
        let r = repo(
            r#"
            path = "~/src/x"
            [remotes.origin]
            url = "https://example.com/fork.git"
            mirrors = ["https://example.com/m1.git"]
            "#,
        );
        assert_eq!(desired(&r), desired(&r));
    }
}
