//! `wits review show` / `draft` — the read path and the stable `--json` contract.
//!
//! `show` with no MR is the inbox; with an MR it is the detail view, which folds
//! the pending draft (`local.json`) into the remote discussion (`comments.json`)
//! so the editor sees one merged picture. Filtering is the knob for large MRs —
//! the payload is always the whole MR, never paginated.

use anyhow::{Context, Result};
use serde::Serialize;

use wits_util::forge::{Anchor, MrSummary, Side};
use wits_util::git::Repository;
use wits_util::time::age_since;

use super::model::{
    short, state_word, Action, Comment, Info, Local, Snapshot, StoredCommit, StoredFile, Thread,
    SCHEMA,
};
use super::{local, ShowArgs};

/// The current review point, as the read contract exposes it: the two endpoints
/// to render a diff between. See [`Snapshot`] for why the forge's own "base" is
/// not among them.
#[derive(Serialize)]
struct SnapshotView {
    fork_sha: String,
    head_sha: String,
}

/// Counts over *all* the MR's threads, before the display filters narrow the
/// `threads` list — so a filtered view still reports the true shape of the
/// discussion.
#[derive(Serialize, Default)]
struct ThreadStats {
    total: usize,
    unresolved: usize,
    resolved: usize,
    outdated: usize,
    /// Someone else's comment is last — likely waiting on you.
    unread: usize,
    /// Present only in your pending draft.
    pending: usize,
}

impl ThreadStats {
    fn of(threads: &[Thread]) -> ThreadStats {
        let count = |f: fn(&Thread) -> bool| threads.iter().filter(|t| f(t)).count();
        ThreadStats {
            total: threads.len(),
            unresolved: count(|t| !t.resolved),
            resolved: count(|t| t.resolved),
            outdated: count(|t| t.outdated),
            unread: count(|t| t.comments.last().is_some_and(|c| c.origin == "remote")),
            pending: count(|t| t.origin == "local"),
        }
    }
}

#[derive(Serialize)]
struct Neighbors {
    position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    prev_mr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_mr: Option<String>,
    nodes: Vec<String>,
}

#[derive(Serialize)]
struct DraftView {
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<wits_util::forge::Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    pending: usize,
}

#[derive(Serialize)]
struct DetailView {
    schema: u32,
    mr: MrSummary,
    snapshot: SnapshotView,
    /// The full snapshot history, oldest first, so the editor can offer
    /// switching (`diff --range <sha>`) or comparing (`diff --against <sha>`).
    snapshots: Vec<Snapshot>,
    /// Unix seconds of the last `fetch` that synced this MR.
    fetched_at: i64,
    neighbors: Neighbors,
    commits: Vec<StoredCommit>,
    files: Vec<StoredFile>,
    thread_stats: ThreadStats,
    threads: Vec<Thread>,
    draft: DraftView,
}

#[derive(Serialize)]
struct InboxReview {
    pending: usize,
    /// How many review points have been fetched — `0` for a feed-only row that
    /// has never had a full `fetch <mr>`.
    snapshots: usize,
    fetched_at: i64,
}

#[derive(Serialize)]
struct InboxItem {
    #[serde(flatten)]
    mr: MrSummary,
    review: InboxReview,
}

#[derive(Serialize)]
struct InboxView {
    schema: u32,
    items: Vec<InboxItem>,
}

pub fn run(repo: &Repository, args: &ShowArgs) -> Result<()> {
    let ctx = local(repo)?;
    match &args.mr {
        Some(handle) => show_detail(&ctx, &super::parse_mr_handle(handle)?, args),
        None => show_inbox(&ctx, args),
    }
}

fn show_detail(ctx: &super::ReviewCtx, id: &str, args: &ShowArgs) -> Result<()> {
    let view = build_detail_view(ctx, id, args)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    print_headline(&view);
    if args.details {
        print_detail_metadata(&view);
    } else {
        print_detail_summary(&view);
    }
    print_detail_body(&view);
    Ok(())
}

