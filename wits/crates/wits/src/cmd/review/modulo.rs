//! Comparing two versions of a series *modulo* the base they sit on.
//!
//! The problem: between two review points the author changed a few lines, but
//! the base branch moved by tens of thousands. A raw `headA..headB` diff is
//! swamped — on a real MR, 24,767 lines across 353 files, of which 3 files were
//! the author's doing.
//!
//! The reduction, after Nicolai Hähnle's
//! [diff-modulo-base](https://github.com/nhaehnle/vctools): a hunk of the target
//! diff is worth showing only if the region it covers was **touched by one of
//! the two series**. If neither series touched it, both heads hold unmodified
//! base content there, so any difference can only be the base having moved.
//!
//! What makes this cheap is that the coordinates already line up — no content
//! matching, no heuristics about similar text:
//!
//! ```text
//! target hunk's old side == headA lines == base_old diff's NEW side
//! target hunk's new side == headB lines == base_new diff's NEW side
//! ```
//!
//! So it is an interval intersection over hunk headers. It always terminates,
//! always produces a valid unified diff, and its output is bounded by the target
//! diff — unlike a merge-based approach, which has no answer at all when the
//! base rewrote lines the series also touches.

use std::collections::HashMap;

/// One hunk, kept verbatim so a reduced patch can be re-emitted byte-for-byte.
#[derive(Debug)]
struct Hunk {
    /// Inclusive line span on the diff's old side.
    old: (u32, u32),
    /// Inclusive line span on the diff's new side.
    new: (u32, u32),
    text: String,
}

/// One file's section of a unified diff.
#[derive(Debug)]
struct FilePatch {
    /// The post-image path (`+++ b/…`), or the pre-image for a deletion.
    path: String,
    /// Everything from `diff --git` up to the first hunk, verbatim.
    header: String,
    hunks: Vec<Hunk>,
}

/// A parsed unified diff.
#[derive(Debug, Default)]
pub struct Patch {
    files: Vec<FilePatch>,
}

/// The literal text one series added and removed in a file.
///
/// Content rather than position, because a line's *number* in the target diff
/// says nothing about who is responsible for it — only whether the same text
/// appears in that series' own change does.
#[derive(Default)]
struct LineSets<'a> {
    added: std::collections::HashSet<&'a str>,
    removed: std::collections::HashSet<&'a str>,
}

impl Patch {
    /// The new-side line spans each file's hunks cover — "what this series
    /// touched, in its own head's coordinates".
    fn touched(&self) -> HashMap<&str, Vec<(u32, u32)>> {
        self.files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.hunks.iter().map(|h| h.new).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    /// Per file, the lines this series added and removed.
    fn changed_lines(&self) -> HashMap<&str, LineSets<'_>> {
        self.files
            .iter()
            .map(|f| {
                let mut sets = LineSets::default();
                for line in f.hunks.iter().flat_map(|h| h.text.lines().skip(1)) {
                    match line.split_at_checked(1) {
                        Some(("+", body)) => sets.added.insert(body),
                        Some(("-", body)) => sets.removed.insert(body),
                        _ => false,
                    };
                }
                (f.path.as_str(), sets)
            })
            .collect()
    }
}

/// Is any changed line in this hunk the *series'* doing rather than the base's?
///
/// A hunk can sit squarely inside a region a series edited and still contain
/// nothing but base movement — the base changed a neighbouring line, and the
/// series' own contribution is byte-identical on both sides. Range overlap alone
/// cannot tell those apart, so each changed line is checked against what the two
/// series actually wrote:
///
/// - an added line (present in B's head, absent in A's) is theirs if B added it,
///   or if A had removed it and B did not;
/// - a removed line, symmetrically.
///
/// Coincidental text matches make this err towards keeping the hunk, which is
/// the right direction: showing a little too much is a nuisance, hiding a real
/// change is a bug.
fn attributable(hunk: &Hunk, a: Option<&LineSets>, b: Option<&LineSets>) -> bool {
    hunk.text
        .lines()
        .skip(1)
        .any(|line| match line.split_at_checked(1) {
            Some(("+", body)) => {
                a.is_some_and(|s| s.removed.contains(body))
                    || b.is_some_and(|s| s.added.contains(body))
            }
            Some(("-", body)) => {
                a.is_some_and(|s| s.added.contains(body))
                    || b.is_some_and(|s| s.removed.contains(body))
            }
            _ => false,
        })
}

