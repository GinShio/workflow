//! Where in a file an edit lands.
//!
//! An anchor resolves to one byte offset in one file's text, or fails loudly. It
//! never guesses: a tree that has drifted past what the catalogue describes must
//! report the rule that no longer fits, because a plausible-looking wrong offset
//! is far more expensive than a refusal.
//!
//! Each shape exists because a current catalogue needs it and none of the others
//! can express that placement.
//!
//! ## Sorted insertion
//!
//! A sorted list may contain independent key partitions. [`Anchor::Sorted`]
//! therefore accepts an optional `group` pattern and compares only entries in the
//! new key's partition.

use anyhow::{bail, Context, Result};
use regex::Regex;

/// A resolved insertion point, plus anything about the resolution the user
/// should not have to discover from the diff.
#[derive(Debug)]
pub struct Placement {
    pub offset: usize,
    pub notes: Vec<String>,
}

impl Placement {
    fn at(offset: usize) -> Self {
        Self {
            offset,
            notes: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum Anchor {
    /// Append at the end of the file. Used where a list is append-ordered and
    /// the tree has no position worth reconstructing.
    Eof,
    /// Immediately before the first match, typically a list's own terminator.
    Before(Regex),
    /// Immediately after the last match. Used where a file holds a *run* of
    /// repeated same-shaped blocks rather than one delimited list, so a new entry
    /// joins the end of the run.
    AfterLast(Regex),
    /// Inside a scope, immediately before `close`.
    ///
    /// `scope` is a *sequence* of successively nested openers, searched one after
    /// another. It is a list because a single pattern cannot always identify a
    /// block: the same inner marker may occur below different outer markers, so
    /// reaching the right one means walking from the outer marker inward.
    InBlock { scope: Vec<Regex>, close: Regex },
    /// At the position a sorted list's own ordering implies.
    ///
    /// `scope` and `close` bound the list; both are optional, because a list is
    /// not always inside a block — some are a whole file of sorted entries with
    /// nothing enclosing them.
    Sorted {
        scope: Vec<Regex>,
        close: Option<Regex>,
        /// Capture group 1 is the sort key of an existing line.
        key: Regex,
        /// Capture group 1 of a key is its partition; keys in other partitions
        /// are ignored. Absent for flat, unsectioned lists.
        group: Option<Regex>,
        /// Existing lines immediately above an entry that move with it.
        attached_before: Option<Regex>,
    },
}

impl Anchor {
    /// Resolve against `text`. `first_line` is the first rendered line of the
    /// body, which [`Anchor::Sorted`] reads the new entry's key out of — the
    /// same pattern reads the existing keys, so a config cannot describe the
    /// two inconsistently.
    pub fn locate(&self, text: &str, first_line: Option<&str>) -> Result<Placement> {
        match self {
            Anchor::Eof => Ok(Placement::at(text.len())),
            Anchor::Before(pattern) => {
                let found = pattern
                    .find(text)
                    .with_context(|| format!("anchor /{pattern}/ not found"))?;
                Ok(Placement::at(found.start()))
            }
            Anchor::AfterLast(pattern) => {
                let found = pattern
                    .find_iter(text)
                    .last()
                    .with_context(|| format!("anchor /{pattern}/ never matches"))?;
                Ok(Placement::at(found.end()))
            }
            Anchor::InBlock { scope, close } => {
                let start = scope_start(text, scope)?;
                let found = close.find(&text[start..]).with_context(|| {
                    format!("block terminator /{close}/ not found after its opener")
                })?;
                Ok(Placement::at(start + found.start()))
            }
            Anchor::Sorted {
                scope,
                close,
                key,
                group,
                attached_before,
            } => {
                let start = scope_start(text, scope)?;
                let end = match close {
                    Some(close) => {
                        start
                            + close
                                .find(&text[start..])
                                .with_context(|| {
                                    format!("block terminator /{close}/ not found after its opener")
                                })?
                                .start()
                    }
                    None => text.len(),
                };
                let line = first_line.context(
                    "a sorted anchor needs a rendered line to read the new entry's key from",
                )?;
                sorted_offset(
                    text,
                    start,
                    end,
                    key,
                    group.as_ref(),
                    attached_before.as_ref(),
                    line,
                )
            }
        }
    }
}

/// Walk successively nested openers, each searched after the previous one's
/// match, and return the offset just past the innermost.
fn scope_start(text: &str, scope: &[Regex]) -> Result<usize> {
    let mut at = 0usize;
    for pattern in scope {
        let found = pattern.find(&text[at..]).with_context(|| {
            format!("scope opener /{pattern}/ not found (searching from offset {at})")
        })?;
        at += found.end();
    }
    Ok(at)
}

/// The alphabetically correct offset for `line` among the keyed lines of
/// `text[start..end]`, restricted to its own partition.
fn sorted_offset(
    text: &str,
    start: usize,
    end: usize,
    key: &Regex,
    group: Option<&Regex>,
    attached_before: Option<&Regex>,
    line: &str,
) -> Result<Placement> {
    let new_key = capture(key, line).with_context(|| {
        format!(
            "sort key /{key}/ does not match the rendered line '{}'",
            line.trim()
        )
    })?;
    let new_group = group.and_then(|g| capture(g, &new_key));

    let mut entries = Vec::new();
    for (offset, text_line) in lines_with_offsets(&text[start..end]) {
        if let Some(existing) = capture(key, text_line) {
            entries.push((start + offset, start + offset + text_line.len(), existing));
        }
    }

    let mut notes = Vec::new();
    let candidates: Vec<_> = match (&new_group, group) {
        (Some(want), Some(pattern)) => entries
            .iter()
            .filter(|(_, _, k)| capture(pattern, k).as_deref() == Some(want.as_str()))
            .collect(),
        _ => entries.iter().collect(),
    };

    if candidates.is_empty() {
        // A new partition has no neighbours to sort against. End of block, said
        // out loud.
        notes.push(
            "no existing entry shares this key's group, appended at the end of the block — \
             check the placement"
                .to_owned(),
        );
        return Ok(Placement { offset: end, notes });
    }

    // The first key above the new one; the entry goes immediately before it.
    let successor = candidates
        .iter()
        .position(|(_, _, existing)| natural_cmp(existing, &new_key).is_gt());

    // Whether the *whole* block is sorted is the wrong question to report on:
    // these lists run to hundreds of hand-maintained entries and none of them is
    // totally ordered under any rule (see [`natural_cmp`]), so asking it produced
    // a note on every run that said nothing about the insertion at hand.
    //
    // What can actually be wrong is this placement. The scan stops at the first
    // key above the new one, so an entry *below* that point whose key sorts below
    // the new one means it stopped too early — and only those entries are
    // evidence of it.
    let overshot: Vec<&str> = candidates[successor.unwrap_or(candidates.len())..]
        .iter()
        .filter(|(_, _, existing)| natural_cmp(existing, &new_key).is_lt())
        .map(|(_, _, existing)| existing.as_str())
        .take(3)
        .collect();
    if !overshot.is_empty() {
        notes.push(format!(
            "placed above {}, which sort before it — this block's own order disagrees here, so \
             check the placement",
            overshot.join(", ")
        ));
    }

    let offset = match successor {
        Some(pos) => back_up_over_attached(text, candidates[pos].0, attached_before),
        // Sorts after everything present: just past the last one's newline.
        None => skip_newline(text, candidates.last().expect("non-empty").1),
    };

    Ok(Placement { offset, notes })
}

/// Move an insertion point back over lines attached to the entry below them.
///
/// Some lists decorate entries with one or more preceding lines:
///
/// ```text
///     BEGIN ITEM
///     ITEM,
///     END ITEM
/// ```
///
/// The keyed line is the *entry*, so an offset computed from it would land
/// between the prefix and its entry. The catalogue supplies the prefix pattern;
/// the placement engine has no knowledge of its syntax.
fn back_up_over_attached(text: &str, line_start: usize, attached_before: Option<&Regex>) -> usize {
    let Some(pattern) = attached_before else {
        return line_start;
    };
    let mut at = line_start;
    loop {
        let Some(previous) = previous_line(text, at) else {
            return at;
        };
        if pattern.is_match(&text[previous..at]) {
            at = previous;
        } else {
            return at;
        }
    }
}

/// Start offset of the line ending at `at`, or `None` at the start of the text.
fn previous_line(text: &str, at: usize) -> Option<usize> {
    if at == 0 {
        return None;
    }
    let before = &text[..at - 1];
    Some(before.rfind('\n').map(|nl| nl + 1).unwrap_or(0))
}

fn skip_newline(text: &str, at: usize) -> usize {
    if text[at..].starts_with('\n') {
        at + 1
    } else {
        at
    }
}

/// `(offset, line)` for every line of `text`, without the terminator.
fn lines_with_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0usize;
    text.split('\n').map(move |line| {
        let offset = at;
        at += line.len() + 1;
        (offset, line)
    })
}

fn capture(pattern: &Regex, text: &str) -> Option<String> {
    pattern
        .captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
}

/// Compare the way the hand-maintained lists are actually ordered: digit runs
/// compare by value, so `A_MAINT9` precedes `A_MAINT10`. A plain byte compare
/// would invert that pair, and version numbers keep climbing.
///
/// Byte order everywhere else is not a preference, it is the closest fit
/// available: these lists disagree with themselves about where `_` ranks. One
/// pair orders `A_WORD_X` before `A_WORDS_Y` (which needs `_` below a letter)
/// while another orders `A_WORDR_X` before `A_WORD_Y` (which needs `_` above
/// one), and no total order on characters satisfies both. So a perfect collation
/// does not exist, and [`sorted_offset`] reports the cases where this one lands
/// an entry somewhere the list's own order contradicts.
fn natural_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut l = left.chars().peekable();
    let mut r = right.chars().peekable();
    loop {
        match (l.peek().copied(), r.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(lc), Some(rc)) if lc.is_ascii_digit() && rc.is_ascii_digit() => {
                let lnum = take_number(&mut l);
                let rnum = take_number(&mut r);
                match lnum.cmp(&rnum) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            (Some(lc), Some(rc)) => {
                match lc.cmp(&rc) {
                    Ordering::Equal => {}
                    other => return other,
                }
                l.next();
                r.next();
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut value = 0u64;
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        // Saturate rather than wrap: an absurdly long digit run is a malformed
        // key, and it only has to sort consistently, not exactly.
        value = value
            .saturating_mul(10)
            .saturating_add(c as u64 - '0' as u64);
        chars.next();
    }
    value
}

/// Parse the `Anchor` out of an already-rendered catalogue entry.
pub fn compile(spec: &AnchorSpec) -> Result<Anchor> {
    let scope = spec
        .scope
        .iter()
        .map(|p| re(p).with_context(|| format!("bad scope pattern /{p}/")))
        .collect::<Result<Vec<_>>>()?;

    match spec {
        AnchorSpec { eof: true, .. } => Ok(Anchor::Eof),
        AnchorSpec { key: Some(key), .. } => Ok(Anchor::Sorted {
            scope,
            close: spec.close.as_deref().map(re).transpose()?,
            key: re(key)?,
            group: spec.group.as_deref().map(re).transpose()?,
            attached_before: spec.attached_before.as_deref().map(re).transpose()?,
        }),
        AnchorSpec {
            close: Some(close), ..
        } if !scope.is_empty() => Ok(Anchor::InBlock {
            scope,
            close: re(close)?,
        }),
        AnchorSpec {
            close: Some(close), ..
        } => Ok(Anchor::Before(re(close)?)),
        AnchorSpec {
            after_last: Some(pattern),
            ..
        } => Ok(Anchor::AfterLast(re(pattern)?)),
        _ => bail!(
            "anchor names no position: expected one of eof, before, after_last, or scope+before"
        ),
    }
}

/// Compile a catalogue pattern in multi-line mode.
///
/// Every anchor in the catalogue is line-oriented — `^\};$` means "a line that
/// is just `};`". Rust's `regex` anchors `^`/`$` to the whole *text* by default,
/// so without this each such pattern would silently match nothing. Forcing the
/// flag here keeps the catalogue readable instead of requiring a `(?m)` prefix
/// on all twenty-odd patterns.
fn re(pattern: &str) -> Result<Regex> {
    regex::RegexBuilder::new(pattern)
        .multi_line(true)
        .build()
        .with_context(|| format!("bad pattern /{pattern}/"))
}

/// The anchor as the catalogue spells it, after template rendering. Kept
/// separate from [`Anchor`] so a bad pattern is reported against the rule that
/// wrote it rather than surfacing as a regex error from nowhere.
#[derive(Debug, Default)]
pub struct AnchorSpec {
    pub eof: bool,
    pub scope: Vec<String>,
    /// The `before` key of a rule: a bare terminator, or a block's terminator
    /// when `scope` is set.
    pub close: Option<String>,
    pub after_last: Option<String>,
    pub key: Option<String>,
    pub group: Option<String>,
    pub attached_before: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_sorted(key: &str, group: Option<&str>) -> AnchorSpec {
        AnchorSpec {
            scope: vec!["enum Id".into()],
            close: Some(r"^\};$".into()),
            key: Some(key.into()),
            group: group.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn eof_appends() {
        let placed = Anchor::Eof.locate("abc\n", None).unwrap();
        assert_eq!(placed.offset, 4);
    }

    #[test]
    fn a_missing_anchor_is_an_error_not_a_guess() {
        let anchor = compile(&AnchorSpec {
            close: Some("^nowhere$".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(anchor.locate("a\nb\n", None).is_err());
    }

    #[test]
    fn after_last_takes_the_final_run() {
        let anchor = compile(&AnchorSpec {
            after_last: Some(r"^opt\n".into()),
            ..Default::default()
        })
        .unwrap();
        let text = "opt\nx\nopt\ny\n";
        // End of the *second* `opt` line, not the first.
        assert_eq!(anchor.locate(text, None).unwrap().offset, 10);
    }

    #[test]
    fn scope_reaches_the_matching_nested_block() {
        let text = "\
outer first
    section Id
        A_ONE,
};
outer wanted
    section Id
        B_TWO,
};
";
        let anchor = compile(&AnchorSpec {
            scope: vec!["outer wanted".into(), "section Id".into()],
            close: Some(r"^\};$".into()),
            ..Default::default()
        })
        .unwrap();
        let placed = anchor.locate(text, None).unwrap();
        // Lands in the second block, after `B_TWO,`.
        assert!(text[..placed.offset].contains("B_TWO"));
    }

    #[test]
    fn sorted_places_within_the_matching_section() {
        // `BBB` sorts before `AAA` as bytes, so a section-blind sort would put
        // this at the top of the AAA run.
        let text = "\
    enum Id
        AAA_ALPHA,
        AAA_ZULU,
        BBB_ALPHA,
        BBB_ZULU,
};
";
        let anchor = compile(&spec_sorted(r"^\s*([A-Z0-9_]+),", Some("^([A-Z]+)_"))).unwrap();
        let placed = anchor.locate(text, Some("        BBB_MIKE,")).unwrap();
        let before = &text[..placed.offset];
        assert!(before.contains("BBB_ALPHA"), "should follow BBB_ALPHA");
        assert!(!before.contains("BBB_ZULU"), "should precede BBB_ZULU");
    }

    #[test]
    fn sorted_without_a_group_treats_the_block_as_flat() {
        let text = "    enum E\n        ALPHA,\n        ZULU,\n};\n";
        let anchor = compile(&AnchorSpec {
            scope: vec!["enum E".into()],
            close: Some(r"^\};$".into()),
            key: Some(r"^\s*([A-Z0-9_]+),".into()),
            ..Default::default()
        })
        .unwrap();
        let placed = anchor.locate(text, Some("        MIKE,")).unwrap();
        assert!(text[..placed.offset].contains("ALPHA"));
        assert!(!text[..placed.offset].contains("ZULU"));
    }

    #[test]
    fn sorted_after_everything_lands_past_the_last_entry() {
        let text = "    enum E\n        ALPHA,\n        MIKE,\n};\n";
        let anchor = compile(&AnchorSpec {
            scope: vec!["enum E".into()],
            close: Some(r"^\};$".into()),
            key: Some(r"^\s*([A-Z0-9_]+),".into()),
            ..Default::default()
        })
        .unwrap();
        let placed = anchor.locate(text, Some("        ZULU,")).unwrap();
        assert_eq!(&text[placed.offset..], "};\n");
    }

    #[test]
    fn insertion_does_not_split_an_entry_from_its_prefix() {
        let text = "\
    enum E
        ALPHA,
BEGIN MIKE
        MIKE,
END MIKE
        ZULU,
};
";
        let anchor = compile(&AnchorSpec {
            scope: vec!["enum E".into()],
            close: Some(r"^\};$".into()),
            key: Some(r"^\s*([A-Z0-9_]+),".into()),
            attached_before: Some(r"^BEGIN ".into()),
            ..Default::default()
        })
        .unwrap();
        // Sorts between ALPHA and MIKE, whose prefix belongs to that entry.
        let placed = anchor.locate(text, Some("        BRAVO,")).unwrap();
        assert!(
            text[..placed.offset].ends_with("ALPHA,\n"),
            "must land before the prefix, not inside the entry"
        );
    }

    #[test]
    fn a_new_section_is_reported_rather_than_placed_silently() {
        let text = "    enum Id\n        AAA_ALPHA,\n};\n";
        let anchor = compile(&spec_sorted(r"^\s*([A-Z0-9_]+),", Some("^([A-Z]+)_"))).unwrap();
        let placed = anchor.locate(text, Some("        CCC_THING,")).unwrap();
        assert!(!placed.notes.is_empty(), "the user has to hear about this");
    }

    fn flat_sorted() -> Anchor {
        compile(&AnchorSpec {
            scope: vec!["enum E".into()],
            close: Some(r"^\};$".into()),
            key: Some(r"^\s*([A-Z0-9_]+),".into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_disorder_the_scan_overshot_is_reported_and_named() {
        let text = "    enum E\n        ALPHA,\n        ZULU,\n        BRAVO,\n};\n";
        let placed = flat_sorted().locate(text, Some("        MIKE,")).unwrap();
        // First key above MIKE is ZULU, so MIKE lands before it — leaving BRAVO,
        // which belongs above MIKE, below it. That is the case worth a note, and
        // the note has to name BRAVO for the reader to act on it.
        assert!(text[..placed.offset].ends_with("ALPHA,\n"));
        assert!(
            placed.notes.iter().any(|n| n.contains("BRAVO")),
            "got: {:?}",
            placed.notes
        );
    }

    #[test]
    fn a_disorder_that_cannot_have_misplaced_the_entry_is_not_reported() {
        // ZULU/ROMEO is disordered, but both sort above BRAVO, so neither could
        // have belonged before it however the scan ran. A note here would fire on
        // every run of every hand-maintained list and mean nothing.
        let text = "    enum E\n        ALPHA,\n        ZULU,\n        ROMEO,\n};\n";
        let placed = flat_sorted().locate(text, Some("        BRAVO,")).unwrap();
        assert!(text[..placed.offset].ends_with("ALPHA,\n"));
        assert!(placed.notes.is_empty(), "got: {:?}", placed.notes);
    }

    #[test]
    fn a_digit_run_compares_by_value_not_by_byte() {
        use std::cmp::Ordering;
        // Version numbers keep climbing, so this is the pair that recurs.
        assert_eq!(natural_cmp("A_MAINT9", "A_MAINT10"), Ordering::Less);
        assert_eq!(natural_cmp("A_ALPHA", "A_ALPHA"), Ordering::Equal);
        assert_eq!(natural_cmp("A_ALPHAS", "A_ALPHA"), Ordering::Greater);
    }

    #[test]
    fn a_sorted_anchor_without_a_rendered_line_is_a_config_error() {
        let anchor = compile(&spec_sorted(r"^\s*([A-Z0-9_]+),", None)).unwrap();
        assert!(anchor.locate("    enum Id\n};\n", None).is_err());
    }

    #[test]
    fn a_whole_file_sorted_list_needs_no_block() {
        // Some of these lists are a whole file of sorted entries.
        let text = "ALPHA = a\nMIKE = m\nZULU = z\n";
        let anchor = compile(&AnchorSpec {
            key: Some(r"^([A-Z][A-Z0-9_]*)\s".to_owned()),
            ..Default::default()
        })
        .unwrap();
        let placed = anchor.locate(text, Some("NOVEMBER = n")).unwrap();
        assert!(text[..placed.offset].ends_with("MIKE = m\n"));
    }
}
