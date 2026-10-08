//! Talking to a git hosting platform — the MR *and* review APIs, behind one
//! hard boundary.
//!
//! One job stated many ways: for a stack, find a branch's MRs, create one, move
//! its base, keep its navigation comment; for review, list/fetch MRs, read
//! threads, and flush a whole review. The temptation — and the mistake the
//! earlier tooling made — is to let each platform's quirks (`number` vs `iid`,
//! `base.ref` vs `target_branch`, GraphQL vs REST, draft-by-field vs
//! draft-by-title-prefix) seep into the code that drives the workflow. So the
//! boundary is deliberately hard: [`MergeRequest`]/[`MrSummary`] and the review
//! shapes ([`review`]) are normalized, the [`Forge`] trait is the whole surface
//! (a small MR half plus a review half), and everything platform-specific is
//! trapped inside one host module behind it. Adding a forge is then a
//! self-contained mapping exercise, never a change to the verbs.
//!
//! The module is organised by concern rather than piled into one file:
//! [`remote`] is the *identity* layer (parse a URL into host/owner/repo and pick
//! a service — the input to [`detect`]); [`transport`] is the shared `ureq`
//! HTTP/credential plumbing every backend maps onto; [`review`] holds the review
//! types; and `github`/`gitlab`/`gitea` are the per-platform mappings. This file
//! is just the boundary: the normalized MR types, the trait, and `detect`.

pub mod gitea;
pub mod github;
pub mod gitlab;
pub mod remote;
pub mod review;
mod transport;

pub use remote::{parse_url, RemoteInfo, Remotes, Service};
pub use review::{
    ActionKey, Anchor, BatchAction, BatchOutcome, DiffVersion, FeedQuery, FeedStates, LineRef,
    MrDetails, MrSummary, RemoteComment, RemoteThread, ReviewBatch, Side, Verdict,
};
// The transport primitives the host backends build on. Re-exported at the crate
// level so a backend writes `super::request` rather than `super::transport::…`.
pub(crate) use transport::{
    delete_idempotent, encode, encode_path, next_link, request, request_every_page,
    request_paginated, request_with_headers, resolve_self, status_of, wait_out_rate_limit, Auth,
    EVERY_PAGE_LIMIT, SELF_REF,
};

use crate::git::Repository;

/// An MR's lifecycle, normalized across platforms that spell it differently
/// (GitHub folds "merged" into "closed"; GitLab keeps them apart).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MrState {
    Open,
    Merged,
    Closed,
}

/// Which repository the head of a looked-up MR lives in. A branch name alone
/// does not identify a branch — every fork has its own `main`, and a stack
/// branch can share its name with someone else's — so a lookup always says
/// whose branch it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadRepo {
    /// The repository this checkout pushes to (the `origin` role), where a
    /// stack's own branches live. That is the merge target itself when it
    /// holds the role too, or when no remote holds it.
    Origin,
    /// The merge target. Every MR's base is a branch there, so this is where
    /// the MR whose source branch is another MR's base comes from: the step up
    /// a stack, whoever pushed it.
    Target,
}

/// The platform-independent view of one merge request. `id` is whatever opaque
/// token the platform needs to address it again (a number, an iid); `display` is
/// the human form (`#123`, `!45`). Nothing above this struct ever sees raw JSON.
#[derive(Debug, Clone)]
pub struct MergeRequest {
    pub id: String,
    pub display: String,
    pub state: MrState,
    pub base: String,
    /// The MR's source branch. Lets a stack be walked in both directions —
    /// one MR's `base` links to its parent's `source` — without the richer
    /// [`MrSummary`]. Empty when the platform withholds it.
    pub source: String,
    /// The commit the MR's head is at — for a closed or merged MR, the one it
    /// was at when it stopped being open, not wherever its branch has moved
    /// since. The closed-MR guard compares it with the local tip to tell a
    /// reused branch name from the branch it was. `None` when unknown.
    pub head_sha: Option<String>,
    pub body: String,
    pub web_url: String,
}

