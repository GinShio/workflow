//! Where a line of a change's post-image sits in its pre-image.
//!
//! A comment on a line the change left untouched names it by its new-side
//! number, and GitLab will only place such a line by both of its numbers (see
//! [`LineRef::old_line`](wits_util::forge::LineRef::old_line)). The old number
//! is read off the change's zero-context hunk headers: a line inside a hunk was
//! added; any other line is unchanged, shifted by what the hunks before it
//! removed and added.

/// One hunk of a zero-context (`-U0`) diff: the `(start, count)` of each side,
/// as `@@ -start,count +start,count @@` gives them. A side with a count of 0
/// is empty, and its start is the line *before* the gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hunk {
    old: (u32, u32),
    new: (u32, u32),
}

/// The hunks of a zero-context diff of one file, in order.
pub(crate) fn hunks(patch: &str) -> Vec<Hunk> {
    patch.lines().filter_map(parse_header).collect()
}

fn parse_header(line: &str) -> Option<Hunk> {
    let (ranges, _) = line.strip_prefix("@@ -")?.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    Some(Hunk {
        old: span(old)?,
        new: span(new)?,
    })
}

/// `start[,count]`, the count defaulting to 1.
fn span(field: &str) -> Option<(u32, u32)> {
    match field.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((field.parse().ok()?, 1)),
    }
}

/// The pre-image number of post-image line `new_line`, or `None` when the
/// change added it.
pub(crate) fn old_line_of(hunks: &[Hunk], new_line: u32) -> Option<u32> {
    let mut shift = 0i64;
    for hunk in hunks {
        let (new_start, new_count) = hunk.new;
        let after = if new_count == 0 {
            new_line > new_start
        } else {
            new_line >= new_start + new_count
        };
        if !after {
            if new_count > 0 && new_line >= new_start {
                return None;
            }
            break;
        }
        shift += i64::from(hunk.old.1) - i64::from(new_count);
    }
    u32::try_from(i64::from(new_line) + shift).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of `git diff -U0`: a header per hunk, then only changed lines.
    const PATCH: &str = "\
diff --git a/f.c b/f.c
--- a/f.c
+++ b/f.c
@@ -2,0 +3,2 @@ int a;
+int b;
+int c;
@@ -5 +6,0 @@ int d;
-int e;
@@ -9,2 +10,3 @@ int f;
-int g;
-int h;
+int i;
+int j;
+int k;
";

    #[test]
    fn reads_every_hunk_header_and_only_those() {
        assert_eq!(
            hunks(PATCH),
            [
                Hunk {
                    old: (2, 0),
                    new: (3, 2)
                },
                Hunk {
                    old: (5, 1),
                    new: (6, 0)
                },
                Hunk {
                    old: (9, 2),
                    new: (10, 3)
                },
            ]
        );
    }

    #[test]
    fn an_unchanged_line_is_shifted_by_the_hunks_before_it() {
        let hunks = hunks(PATCH);
        let old = |new| old_line_of(&hunks, new);
        // Before any hunk, a line keeps its number.
        assert_eq!((old(1), old(2)), (Some(1), Some(2)));
        // Two lines were added at 3 and 4…
        assert_eq!((old(3), old(4)), (None, None));
        // …so the lines after them sit two lower in the old file, up to the
        // deletion of old line 5, after which they sit one lower.
        assert_eq!((old(5), old(6)), (Some(3), Some(4)));
        assert_eq!((old(7), old(9)), (Some(6), Some(8)));
        // Lines 10-12 replaced old 9-10, and after them the shift is two again.
        assert_eq!((old(10), old(12)), (None, None));
        assert_eq!(old(13), Some(11));
    }

    #[test]
    fn a_new_file_has_no_unchanged_line() {
        let hunks = hunks("@@ -0,0 +1,3 @@\n+a\n+b\n+c\n");
        assert_eq!(old_line_of(&hunks, 1), None);
        assert_eq!(old_line_of(&hunks, 3), None);
    }
}
