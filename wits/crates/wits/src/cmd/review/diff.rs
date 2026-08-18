//! `wits review diff` — the diff's *coordinates*, not its rendering.
//!
//! The tool does not render diffs (the editor and `git` do that well); it owns
//! the coordinate layer — the endpoints, the commit list, and the changed files a
//! comment may anchor in. `--patch` is a terminal/debug convenience that shells
//! out; `--json` is the coordinate payload an editor consumes.
//!
//! **The unit of input is a range, not a commit.** One range answers *what does
//! this change?*; two answer *what changed between these two versions of it?* —
//! the question a force-push raises in a stack-based workflow, where a rebase
//! would otherwise drown the real edit in base noise. Answering the second
//! requires knowing what each side does *relative to its own base*, which only a
//! range carries; hence `--range` and `--against`, one grammar between them.

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde::Serialize;

use wits_util::git::Repository;

use super::model::{range_artifacts, short, Info, Snapshot, StoredCommit, StoredFile, SCHEMA};
use super::{local, modulo, DiffArgs, PatchMode};

// ---------------------------------------------------------------------------
// A series: one end of a comparison.
// ---------------------------------------------------------------------------

/// One version of a patch series — the range it occupies.
///
/// This is the currency of the whole command, and it is a *range* rather than a
/// commit for a reason the interface can't paper over: excluding the base from a
/// comparison requires knowing what each side does **relative to its own base**,
/// and only a range carries that. A bare commit forces the tool to guess a base,
/// which is the hidden assumption this type exists to remove.
///
/// [`fork`](Self::fork) is always an ancestor of [`head`](Self::head), so
/// `fork..head` is a real patch series and never a two-endpoint tree compare
/// (which is what produces those baffling inverted hunks).
#[derive(Debug, Clone)]
struct Series {
    /// Exclusive start — where this version of the series begins.
    fork: String,
    /// Inclusive end — its tip.
    head: String,
    /// Whether this end is a fetched review point rather than a hand-written
    /// range — the fork of a review point was pinned at fetch, a range's is
    /// derived on the spot.
    review_point: bool,
}

impl Series {
    fn range(&self) -> String {
        format!("{}..{}", self.fork, self.head)
    }

    fn from_snapshot(s: &Snapshot) -> Series {
        Series {
            fork: s.fork().to_owned(),
            head: s.head_sha.clone(),
            review_point: true,
        }
    }
}

/// Resolve a range spec. Exactly two forms, deliberately — no keywords, no
/// sugar, nothing that reinterprets something which looks like a git revision:
///
/// - a fetched review point's head SHA (a prefix is fine), which expands to that
///   review point's `fork..head` using the fork **pinned at fetch**. This is the
///   one shorthand, and it is only a shorthand: `<base>..<head>` resolves to the
///   same place.
/// - `A..B`, git's own spelling, resolved to `merge-base(A,B)..B`.
///
/// Deriving the merge base unconditionally is free rather than surprising. When
/// `A` is an ancestor of `B` — the overwhelmingly common case — `merge-base(A,B)`
/// *is* `A`, so the result is `A..B` unchanged. The two only differ when `A` has
/// diverged, and there a two-endpoint `git diff A..B` is precisely the misleading
/// answer (it replays `A`'s side as inverted hunks). `git log` agrees either way:
/// `A..B` excludes `A`'s divergent commits, which aren't reachable from `B`
/// anyway, so it lists the same commits as `fork..B`. Commits and diff therefore
/// describe the same series, which is what makes a separate `A...B` spelling
/// unnecessary.
fn resolve_spec(repo: &Repository, info: &Info, spec: &str) -> Result<Series> {
    // A review point brings its own fork, recorded when its objects were pinned.
    // Checked before the range forms, so naming one uses the pinned fork.
    if let Some(snapshot) = info.snapshot_matching(spec) {
        // A fork must be an ancestor of its own head; that is what makes
        // `fork..head` a series. A record failing that was written before
        // `fetch` insisted on resolving the fork, and using it drags the base's
        // own commits into everything built on it — on a real MR, 22 files
        // reported against 4 actually touched. Nothing here can repair it (the
        // forge's base is deliberately not kept), so say so loudly.
        if !repo.is_ancestor(&snapshot.fork_sha, &snapshot.head_sha) {
            log::warn!(
                "review point {} records a fork ({}) that is not an ancestor of it — the \
                 comparison will be too wide. Re-run `wits review fetch` to rebuild it.",
                short(&snapshot.head_sha),
                short(&snapshot.fork_sha)
            );
        }
        return Ok(Series::from_snapshot(snapshot));
    }

    let (left, right) = spec.split_once("..").with_context(|| {
        format!(
            "'{spec}' is not a range and not a fetched review point's head — write a \
             range like `A..B`, or a head SHA from `wits review show <mr> --details`"
        )
    })?;
    // Splitting on the first `..` leaves a three-dot form as `("A", ".B")`, so
    // the stray dot lands on the right. Name it, rather than letting git fail on
    // a nonsense revision.
    if let Some(tip) = right.strip_prefix('.') {
        anyhow::bail!(
            "'{spec}' is git's three-dot form; write `{left}..{tip}` — the merge base is \
             always computed here, so the two mean the same thing"
        );
    }
    // Git would read an omitted side as HEAD. In a review that is almost never
    // what someone means, so it is an error rather than a silent guess.
    if left.is_empty() || right.is_empty() {
        anyhow::bail!("'{spec}' leaves one side of the range empty — name both ends");
    }

    let head = repo
        .rev_parse(right)
        .with_context(|| format!("'{right}' is not a revision this repository knows"))?;
    let left_sha = repo
        .rev_parse(left)
        .with_context(|| format!("'{left}' is not a revision this repository knows"))?;
    let fork = repo.merge_base(&left_sha, &head).with_context(|| {
        format!("'{left}' and '{right}' share no history, so the range has no starting point")
    })?;
    Ok(Series {
        fork,
        head,
        review_point: false,
    })
}

