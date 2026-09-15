//! What a checkout's remotes *mean*: the role vocabulary, and how a checkout
//! answers "which remote is which".
//!
//! Two roles carry meaning for the tools, and everything any of them does with a
//! remote reduces to one of the two:
//!
//! - **`origin`** — where we push, and whose owner heads a cross-fork MR.
//! - **`upstream`** — the merge target: the repository `main` fast-forwards
//!   against, whose forge we talk to, and where MRs merge.
//!
//! Fetching is deliberately *not* on that list. `update` fetches every remote a
//! repo declares, so "what do we fetch" is a question about the remote list and
//! not about roles; only `main`'s fast-forward source belongs to the merge
//! target. The initial `clone` is the one exception, and for a reason that is not
//! about fetching: it must name a single repository, and the merge target is the
//! one that is certain to exist.
//!
//! Neither name is git's. Git privileges `origin` only weakly — as `git clone`'s
//! default remote name, and as the terminal fallback in `branch.<name>.remote` —
//! and it has no notion of `upstream` at all; that one is community convention.
//! So this vocabulary is *ours*. That is why it is defined here rather than
//! inherited, and why it can be revised without arguing with git.
//!
//! A role is also a privileged *remote name*: the remote called `origin` holds
//! the `origin` role without declaring anything. That equivalence is what lets a
//! checkout following the usual convention need no declaration at all, which in
//! turn is what lets `stack` and `review` work in a repo no project knows about.
//!
//! # The one fallback
//!
//! When no remote holds `upstream`, the `origin` holder is the merge target too.
//! [`RemoteRoles::merge_target`] is that rule's only home; it used to be
//! re-derived at six call sites, each spelling "upstream if declared, else origin"
//! its own way, one of them with the priorities inverted.
//!
//! There is deliberately no fallback the other way. A checkout that tracks an
//! upstream and has no push remote is a legitimate read-only state, so push-side
//! operations report the absence instead of pushing somewhere unasked.
//!
//! # Why this is neither in `git` nor in `project`
//!
//! [`crate::git`] is porcelain with no policy, and "there are exactly two roles,
//! spelled thus" is policy. [`crate::project`] is where a *user's declaration* of
//! the roles lives, but the tools must also serve a checkout no project declares.
//! So the vocabulary and the name-only reading sit below both, and the
//! registry-aware resolution sits above, in [`crate::project::remotes`].

use serde::de::{self, Deserializer};
use serde::Deserialize;

use crate::git::Repository;

/// The role a remote plays. Each variant's name is also the remote name that
/// holds it by default (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Origin,
    Upstream,
}

impl Role {
    /// Both roles, in the order a reader expects them: push side, then merge side.
    pub const ALL: [Role; 2] = [Role::Origin, Role::Upstream];

    /// The role's name, which is equally the remote name that holds it by
    /// default and the spelling a `role =` declaration uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Origin => "origin",
            Role::Upstream => "upstream",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "origin" => Ok(Role::Origin),
            "upstream" => Ok(Role::Upstream),
            other => Err(format!("unknown role '{other}' (use origin|upstream)")),
        }
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

/// Which git remote holds each role in one checkout.
///
/// These are git remote *names*, not URLs: every consumer turns one into a
/// `git fetch <name>`, a `git push <name>`, or a `refs/remotes/<name>/…` lookup.
///
/// Either role may be unheld, and the two absences mean different things. No
/// `origin` is a working read-only checkout, so a push-side caller reports it and
/// carries on being useful. No merge target at all — which, given the fallback,
/// means no remote holds either role — leaves nothing for `main` to follow and no
/// forge to talk to, so its callers treat it as fatal.
///
/// The fields are private so that [`merge_target`](Self::merge_target) stays the
/// only expression of the upstream-to-origin fallback; a caller reaching for the
/// two names separately is how that rule got copied six times before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRoles {
    origin: Option<String>,
    upstream: Option<String>,
}

impl RemoteRoles {
    pub fn new(origin: Option<String>, upstream: Option<String>) -> Self {
        Self { origin, upstream }
    }

    /// The remote we push to, and whose owner heads a cross-fork MR.
    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    /// The remote that literally holds the `upstream` role, with no fallback
    /// applied.
    ///
    /// Nearly every caller wants [`merge_target`](Self::merge_target) instead.
    /// This exists for the one that must tell "tracks a separate fork source" apart
    /// from "origin doubles as the merge target" — trunk detection prefers the
    /// fork's own tip, so it walks the two roles in its own order rather than
    /// taking the merge target.
    pub fn upstream(&self) -> Option<&str> {
        self.upstream.as_deref()
    }

