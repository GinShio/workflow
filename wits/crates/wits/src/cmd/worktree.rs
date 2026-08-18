//! `wits worktree` — make, inspect, and reclaim git worktrees, in any repo.
//!
//! Deliberately **project-agnostic**: it works on whatever repository you are
//! standing in, reads nothing from the project registry, and keeps no state of
//! its own. Everything it needs it asks git. That is what lets it serve both a
//! registered project and a repo you cloned five minutes ago.
//!
//! It is not a wrapper around `git worktree`. Three verbs exist because git
//! leaves three gaps:
//!
//! - **`create`** — `git worktree add` never materialises submodules, and a
//!   linked worktree shares no submodule object store with the primary, so the
//!   obvious `submodule update --init` re-clones everything. `--submodules`
//!   borrows instead.
//! - **`info`** — `git worktree list` says where a worktree is, not whether you
//!   still need it. This adds the facts that answer that.
//! - **`prune`** — `git worktree prune` only forgets records of directories that
//!   are *already* deleted. It will not reclaim a worktree whose branch merged
//!   last week. This does, and folds git's record cleanup in.
//!
//! `move`/`lock`/`unlock`/`repair` are absent on purpose: they would be pure
//! pass-throughs, and `git worktree` already spells them.
//!
//! The mechanics — where a worktree goes, how the add is driven, the submodule
//! borrow, the reclaim predicates — are [`wits_util::worktree`], shared with
//! `wits review checkout`. This module owns the *rendering*, plus the one policy
//! the library deliberately leaves to its caller: [`default_sweep`], what a bare
//! `prune` reclaims.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use wits_util::git::{Repository, StatusCounts};
use wits_util::time::{age_since, parse_cutoff};
use wits_util::worktree::{self, Entry, Filter, Inventory};

/// Width of the label column, sized to the longest label (`submodules`) plus a
/// gap. Shared by the repository block and the panel so the two line up.
const LABEL: usize = 12;

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub action: WorktreeAction,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeAction {
    /// Create a new worktree for an existing branch.
    Create(CreateArgs),
    /// Move an existing worktree to another branch.
    Switch(SwitchArgs),
    /// List every worktree and what you'd need to know to reclaim it, or show one.
    Info(InfoArgs),
    /// Remove worktrees whose work is finished, or one you name.
    Prune(PruneArgs),
}