// ---------------------------------------------------------------------------
// Output payloads.
// ---------------------------------------------------------------------------

/// One end of a comparison, as the read contract exposes it. Shared by both
/// payload shapes so an editor parses an end the same way whether it came from
/// `--range` or `--against`.
#[derive(Serialize)]
struct EndView {
    fork_sha: String,
    head_sha: String,
    range: String,
    /// True when this end is a fetched review point, whose fork was pinned at
    /// fetch; false for a hand-written range, whose fork was derived just now.
    review_point: bool,
}

impl From<&Series> for EndView {
    fn from(s: &Series) -> Self {
        EndView {
            fork_sha: s.fork.clone(),
            head_sha: s.head.clone(),
            range: s.range(),
            review_point: s.review_point,
        }
    }
}

/// One range described — what this version of the series changes.
#[derive(Serialize)]
struct RangeView {
    schema: u32,
    mr: String,
    mode: &'static str,
    to: EndView,
    commits: Vec<StoredCommit>,
    files: Vec<StoredFile>,
}

/// How a commit in the older series corresponds to one in the newer, as
/// `git range-diff` pairs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Pairing {
    /// Same patch on both sides — carried through a rebase untouched.
    Unchanged,
    /// Paired, but the patch differs: the real edit, or a conflict resolution.
    Reworked,
    /// Only in the newer series.
    Added,
    /// Only in the older series.
    Dropped,
}

impl Pairing {
    /// The `git range-diff` marker column, which is its whole vocabulary.
    fn from_marker(c: char) -> Option<Pairing> {
        match c {
            '=' => Some(Pairing::Unchanged),
            '!' => Some(Pairing::Reworked),
            '>' => Some(Pairing::Added),
            '<' => Some(Pairing::Dropped),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Pairing::Unchanged => "unchanged",
            Pairing::Reworked => "reworked",
            Pairing::Added => "added",
            Pairing::Dropped => "dropped",
        }
    }
}

#[derive(Serialize)]
struct CommitPair {
    pairing: Pairing,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_sha: Option<String>,
    subject: String,
}

/// Two ranges compared — the `--against` view.
#[derive(Serialize)]
struct InterdiffView {
    schema: u32,
    mr: String,
    mode: &'static str,
    from: EndView,
    to: EndView,
    /// Whether the fork point moved between the two, i.e. there was a rebase.
    /// This is exactly when a plain `head..head` diff becomes misleading.
    rebased: bool,
    commits: Vec<CommitPair>,
    files: Vec<StoredFile>,
}

// ---------------------------------------------------------------------------
// Dispatch: the number of ranges decides what question is being asked.
// ---------------------------------------------------------------------------

