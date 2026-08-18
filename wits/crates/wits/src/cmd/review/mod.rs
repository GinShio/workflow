//! `wits review` — local-first, forge-agnostic code review.
//!
//! The mirror image of `stack`: where `stack` owns the *existence and structure*
//! of a set of MRs, `review` owns their *review content* — the diff a reviewer
//! reads, the threads they leave, the verdict they render. It never touches the
//! code or the branches; `git` and `stack` do that.
//!
//! Two principles shape it. Acquisition is **forge-first**: an MR is addressed
//! by number, and its objects are fetched and pinned locally, so any MR in the
//! repo can be reviewed without a local branch. And you **author by editing a
//! local file**, not by running commands — only two verbs touch the network,
//! `fetch` (read) and `submit` (write); in between you edit `local.json` and
//! `submit` flushes it as one batch. See `docs/review/design.md`.

mod checkout;
mod config;
mod diff;
mod fetch;
mod model;
mod prune;
mod show;
mod store;
mod submit;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use wits_util::forge::{self, Forge, RemoteInfo, Remotes};
use wits_util::git::Repository;

use store::Store;

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(subcommand)]
    pub action: ReviewAction,
}

#[derive(Debug, Subcommand)]
pub enum ReviewAction {
    /// Fetch an MR, a feed, or every feed from the forge into the local store.
    Fetch(FetchArgs),
    /// Show the inbox, or one MR's review state (`--json` for editors).
    Show(ShowArgs),
    /// Show a diff's coordinates for an MR (`--patch` for text, `--json` for editors).
    Diff(DiffArgs),
    /// Show the pending local draft for an MR (`--json` for editors).
    Draft(DraftArgs),
    /// Flush the local draft to the forge (the only network write).
    Submit(SubmitArgs),
    /// Materialize an MR's code into a worktree (or in place) to build and test.
    Checkout(CheckoutArgs),
    /// Drop the store for terminal (merged/closed) or dormant MRs.
    Prune(PruneArgs),
}