/// Assemble the merged detail view for one MR: load its three store files, fold
/// the draft into the remote threads, recompute outdate flags locally, apply the
/// display filters, and stitch in stack neighbours. Pure data — no I/O to
/// stdout — so it is the one place the read model is built (and the natural test
/// seam), leaving `show_detail` to only choose JSON vs. human rendering.
fn build_detail_view(ctx: &super::ReviewCtx, id: &str, args: &ShowArgs) -> Result<DetailView> {
    let info = ctx.store.load_info(id).with_context(|| {
        format!("MR {id} isn't in the store yet — run `wits review fetch {id}` first")
    })?;
    let comments = ctx.store.load_comments(id);
    let mut draft = ctx.store.load_local(id)?;
    draft.normalize(info.head().unwrap_or_default());

    let mut threads = merge_threads(comments.threads, &draft, info.head());
    recompute_outdated(&ctx.repo, &mut threads, info.head());
    // Counted before filtering: a `--file`-narrowed view should still be able to
    // say how many threads the MR has in total.
    let thread_stats = ThreadStats::of(&threads);
    apply_filters(&mut threads, args);

    let current = info.current();
    Ok(DetailView {
        schema: SCHEMA,
        snapshot: SnapshotView {
            fork_sha: current.map(|s| s.fork().to_owned()).unwrap_or_default(),
            head_sha: current.map(|s| s.head_sha.clone()).unwrap_or_default(),
        },
        snapshots: info.snapshots.clone(),
        fetched_at: info.fetched_at,
        neighbors: neighbors(&ctx.store.list_infos(), id),
        commits: info.commits.clone(),
        files: info.files.clone(),
        thread_stats,
        threads,
        draft: DraftView {
            verdict: draft.verdict,
            summary: draft.summary().map(str::to_owned),
            pending: pending_count(&draft),
        },
        mr: info.mr,
    })
}

/// The stack neighbours of `id`: its position in the reconstructed chain plus
/// the ids immediately below and above it.
fn neighbors(infos: &[Info], id: &str) -> Neighbors {
    let (nodes, position) = super::stack_chain(infos, id);
    Neighbors {
        position,
        prev_mr: position.checked_sub(1).and_then(|i| nodes.get(i).cloned()),
        next_mr: nodes.get(position + 1).cloned(),
        nodes,
    }
}

/// Fold the draft into the remote threads: new comments become local threads,
/// replies attach to their remote thread as pending comments, resolutions flip
/// the flag. Pending items use their action id, so clients can address them
/// directly with a later replacement or `drop`.
fn merge_threads(mut threads: Vec<Thread>, draft: &Local, head: Option<&str>) -> Vec<Thread> {
    for action in &draft.actions {
        let action_id = action
            .id()
            .expect("normalized local actions have ids")
            .to_owned();
        match action {
            Action::Comment { body, .. } => {
                let (anchor, commit) = action.read_anchor(head);
                threads.push(Thread {
                    id: action_id.clone(),
                    origin: "local".into(),
                    resolved: false,
                    outdated: false,
                    anchor,
                    commit,
                    comments: vec![pending_comment(&action_id, body)],
                });
            }
            Action::Reply { thread, body, .. } => {
                let target = thread.remote_ref();
                if let Some(t) = threads.iter_mut().find(|t| t.id == target) {
                    t.comments.push(pending_comment(&action_id, body));
                } else {
                    // Surface rather than silently drop: the target thread isn't
                    // in the local cache (never fetched, or already gone), so the
                    // reply can't attach. Show it so the editor/user notices.
                    threads.push(orphan_thread(
                        &action_id,
                        &format!(
                            "reply to unknown thread {thread} — run `wits review fetch` (was: {})",
                            first_line(body)
                        ),
                    ));
                }
            }
            Action::Resolve {
                thread, resolved, ..
            } => {
                let target = thread.remote_ref();
                if let Some(t) = threads.iter_mut().find(|t| t.id == target) {
                    t.resolved = *resolved;
                } else {
                    let verb = if *resolved { "resolve" } else { "unresolve" };
                    threads.push(orphan_thread(
                        &action_id,
                        &format!(
                            "pending {verb} of unknown thread {thread} — run `wits review fetch`"
                        ),
                    ));
                }
            }
            Action::Summary { .. } | Action::Drop { .. } => {}
        }
    }
    threads
}