/// What to check out, and whether to attach to it.
///
/// `create` and `switch` take exactly the same pair, because "which commit" and
/// "as a branch or detached" are the same two questions whether the worktree is
/// new or already on disk.
#[derive(Debug, Args)]
pub struct RevArgs {
    /// The branch to check out. It must already **exist locally** — creating a
    /// worktree and creating a branch are separate acts, and this verb does only
    /// the one you asked for. With `--detach`, any revision instead.
    pub rev: String,
    /// Check `rev` out without attaching to a branch, leaving a detached HEAD.
    /// This is how to take any non-branch revision — a tag, a commit, or a
    /// remote-tracking branch such as `origin/theirs` — without adding a local
    /// branch you did not ask for.
    #[arg(long)]
    pub detach: bool,
    /// Also materialise submodules, **borrowing objects** from your primary
    /// checkout so even large submodules cost no re-download. Idempotent, so it
    /// also works as a second pass over a worktree you first made lightweight.
    #[arg(long)]
    pub submodules: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub rev: RevArgs,
    /// Where to put it. Defaults to a sibling of the main worktree named after
    /// the revision (`../<repo>.<slug>`), which keeps worktrees of one repo
    /// together and never nests one inside another.
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SwitchArgs {
    #[command(flatten)]
    pub rev: RevArgs,
    /// Which worktree to move, by path, branch, or directory name. Omit for the
    /// one you are standing in.
    pub target: Option<String>,
}

/// The predicates shared by `info` (which shows) and `prune` (which removes), so
/// `info --merged` is an exact preview of what `prune --merged` would take.
#[derive(Debug, Args)]
pub struct SelectArgs {
    /// Worktrees whose work already landed on the trunk.
    #[arg(long)]
    pub merged: bool,
    /// Worktrees whose branch had an upstream that is now gone from the remote —
    /// how a merged-and-deleted branch looks after `git fetch --prune`.
    #[arg(long)]
    pub gone: bool,
    /// Worktrees whose HEAD commit is older than this: a number of days (`30`,
    /// `30d`, `4w`) or an ISO-8601 date (`2026-06-01`).
    ///
    /// Dormancy is never implied. A bare `prune` reclaims only work that
    /// demonstrably landed; "nobody has touched this lately" is a judgement call,
    /// so it applies only when you ask for it here.
    #[arg(long, value_name = "DAYS|DATE")]
    pub older_than: Option<String>,
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// One worktree, named by its path, its branch, or its directory name. Shown
    /// as a full panel. Omit for a table of all of them.
    #[arg(conflicts_with_all = ["merged", "gone", "older_than"])]
    pub target: Option<String>,
    #[command(flatten)]
    pub select: SelectArgs,
    /// Show every worktree as a full panel rather than one table row — the long
    /// form of the same listing, the way `git status --long` relates to `--short`.
    #[arg(long)]
    pub long: bool,
    /// Print just the path, one per line — for `cd "$(wits worktree info x --path)"`.
    #[arg(long, conflicts_with = "long")]
    pub path: bool,
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Drop one worktree, named by its path, branch, or directory name —
    /// whatever its state. The "I'm done with this one" path.
    #[arg(conflicts_with_all = ["merged", "gone", "older_than"])]
    pub target: Option<String>,
    #[command(flatten)]
    pub select: SelectArgs,
    /// Remove even a worktree holding uncommitted changes, discarding them.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: &WorktreeArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repository::new(&cwd);
    // `git_dir`, not `is_repo`: the latter asks whether we are inside a *work
    // tree*, which excludes a bare repository — and "one bare clone plus a
    // worktree per branch" is a setup this command exists to serve. Everything
    // here works there, since `worktree::anchor` falls back to the repo itself
    // when there is no main working tree to anchor to.
    if repo.git_dir().is_none() {
        bail!("not a git repository ({})", cwd.display());
    }
    match &args.action {
        WorktreeAction::Create(a) => create(&repo, a),
        WorktreeAction::Switch(a) => switch(&repo, a),
        WorktreeAction::Info(a) => info(&repo, a),
        WorktreeAction::Prune(a) => prune(&repo, a),
    }
}

/// Refuse a revision that would not mean what the caller asked for.
///
/// Without `--detach` the revision must be a **local branch**: `git worktree add`
/// would otherwise happily invent one (from a same-named remote branch, or from
/// the directory name), and `git checkout` would silently detach. Both are
/// different acts from the one that was requested, so each gets an error naming
/// the flag or the command that does mean it.
fn require_checkoutable(repo: &Repository, args: &RevArgs) -> Result<()> {
    let rev = &args.rev;
    if args.detach {
        if !repo.rev_exists(rev) {
            bail!("'{rev}' does not name an existing commit, branch, or tag");
        }
        return Ok(());
    }
    if repo.local_branch_exists(rev) {
        return Ok(());
    }
    // Not a local branch. Which of the three near-misses is it?
    let candidates = repo.remote_tracking_candidates(rev);
    if let Some(remote) = candidates.first() {
        bail!(
            "branch '{rev}' does not exist locally, only as '{remote}'; \
             create it with `git branch {rev} {remote}`, \
             or use --detach to check out '{remote}' without adding a branch"
        );
    }
    if repo.rev_exists(rev) {
        bail!("'{rev}' is not a branch; use --detach to check it out without one");
    }
    bail!("branch '{rev}' does not exist")
}

/// Materialise submodules for a checkout, reporting what it found.
fn report_submodules(dir: &std::path::Path) -> Result<()> {
    // A dry run never moved the checkout, so there is nothing to inspect: say what
    // would happen instead of reporting on a directory that may not be there.
    if wits_util::log::is_dry_run() && !dir.exists() {
        wits_util::log::dry_run(&format!("materialise submodules in {}", dir.display()));
        return Ok(());
    }
    match worktree::sync_submodules(dir)? {
        0 => log::info!("no submodules present"),
        n => log::info!("synced {n} submodule(s)"),
    }
    Ok(())
}

/// What a bare `prune` reclaims: the two signals that work **demonstrably
/// landed**. Dormancy is deliberately absent — "untouched for a while" is not
/// evidence that work is finished, so it stays behind `--older-than`.
///
/// `info` reads this too, so the `prune` row of a panel and the sweep itself can
/// never disagree about the default.
fn default_sweep() -> Filter {
    Filter {
        merged: true,
        gone: true,
        dormant_before: None,
    }
}

/// Turn the selection flags into a [`Filter`], resolving `--older-than` to an
/// instant. An all-false result means the caller asked for nothing, which `info`
/// reads as *show everything* and `prune` as *use the default*.
fn build_filter(args: &SelectArgs) -> Result<Filter> {
    Ok(Filter {
        merged: args.merged,
        gone: args.gone,
        dormant_before: args.older_than.as_deref().map(parse_cutoff).transpose()?,
    })
}

// --- create -------------------------------------------------------------------

fn create(repo: &Repository, args: &CreateArgs) -> Result<()> {
    require_checkoutable(repo, &args.rev)?;
    let rev = &args.rev.rev;

    // Resolve a user-supplied path against *this* cwd up front, so the
    // already-exists check below and the worktree that gets made agree on which
    // directory is meant (see `worktree::create`).
    let dir = match &args.dir {
        Some(dir) => {
            std::path::absolute(dir).with_context(|| format!("resolving {}", dir.display()))?
        }
        None => worktree::default_dir(repo, &worktree::slug(rev)),
    };

    // Idempotent, like the rest of the toolset: a second run reports and returns
    // rather than failing, so `create` is safe to put in a script or a hook. It
    // deliberately does *not* move an existing worktree onto `rev` — that would
    // pull someone's HEAD out from under them on what reads like a create, and it
    // is what `switch` is for.
    if dir.exists() {
        log::info!("worktree already exists at {}", dir.display());
    } else {
        worktree::create(repo, &dir, rev)?;
        log::info!(
            "{} worktree for '{rev}' at {}",
            past_or_planned("created", "would create"),
            dir.display()
        );
    }

    if args.rev.submodules {
        report_submodules(&dir)?;
    }
    Ok(())
}

// --- switch -------------------------------------------------------------------

fn switch(repo: &Repository, args: &SwitchArgs) -> Result<()> {
    require_checkoutable(repo, &args.rev)?;
    let rev = &args.rev.rev;

    let inventory = Inventory::gather(repo);
    let entry = match &args.target {
        Some(target) => inventory.resolve(target)?,
        // No target means "the one I am in", which is the only worktree a bare
        // `switch` could sensibly mean. Standing outside every worktree — in a
        // bare repository's own directory, say — there is nothing to default to.
        None => inventory
            .entries()
            .iter()
            .find(|entry| entry.is_current)
            .context("not inside a worktree of this repository; name the one to switch")?,
    };

    if entry.bare {
        bail!(
            "{} is a bare repository — it has no working tree to switch",
            entry.path.display()
        );
    }
    if entry.prunable {
        bail!(
            "the worktree recorded at {} is gone; `wits worktree prune` forgets the record",
            entry.path.display()
        );
    }

    worktree::repoint(&entry.path, rev)?;
    log::info!(
        "{} {} to '{rev}'",
        past_or_planned("switched", "would switch"),
        entry.path.display()
    );

    if args.rev.submodules {
        report_submodules(&entry.path)?;
    }
    Ok(())
}

/// Pick the tense for a log line describing a mutation. Under `-n` the git calls
/// are printed rather than performed, so a line that claimed to have removed
/// something would be a plain lie about what happened.
fn past_or_planned(past: &'static str, planned: &'static str) -> &'static str {
    if wits_util::log::is_dry_run() {
        planned
    } else {
        past
    }
}