/// Everything needed to open a new MR. The forge turns `branch` (plus the push
/// repository it already knows) into the right head reference; the caller never
/// has to reason about cross-fork head syntax.
#[derive(Debug, Clone)]
pub struct NewMr {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// One conversation comment on an MR — neither a review thread nor a system
/// note.
#[derive(Debug, Clone)]
pub struct MrComment {
    /// What the platform needs to address the comment again: GitHub's node id,
    /// GitLab's note id, Gitea's comment id.
    pub id: String,
    pub body: String,
    /// Where a person can see the comment.
    pub url: String,
    /// Whether the authenticated user wrote it.
    pub own: bool,
}

/// Attributes layered onto an existing MR by `decorate`. Applied *additively*:
/// the platform adds what's listed and never removes anything, so a project's own
/// label/reviewer automation is never fought — short of the platform's own
/// rules, such as a Gitea or Forgejo exclusive scoped label displacing the other
/// labels of its scope. Only labels that exist are added: a missing one is
/// warned about, never created. The literal `@me` resolves to the authenticated
/// user.
#[derive(Debug, Clone, Default)]
pub struct Attributes {
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub reviewers: Vec<String>,
}

impl Attributes {
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.assignees.is_empty() && self.reviewers.is_empty()
    }

    /// A short, human description for dry-run / log lines.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.labels.is_empty() {
            parts.push(format!("labels={:?}", self.labels));
        }
        if !self.assignees.is_empty() {
            parts.push(format!("assignees={:?}", self.assignees));
        }
        if !self.reviewers.is_empty() {
            parts.push(format!("reviewers={:?}", self.reviewers));
        }
        parts.join(" ")
    }
}

/// Everything the workflow verbs need from a platform: a small MR half, used by
/// `stack`, and a review half, used by `review`. The verbs are written once
/// against this trait; see the module note for why the surface is this small.
pub trait Forge: Send + Sync {
    /// The user-facing noun for a merge request here — GitHub's "PR",
    /// GitLab's "MR", Gitea's "PR".
    fn noun(&self) -> &'static str;

    /// Every MR in the merge target whose head is `branch` in the `head`
    /// repository, in any state, most recently updated first.
    ///
    /// The answer is complete and exact. Every page is read, so an empty list
    /// means there is none, and the head repository is matched by identity
    /// rather than by name, so a same-named branch elsewhere never counts. The
    /// base is deliberately *not* a criterion: a caller fixing a drifted base
    /// needs the MR precisely when its base no longer matches. Several open
    /// MRs can come back — every platform refuses a second open MR only into
    /// the *same* base — so choosing among them is left to the caller.
    fn mrs_for_branch(&self, head: HeadRepo, branch: &str) -> anyhow::Result<Vec<MergeRequest>>;

    /// The open MRs whose *target* branch is `base_branch`, from every page —
    /// the children of a stack node. Used by `review fetch --stack` to walk a
    /// stack downward toward its leaves (the upward walk is
    /// [`mrs_for_branch`](Forge::mrs_for_branch) on the base). Defaults to empty
    /// for a backend that can't enumerate them, so stack completion degrades to
    /// the upward direction rather than failing.
    fn find_children(&self, _base_branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        Ok(Vec::new())
    }

    fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest>;
    fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()>;
    fn set_body(&self, id: &str, body: &str) -> anyhow::Result<()>;

    /// Add labels/assignees/reviewers to an existing MR, additively and
    /// best-effort: a sub-item that fails (an unknown label, a self-review the
    /// platform forbids) is logged and skipped rather than aborting the rest,
    /// and so is one the platform accepts but silently drops, where its answer
    /// shows it.
    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> anyhow::Result<()>;

    /// The MR's conversation comments, oldest first, from every page, each
    /// marked with whether the authenticated user wrote it. Complete, so a
    /// comment that is not in the list does not exist.
    fn list_comments(&self, mr: &str) -> anyhow::Result<Vec<MrComment>>;

    /// Post a conversation comment on the MR.
    fn add_comment(&self, mr: &str, body: &str) -> anyhow::Result<()>;

    /// Replace the body of a comment [`list_comments`](Forge::list_comments)
    /// returned. The MR is passed too, because GitLab addresses a note through
    /// its MR.
    fn edit_comment(&self, mr: &str, comment: &str, body: &str) -> anyhow::Result<()>;

    // -- Review half ---------------------------------------------------------
    //
    // These carry default `bail` bodies so a forge without a review backend
    // (Gitea today) keeps compiling and fails loudly only when review is
    // actually asked of it. GitHub and GitLab override them.

    /// The MRs matching a feed's filter, pushed down to the platform's
    /// list/search query and paginated server-side.
    fn list_mrs(&self, _query: &FeedQuery) -> anyhow::Result<Vec<MrSummary>> {
        anyhow::bail!("`wits review` has no backend for this forge yet")
    }