/// A local thread carrying a note about a draft action that couldn't attach to a
/// remote thread (an orphaned reply/resolve), so it is surfaced in the view
/// rather than silently dropped.
fn orphan_thread(id: &str, note: &str) -> Thread {
    Thread {
        id: id.to_owned(),
        origin: "local".into(),
        resolved: false,
        outdated: false,
        anchor: None,
        commit: None,
        comments: vec![pending_comment(id, note)],
    }
}

fn pending_comment(id: &str, body: &str) -> Comment {
    Comment {
        id: id.to_owned(),
        author: "@me".into(),
        origin: "local".into(),
        body: body.to_owned(),
        created_at: String::new(),
        state: "pending".into(),
    }
}

/// Recompute each line thread's `outdated` locally (design.md §6): a thread is
/// outdated when the line(s) it is anchored to fall inside a region the file
/// changed between the commit the comment was written on and the current head.
/// Uniform across forges and offline, from the objects `fetch` already pins.
///
/// The anchor's line number is a line in *its own* commit — which is the **old**
/// side of the `commit..head` diff — so a `New`-side anchor intersects the diff's
/// old-side hunk ranges (a multi-line span uses its whole `[start, end]`). An
/// `Old`-side (deleted-line) anchor names a line in a base we don't diff here, so
/// it can't be computed cleanly and keeps whatever the forge reported. The forge
/// flag is also the fallback when the anchor commit's objects aren't local.
/// File/MR-level threads are never outdated.
fn recompute_outdated(repo: &Repository, threads: &mut [Thread], head: Option<&str>) {
    use std::collections::HashMap;
    let Some(head) = head else { return };
    let mut cache: HashMap<(String, String), Vec<(u32, u32)>> = HashMap::new();
    for t in threads.iter_mut() {
        let (
            Some(Anchor::Line {
                path, end, start, ..
            }),
            Some(commit),
        ) = (&t.anchor, t.commit.clone())
        else {
            continue;
        };
        if commit == head {
            t.outdated = false;
            continue;
        }
        // A deleted-line anchor can't be mapped onto commit..head — keep the flag.
        if end.side != Side::New {
            continue;
        }
        // Objects for the anchor commit aren't local → fall back to the forge flag.
        if repo.rev_parse(&commit).is_none() {
            continue;
        }
        // The new-side span this comment covers, in `commit`'s line numbers.
        let (lo, hi) = match start.filter(|s| s.side == Side::New) {
            Some(s) => (s.line.min(end.line), s.line.max(end.line)),
            None => (end.line, end.line),
        };
        let ranges = cache
            .entry((commit.clone(), path.clone()))
            .or_insert_with(|| changed_old_ranges(repo, &commit, head, path));
        // Overlap of [lo, hi] (inclusive) with a changed hunk [start, start+count).
        t.outdated = ranges
            .iter()
            .any(|&(start, count)| count > 0 && hi >= start && lo < start + count);
    }
}

/// The old-side line ranges a file changed across `from..to`, from the unified
/// diff hunk headers (`@@ -start,count +… @@`).
fn changed_old_ranges(repo: &Repository, from: &str, to: &str, path: &str) -> Vec<(u32, u32)> {
    repo.diff_patch(from, to, Some(path))
        .map(|patch| patch.lines().filter_map(parse_hunk_old_range).collect())
        .unwrap_or_default()
}

/// Parse a unified-diff hunk header `@@ -a,b +c,d @@` into its old-side range
/// `(a, b)`; `b` defaults to 1 when omitted (a single-line hunk).
fn parse_hunk_old_range(line: &str) -> Option<(u32, u32)> {
    let old = line.strip_prefix("@@ -")?.split(' ').next()?;
    let mut parts = old.split(',');
    let start: u32 = parts.next()?.parse().ok()?;
    let count: u32 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
    Some((start, count))
}

fn apply_filters(threads: &mut Vec<Thread>, args: &ShowArgs) {
    if args.outdated {
        threads.retain(|t| t.outdated);
    }
    if args.resolved {
        threads.retain(|t| t.resolved);
    }
    if args.unresolved {
        threads.retain(|t| !t.resolved);
    }
    if args.unread {
        threads.retain(|t| t.comments.last().is_some_and(|c| c.origin == "remote"));
    }
    if let Some(path) = &args.file {
        threads.retain(|t| anchor_path(t.anchor.as_ref()) == Some(path.as_str()));
    }
}

fn anchor_path(a: Option<&Anchor>) -> Option<&str> {
    a.map(Anchor::path)
}