// --- info ---------------------------------------------------------------------

fn info(repo: &Repository, args: &InfoArgs) -> Result<()> {
    let inventory = Inventory::gather(repo);
    let filter = build_filter(&args.select)?;

    // A named target is shown whatever its state; otherwise the filter decides,
    // and an absent filter shows everything.
    let selected: Vec<&Entry> = match &args.target {
        Some(target) => vec![inventory.resolve(target)?],
        None => inventory
            .entries()
            .iter()
            .filter(|entry| filter.is_empty() || filter.reason(entry).is_some())
            .collect(),
    };

    if args.path {
        for entry in &selected {
            println!("{}", entry.path.display());
        }
        return Ok(());
    }

    // The repository block belongs to a *listing* — it describes the repo, not a
    // worktree. Naming one worktree is a question about that worktree, and its
    // `trunk` row already spells the trunk out, so the block would be noise.
    if args.target.is_none() {
        print_repository(repo, &inventory);
        println!();
    }

    if args.target.is_some() || args.long {
        // Which filter the `prune` row answers for: the one the caller asked
        // about, or the default sweep when they asked for nothing.
        let verdict = if filter.is_empty() {
            default_sweep()
        } else {
            filter
        };
        for (index, entry) in selected.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_panel(entry, inventory.trunk(), &verdict);
        }
        if selected.is_empty() {
            println!("(no worktree matches)");
        }
    } else {
        print_table(&selected, &filter);
    }
    Ok(())
}