pub fn run(repo: &Repository, args: &DiffArgs) -> Result<()> {
    let ctx = local(repo)?;
    let id = super::parse_mr_handle(&args.mr)?;
    let info = ctx.store.load_info(&id).with_context(|| {
        format!("MR {id} isn't in the store yet — run `wits review fetch {id}` first")
    })?;

    // An omitted `--range` is the only thing that needs the store's own view of
    // where the MR stands; two explicit ranges never touch the review history.
    let to = match &args.range {
        Some(spec) => resolve_spec(&ctx.repo, &info, spec)
            .with_context(|| format!("MR {id}: cannot resolve --range '{spec}'"))?,
        None => info
            .current()
            .map(Series::from_snapshot)
            .with_context(|| format!("MR {id} has no fetched review point — run `wits review fetch {id}` first, or name a range with --range"))?,
    };

    match &args.against {
        Some(spec) => {
            let from = resolve_spec(&ctx.repo, &info, spec)
                .with_context(|| format!("MR {id}: cannot resolve --against '{spec}'"))?;
            if from.head == to.head && from.fork == to.fork {
                anyhow::bail!("--against names the same range as --range");
            }
            compare_two(&ctx.repo, &id, &from, &to, args)
        }
        None => describe_one(&ctx.repo, &id, &to, args),
    }
}

// ---------------------------------------------------------------------------
// One range.
// ---------------------------------------------------------------------------