/// How much of an MR's stack a fetch pulls in. The same three modes govern both
/// a single `fetch <mr>` and a `fetch --feed`, so the rule is one thing to learn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StackMode {
    /// Never fetch the rest of the stack — only the named MR(s).
    None,
    /// Complete the stack for an MR that sits *on* another one (its base is a
    /// feature branch, i.e. it is not at the bottom). This pulls in a stack you
    /// are clearly inside, while a lone or bottom MR is left alone — so an inbox
    /// of unrelated MRs costs no extra forge calls. The default.
    #[default]
    Auto,
    /// Always try to complete the stack, even from its bottom-most MR — never
    /// misses a member, at the cost of one children probe per bottom/lone MR.
    All,
}

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// The MR to fetch, by number or URL (a full pull, with its stack per `--stack`).
    pub mr: Option<String>,
    /// Fetch a configured feed's MRs (a light, inbox-only refresh).
    #[arg(long, conflicts_with = "mr")]
    pub feed: Option<String>,
    /// How much of the stack to pull: `auto` (default) completes a stack for an
    /// MR sitting on another; `all` completes even from the bottom MR (never
    /// misses, more API calls); `none` fetches only the named MR(s). A feed may
    /// set its own default in `review.toml`; this flag overrides it.
    #[arg(long)]
    pub stack: Option<StackMode>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The MR to show. Omit for the inbox (every fetched MR).
    pub mr: Option<String>,
    /// Only threads whose anchored line has fallen out of the current diff.
    #[arg(long)]
    pub outdated: bool,
    /// Only resolved threads.
    #[arg(long)]
    pub resolved: bool,
    /// Only unresolved threads.
    #[arg(long, conflicts_with = "resolved")]
    pub unresolved: bool,
    /// Only threads whose last comment is someone else's.
    #[arg(long)]
    pub unread: bool,
    /// Only threads anchored in this file.
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,
    /// Emit machine-readable JSON — the stable editor contract.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// The MR whose diff to describe.
    pub mr: String,
    /// A git range or rev: `A..B`, a single `<sha>`, or `all` (base..head).
    #[arg(long, default_value = "all")]
    pub range: String,
    /// Browse a historical snapshot by its head SHA (a prefix is fine); its
    /// base..head replaces `--range`.
    #[arg(long, value_name = "SHA", conflicts_with = "range")]
    pub snapshot: Option<String>,
    /// Print the textual patch (shells to git) instead of coordinates.
    #[arg(long)]
    pub patch: bool,
    /// Emit machine-readable JSON.
    #[arg(long, conflicts_with = "patch")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DraftArgs {
    /// The MR whose draft to show or append to.
    pub mr: String,
    /// A JSON batch of actions to append to the draft; `-` or a file. When
    /// given, `draft` ingests (the tool owns the write); when omitted, it shows.
    pub input: Option<PathBuf>,
    /// Emit machine-readable JSON (on show).
    #[arg(long)]
    pub json: bool,
    /// Compact the append-only draft in place before showing it.
    #[arg(long)]
    pub dedup: bool,
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The MR to submit. Omit with `--all`.
    pub mr: Option<String>,
    /// Submit every MR in the stack around the given MR.
    #[arg(long, requires = "mr")]
    pub stack: bool,
    /// Submit every MR that has a pending draft.
    #[arg(long, conflicts_with_all = ["mr", "stack"])]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    /// The MR to materialize (optional with `--next`/`--prev`).
    pub mr: Option<String>,
    /// Materialize the next MR up the stack from the current checkout.
    #[arg(long, conflicts_with = "prev")]
    pub next: bool,
    /// Materialize the previous MR down the stack from the current checkout.
    #[arg(long)]
    pub prev: bool,
    /// Check out in the current working tree (moves HEAD; refuses a dirty tree)
    /// rather than adding a worktree.
    #[arg(long)]
    pub in_place: bool,
    /// Where to put the worktree (default: a sibling `<repo>.review/mr-<id>`).
    #[arg(long, value_name = "DIR", conflicts_with = "in_place")]
    pub worktree: Option<PathBuf>,
    /// After the checkout, initialise its submodules for this snapshot,
    /// **borrowing objects** from your primary checkout, so even large
    /// submodules cost no re-download. Only submodules present in the (possibly
    /// sparse) checkout are touched, and only this repo's direct submodules.
    /// Idempotent: run it as a cheap second pass over a checkout you first made
    /// lightweight.
    #[arg(long)]
    pub submodules: bool,
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Drop one MR by number or URL, whatever its state — for reclaiming a
    /// review worktree and store of an MR you're done with before it merges.
    #[arg(conflicts_with = "older_than")]
    pub mr: Option<String>,
    /// Also drop MRs untouched for at least this long: a number of days, or an
    /// ISO-8601 date (`2026-06-01`) — anything last updated before it.
    #[arg(long, value_name = "DAYS|DATE")]
    pub older_than: Option<String>,
}

pub fn run(args: &ReviewArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repository::new(&cwd);

    match &args.action {
        ReviewAction::Fetch(a) => fetch::run(&repo, a),
        ReviewAction::Show(a) => show::run(&repo, a),
        ReviewAction::Diff(a) => diff::run(&repo, a),
        ReviewAction::Draft(a) => show::run_draft(&repo, a),
        ReviewAction::Submit(a) => submit::run(&repo, a),
        ReviewAction::Checkout(a) => checkout::run(&repo, a),
        ReviewAction::Prune(a) => prune::run(&repo, a),
    }
}

// ---------------------------------------------------------------------------
// Shared context.
// ---------------------------------------------------------------------------

/// The repo's target identity and its store — everything a *local* verb needs,
/// without a forge token. Named to stand apart from [`model::Local`], the draft
/// JSON: this is the review *context*, that is the file you edit.
pub(crate) struct ReviewCtx {
    pub repo: Repository,
    pub target: RemoteInfo,
    pub store: Store,
}

pub(crate) fn local(repo: &Repository) -> Result<ReviewCtx> {
    let remotes = Remotes::resolve(repo);
    local_from_remotes(repo, &remotes)
}

fn local_from_remotes(repo: &Repository, remotes: &Remotes) -> Result<ReviewCtx> {
    let target = remotes
        .target()
        .cloned()
        .context("no 'origin' or 'upstream' remote to derive the forge from")?;
    let store = Store::open(repo, &target)?;
    Ok(ReviewCtx {
        repo: repo.clone(),
        target,
        store,
    })
}