/// The repository itself: where its git dir is, and what `merged` is measured
/// against. Both are properties of the repository rather than of any worktree,
/// which is why they sit in their own block above the listing.
fn print_repository(repo: &Repository, inventory: &Inventory) {
    let bare = inventory.entries().first().is_some_and(|entry| entry.bare);
    let suffix = if bare { " (bare)" } else { "" };
    if let Some(dir) = repo.git_common_dir() {
        println!("{:<LABEL$}{}{suffix}", "repository", dir.display());
    }
    match inventory.trunk() {
        Some(trunk) => println!("{:<LABEL$}{trunk}", "trunk"),
        None => println!(
            "{:<LABEL$}(none — the repo publishes no origin/HEAD)",
            "trunk"
        ),
    }
}

fn print_table(rows: &[&Entry], filter: &Filter) {
    if rows.is_empty() {
        println!("(no worktree matches)");
        return;
    }
    let states: Vec<String> = rows.iter().map(|e| state_of(e, filter)).collect();
    let branches: Vec<String> = rows.iter().map(|e| branch_label(e)).collect();

    // Every column but the last is padded to its widest cell (header included),
    // so PATH — the only unbounded one — is what trails.
    let width = |header: &str, cells: &[String]| {
        cells
            .iter()
            .map(|c| c.chars().count())
            .chain(std::iter::once(header.chars().count()))
            .max()
            .unwrap_or(0)
    };
    let bw = width("BRANCH", &branches);
    let sw = width("STATE", &states);
    let heads: Vec<String> = rows
        .iter()
        .map(|e| e.short_head.clone().unwrap_or_else(|| "-".to_owned()))
        .collect();
    let hw = width("HEAD", &heads);

    println!("{:<bw$}  {:<hw$}  {:<sw$}  PATH", "BRANCH", "HEAD", "STATE");
    for (index, entry) in rows.iter().enumerate() {
        println!(
            "{:<bw$}  {:<hw$}  {:<sw$}  {}",
            branches[index],
            heads[index],
            states[index],
            entry.path.display()
        );
    }
}

fn print_panel(entry: &Entry, trunk: Option<&str>, verdict: &Filter) {
    println!("{}", entry.path.display());
    let row = |label: &str, value: String| println!("  {:<LABEL$}{value}", label);

    row("branch", branch_label(entry));
    // No HEAD means nothing to date or to measure against the trunk — a bare
    // repository. Both rows would be blanks, so neither appears.
    if let Some(short) = &entry.short_head {
        let age = entry.head_time.map(age_since);
        row(
            "head",
            match age {
                Some(age) => format!("{short}   {age}"),
                None => short.clone(),
            },
        );
        row("trunk", trunk_phrase(entry, trunk));
    }
    // `changes` is about a working tree, so where there is none it says so rather
    // than claiming a directory that cannot be read is clean.
    row(
        "changes",
        match (entry.bare, entry.prunable) {
            (true, _) => "(bare — no working tree)".to_owned(),
            (_, true) => "(the directory is gone)".to_owned(),
            _ => changes_phrase(&entry.changes),
        },
    );

    // Conditional rows: each appears only when it has something to say, so every
    // line present in a panel carries signal.
    if let Some(tracking) = &entry.tracking {
        let state = if entry.upstream_gone {
            "gone from the remote".to_owned()
        } else {
            distance(entry.tracking_ahead, entry.tracking_behind)
        };
        row("tracking", format!("{tracking} — {state}"));
    }
    if entry.locked {
        row(
            "locked",
            entry
                .lock_reason
                .clone()
                .unwrap_or_else(|| "(no reason given)".to_owned()),
        );
    }
    let details = worktree::details(entry);
    if !details.sparse.is_empty() {
        row("sparse", details.sparse.join(", "));
    }
    if details.submodules > 0 {
        row(
            "submodules",
            format!(
                "{} of {} initialised",
                details.submodules_initialised, details.submodules
            ),
        );
    }
    row("prune", prune_phrase(entry, verdict, trunk));
}

