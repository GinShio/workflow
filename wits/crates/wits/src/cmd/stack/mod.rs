//! `wits stack` — turning a chain of local branches into a navigable set of MRs.
//!
//! The verbs are deliberately orthogonal facets of remote state: `sync` is
//! branch content (push), `submit` is MR existence and base, `anno` is the MR
//! description. Each is an idempotent reconcile that can be re-run on its own,
//! which is what makes a stack workflow recoverable — when one step fails you
//! re-run that step, not a monolith. `slice` is the one local authoring verb.
//!
//! All four share a single notion of *scope* (which branches this invocation
//! touches), computed once in `resolution`; see `docs/stack/design.md` for the
//! reasoning behind the topology rules and the forge abstraction.

mod anno;
mod decorate;
mod resolution;
mod slice;
mod submit;
mod sync;
mod topology;
mod tree;

use clap::{Args, Subcommand, ValueEnum};

use wits_util::forge::{self, Forge, MergeRequest, Remotes, StateFilter};
use wits_util::git::Repository;

/// How many forge/push operations run at once. Stacks are small and the work is
/// network-bound, so a modest fixed width keeps us from opening a connection per
/// branch without any real tuning need.
const MAX_PARALLEL: usize = 8;

#[derive(Debug, Args)]
pub struct StackArgs {
    #[command(subcommand)]
    pub action: StackAction,
}

#[derive(Debug, Subcommand)]
pub enum StackAction {
    /// Push in-scope branches to origin (force-with-lease).
    Sync(ScopeArgs),
    /// Create missing MRs and correct drifted bases.
    Submit(SubmitArgs),
    /// Rewrite MR descriptions with stack navigation.
    Anno(ScopeArgs),
    /// Add labels / assignees / reviewers to an MR (additive).
    Decorate(DecorateArgs),
    /// Interactively cut HEAD's commits into a stack of branches.
    Slice(SliceArgs),
    /// Edit the stack's structure in `.git/machete` (prune, remove, move).
    Tree(TreeArgs),
}

#[derive(Debug, Args)]
pub struct TreeArgs {
    #[command(subcommand)]
    pub action: TreeAction,
}

#[derive(Debug, Subcommand)]
pub enum TreeAction {
    /// Drop entries whose local branch no longer exists (children splice up).
    Prune,
    /// Remove branches from the stack; their children splice up to the parent.
    Rm(RmArgs),
    /// Move a branch — and everything stacked on it — onto a new parent.
    Mv(MvArgs),
}

#[derive(Debug, Args)]
pub struct RmArgs {
    /// Branches to drop from the stack.
    #[arg(required = true)]
    pub branches: Vec<String>,

    /// Also delete the git branch (refuses an unmerged branch without --force).
    #[arg(long)]
    pub delete: bool,

    /// With --delete, force-delete an unmerged branch (`git branch -D`).
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MvArgs {
    /// The branch to move. Created in the stack if not already recorded.
    pub branch: String,

    /// The new parent (an existing branch, or the base branch).
    #[arg(long)]
    pub onto: String,
}

/// Scope shared by the verbs that walk the stack. The optional branch is a
/// *scope anchor*: the whole stack around it is operated on, not just that one
/// branch (that is the per-stack semantics, unlike `decorate`'s per-MR branch).
/// It defaults to the checked-out branch and is mutually exclusive with `--all`.
#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// Branch to anchor the stack on (default: the current branch). The whole
    /// stack around it is operated on, not just this branch. Not valid with --all.
    pub branch: Option<String>,

    /// Operate on every recorded stack, not just the current branch's.
    #[arg(long, conflicts_with = "branch")]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    #[command(flatten)]
    pub scope: ScopeArgs,

    /// Open MRs ready for review. By default a mid-stack MR (one not targeting
    /// the base branch) starts as a draft, since it shouldn't merge before what
    /// it sits on.
    #[arg(long)]
    pub no_draft: bool,

    /// Recreate an MR even when a closed/merged one already exists at the same
    /// commit. Without this, a leftover closed MR at the current tip is left
    /// alone rather than re-opened.
    #[arg(long)]
    pub force: bool,