    /// One MR's metadata and current diff-version SHAs, addressed by its number.
    fn mr_details(&self, _id: &str) -> anyhow::Result<MrDetails> {
        anyhow::bail!("`wits review` has no backend for this forge yet")
    }

    /// The fetchable ref that exposes an MR's head on the target remote (e.g.
    /// `pull/<n>/head`), so its objects can be pulled even across a fork.
    fn mr_ref(&self, _id: &str) -> anyhow::Result<String> {
        anyhow::bail!("`wits review` has no backend for this forge yet")
    }

    /// The review discussion currently on the MR, with each thread's resolved
    /// and outdated flags.
    fn list_threads(&self, _id: &str) -> anyhow::Result<Vec<RemoteThread>> {
        anyhow::bail!("`wits review` has no backend for this forge yet")
    }

    /// Flush a whole review — verdict, summary, comments (line / file / MR-level),
    /// replies, and resolves — folding as many actions as the platform's native
    /// batch primitive allows into one notification, and doing the rest as
    /// separate calls. The result is a granular [`BatchOutcome`] keyed by action,
    /// so the orchestration layer reconciles per action: a landed action is
    /// cleared from the draft, a failed one stays, and a verdict failure never
    /// poisons comments already posted.
    ///
    /// `Err` means *nothing* landed (a total failure, or an atomic batch the
    /// backend rolled back to nothing); the caller keeps the whole draft. A
    /// partial success is always `Ok` with the per-action outcomes filled in.
    fn submit(&self, _id: &str, _batch: &ReviewBatch) -> anyhow::Result<BatchOutcome> {
        anyhow::bail!("`wits review` has no backend for this forge yet")
    }

    /// A web permalink to a file (optionally a line or line range) at a ref, for
    /// expanding a `[[path:line]]` reference in a comment body. The default has
    /// no web URL and degrades to a readable `path:line@ref`; GitHub and GitLab
    /// override it with real blob URLs.
    fn permalink(&self, r#ref: &str, path: &str, lines: Option<(u32, Option<u32>)>) -> String {
        match lines {
            Some((a, Some(b))) => format!("{path}:{a}-{b}@{ref}"),
            Some((a, None)) => format!("{path}:{a}@{ref}"),
            None => format!("{path}@{ref}"),
        }
    }
}

// ----------------------------------------------------------------------------
// Detection & credentials.
// ----------------------------------------------------------------------------

/// Pick and configure the forge for this checkout, or explain why we can't.
///
/// The merge target decides everything: the platform we talk to is the one
/// hosting the `upstream` role (or `origin` when there is no fork). Service
/// detection from the hostname can be overridden per host for self-hosted
/// instances, and a token must resolve or there is nothing to authenticate with.
pub fn detect(repo: &Repository, remotes: &Remotes) -> anyhow::Result<Box<dyn Forge>> {
    // Names roles rather than remotes: in a declared project the holders may be
    // called anything, so "add an `upstream` remote" would be wrong advice.
    let target = remotes
        .target
        .as_ref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no remote holds the origin or upstream role, so there is no forge to derive"
            )
        })?
        .clone();

    // A host override lets a self-hosted GitLab/Gitea behind a custom domain
    // declare itself when the hostname gives nothing away.
    let service = repo
        .get_config(&format!("wits.forge.{}.service", target.host))
        .ok()
        .flatten()
        .and_then(|s| Service::parse(&s))
        .unwrap_or(target.service);

    // Turn away a recognized-but-unsupported host *before* hunting for a token,
    // so the failure names the real reason instead of masquerading as a missing
    // token. Bitbucket is the live case: we still parse its remotes and
    // `wits stack sync` pushes to it, but the MR verbs have no backend for it.
    match service {
        Service::GitHub
        | Service::GitLab
        | Service::Gitea
        | Service::Forgejo
        | Service::Codeberg => {}
        Service::Bitbucket => anyhow::bail!(
            "`wits stack` speaks to GitHub, GitLab and Gitea; Bitbucket has no MR backend here \
             (`wits stack sync` still pushes to it)"
        ),
        Service::Unknown => anyhow::bail!(
            "could not detect the forge for host '{}'; set wits.forge.{}.service",
            target.host,
            target.host
        ),
    }

    let token = resolve_token(repo, &target, service).ok_or_else(|| {
        anyhow::anyhow!(
            "no API token for {}/{} ({}); set wits.forge.{}/{}.token for this owner or \
             wits.forge.{}.token for the whole host, or the platform's *_TOKEN env var",
            target.host,
            target.project_path(),
            service.as_str(),
            target.host,
            target.owner,
            target.host
        )
    })?;

    // Every fork decision — the head a create names, the repository a lookup
    // pins a branch to — derives from this one identity.
    let push_repo = remotes.push_repo().cloned();

    let api_url_override = repo
        .get_config(&format!("wits.forge.{}.api-url", target.host))
        .ok()
        .flatten();

    match service {
        Service::GitHub => Ok(Box::new(github::GitHub::new(
            target,
            push_repo,
            token,
            api_url_override,
        ))),
        // One API family, one impl: only their identities (token env, config key,
        // detection) set Gitea, Forgejo and Codeberg apart.
        Service::Gitea | Service::Forgejo | Service::Codeberg => Ok(Box::new(gitea::Gitea::new(
            target,
            push_repo,
            token,
            api_url_override,
        ))),
        Service::GitLab => Ok(Box::new(gitlab::GitLab::new(
            target,
            push_repo,
            token,
            api_url_override,
        ))),
        // The unsupported services were already rejected above.
        Service::Bitbucket | Service::Unknown => unreachable!(),
    }
}