/// What to show in the branch column. A bare repository and a detached HEAD both
/// report no branch, but they are not the same thing, so they do not share a label.
fn branch_label(entry: &Entry) -> String {
    match (&entry.branch, entry.bare) {
        (Some(branch), _) => branch.clone(),
        (None, true) => "(bare)".to_owned(),
        (None, false) => "(detached)".to_owned(),
    }
}

/// The facts worth showing beside a worktree in a table, in the order a reader
/// cares about: what protects it, what makes it a candidate, what would be lost.
fn state_of(entry: &Entry, filter: &Filter) -> String {
    // Exclusive: with the directory gone, nothing else about this entry bears on
    // what happens to it — only the record is left, and only `prune` forgets it.
    // Listing it as "records only, merged" would invite the reader to expect a
    // sweep to take it for being merged, which is not what happens.
    if entry.prunable {
        return "records only".to_owned();
    }
    let tags: Vec<&str> = [
        (entry.is_main, "main"),
        (entry.is_current, "current"),
        (entry.locked, "locked"),
        (entry.dirty(), "dirty"),
        (entry.merged() && !entry.is_main, "merged"),
        (entry.upstream_gone, "upstream gone"),
        (filter.reason(entry) == Some("dormant"), "dormant"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, label)| label)
    .collect();
    if tags.is_empty() {
        "-".to_owned()
    } else {
        tags.join(", ")
    }
}

/// Commits each side has that the other lacks, in words. Zero components are left
/// out — "0 behind" is noise — and no `+`/`-` appears, since those read as counts
/// of changed *lines*.
fn distance(ahead: u32, behind: u32) -> String {
    match (ahead, behind) {
        (0, 0) => "up to date".to_owned(),
        (ahead, 0) => format!("{ahead} ahead"),
        (0, behind) => format!("{behind} behind"),
        (ahead, behind) => format!("{ahead} ahead, {behind} behind"),
    }
}

fn trunk_phrase(entry: &Entry, trunk: Option<&str>) -> String {
    let Some(trunk) = trunk else {
        return "(none found, so nothing reads as merged)".to_owned();
    };
    match (entry.trunk_ahead, entry.trunk_behind) {
        (Some(0), _) => format!("merged into {trunk}"),
        (Some(ahead), Some(0)) => format!("{ahead} ahead of {trunk}"),
        (Some(ahead), Some(behind)) => format!("{ahead} ahead, {behind} behind {trunk}"),
        _ => format!("unknown against {trunk}"),
    }
}