/// A [`ReviewCtx`] plus a live forge — what the network verbs (`fetch`,
/// `submit`) need. Detecting the forge resolves the token, so a missing token is
/// reported here.
pub(crate) struct Online {
    pub local: ReviewCtx,
    pub remotes: Remotes,
    pub forge: Box<dyn Forge>,
}

pub(crate) fn online(repo: &Repository) -> Result<Online> {
    let remotes = Remotes::resolve(repo);
    let forge = forge::detect(repo, &remotes)?;
    let local = local_from_remotes(repo, &remotes)?;
    Ok(Online {
        local,
        remotes,
        forge,
    })
}

/// How many forge operations run at once. Review submissions are independent per
/// MR and network-bound, so scoped OS threads keep the latency down without
/// any tuning burden.
const MAX_PARALLEL: usize = 8;

/// Run `f` over `items` with bounded parallelism, returning results in input
/// order. Uses scoped threads so the closure can borrow freely from the
/// surrounding stack frame — no `Arc`, no `'static` bounds.
pub(crate) fn map_parallel<I, T>(items: &[I], f: impl Fn(&I) -> T + Sync) -> Vec<T>
where
    I: Sync,
    T: Send,
{
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(MAX_PARALLEL.max(1)) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|item| scope.spawn(|| f(item))).collect();
            for handle in handles {
                out.push(handle.join().expect("worker thread panicked"));
            }
        });
    }
    out
}

/// The **single** review worktree's location: a peer of the repository's own
/// checkouts, named `review`. There is one review worktree, not one per MR —
/// `checkout` re-points it at each MR by switching HEAD, so a whole stack costs
/// one worktree and one submodule tree. `checkout` creates it (unless
/// `--worktree` overrides); `prune` reclaims it once the store is empty.
///
/// The `review` suffix is review's contribution; where a worktree of that name
/// goes (and why it is anchored on the repository rather than the caller's cwd)
/// belongs to [`wits_util::worktree::default_dir`].
pub(crate) fn default_worktree_dir(repo: &Repository) -> PathBuf {
    wits_util::worktree::default_dir(repo, "review")
}

