//! Talking to a git hosting platform — the MR *and* review APIs, behind one
//! hard boundary.
//!
//! One job stated many ways: for a stack, find an MR for a branch, create one,
//! move its base, rewrite its body; for review, list/fetch MRs, read threads,
//! and flush a whole review. The temptation — and the mistake the earlier
//! tooling made — is to let each platform's quirks (`number` vs `iid`,
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
    current_user, delete_idempotent, encode, encode_path, request, request_paginated, resolve_self,
    Auth, SELF_REF,
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

/// Which lifecycle states a `find` should accept. Kept separate from [`MrState`]
/// because callers think in terms of intent ("is there one open?", "is there any
/// closed leftover?") rather than enumerating states.
#[derive(Debug, Clone, Copy)]
pub enum StateFilter {
    Open,
    NotOpen,
}

impl StateFilter {
    fn accepts(self, state: MrState) -> bool {
        match self {
            StateFilter::Open => state == MrState::Open,
            StateFilter::NotOpen => state != MrState::Open,
        }
    }
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
    pub head_sha: Option<String>,
    pub body: String,
    pub web_url: String,
}

/// Everything needed to open a new MR. The forge turns `branch` (plus the fork
/// owner it already knows) into the right head reference; the caller never has to
/// reason about cross-fork head syntax.
#[derive(Debug, Clone)]
pub struct NewMr {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// Attributes layered onto an existing MR by `decorate`. Applied *additively*:
/// the platform adds what's listed and never removes anything, so a project's own
/// label/reviewer automation is never fought. The literal `@me` resolves to the
/// authenticated user.
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

/// The four primitives every platform must provide. The workflow verbs are
/// written once against this trait; see the module note for why the surface is
/// this small.
pub trait Forge: Send + Sync {
    /// The user-facing noun for a merge request here — GitHub's "PR",
    /// GitLab's "MR", Gitea's "PR".
    fn noun(&self) -> &'static str;

    /// The MR for `branch` matching the state filter, regardless of its current
    /// base. Base is deliberately *not* a match criterion: a caller fixing a
    /// drifted base needs to find the MR precisely when its base no longer
    /// matches, then compare and retarget it.
    fn find(&self, branch: &str, state: StateFilter) -> anyhow::Result<Option<MergeRequest>>;

    /// The MR for `branch` in *any* state, preferring an open one. This is the
    /// single-fetch form of [`find`](Forge::find): a caller that has to inspect
    /// the state itself (stack `submit` deciding create-vs-retarget-vs-skip) does
    /// one round trip instead of the two a paired `find(Open)` + `find(NotOpen)`
    /// would cost — every backend's list query already returns all states at once.
    fn find_any(&self, branch: &str) -> anyhow::Result<Option<MergeRequest>>;

    /// The open MRs whose *target* branch is `base_branch` — the children of a
    /// stack node. Used by `review fetch --stack` to walk a stack downward toward
    /// its leaves (the upward walk is [`find_any`](Forge::find_any) on the base).
    /// Defaults to empty for a backend that can't enumerate them, so stack
    /// completion degrades to the upward direction rather than failing.
    fn find_children(&self, _base_branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        Ok(Vec::new())
    }

    fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest>;
    fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()>;
    fn set_body(&self, id: &str, body: &str) -> anyhow::Result<()>;

    /// Add labels/assignees/reviewers to an existing MR, additively and
    /// best-effort: a sub-item that fails (an unknown label, a self-review the
    /// platform forbids) is logged and skipped rather than aborting the rest.
    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> anyhow::Result<()>;

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
/// hosting `upstream` (or `origin` when there is no fork). Service detection from
/// the hostname can be overridden per host for self-hosted instances, and a
/// token must resolve or there is nothing to authenticate with.
pub fn detect(repo: &Repository, remotes: &Remotes) -> anyhow::Result<Box<dyn Forge>> {
    let target = remotes
        .target()
        .ok_or_else(|| anyhow::anyhow!("no 'origin' or 'upstream' remote to derive a forge from"))?
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

    let token = resolve_token(repo, &target.host, service).ok_or_else(|| {
        anyhow::anyhow!(
            "no API token for {} ({}); set wits.forge.{}.token or the platform's *_TOKEN env var",
            target.host,
            service.as_str(),
            target.host
        )
    })?;

    // Cross-fork MRs express the head as `origin_owner:branch`; same-repo MRs
    // just use the branch name. We compute the owner once here.
    let head_owner = if remotes.is_cross_fork() {
        remotes.head_owner().map(str::to_owned)
    } else {
        None
    };

    let api_url_override = repo
        .get_config(&format!("wits.forge.{}.api-url", target.host))
        .ok()
        .flatten();

    match service {
        Service::GitHub => Ok(Box::new(github::GitHub::new(
            target,
            head_owner,
            token,
            api_url_override,
        ))),
        // One API family, one impl: only their identities (token env, config key,
        // detection) set Gitea, Forgejo and Codeberg apart.
        Service::Gitea | Service::Forgejo | Service::Codeberg => Ok(Box::new(gitea::Gitea::new(
            target,
            head_owner,
            token,
            api_url_override,
        ))),
        Service::GitLab => Ok(Box::new(gitlab::GitLab::new(
            target,
            remotes.origin.clone(),
            token,
            api_url_override,
        )?)),
        // The unsupported services were already rejected above.
        Service::Bitbucket | Service::Unknown => unreachable!(),
    }
}

/// Find a token, most specific first: per-host config, then per-service config,
/// then a blanket config key, then the platform's conventional env var. Config
/// before env would invert the codebase's rule that the environment is the
/// deliberate, throwaway override — so env comes last only among *config*, but a
/// set env var still wins via the resolver elsewhere; here a token is a single
/// secret and the explicit per-host config is the most precise answer.
fn resolve_token(repo: &Repository, host: &str, service: Service) -> Option<String> {
    let config_keys = [
        format!("wits.forge.{host}.token"),
        format!("wits.forge.{}.token", service.as_str()),
        "wits.forge.token".to_owned(),
    ];
    for key in &config_keys {
        if let Some(v) = repo.get_config(key).ok().flatten() {
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

/// Shared helper for host modules: from a list of candidate MRs (already parsed),
/// return the first that satisfies the state filter. A branch has at most one MR
/// in a given state for our purposes, so "first" is unambiguous.
pub(crate) fn pick<'a>(
    candidates: impl IntoIterator<Item = &'a MergeRequest>,
    state: StateFilter,
) -> Option<MergeRequest> {
    candidates
        .into_iter()
        .find(|mr| state.accepts(mr.state))
        .cloned()
}

/// Shared helper for [`Forge::find_any`]: from the branch's candidate MRs, return
/// the open one if there is one, else the first non-open leftover. A branch has
/// at most one relevant MR per state, so this is unambiguous.
pub(crate) fn pick_any<'a>(
    candidates: impl IntoIterator<Item = &'a MergeRequest>,
) -> Option<MergeRequest> {
    let mut leftover = None;
    for mr in candidates {
        if mr.state == MrState::Open {
            return Some(mr.clone());
        }
        leftover.get_or_insert_with(|| mr.clone());
    }
    leftover
}

#[cfg(test)]
mod tests {
    use super::*;

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