fn changes_phrase(counts: &StatusCounts) -> String {
    if counts.is_clean() {
        return "clean".to_owned();
    }
    [
        (counts.staged, "staged"),
        (counts.modified, "modified"),
        (counts.untracked, "untracked"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// What `prune` would do to this worktree, and why.
///
/// The *decision* is always [`Filter::reason`]'s — this only chooses the wording,
/// including the specific phrasing each kind of immunity deserves (they all look
/// alike to `reason`, which just returns `None`). A future immunity that is not
/// spelled out here therefore lands on the generic "nothing selects it": less
/// informative, never wrong about the action.
fn prune_phrase(entry: &Entry, filter: &Filter, trunk: Option<&str>) -> String {
    if entry.is_main {
        return "never — this is the repository itself".to_owned();
    }
    if entry.is_current {
        return "never — you are in it".to_owned();
    }
    if entry.prunable {
        return "n/a — only a stale record remains".to_owned();
    }
    if entry.locked {
        return "kept — locked".to_owned();
    }
    match filter.reason(entry) {
        Some(_) if entry.dirty() => "kept — uncommitted changes (--force to remove)".to_owned(),
        Some(why) => format!("would remove — {why}"),
        // Not selected. Where the worktree holds commits the trunk lacks, say so:
        // that is the reason a sweep leaves it, in the terms that matter.
        None => match (entry.trunk_ahead, trunk) {
            (Some(ahead), Some(trunk)) if ahead > 0 => {
                let plural = if ahead == 1 { "commit" } else { "commits" };
                format!("kept — {ahead} {plural} not on {trunk}")
            }
            _ => "kept — nothing selects it".to_owned(),
        },
    }
}

// --- prune --------------------------------------------------------------------

fn prune(repo: &Repository, args: &PruneArgs) -> Result<()> {
    let inventory = Inventory::gather(repo);

    // A named worktree is dropped whatever its state — the explicit request. It
    // still refuses to discard uncommitted work without `--force`, and it is an
    // error rather than a skip: you asked for this one by name.
    if let Some(target) = &args.target {
        let entry = inventory.resolve(target)?;
        if entry.is_main {
            bail!(
                "{} is the main worktree — that is the repository itself, not a worktree to reclaim",
                entry.path.display()
            );
        }
        if entry.is_current {
            bail!(
                "{} is the worktree you are in; run this from elsewhere to remove it",
                entry.path.display()
            );
        }
        if entry.dirty() && !args.force {
            bail!(
                "worktree {} has uncommitted changes; commit or stash them, \
                 or pass --force to discard them",
                entry.path.display()
            );
        }
        worktree::remove(repo, &entry.path, args.force)?;
        log::info!(
            "{} worktree {}",
            past_or_planned("removed", "would remove"),
            entry.path.display()
        );
        return Ok(());
    }

    let asked = build_filter(&args.select)?;
    let filter = if asked.is_empty() {
        default_sweep()
    } else {
        asked
    };

    let mut removed = 0;
    let mut skipped = 0;
    for entry in inventory.entries() {
        let Some(why) = filter.reason(entry) else {
            continue;
        };
        // In a sweep a dirty worktree is skipped with a note rather than failing
        // the run: one worktree holding work in progress must not stop the other
        // five from being reclaimed, and silence would be worse than either.
        if entry.dirty() && !args.force {
            log::info!(
                "kept {} ({why}, but has uncommitted changes — --force to remove)",
                entry.name()
            );
            skipped += 1;
            continue;
        }
        match worktree::remove(repo, &entry.path, args.force) {
            Ok(()) => {
                log::info!(
                    "{} worktree {} ({why})",
                    past_or_planned("removed", "would remove"),
                    entry.path.display()
                );
                removed += 1;
            }
            Err(e) => log::warn!("could not remove {}: {e:#}", entry.path.display()),
        }
    }

    // Tidy git's own records regardless of what the filter selected: a worktree
    // whose directory someone deleted by hand leaves a stale record that nothing
    // else cleans up, and this is the housekeeping command. Counted from the
    // inventory rather than from git's output, which is silent about what it did.
    let stale = inventory.entries().iter().filter(|e| e.prunable).count();
    worktree::prune_records(repo)?;
    if stale > 0 {
        log::info!(
            "{} {stale} stale worktree record(s)",
            past_or_planned("forgot", "would forget")
        );
    }

    if removed == 0 && skipped == 0 && stale == 0 {
        log::info!("nothing to reclaim");
    } else if removed > 0 {
        log::info!(
            "{} {removed} worktree(s)",
            past_or_planned("reclaimed", "would reclaim")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero components are dropped and no `+`/`-` appears, so a count of commits
    /// can never be misread as a count of changed lines.
    #[test]
    fn distance_omits_zero_components() {
        assert_eq!(distance(0, 0), "up to date");
        assert_eq!(distance(3, 0), "3 ahead");
        assert_eq!(distance(0, 2), "2 behind");
        assert_eq!(distance(3, 2), "3 ahead, 2 behind");
        for rendered in [distance(0, 0), distance(3, 0), distance(3, 2)] {
            assert!(
                !rendered.contains('+') && !rendered.contains('-'),
                "{rendered}"
            );
        }
    }

    #[test]
    fn changes_names_only_the_non_empty_buckets() {
        let counts = |staged, modified, untracked| StatusCounts {
            staged,
            modified,
            untracked,
        };
        assert_eq!(changes_phrase(&counts(0, 0, 0)), "clean");
        assert_eq!(changes_phrase(&counts(0, 2, 1)), "2 modified, 1 untracked");
        assert_eq!(changes_phrase(&counts(1, 0, 0)), "1 staged");
    }
}