fn describe_one(repo: &Repository, id: &str, series: &Series, args: &DiffArgs) -> Result<()> {
    match args.patch {
        Some(PatchMode::ThreeWay) => anyhow::bail!(
            "--patch=3way shows two versions against their base, so it needs a second \
             range — add `--against <SPEC>`"
        ),
        Some(PatchMode::TwoWay) => {
            print!("{}", patch_text(repo, series)?);
            return Ok(());
        }
        None => {}
    }

    let (commits, files) = range_artifacts(repo, &series.fork, &series.head);
    let view = RangeView {
        schema: SCHEMA,
        mr: id.to_owned(),
        mode: "range",
        to: EndView::from(series),
        commits,
        files,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    println!("{} {}", view.mr, view.to.range);
    for c in &view.commits {
        println!("  {} {}", short(&c.sha), c.subject);
    }
    for f in &view.files {
        println!("  {} {}", f.status, f.path);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Two ranges.
// ---------------------------------------------------------------------------

fn compare_two(
    repo: &Repository,
    id: &str,
    from: &Series,
    to: &Series,
    args: &DiffArgs,
) -> Result<()> {
    if let Some(mode) = args.patch {
        print!("{}", render_comparison(repo, from, to, mode)?);
        return Ok(());
    }

    let view = InterdiffView {
        schema: SCHEMA,
        mr: id.to_owned(),
        mode: "interdiff",
        rebased: from.fork != to.fork,
        commits: commit_pairs(repo, from, to),
        files: differing_files(repo, from, to)?,
        from: EndView::from(from),
        to: EndView::from(to),
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    println!(
        "{} {}..{}",
        view.mr,
        short(&view.from.head_sha),
        short(&view.to.head_sha)
    );
    let rebased = if view.rebased { "  (rebased)" } else { "" };
    println!(
        "  from  {}  fork {}",
        short(&view.from.head_sha),
        short(&view.from.fork_sha)
    );
    println!(
        "  to    {}  fork {}{rebased}",
        short(&view.to.head_sha),
        short(&view.to.fork_sha)
    );
    if view.commits.is_empty() {
        println!("  (no commits)");
    } else {
        println!("  commits:");
        for p in &view.commits {
            let old = p.old_sha.as_deref().map(short).unwrap_or("-");
            let new = p.new_sha.as_deref().map(short).unwrap_or("-");
            println!(
                "    {:<10} {old:<11} {new:<11} {}",
                p.pairing.word(),
                p.subject
            );
        }
    }
    for f in &view.files {
        println!("  {} {}", f.status, f.path);
    }
    Ok(())
}

/// Pair the two series' commits by asking `git range-diff`, which does the
/// patch-id matching that makes a rebase legible. Its `--no-patch` table is
/// already the coordinate answer — `1: abc = 1: def subject` — so we parse that
/// rather than reimplement the pairing.
fn commit_pairs(repo: &Repository, from: &Series, to: &Series) -> Vec<CommitPair> {
    repo.range_diff(&from.range(), &to.range(), false)
        .map(|out| out.lines().filter_map(parse_pair_line).collect())
        .unwrap_or_default()
}

/// One `git range-diff --no-patch` row: `<n|->: <sha|---> <marker> <n|->: <sha|---> <subject>`.
/// A placeholder (`-:` / `-------`) on either side means the commit exists only
/// on the other.
fn parse_pair_line(line: &str) -> Option<CommitPair> {
    let mut fields = line.split_whitespace();
    let old_index = fields.next()?;
    let old_sha = fields.next()?;
    let pairing = Pairing::from_marker(one_char(fields.next()?)?)?;
    let new_index = fields.next()?;
    let new_sha = fields.next()?;
    if !old_index.ends_with(':') || !new_index.ends_with(':') {
        return None;
    }
    let subject: Vec<&str> = fields.collect();
    Some(CommitPair {
        pairing,
        old_sha: real_sha(old_sha),
        new_sha: real_sha(new_sha),
        subject: subject.join(" "),
    })
}

fn one_char(field: &str) -> Option<char> {
    let mut chars = field.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// A range-diff cell holds either a SHA or a `-----` placeholder for "absent".
fn real_sha(field: &str) -> Option<String> {
    (!field.starts_with('-')).then(|| field.to_owned())
}

/// The files that actually differ between the two series, base movement excluded.
///
/// Derived from the very same reduction the patch uses, so the coordinate view
/// and `--patch` can never disagree about what changed — the file list is just
/// the reduced diff's file headers.
fn differing_files(repo: &Repository, from: &Series, to: &Series) -> Result<Vec<StoredFile>> {
    let inputs = inputs(repo, from, to)?;
    let reduced = modulo::reduce(
        &modulo::parse(&inputs.target),
        &modulo::parse(&inputs.base_old),
        &modulo::parse(&inputs.base_new),
    );
    let surviving: HashSet<String> = modulo::paths(&modulo::parse(&reduced));

    Ok(repo
        .changed_files(&from.head, &to.head)
        .into_iter()
        .filter(|f| surviving.contains(&f.path))
        .map(|f| StoredFile {
            path: f.path,
            old_path: f.old_path,
            status: f.status.to_string(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Rendering a two-range comparison as text.
// ---------------------------------------------------------------------------

/// The three diffs every comparison is built from: each series' own change, and
/// the raw difference across the two heads.
struct Inputs {
    base_old: String,
    base_new: String,
    target: String,
}

fn inputs(repo: &Repository, from: &Series, to: &Series) -> Result<Inputs> {
    Ok(Inputs {
        base_old: patch_text_between(repo, &from.fork, &from.head)?,
        base_new: patch_text_between(repo, &to.fork, &to.head)?,
        target: patch_text_between(repo, &from.head, &to.head)?,
    })
}

/// Render the comparison, newline-terminated so the caller can print it verbatim.
///
/// `2way` is the target diff reduced to the hunks the two series are responsible
/// for — see [`modulo`] for why that is an interval intersection rather than a
/// judgement call. `3way` is [`three_way`]: the constituent diffs instead of the
/// conclusion.
fn render_comparison(
    repo: &Repository,
    from: &Series,
    to: &Series,
    mode: PatchMode,
) -> Result<String> {
    let inputs = inputs(repo, from, to)?;
    if mode == PatchMode::ThreeWay {
        return three_way(repo, from, to, &inputs);
    }

    let text = modulo::reduce(
        &modulo::parse(&inputs.target),
        &modulo::parse(&inputs.base_old),
        &modulo::parse(&inputs.base_new),
    );
    if text.trim().is_empty() && series_differs(repo, from, to) {
        // The two heads hold identical content, so there is no target diff to
        // reduce — yet the commits differ against their bases. Either the series
        // was restructured (split, squashed, reordered) with the same result, or
        // a rebase reverted something the base had done. Silence would read as
        // "nothing changed", which is the one wrong thing to take away.
        log::warn!(
            "the two ranges produce identical content, yet their commits differ against \
             their bases — the series was restructured, or a rebase undid something the \
             base did. Drop `--patch` for the pairing, or use `--patch=3way`."
        );
    }
    Ok(text)
}

/// The three diffs a comparison is *made of*, printed in full and labelled —
/// what A does to its base, what B does to its base, and what the base itself
/// did in between.
///
/// Where `2way` gives the filtered conclusion, this gives the raw material behind
/// it. Reach for it when you want the base's change in full rather than the slice
/// of it that sits near the reviewed code, or to check the filtered answer
/// against its inputs. An empty `base` section means the fork point did not move,
/// i.e. there was no rebase.
fn three_way(repo: &Repository, from: &Series, to: &Series, inputs: &Inputs) -> Result<String> {
    // The base's own change, kept to the files anyone involved touched. On a busy
    // trunk the unrestricted diff runs to tens of thousands of lines, essentially
    // none of it about the code under review — which is not "showing you the
    // base change", it is burying it.
    let base_move = modulo::restrict_to_files(
        &modulo::parse(&patch_text_between(repo, &from.fork, &to.fork)?),
        &modulo::parse(&inputs.base_old),
        &modulo::parse(&inputs.base_new),
    );

    let sections = [
        (
            format!(
                "A     {}  (its own change, from fork {})",
                short(&from.head),
                short(&from.fork)
            ),
            inputs.base_old.clone(),
        ),
        (
            format!(
                "B     {}  (its own change, from fork {})",
                short(&to.head),
                short(&to.fork)
            ),
            inputs.base_new.clone(),
        ),
        (
            format!(
                "base  {}..{}  (what the base did, in the files these series touch)",
                short(&from.fork),
                short(&to.fork)
            ),
            base_move,
        ),
    ];

    let mut out = String::new();
    for (label, patch) in sections {
        out.push_str(&format!("=== {label} ===\n"));
        out.push_str(if patch.trim().is_empty() {
            "(no change)\n"
        } else {
            &patch
        });
        out.push('\n');
    }
    Ok(out)
}

/// Did the series change at all between the two ends? Any pairing other than
/// `unchanged` means at least one commit's patch differs.
fn series_differs(repo: &Repository, from: &Series, to: &Series) -> bool {
    let pairs = commit_pairs(repo, from, to);
    pairs.iter().any(|p| p.pairing != Pairing::Unchanged)
}

/// The patch between two tree-ish objects, newline-terminated.
///
/// Two things the git floor's [`Repository::diff_patch`] leaves to the caller.
/// It trims trailing whitespace, which costs the patch its final newline — a
/// patch parser rejects that outright — so the terminator is restored. And it
/// reports "no output" and "the command failed" identically as `None`, so the
/// endpoints are probed to tell an honestly empty diff (a series that changed
/// nothing) from objects that aren't present.
fn patch_text_between(repo: &Repository, from: &str, to: &str) -> Result<String> {
    if let Some(text) = repo.diff_patch(from, to, None) {
        return Ok(text + "\n");
    }
    if repo.rev_parse(from).is_some() && repo.rev_parse(to).is_some() {
        return Ok(String::new());
    }
    anyhow::bail!(
        "could not diff {}..{} — are the objects present?",
        short(from),
        short(to)
    )
}

/// A series' own patch: `fork..head`, which for two commits is just the diff
/// between them since `fork` is always an ancestor.
fn patch_text(repo: &Repository, series: &Series) -> Result<String> {
    patch_text_between(repo, &series.fork, &series.head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_range_diff_pairing_table() {
        let table = "\
1:  abc1234 = 1:  def5678 Add the thing
2:  bcd2345 ! 2:  efa6789 Wire it up
-:  ------- > 3:  fab7890 Handle the null case
3:  cde3456 < -:  ------- Drop the workaround
";
        let pairs: Vec<CommitPair> = table.lines().filter_map(parse_pair_line).collect();
        assert_eq!(pairs.len(), 4);

        assert_eq!(pairs[0].pairing, Pairing::Unchanged);
        assert_eq!(pairs[0].old_sha.as_deref(), Some("abc1234"));
        assert_eq!(pairs[0].new_sha.as_deref(), Some("def5678"));
        assert_eq!(pairs[0].subject, "Add the thing");

        assert_eq!(pairs[1].pairing, Pairing::Reworked);

        // A commit only on one side has a placeholder on the other, which must
        // read as absent rather than as a SHA.
        assert_eq!(pairs[2].pairing, Pairing::Added);
        assert_eq!(pairs[2].old_sha, None);
        assert_eq!(pairs[2].new_sha.as_deref(), Some("fab7890"));

        assert_eq!(pairs[3].pairing, Pairing::Dropped);
        assert_eq!(pairs[3].new_sha, None);
        assert_eq!(pairs[3].subject, "Drop the workaround");
    }

    #[test]
    fn non_table_lines_are_ignored() {
        // Anything that isn't the `n: sha <marker> n: sha subject` shape — a
        // patch body line, a header, a blank — must not parse as a pairing.
        for line in [
            "",
            "    @@ -1,2 +1,2 @@",
            "    +added line",
            "Ranges differ",
            "1:  abc1234 ? 1:  def5678 unknown marker",
        ] {
            assert!(parse_pair_line(line).is_none(), "parsed: {line:?}");
        }
    }
}