    /// Which commit's message seeds a new MR's title and body.
    #[arg(long, value_enum, default_value_t = TitleSource::Last)]
    pub title_source: TitleSource,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TitleSource {
    /// The oldest commit on the branch.
    First,
    /// The newest commit on the branch.
    Last,
}

#[derive(Debug, Args)]
pub struct DecorateArgs {
    /// The branch whose MR to decorate (default: current). Not valid with --all.
    pub branch: Option<String>,

    /// Apply the same attributes to every MR in the current stack instead of one.
    #[arg(long, conflicts_with = "branch")]
    pub all: bool,

    /// A label to add (repeatable).
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,

    /// A reviewer to request; `@me` is you (repeatable).
    #[arg(long = "reviewer", value_name = "USER")]
    pub reviewers: Vec<String>,

    /// An assignee to add; `@me` is you (repeatable).
    #[arg(long = "assignee", value_name = "USER")]
    pub assignees: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SliceArgs {
    /// Slice commits reachable from HEAD but not this branch (default: the
    /// resolved base branch).
    #[arg(long)]
    pub base: Option<String>,
}

pub fn run(args: &StackArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repository::new(&cwd);

    match &args.action {
        StackAction::Sync(s) => sync::run(&repo, s),
        StackAction::Submit(s) => submit::run(&repo, s),
        StackAction::Anno(s) => anno::run(&repo, s),
        StackAction::Decorate(d) => decorate::run(&repo, d),
        StackAction::Slice(s) => slice::run(&repo, s.base.as_deref()),
        StackAction::Tree(t) => tree::run(&repo, &t.action),
    }
}

/// Run `f` over `items` with bounded parallelism, returning results in input
/// order. The work is independent per branch and dominated by network latency,
/// so scoped OS threads are the natural fit — no async runtime, and the scope
/// guarantees every borrow outlives the threads.
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

/// Fold an accumulated per-item failure count into the command's exit status.
///
/// The MR verbs (`submit`, `anno`, `decorate`) log a warning and carry on when
/// one branch fails, so a single bad MR never strands the rest of the batch. But
/// the *command* must still exit non-zero when anything failed — otherwise a
/// script sees success while MRs silently went untouched. This is the shared
/// tail that makes that true, matching `sync`'s all-or-nothing exit.
pub(crate) fn fail_if_any(failures: usize) -> anyhow::Result<()> {
    if failures > 0 {
        anyhow::bail!("{failures} branch(es) failed");
    }
    Ok(())
}

/// A forge for this repo plus its user-facing noun ("PR"/"MR"), resolved once.
///
/// The three MR verbs (`submit`, `anno`, `decorate`) all open on the same
/// `Remotes::resolve → detect → noun` bootstrap, so it lives here rather than
/// being re-typed — and copied wrong — in each verb.
pub(crate) struct ForgeSession {
    pub forge: Box<dyn Forge>,
    pub noun: &'static str,
}

impl ForgeSession {
    pub(crate) fn open(repo: &Repository) -> anyhow::Result<Self> {
        let remotes = Remotes::resolve(repo);
        let forge = forge::detect(repo, &remotes)?;
        let noun = forge.noun();
        Ok(Self { forge, noun })
    }
}

/// Find the open MR for each branch, in parallel, applying the verbs' shared
/// reconciliation: a branch with no open MR logs a "no open <noun>" note, an
/// error is counted and warned. Returns the `(branch, MR)` pairs that were found
/// (in `branches` order) and the failure tally — the common front half of `anno`
/// and `decorate`, which then each do their own thing with the MRs that exist.
pub(crate) fn find_open_mrs(
    session: &ForgeSession,
    branches: &[String],
) -> (Vec<(String, MergeRequest)>, usize) {
    let found = map_parallel(branches, |branch| {
        (
            branch.clone(),
            session.forge.find(branch, StateFilter::Open),
        )
    });
    let mut mrs = Vec::new();
    let mut failures = 0usize;
    for (branch, result) in found {
        match result {
            Ok(Some(mr)) => mrs.push((branch, mr)),
            Ok(None) => log::info!("{branch}: no open {}", session.noun),
            Err(e) => {
                failures += 1;
                log::warn!("{branch}: {e}");
            }
        }
    }
    (mrs, failures)
}