    /// The repository we merge into: what `main` fast-forwards against, which
    /// forge to talk to, and where MRs land. The `upstream` holder when there is
    /// one, else the `origin` holder — this method is that fallback's only home.
    ///
    /// Not "what to fetch": every declared remote is fetched (module docs). The
    /// one place this doubles as a fetch source is the initial `clone`, which has
    /// to name a single repository and picks the one certain to exist.
    pub fn merge_target(&self) -> Option<&str> {
        self.upstream.as_deref().or(self.origin.as_deref())
    }

    /// The roles a checkout carries by *name* alone — the answer for a repo no
    /// project declares.
    ///
    /// Only the privileged names are recognised here, because a name is the only
    /// signal available: nothing is written into the repository to record a role,
    /// precisely so that a project's config file stays the single source of truth.
    /// A checkout whose remotes are named otherwise therefore yields no roles at
    /// all, and its caller reports that rather than guessing which of them to use.
    pub fn from_git(repo: &Repository) -> Self {
        let names = repo.remote_names();
        let held = |role: Role| {
            names
                .iter()
                .any(|n| n == role.as_str())
                .then(|| role.as_str().to_owned())
        };
        Self {
            origin: held(Role::Origin),
            upstream: held(Role::Upstream),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        crate::process::Command::new("git")
            .args(args.iter().copied())
            .current_dir(dir)
            .force_run()
            .exec()
            .unwrap();
    }

    #[test]
    fn role_parses_known_and_rejects_unknown() {
        use std::str::FromStr;
        assert_eq!(Role::from_str("origin").unwrap(), Role::Origin);
        assert_eq!(Role::from_str("upstream").unwrap(), Role::Upstream);
        // `mirrors` is a per-remote attribute, not a role, so it must not parse.
        let err = Role::from_str("mirrors").unwrap_err();
        assert!(err.contains("mirrors"), "{err}");
        assert!(Role::from_str("Origin").is_err(), "roles are lowercase");
    }

    #[test]
    fn the_merge_target_prefers_upstream_and_falls_back_to_origin() {
        let forked = RemoteRoles::new(Some("myfork".into()), Some("fdo".into()));
        assert_eq!(forked.merge_target(), Some("fdo"));
        assert_eq!(forked.origin(), Some("myfork"));

        let single = RemoteRoles::new(Some("origin".into()), None);
        assert_eq!(single.merge_target(), Some("origin"));
        // The raw role is still absent; only `merge_target` applies the fallback.
        assert_eq!(single.upstream(), None);
    }

    #[test]
    fn there_is_no_fallback_from_upstream_to_origin() {
        // A read-only checkout: somewhere to merge into, nowhere to push. Push-side
        // callers must see the absence instead of being handed the merge target.
        let read_only = RemoteRoles::new(None, Some("fdo".into()));
        assert_eq!(read_only.merge_target(), Some("fdo"));
        assert_eq!(read_only.origin(), None);
    }

    #[test]
    fn a_checkout_with_neither_role_holds_nothing() {
        let empty = RemoteRoles::default();
        assert_eq!(empty.merge_target(), None);
        assert_eq!(empty.origin(), None);
    }

    #[test]
    fn from_git_reads_privileged_names_and_ignores_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main", "."]);
        git(
            dir,
            &["remote", "add", "upstream", "https://example.com/a.git"],
        );
        git(dir, &["remote", "add", "rocm", "https://example.com/b.git"]);

        let roles = RemoteRoles::from_git(&Repository::new(dir));
        assert_eq!(roles.upstream(), Some("upstream"));
        assert_eq!(roles.merge_target(), Some("upstream"));
        // `rocm` is a remote with no role: it is never guessed into one.
        assert_eq!(roles.origin(), None);

        git(
            dir,
            &["remote", "add", "origin", "https://example.com/c.git"],
        );
        let roles = RemoteRoles::from_git(&Repository::new(dir));
        assert_eq!(roles.origin(), Some("origin"));
    }

    #[test]
    fn from_git_finds_no_roles_when_no_remote_uses_a_privileged_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main", "."]);
        git(dir, &["remote", "add", "fdo", "https://example.com/a.git"]);

        let roles = RemoteRoles::from_git(&Repository::new(dir));
        assert_eq!(roles, RemoteRoles::default());
    }
}