fn pending_count(draft: &Local) -> usize {
    draft.actions.len() + usize::from(draft.verdict.is_some())
}

fn show_inbox(ctx: &super::ReviewCtx, args: &ShowArgs) -> Result<()> {
    let mut infos = ctx.store.list_infos();
    infos.sort_by(|a, b| b.mr.updated_at.cmp(&a.mr.updated_at));

    let items: Vec<InboxItem> = infos
        .into_iter()
        .map(|info| {
            // One MR's corrupt draft shouldn't sink the rest of the inbox —
            // degrade to "no pending actions" with a per-MR warning.
            let pending = match ctx.store.load_local(&info.mr.id) {
                Ok(mut draft) => {
                    draft.normalize(info.head().unwrap_or_default());
                    pending_count(&draft)
                }
                Err(e) => {
                    log::warn!("MR {}: skipping draft in inbox: {e}", info.mr.id);
                    0
                }
            };
            InboxItem {
                review: InboxReview {
                    pending,
                    snapshots: info.snapshots.len(),
                    fetched_at: info.fetched_at,
                },
                mr: info.mr,
            }
        })
        .collect();

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&InboxView {
                schema: SCHEMA,
                items
            })?
        );
    } else if items.is_empty() {
        println!(
            "(nothing fetched — `wits review fetch <mr>` or `wits review fetch --feed <name>`)"
        );
    } else {
        for item in &items {
            let pending = if item.review.pending > 0 {
                format!("  pending:{}", item.review.pending)
            } else {
                String::new()
            };
            println!(
                "{:<7} [{}] {}  ({}){pending}",
                item.mr.display,
                state_word(item.mr.state, item.mr.draft),
                item.mr.title,
                item.mr.author
            );
            if args.details {
                println!("        {}", inbox_details_line(item));
            }
        }
    }
    Ok(())
}

/// The second line `show --details` gives each inbox row: the metadata that
/// decides whether an MR is worth opening — where it sits, how it is labelled,
/// and whether what we hold is current.
fn inbox_details_line(item: &InboxItem) -> String {
    let mut parts = vec![format!("{} -> {}", item.mr.source, item.mr.base)];
    if !item.mr.labels.is_empty() {
        parts.push(item.mr.labels.join(","));
    }
    parts.push(format!("updated {}", iso_day(&item.mr.updated_at)));
    parts.push(match item.review.fetched_at {
        0 => "not fetched".to_owned(),
        at => format!(
            "fetched {} · {} snapshot(s)",
            age_since(at),
            item.review.snapshots
        ),
    });
    parts.join(" · ")
}

/// The date part of an ISO-8601 timestamp — enough to judge staleness at a
/// glance, where the wall-clock time would just be column noise.
fn iso_day(timestamp: &str) -> &str {
    timestamp.split('T').next().unwrap_or(timestamp)
}