/// Find a token for `target`, most specific first: config scoped to a path of
/// the repository on its host (`wits.forge.<host>/<owner>[/<repo>].token`, the
/// longest matching path first), then per-host config, then per-service config,
/// then a blanket config key, then the platform's conventional env var.
///
/// The path-scoped keys exist because a token need not cover its whole host: a
/// GitHub fine-grained PAT "is limited to access resources owned by a single
/// user or organization" (GitHub Docs, "Managing your personal access
/// tokens"), so one host can need a different token per owner.
///
/// Config comes before the environment here, unlike a setting, where the
/// environment is the deliberate, throwaway override: a token is one standing
/// secret, the most specific key is the most precise answer to which one, and
/// the env var is the CI-shaped fallback (`docs/reference/stack-design.rst`,
/// "Transport and credentials").
fn resolve_token(repo: &Repository, target: &RemoteInfo, service: Service) -> Option<String> {
    let path_scoped = repo.config_names(r"^wits\.forge\.[^/]+/.+\.token$");
    let config_keys = path_scoped_token_keys(&path_scoped, target)
        .into_iter()
        .map(str::to_owned)
        .chain([
            format!("wits.forge.{}.token", target.host),
            format!("wits.forge.{}.token", service.as_str()),
            "wits.forge.token".to_owned(),
        ]);
    for key in config_keys {
        if let Some(v) = repo.get_config(&key).ok().flatten() {
            return Some(v);
        }
    }

    let env_vars: &[&str] = match service {
        Service::GitHub => &["GITHUB_TOKEN"],
        Service::GitLab => &["GITLAB_TOKEN"],
        Service::Gitea => &["GITEA_TOKEN"],
        Service::Forgejo => &["FORGEJO_TOKEN", "GITEA_TOKEN"],
        // Codeberg runs Forgejo, so it falls back to Forgejo's env.
        Service::Codeberg => &["CODEBERG_TOKEN", "FORGEJO_TOKEN"],
        Service::Bitbucket => &["BITBUCKET_TOKEN"],
        Service::Unknown => &[],
    };
    env_vars.iter().find_map(|v| std::env::var(v).ok())
}

/// The path-scoped token keys among `names` that cover `target`, the most
/// specific first. A key covers the target when its host is the target's and
/// its path segments lead the target's `owner/repo` path: whole segments,
/// compared case-insensitively, as [`RemoteInfo::same_repository`] compares
/// repositories. Of two keys equally specific, the one git read later comes
/// first, as a later config value overrides an earlier one.
fn path_scoped_token_keys<'a>(names: &'a [String], target: &RemoteInfo) -> Vec<&'a str> {
    let project = target.project_path();
    let repo_path: Vec<&str> = project.split('/').collect();
    let mut keys: Vec<(usize, usize, &str)> = names
        .iter()
        .enumerate()
        .filter_map(|(read_at, name)| {
            let scope = name.strip_prefix("wits.forge.")?.strip_suffix(".token")?;
            let (host, path) = scope.split_once('/')?;
            let path: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let covers = host.eq_ignore_ascii_case(&target.host)
                && !path.is_empty()
                && path.len() <= repo_path.len()
                && path
                    .iter()
                    .zip(&repo_path)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b));
            covers.then_some((path.len(), read_at, name.as_str()))
        })
        .collect();
    keys.sort_unstable_by_key(|&(specificity, read_at, _)| {
        std::cmp::Reverse((specificity, read_at))
    });
    keys.into_iter().map(|(_, _, name)| name).collect()
}