/// Parse an MR handle: a bare number, or a forge URL whose last numeric path
/// segment is the number (`…/pull/123`, `…/merge_requests/123`).
pub(crate) fn parse_mr_handle(handle: &str) -> Result<String> {
    let handle = handle.trim();
    if !handle.is_empty() && handle.chars().all(|c| c.is_ascii_digit()) {
        return Ok(handle.to_owned());
    }
    handle
        .trim_end_matches('/')
        .rsplit(['/', '#'])
        .find(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .with_context(|| format!("could not read an MR number from '{handle}'"))
}

/// A sort key that orders MR ids numerically when they are numbers (so `2`
/// precedes `10`), falling back to lexical order for anything non-numeric.
fn mr_id_key(id: &str) -> (u64, &str) {
    (id.parse::<u64>().unwrap_or(u64::MAX), id)
}

/// Reconstruct the stack chain containing `current`, bottom (base-most) to top,
/// by linking one MR's `base` to another's `source` branch. Returns the ordered
/// ids and `current`'s index. A lone MR yields just itself.
pub(crate) fn stack_chain(infos: &[model::Info], current: &str) -> (Vec<String>, usize) {
    use std::collections::{HashMap, HashSet};

    let by_id: HashMap<&str, &model::Info> = infos.iter().map(|i| (i.mr.id.as_str(), i)).collect();
    let by_source: HashMap<&str, &str> = infos
        .iter()
        .filter(|i| !i.mr.source.is_empty())
        .map(|i| (i.mr.source.as_str(), i.mr.id.as_str()))
        .collect();
    let mut by_base: HashMap<&str, Vec<&str>> = infos.iter().fold(HashMap::new(), |mut map, i| {
        if !i.mr.base.is_empty() {
            map.entry(i.mr.base.as_str())
                .or_default()
                .push(i.mr.id.as_str());
        }
        map
    });
    // A fork (one base, several children) has no inherent "primary line", so we
    // pick one deterministically — the lowest MR id, numeric-aware — rather than
    // whatever order the infos happened to arrive in. Navigation is then stable
    // across fetches instead of depending on HashMap iteration order.
    for children in by_base.values_mut() {
        children.sort_by(|a, b| mr_id_key(a).cmp(&mr_id_key(b)));
    }

    let Some(cur) = by_id.get(current) else {
        return (vec![current.to_owned()], 0);
    };

    let mut seen: HashSet<&str> = HashSet::from([current]);
    let mut down = Vec::new();
    let mut node = *cur;
    while let Some(&prev) = by_source.get(node.mr.base.as_str()) {
        if !seen.insert(prev) {
            break;
        }
        down.push(prev.to_owned());
        node = by_id[prev];
    }
    down.reverse();

    let mut up = Vec::new();
    let mut node = *cur;
    while let Some(children) = by_base.get(node.mr.source.as_str()) {
        if children.len() > 1 {
            log::warn!(
                "stack fork at {}: {} children ({}), following first",
                node.mr.source,
                children.len(),
                children.join(", ")
            );
        }
        let &next = &children[0];
        if !seen.insert(next) {
            break;
        }
        up.push(next.to_owned());
        node = by_id[next];
    }

    let index = down.len();
    let mut chain = down;
    chain.push(current.to_owned());
    chain.extend(up);
    (chain, index)
}

#[cfg(test)]
pub(crate) fn stub_info(id: &str, base: &str, source: &str) -> model::Info {
    model::Info {
        schema: model::SCHEMA,
        mr: wits_util::forge::MrSummary {
            id: id.into(),
            display: format!("#{id}"),
            state: wits_util::forge::MrState::Open,
            draft: false,
            title: String::new(),
            author: String::new(),
            base: base.into(),
            source: source.into(),
            head_sha: None,
            updated_at: String::new(),
            labels: Vec::new(),
            web_url: String::new(),
        },
        snapshots: Vec::new(),
        fetched_at: 0,
        commits: Vec::new(),
        files: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_a_stack_from_mr_base_links() {
        let infos = vec![
            stub_info("a", "main", "feat-a"),
            stub_info("b", "feat-a", "feat-b"),
            stub_info("c", "feat-b", "feat-c"),
        ];
        assert_eq!(
            stack_chain(&infos, "b"),
            (vec!["a".into(), "b".into(), "c".into()], 1)
        );
        assert_eq!(
            stack_chain(&infos, "a"),
            (vec!["a".into(), "b".into(), "c".into()], 0)
        );
    }

    #[test]
    fn chain_order_follows_links_not_mr_numbers() {
        // MRs in a stack are opened in parallel, so their numbers need not
        // increase with stack position. Reconstruction must follow the
        // base/source links, not the ids: here the base-most MR has the highest
        // number and the tip the middle one.
        let infos = vec![
            stub_info("10", "main", "feat-a"),
            stub_info("3", "feat-a", "feat-b"),
            stub_info("7", "feat-b", "feat-c"),
        ];
        assert_eq!(
            stack_chain(&infos, "3"),
            (vec!["10".into(), "3".into(), "7".into()], 1),
            "chain is base->tip by links, independent of id magnitude"
        );
    }

    #[test]
    fn a_lone_mr_is_its_own_chain() {
        let infos = vec![stub_info("x", "main", "feature-x")];
        assert_eq!(stack_chain(&infos, "x"), (vec!["x".to_owned()], 0));
        assert_eq!(stack_chain(&infos, "z"), (vec!["z".to_owned()], 0));
    }

    #[test]
    fn fork_follows_the_lowest_id_child_regardless_of_order() {
        // Two MRs branch off the same base (a fork). The chain follows a single
        // primary line, chosen deterministically as the lowest MR id — and the
        // choice must not depend on the order the infos arrive in.
        let forward = vec![
            stub_info("1", "main", "feat-a"),
            stub_info("2", "feat-a", "feat-b"),
            stub_info("3", "feat-a", "feat-c"),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();

        for infos in [&forward, &reversed] {
            let (chain, idx) = stack_chain(infos, "1");
            assert_eq!(idx, 0);
            assert_eq!(chain, ["1", "2"], "fork must pick the lowest-id child");
        }
    }

    #[test]
    fn parses_mr_numbers_and_urls() {
        assert_eq!(parse_mr_handle("123").unwrap(), "123");
        assert_eq!(
            parse_mr_handle("https://github.com/o/r/pull/456").unwrap(),
            "456"
        );
        assert_eq!(
            parse_mr_handle("https://gitlab.com/o/r/-/merge_requests/789/").unwrap(),
            "789"
        );
        assert!(parse_mr_handle("not-a-number").is_err());
    }
}