/// The `--details` block: everything the store knows *about* the MR, laid out
/// for a person. Deliberately metadata only — no diff, no comment bodies. Those
/// are what `diff` and the thread list below are for; this answers "what am I
/// actually looking at, and how did it get here".
fn print_detail_metadata(view: &DetailView) {
    let field = |name: &str, value: &str| println!("  {name:<10} {value}");
    field("author", &view.mr.author);
    field(
        "branches",
        &format!("{} -> {}", view.mr.source, view.mr.base),
    );
    if !view.mr.labels.is_empty() {
        field("labels", &view.mr.labels.join(", "));
    }
    field("url", &view.mr.web_url);
    field("updated", &view.mr.updated_at);
    field(
        "fetched",
        &match view.fetched_at {
            0 => "never (feed entry only — `wits review fetch` for full detail)".to_owned(),
            at => age_since(at),
        },
    );
    if view.neighbors.nodes.len() > 1 {
        let chain: Vec<String> = view
            .neighbors
            .nodes
            .iter()
            .enumerate()
            .map(|(i, id)| {
                if i == view.neighbors.position {
                    format!("*{id}*")
                } else {
                    id.clone()
                }
            })
            .collect();
        field(
            "stack",
            &format!(
                "{}   ({} of {})",
                chain.join(" -> "),
                view.neighbors.position + 1,
                view.neighbors.nodes.len()
            ),
        );
    }

    println!();
    if view.snapshots.is_empty() {
        field(
            "snapshot",
            "none fetched — `wits review fetch` for the diff",
        );
    } else {
        field(
            "snapshot",
            &format!("the current of {} fetched", view.snapshots.len()),
        );
        field("  fork", short(&view.snapshot.fork_sha));
        field("  head", short(&view.snapshot.head_sha));
        field(
            "  change",
            &format!(
                "{} commit(s), {} file(s)",
                view.commits.len(),
                view.files.len()
            ),
        );
    }

    // These head SHAs are how `diff --range`/`--against` name a review point —
    // there is no shorthand for "the previous one" — so print them in a form
    // that is copied straight onto a command line, and mark the rebases, the
    // points where a plain head-to-head diff would mislead.
    if view.snapshots.len() > 1 {
        println!();
        println!("  history");
        let mut prev_fork: Option<&str> = None;
        for (i, s) in view.snapshots.iter().enumerate() {
            let rebased = prev_fork.is_some_and(|f| f != s.fork());
            prev_fork = Some(s.fork());
            let marks = [
                (rebased, "rebased"),
                (i + 1 == view.snapshots.len(), "current"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| *s)
            .collect::<Vec<_>>()
            .join(", ");
            let marks = if marks.is_empty() {
                String::new()
            } else {
                format!("   ({marks})")
            };
            println!(
                "    {:<3} {}  fork {}{marks}",
                i + 1,
                short(&s.head_sha),
                short(s.fork())
            );
        }
    }

    println!();
    let t = &view.thread_stats;
    field(
        "threads",
        &format!(
            "{} total · {} unresolved · {} outdated · {} awaiting you · {} pending",
            t.total, t.unresolved, t.outdated, t.unread, t.pending
        ),
    );
    println!();
}

/// The one-line identity every detail view opens with, whichever header
/// follows it.
fn print_headline(view: &DetailView) {
    println!(
        "{} [{}] {}",
        view.mr.display,
        state_word(view.mr.state, view.mr.draft),
        view.mr.title
    );
}

/// The default header: the three facts you need to orient, on three lines.
/// `--details` replaces this with [`print_detail_metadata`].
fn print_detail_summary(view: &DetailView) {
    println!(
        "  by {} · base {} · {}",
        view.mr.author, view.mr.base, view.mr.web_url
    );
    println!(
        "  snapshot {}..{}",
        short(&view.snapshot.fork_sha),
        short(&view.snapshot.head_sha)
    );
    if view.neighbors.nodes.len() > 1 {
        println!("  stack: {}", view.neighbors.nodes.join(" -> "));
    }
}

/// The discussion itself — files, threads, and what the draft holds. Shared by
/// both headers, since this is the part you actually read.
fn print_detail_body(view: &DetailView) {
    if !view.files.is_empty() {
        println!("  files:");
        for f in &view.files {
            println!("    {} {}", f.status, f.path);
        }
    }
    if view.threads.is_empty() {
        println!("  (no threads)");
    } else {
        println!("  threads:");
        for t in &view.threads {
            let flags = [
                (t.resolved, "resolved"),
                (t.outdated, "outdated"),
                (t.origin == "local", "pending"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| *s)
            .collect::<Vec<_>>()
            .join(",");
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{flags}]")
            };
            println!("    {} {}{flags}", t.id, describe_anchor(t.anchor.as_ref()));
            for c in &t.comments {
                println!("      {} ({}): {}", c.author, c.origin, first_line(&c.body));
            }
        }
    }
    if view.draft.pending > 0 {
        let verdict = view
            .draft
            .verdict
            .map(|v| format!(" verdict={}", v.display_str()))
            .unwrap_or_default();
        println!("  draft: {} pending action(s){verdict}", view.draft.pending);
    }
}