/// Drop repeats of an MR, keeping its first — most recently updated —
/// occurrence. Offset pagination over a list that changes while it is read can
/// show one MR on two pages: an MR updated meanwhile moves to the front, and
/// everything behind it shifts back a place.
pub(crate) fn dedup_mrs(mut mrs: Vec<MergeRequest>) -> Vec<MergeRequest> {
    let mut seen = std::collections::HashSet::new();
    mrs.retain(|mr| seen.insert(mr.id.clone()));
    mrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_mr_is_kept_once_in_its_first_place() {
        let mr = |id: &str| MergeRequest {
            id: id.into(),
            display: format!("#{id}"),
            state: MrState::Open,
            base: String::new(),
            source: String::new(),
            head_sha: None,
            body: String::new(),
            web_url: String::new(),
        };
        let kept: Vec<String> = dedup_mrs(vec![mr("3"), mr("1"), mr("3"), mr("2")])
            .into_iter()
            .map(|mr| mr.id)
            .collect();
        assert_eq!(kept, ["3", "1", "2"]);
    }

    fn repository(host: &str, owner: &str, repo: &str) -> RemoteInfo {
        RemoteInfo {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
            service: Service::GitHub,
        }
    }

    fn names(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    /// A fine-grained PAT reaches one owner, so an owner's key must reach that
    /// owner's repositories and no one else's, a repository's own key first.
    #[test]
    fn a_path_scoped_token_key_covers_only_its_own_path() {
        let keys = names(&[
            "wits.forge.github.com/acme.token",
            "wits.forge.github.com/acme/driver.token",
            "wits.forge.github.com/octo-org.token",
            "wits.forge.gitlab.com/acme.token",
            "wits.forge.github.com.token",
        ]);
        let covering = |owner: &str, repo: &str| {
            path_scoped_token_keys(&keys, &repository("github.com", owner, repo))
        };
        assert_eq!(
            covering("acme", "driver"),
            [
                "wits.forge.github.com/acme/driver.token",
                "wits.forge.github.com/acme.token",
            ]
        );
        assert_eq!(
            covering("acme", "other"),
            ["wits.forge.github.com/acme.token"]
        );
        // Whole segments only: an owner whose name merely starts the same is
        // someone else.
        assert!(covering("acme-labs", "driver").is_empty());
        // An owner no key names is covered by none of them.
        assert!(covering("globex", "widget").is_empty());
    }

    #[test]
    fn a_path_scoped_token_key_matches_however_the_path_is_spelled() {
        let keys = names(&["wits.forge.GitHub.com/acme/.token"]);
        assert_eq!(
            path_scoped_token_keys(&keys, &repository("github.com", "Acme", "r")),
            ["wits.forge.GitHub.com/acme/.token"]
        );
    }

    /// GitLab nests groups, so an owner can be several segments long; the
    /// longest covering path still wins.
    #[test]
    fn a_nested_group_is_matched_by_its_longest_path() {
        let keys = names(&[
            "wits.forge.gitlab.com/group.token",
            "wits.forge.gitlab.com/group/subx.token",
            "wits.forge.gitlab.com/group/sub/proj.token",
            "wits.forge.gitlab.com/group/sub.token",
        ]);
        assert_eq!(
            path_scoped_token_keys(&keys, &repository("gitlab.com", "group/sub", "proj")),
            [
                "wits.forge.gitlab.com/group/sub/proj.token",
                "wits.forge.gitlab.com/group/sub.token",
                "wits.forge.gitlab.com/group.token",
            ]
        );
    }

    #[test]
    fn of_two_equally_specific_keys_the_later_one_wins() {
        let keys = names(&[
            "wits.forge.github.com/acme.token",
            "wits.forge.github.com/Acme.token",
        ]);
        assert_eq!(
            path_scoped_token_keys(&keys, &repository("github.com", "acme", "r")),
            [
                "wits.forge.github.com/Acme.token",
                "wits.forge.github.com/acme.token",
            ]
        );
    }

    #[test]
    fn attributes_emptiness_and_summary() {
        assert!(Attributes::default().is_empty());
        let a = Attributes {
            labels: vec!["bug".into()],
            assignees: vec!["@me".into()],
            reviewers: vec![],
        };
        assert!(!a.is_empty());
        let s = a.summary();
        assert!(s.contains("labels") && s.contains("assignees") && !s.contains("reviewers"));
    }
}