/// Parse a unified diff. Anything unrecognised is skipped rather than rejected:
/// this reads git's own output, and a reduction that refused to run on an
/// unusual header would be worse than one that keeps a file it can't split.
pub fn parse(text: &str) -> Patch {
    let mut patch = Patch::default();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("diff --git ") {
            continue;
        }
        let mut header = String::from(line);
        header.push('\n');
        let mut pre_image = None;
        let mut post_image = None;

        // Header runs until the first hunk or the next file.
        while let Some(next) = lines.peek() {
            if next.starts_with("@@ ") || next.starts_with("diff --git ") {
                break;
            }
            let next = lines.next().expect("peeked");
            // Take the path from `--- a/…` / `+++ b/…` rather than the
            // `diff --git` line: those have nothing after the path, so a path
            // containing spaces stays unambiguous.
            if let Some(p) = next.strip_prefix("--- a/") {
                pre_image = Some(p.to_owned());
            } else if let Some(p) = next.strip_prefix("+++ b/") {
                post_image = Some(p.to_owned());
            }
            header.push_str(next);
            header.push('\n');
        }

        let Some(path) = post_image.or(pre_image).or_else(|| git_header_path(line)) else {
            continue;
        };
        let mut file = FilePatch {
            path,
            header,
            hunks: Vec::new(),
        };

        while let Some(next) = lines.peek() {
            if next.starts_with("diff --git ") {
                break;
            }
            let next = lines.next().expect("peeked");
            let Some((old, new)) = parse_hunk_header(next) else {
                continue;
            };
            let mut text = String::from(next);
            text.push('\n');
            while let Some(body) = lines.peek() {
                if body.starts_with("@@ ") || body.starts_with("diff --git ") {
                    break;
                }
                text.push_str(lines.next().expect("peeked"));
                text.push('\n');
            }
            file.hunks.push(Hunk { old, new, text });
        }
        patch.files.push(file);
    }
    patch
}

/// Last-resort path extraction from `diff --git a/X b/X`, for a file whose
/// header carried no `---`/`+++` (a pure mode change, say). Ambiguous when the
/// path contains a space, so it is only reached when the reliable lines are
/// absent.
fn git_header_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let (_, b) = rest.split_once(" b/")?;
    Some(b.to_owned())
}

/// `@@ -a,b +c,d @@` into inclusive `(old, new)` spans. A count of `0` marks an
/// empty side (a pure addition or deletion); it still occupies a position, so it
/// is given a zero-width span *at* that position rather than being skipped.
fn parse_hunk_header(line: &str) -> Option<((u32, u32), (u32, u32))> {
    let body = line.strip_prefix("@@ -")?;
    let (ranges, _) = body.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    Some((span(old)?, span(new)?))
}

fn span(field: &str) -> Option<(u32, u32)> {
    let (start, count) = match field.split_once(',') {
        Some((s, c)) => (s.parse().ok()?, c.parse().ok()?),
        None => (field.parse().ok()?, 1u32),
    };
    // An empty side sits *between* lines; treat it as touching the line it
    // abuts so an insertion still counts as overlapping neighbouring work.
    Some((start, start + count.max(1) - 1))
}

/// Do two inclusive spans come within `fuzz` lines of each other?
fn near(a: (u32, u32), spans: &[(u32, u32)], fuzz: u32) -> bool {
    spans
        .iter()
        .any(|&(lo, hi)| a.1 + fuzz >= lo && a.0 <= hi + fuzz)
}

/// How far apart two regions can be and still be considered related. Hunks
/// already carry three lines of context either side, so an exact overlap test
/// almost always suffices; the margin only matters for edits that land just
/// outside a neighbouring hunk's context.
const FUZZ: u32 = 3;