fn describe_anchor(a: Option<&Anchor>) -> String {
    match a {
        Some(Anchor::Line {
            path, end, start, ..
        }) => {
            let span = match start {
                Some(s) => format!("{}-{}", s.line, end.line),
                None => end.line.to_string(),
            };
            format!("{path}:{span} ({})", end.side.as_str())
        }
        Some(Anchor::File { path }) => format!("{path} (file)"),
        None => "(conversation)".to_owned(),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Read a JSON batch (`{verdict?, actions:[…]}`) from a file or stdin and append
/// its actions to the draft, setting the verdict when the batch provides one.
/// The tool owns the write; the editor only provides the content. Surgery on a
/// queued action is represented by appending another action with the same id.
fn ingest(ctx: &super::ReviewCtx, id: &str, input: &std::path::Path, dedup: bool) -> Result<()> {
    use std::io::Read;
    let text = if input.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading the draft batch from stdin")?;
        buf
    } else {
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?
    };
    let batch: Local = serde_json::from_str(&text).context("parsing the draft batch as JSON")?;
    if batch.schema != SCHEMA {
        anyhow::bail!(
            "draft batch schema {} is unsupported (expected {}). The `local.json` \
             contract has likely changed — regenerate the batch with the current shape.",
            batch.schema,
            SCHEMA
        );
    }

    let mut draft = ctx.store.load_local(id)?;
    let added = batch.actions.len();

    // Stamp each incoming comment's `commit` with the current snapshot head, so
    // the comment is anchored to the snapshot it was written against. Actions
    // that already carry a `commit` (explicit hand-edit) are left as-is.
    let head = ctx
        .store
        .load_info(id)
        .and_then(|i| i.head().map(str::to_owned));
    let mut actions = batch.actions;
    if let Some(head) = &head {
        for action in &mut actions {
            if let Action::Comment { ref mut commit, .. } = action {
                if commit.is_none() {
                    *commit = Some(head.clone());
                }
            }
        }
    }
    draft.actions.extend(actions);
    if batch.verdict.is_some() {
        draft.verdict = batch.verdict;
    }
    if dedup {
        draft.normalize(head.as_deref().unwrap_or_default());
    } else {
        draft.ensure_action_ids();
    }
    ctx.store.save_local(id, &draft)?;
    log::info!("appended {added} action(s) to MR {id}'s draft");
    Ok(())
}

pub fn run_draft(repo: &Repository, args: &super::DraftArgs) -> Result<()> {
    let ctx = local(repo)?;
    let id = super::parse_mr_handle(&args.mr)?;

    // With an input (file or `-`), ingest a batch into the draft (the tool owns
    // the write); otherwise show the current draft.
    if let Some(input) = &args.input {
        return ingest(&ctx, &id, input, args.dedup);
    }

    let mut draft = ctx.store.load_local(&id)?;
    if args.dedup {
        let head = ctx
            .store
            .load_info(&id)
            .and_then(|i| i.head().map(str::to_owned));
        draft.normalize(head.as_deref().unwrap_or_default());
        ctx.store.save_local(&id, &draft)?;
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&draft)?);
        return Ok(());
    }
    if draft.is_empty() {
        println!("(no pending draft for MR {id})");
        return Ok(());
    }
    if let Some(v) = draft.verdict {
        println!("verdict: {}", v.display_str());
    }
    for action in &draft.actions {
        let id = action.id().expect("stored local actions have ids");
        match action {
            Action::Comment { body, commit, .. } => {
                let where_ = describe_anchor(action.read_anchor(None).0.as_ref());
                let at = commit
                    .as_deref()
                    .map(|s| format!(" @{}", short(s)))
                    .unwrap_or_default();
                println!("  {id}  comment {where_}{at}  {}", first_line(body));
            }
            Action::Reply { thread, body, .. } => {
                println!(
                    "  {id}  reply -> {}  {}",
                    thread.remote_ref(),
                    first_line(body)
                )
            }
            Action::Summary { body, .. } => {
                println!("  {id}  summary  {}", first_line(body));
            }
            Action::Resolve {
                thread, resolved, ..
            } => {
                let verb = if *resolved { "resolve" } else { "unresolve" };
                println!("  {id}  {verb} {}", thread.remote_ref())
            }
            Action::Drop { id } => println!("  drop {id}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk_old_ranges() {
        assert_eq!(
            parse_hunk_old_range("@@ -10,5 +12,6 @@ fn foo()"),
            Some((10, 5))
        );
        assert_eq!(parse_hunk_old_range("@@ -7 +7 @@"), Some((7, 1)));
        assert_eq!(parse_hunk_old_range("not a hunk"), None);
        assert_eq!(parse_hunk_old_range("+ added line"), None);
    }
}