/// Reduce `target` to the hunks the two series are responsible for.
///
/// `base_old` and `base_new` are each series' own change (`fork..head`), and
/// `target` is the raw diff across the two heads. A hunk survives when it meets
/// either series' footprint; a file survives when any of its hunks do.
///
/// A file with no hunks at all — a binary change, a mode change — carries no
/// line coordinates to test, so it is kept when either series touched that path
/// and dropped otherwise.
pub fn reduce(target: &Patch, base_old: &Patch, base_new: &Patch) -> String {
    let old_touch = base_old.touched();
    let new_touch = base_new.touched();
    let old_lines = base_old.changed_lines();
    let new_lines = base_new.changed_lines();
    let mut out = String::new();

    for file in &target.files {
        let path = file.path.as_str();
        let in_old = old_touch.get(path);
        let in_new = new_touch.get(path);
        if file.hunks.is_empty() {
            if in_old.is_some() || in_new.is_some() {
                out.push_str(&file.header);
            }
            continue;
        }

        let (a_lines, b_lines) = (old_lines.get(path), new_lines.get(path));
        let kept: Vec<&Hunk> = file
            .hunks
            .iter()
            .filter(|h| {
                // Cheap range test first; only then ask who wrote the lines.
                (near(h.old, in_old.map_or(&[], Vec::as_slice), FUZZ)
                    || near(h.new, in_new.map_or(&[], Vec::as_slice), FUZZ))
                    && attributable(h, a_lines, b_lines)
            })
            .collect();
        if kept.is_empty() {
            continue;
        }
        out.push_str(&file.header);
        for hunk in kept {
            out.push_str(&hunk.text);
        }
    }
    out
}

/// The post-image paths a patch covers.
pub fn paths(patch: &Patch) -> std::collections::HashSet<String> {
    patch.files.iter().map(|f| f.path.clone()).collect()
}

/// Restrict a diff to the files two series touch, dropping the rest whole.
///
/// Used for the base's own change in a three-way view: on a busy trunk that diff
/// runs to tens of thousands of lines, virtually none of it about the code under
/// review. Filtering by file rather than by hunk keeps the promise that this
/// view shows the base's change *in full* — just not for files nobody involved
/// has touched.
pub fn restrict_to_files(diff: &Patch, base_old: &Patch, base_new: &Patch) -> String {
    let old_touch = base_old.touched();
    let new_touch = base_new.touched();
    let mut out = String::new();
    for file in &diff.files {
        let path = file.path.as_str();
        if old_touch.contains_key(path) || new_touch.contains_key(path) {
            out.push_str(&file.header);
            for hunk in &file.hunks {
                out.push_str(&hunk.text);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-file unified diff. Each hunk is `(start, body)`, where every body
    /// line already carries its own `+`/`-`/space marker — the fixtures have to
    /// be *coherent*, since the reduction reasons about who wrote which line.
    fn diff_of(path: &str, hunks: &[(u32, &[&str])]) -> String {
        let mut s = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
        for (start, body) in hunks {
            let len = body.len() as u32;
            s.push_str(&format!("@@ -{start},{len} +{start},{len} @@\n"));
            for line in *body {
                s.push_str(line);
                s.push('\n');
            }
        }
        s
    }

    /// The common shape: a series that contributed one line, reworked between
    /// the two review points. `a` wrote `V1`, `b` wrote `V2`.
    fn reworked(path: &str, at: u32) -> (Patch, Patch, Patch) {
        (
            parse(&diff_of(path, &[(at, &[" ctx", "-V1", "+V2"])])),
            parse(&diff_of(path, &[(at, &[" ctx", "+V1"])])),
            parse(&diff_of(path, &[(at, &[" ctx", "+V2"])])),
        )
    }

    #[test]
    fn parses_spans_from_hunk_headers() {
        assert_eq!(
            parse_hunk_header("@@ -10,5 +20,7 @@ fn foo()"),
            Some(((10, 14), (20, 26)))
        );
        // A missing count means one line.
        assert_eq!(parse_hunk_header("@@ -7 +9 @@"), Some(((7, 7), (9, 9))));
        // A zero count is an empty side; it still occupies its position.
        assert_eq!(parse_hunk_header("@@ -0,0 +1,3 @@"), Some(((0, 0), (1, 3))));
        assert_eq!(parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn a_hunk_neither_series_touched_is_dropped() {
        // The base moved `far.c` and line 900 of `shared.c`; the series only
        // ever touched line 10 of `shared.c`. Only that survives.
        let target = parse(&format!(
            "{}{}",
            diff_of(
                "shared.c",
                &[
                    (10, &[" ctx", "-V1", "+V2"]),
                    (900, &[" ctx", "+BASE-ONLY"]),
                ]
            ),
            diff_of("far.c", &[(1, &[" ctx", "+BASE-ONLY"])])
        ));
        let a = parse(&diff_of("shared.c", &[(10, &[" ctx", "+V1"])]));
        let b = parse(&diff_of("shared.c", &[(10, &[" ctx", "+V2"])]));
        let out = reduce(&target, &a, &b);

        assert!(out.contains("shared.c"), "{out}");
        assert!(!out.contains("far.c"), "a file no series touched: {out}");
        assert!(out.contains("@@ -10,3 +10,3 @@"), "{out}");
        assert!(
            !out.contains("@@ -900,2 +900,2 @@"),
            "a far-away hunk in a touched file: {out}"
        );
    }

    #[test]
    fn a_touched_region_whose_difference_is_all_base_is_still_dropped() {
        // The refinement's whole reason for existing, and a case seen in the
        // wild. Both review points contribute the *same* line here, so the
        // series' own work is byte-identical; the heads differ only because the
        // base added a line right beside it. Range overlap alone would keep this
        // — checking who wrote the changed line does not.
        let target = parse(&diff_of("a.c", &[(10, &[" ctx", "+THEIRS", " MINE"])]));
        let same = parse(&diff_of("a.c", &[(10, &[" ctx", "+MINE"])]));

        assert!(
            reduce(&target, &same, &same).is_empty(),
            "a hunk whose only change is the base's must go"
        );
    }

    #[test]
    fn either_side_touching_is_enough() {
        // A change may exist on only one side — dropped by the newer series, or
        // added by it — and either is the series' doing.
        let empty = parse("");
        let dropped = parse(&diff_of("a.c", &[(50, &[" ctx", "-V1"])]));
        let a_wrote_v1 = parse(&diff_of("a.c", &[(50, &[" ctx", "+V1"])]));
        assert!(
            reduce(&dropped, &a_wrote_v1, &empty).contains("@@ -50"),
            "a line the older series added and the newer dropped"
        );

        let added = parse(&diff_of("a.c", &[(50, &[" ctx", "+V2"])]));
        let b_wrote_v2 = parse(&diff_of("a.c", &[(50, &[" ctx", "+V2"])]));
        assert!(
            reduce(&added, &empty, &b_wrote_v2).contains("@@ -50"),
            "a line only the newer series added"
        );

        // With neither series involved there is nothing to attribute it to.
        assert!(!reduce(&added, &empty, &empty).contains("@@"));
    }

    #[test]
    fn the_output_is_a_valid_patch_with_its_file_header() {
        let (target, a, b) = reworked("a.c", 1);
        let out = reduce(&target, &a, &b);
        assert!(out.starts_with("diff --git a/a.c b/a.c\n"), "{out}");
        assert!(out.contains("--- a/a.c\n+++ b/a.c\n"), "{out}");
        assert!(out.contains("@@ -1,3 +1,3 @@"), "{out}");
    }

    #[test]
    fn restricting_keeps_whole_files_and_drops_the_rest() {
        let base = parse(&format!(
            "{}{}",
            diff_of("mine.c", &[(1, &[" ctx", "+X"]), (500, &[" ctx", "+FAR"])]),
            diff_of("theirs.c", &[(1, &[" ctx", "+Y"])])
        ));
        let series = parse(&diff_of("mine.c", &[(1, &[" ctx", "+X"])]));
        let out = restrict_to_files(&base, &series, &parse(""));

        assert!(out.contains("mine.c"));
        assert!(!out.contains("theirs.c"));
        // "In full" means every hunk of a kept file, including distant ones.
        assert!(out.contains("@@ -500,2"), "{out}");
    }

    #[test]
    fn a_path_containing_a_space_is_read_from_the_marker_lines() {
        let text = "diff --git a/dir/my file.c b/dir/my file.c\n\
                    --- a/dir/my file.c\n\
                    +++ b/dir/my file.c\n\
                    @@ -1,1 +1,1 @@\n-a\n+b\n";
        let p = parse(text);
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].path, "dir/my file.c");
    }
}
