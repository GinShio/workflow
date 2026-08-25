//! Recover token assignments from a Khronos extension asciidoc.
//!
//! This is the only inexact source, and the reason the descriptor is a
//! reviewable intermediate rather than an internal step. Registry specs are
//! hand-written and their table layout drifts between authors and years, so
//! everything here is a heuristic that **reports what it could not classify
//! instead of guessing**. Anything it gets wrong is meant to be fixed in the
//! emitted descriptor, not in this parser.
//!
//! It exists because some specifications have no matching machine-readable
//! grammar entry.
//!
//! ## The shapes it looks for
//!
//! An enumerant table, classified by its *header row* rather than the enclosing
//! section heading, because the heading wording varies far more:
//!
//! ```text
//! |====
//! 2+^| Capability ^| Implicitly Declares
//! | 6141 | *WidgetVND*
//! |
//! |====
//! ```
//!
//! and an instruction, whose opcode sits in an encoding row identified by its
//! leading word count:
//!
//! ```text
//! 4+|[[OpWidgetArriveVND]]*OpWidgetArriveVND* +
//! 1+|Capability: +
//! *WidgetVND*
//! 1+| 4 | 6142
//! ```

use anyhow::{bail, Context, Result};
use regex::Regex;

use crate::kinds::KindTable;
use crate::model::{Enumerant, KindGroup, SpvEncoding, SpvOpcode, SpvPlane};

/// The compiled patterns, built once per extraction.
struct Patterns {
    /// A table delimiter: `|====`, with however many `=` the author felt like.
    delimiter: Regex,
    /// A value/name row: `| 6141 | *WidgetVND*`, possibly continuing.
    enum_row: Regex,
    /// An instruction anchor: `[[OpWidgetArriveVND]]`.
    anchor: Regex,
    /// The encoding row: a word count, then the opcode.
    ///
    /// The trailing `|` is optional because an instruction with no operands ends
    /// the row at the opcode — `1+| 4 | 6142` — and requiring the separator
    /// silently loses exactly those instructions.
    encoding: Regex,
    capability_cell: Regex,
    row_start: Regex,
    bold: Regex,
    name_string: Regex,
}

impl Patterns {
    fn new() -> Result<Self> {
        Ok(Self {
            delimiter: Regex::new(r"^\|=+\s*$")?,
            enum_row: Regex::new(r"^\|\s*(\d+)\s*\|\s*\*([A-Za-z_]\w*)\*(.*)$")?,
            anchor: Regex::new(r"\[\[(Op[A-Za-z0-9_]+)\]\]")?,
            encoding: Regex::new(
                r"^(?:\d+\+)?\|\s*(\d+)(?:\s*\+\s*variable)?\s*\|\s*(\d+)\s*(?:\||$)",
            )?,
            capability_cell: Regex::new(r"^(?:\d+\+)?\|\s*(?i:Capability):")?,
            row_start: Regex::new(r"^(?:\d+\+)?\|")?,
            bold: Regex::new(r"\*([A-Za-z_]\w*)\*")?,
            name_string: Regex::new(r"\bSPV_[A-Za-z0-9_]+\b")?,
        })
    }
}

/// Extract what can be read out of one extension spec.
pub fn extract(
    text: &str,
    name: Option<&str>,
    table: &KindTable,
) -> Result<(SpvPlane, Vec<String>)> {
    let re = Patterns::new()?;
    let mut notes = Vec::new();

    let name = match name {
        Some(given) => given.to_owned(),
        None => find_name(text, &re)
            .context("no SPV_ name string found in this spec; pass the extension name")?,
    };
    let mut plane = SpvPlane::new(&name);
    let lines: Vec<&str> = text.lines().collect();

    // Collect per kind first, so a kind whose rows are split across two tables
    // (a definition table and a summary appendix) ends up as one group.
    let mut collected: Vec<(String, Vec<Enumerant>)> = Vec::new();
    for (header, body) in tables(&lines, &re) {
        let Some(kind) = table.by_header(header) else {
            continue;
        };
        let rows = enum_rows(&body, &re);
        if rows.is_empty() {
            continue;
        }
        match collected.iter_mut().find(|(name, _)| *name == kind.name) {
            Some((_, existing)) => existing.extend(rows),
            None => collected.push((kind.name.clone(), rows)),
        }
    }

    for (kind_name, rows) in collected {
        let kind = table
            .by_name(&kind_name)
            .expect("collected under a name from the table");
        let rows = dedupe_enumerants(rows, &kind_name, &mut notes);
        plane.kinds.push(KindGroup {
            name: kind.name.clone(),
            meta: kind.meta.clone(),
            enumerants: rows,
        });
    }

    let found = instructions(&lines, &re, &mut notes);
    for opcode in dedupe_opcodes(found, &mut notes) {
        if opcode.class == "Type-Declaration" {
            plane.types.push(opcode);
        } else {
            plane.operations.push(opcode);
        }
    }

    if plane.kinds.is_empty() && plane.types.is_empty() && plane.operations.is_empty() {
        bail!(
            "found no capability, enumerant or instruction tables in this spec — write the \
             descriptor by hand and generate from that"
        );
    }
    notes.push(
        "extracted from hand-written asciidoc: check every value before generating".to_owned(),
    );
    Ok((plane, notes))
}

fn dedupe_enumerants(
    enumerants: Vec<Enumerant>,
    kind: &str,
    notes: &mut Vec<String>,
) -> Vec<Enumerant> {
    let mut kept: Vec<Enumerant> = Vec::new();
    for enumerant in enumerants {
        if let Some(existing) = kept.iter().find(|entry| entry.name == enumerant.name) {
            if existing.value != enumerant.value {
                notes.push(format!(
                    "{kind} {}: conflicting values {} and {}, kept the first",
                    enumerant.name, existing.value, enumerant.value
                ));
            }
            continue;
        }
        if let Some(existing) = kept.iter_mut().find(|entry| entry.value == enumerant.value) {
            existing.aliases.push(enumerant.name);
            existing.aliases.extend(enumerant.aliases);
            existing.aliases.sort();
            existing.aliases.dedup();
            continue;
        }
        kept.push(enumerant);
    }
    kept
}

/// The extension's own name: the one under `Name Strings` if that section is
/// present, else the first `SPV_` token anywhere.
///
/// Preferring the section matters because a spec's Dependencies prose often
/// names *other* extensions ("this is a promotion of SPV_OTHER_widget"),
/// and those appear early enough to win a naive first-match.
fn find_name(text: &str, re: &Patterns) -> Option<String> {
    let section = text
        .split("Name Strings")
        .nth(1)
        .and_then(|rest| rest.split("\n\n\n").next());
    let haystack = section.unwrap_or(text);
    re.name_string
        .find(haystack)
        .or_else(|| re.name_string.find(text))
        .map(|m| m.as_str().to_owned())
}

/// `(header row, body rows)` for every `|====` delimited table.
fn tables<'a>(lines: &[&'a str], re: &Patterns) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !re.delimiter.is_match(lines[index]) {
            index += 1;
            continue;
        }
        index += 1;
        let start = index;
        while index < lines.len() && !re.delimiter.is_match(lines[index]) {
            index += 1;
        }
        if let Some((header, body)) = lines[start..index].split_first() {
            found.push((*header, body.to_vec()));
        }
        index += 1;
    }
    found
}

/// Parse `| <value> | *<Name>* … | <capabilities>` rows, which may span lines.
fn enum_rows(body: &[&str], re: &Patterns) -> Vec<Enumerant> {
    let starts: Vec<usize> = (0..body.len())
        .filter(|&i| re.enum_row.is_match(body[i]))
        .collect();
    let mut rows = Vec::with_capacity(starts.len());
    for (position, &start) in starts.iter().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(body.len());
        let caps = re.enum_row.captures(body[start]).expect("just matched");
        let Ok(value) = caps[1].parse() else { continue };
        rows.push(Enumerant {
            name: caps[2].to_owned(),
            aliases: Vec::new(),
            value,
            requires: last_cell_names(&body[start + 1..end], &caps[3], re),
        });
    }
    rows
}

/// Read the last cell of a row, which holds the required or enabling
/// capabilities.
///
/// A row either ends on its own line (`| 0 | *WidgetVND* |`) or carries a
/// description across several lines before the final cell. Only lines that *open
/// a cell* are considered, so bold names inside the prose of a description are
/// not mistaken for capabilities.
fn last_cell_names(continuation: &[&str], remainder: &str, re: &Patterns) -> Vec<String> {
    let cell = match continuation.iter().rposition(|line| line.starts_with('|')) {
        Some(last) => continuation[last..]
            .join("\n")
            .trim_start_matches('|')
            .to_owned(),
        None => remainder
            .split_once('|')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default(),
    };
    re.bold
        .captures_iter(&cell)
        .map(|caps| caps[1].to_owned())
        .collect()
}

/// Pair every `[[OpName]]` anchor with the encoding row of the table defining it.
fn instructions(lines: &[&str], re: &Patterns, notes: &mut Vec<String>) -> Vec<SpvOpcode> {
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(caps) = re.anchor.captures(line) else {
            continue;
        };
        let name = caps[1].to_owned();
        let end = (index..lines.len())
            .find(|&i| re.delimiter.is_match(lines[i]))
            .unwrap_or(lines.len());

        let mut opcode = None;
        let mut capabilities = Vec::new();
        for cursor in index..end {
            if opcode.is_none() {
                if let Some(caps) = re.encoding.captures(lines[cursor]) {
                    opcode = caps[1]
                        .parse::<usize>()
                        .ok()
                        .zip(caps[2].parse::<i64>().ok())
                        .map(|(words, value)| (words, value, lines[cursor].contains("+ variable")));
                    continue;
                }
            }
            if re.capability_cell.is_match(lines[cursor]) {
                capabilities = bold_until_next_cell(lines, cursor + 1, end, re);
            }
        }

        match opcode {
            Some((min_word_count, value, variable_word_count)) => found.push(SpvOpcode {
                class: if name.starts_with("OpType") {
                    "Type-Declaration".to_owned()
                } else {
                    "Unknown".to_owned()
                },
                name,
                aliases: Vec::new(),
                value,
                operands: Vec::new(),
                capabilities,
                encoding: SpvEncoding {
                    min_word_count,
                    variable_word_count,
                    incompatibility: Some(
                        "prose specification does not provide an operand schema".to_owned(),
                    ),
                    ..Default::default()
                },
                meta: Default::default(),
            }),
            // Reported, not defaulted: an invented opcode compiles and then
            // mis-encodes every module that uses the instruction.
            None => notes.push(format!(
                "{name}: no encoding row found near its anchor, so it has no opcode — add it by hand"
            )),
        }
    }
    found
}

fn dedupe_opcodes(opcodes: Vec<SpvOpcode>, notes: &mut Vec<String>) -> Vec<SpvOpcode> {
    let mut kept: Vec<SpvOpcode> = Vec::new();
    for opcode in opcodes {
        if let Some(existing) = kept.iter().find(|entry| entry.name == opcode.name) {
            if existing.value != opcode.value {
                notes.push(format!(
                    "instruction {}: conflicting values {} and {}, kept the first",
                    opcode.name, existing.value, opcode.value
                ));
            }
            continue;
        }
        if let Some(existing) = kept.iter_mut().find(|entry| entry.value == opcode.value) {
            existing.aliases.push(opcode.name);
            existing.aliases.extend(opcode.aliases);
            existing.aliases.sort();
            existing.aliases.dedup();
            continue;
        }
        kept.push(opcode);
    }
    kept
}

/// Bold names below a `Capability:` cell, stopping at the next cell.
fn bold_until_next_cell(lines: &[&str], from: usize, end: usize, re: &Patterns) -> Vec<String> {
    let mut names = Vec::new();
    for line in &lines[from..end] {
        if re.row_start.is_match(line) {
            break;
        }
        names.extend(re.bold.captures_iter(line).map(|caps| caps[1].to_owned()));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[[kind]]
name = "Capability"
aliases = ["capability", "capabilities"]
[kind.meta]
label = "A"

[[kind]]
name = "BuiltIn"
aliases = ["builtin", "built-in"]
[kind.meta]
label = "B"
qualified = true
"#;

    fn table() -> KindTable {
        KindTable::parse(TABLE).unwrap()
    }

    /// Includes the operand-less encoding row that ends at the opcode.
    const SPEC: &str = r#"
Name Strings
------------

SPV_TEST_widget

Dependencies
------------

This is a promotion of SPV_OTHER_widget.

Modify the Capability section, adding rows to the Capability table:

--
[options="header"]
|====
2+^| Capability ^| Implicitly Declares
| 6141 | *WidgetVND*
|
|====
--

Add to the instruction section:

[cols="1,1,3*3",width="100%"]
|=====
4+|[[OpWidgetArriveVND]]*OpWidgetArriveVND* +
 +
Prose describing the instruction.
If _Semantics_ is not *None*, this also serves as a memory barrier. +
1+|Capability: +
*WidgetVND*
1+| 4 | 6142
| _Scope <id>_ +
_Execution_
|=====
"#;

    #[test]
    fn reads_the_capability_table() {
        let (plane, _) = extract(SPEC, None, &table()).unwrap();
        let caps = plane.kind("Capability");
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].name, "WidgetVND");
        assert_eq!(caps[0].value, 6141);
        // The "Implicitly Declares" cell is empty in this spec.
        assert!(caps[0].requires.is_empty());
    }

    #[test]
    fn reads_an_instruction_whose_row_ends_at_the_opcode() {
        // The regression this parser was written for: `1+| 4 | 6142` has no
        // trailing separator, and requiring one drops the instruction silently.
        let (plane, _) = extract(SPEC, None, &table()).unwrap();
        assert_eq!(plane.operations.len(), 1);
        assert_eq!(plane.operations[0].name, "OpWidgetArriveVND");
        assert_eq!(plane.operations[0].value, 6142);
        assert_eq!(plane.operations[0].capabilities, ["WidgetVND"]);
    }

    #[test]
    fn prose_optype_names_are_kept_separate_from_operations() {
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *WidgetVND*
|
|====
|=====
2+|[[OpTypeWidgetVND]]*OpTypeWidgetVND*
1+| 2 | 7000
|=====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_widget"), &table()).unwrap();
        assert_eq!(plane.types.len(), 1);
        assert_eq!(plane.types[0].name, "OpTypeWidgetVND");
        assert!(plane.operations.is_empty());
        assert_eq!(plane.types[0].encoding.min_word_count, 2);
        assert!(!plane.types[0].encoding.literal_indices_known);
    }

    #[test]
    fn prose_names_with_one_opcode_become_an_alias_family() {
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *WidgetVND*
|
|====
|=====
2+|[[OpWidgetVND]]*OpWidgetVND*
1+| 2 | 7000
|=====
|=====
2+|[[OpWidgetAliasVND]]*OpWidgetAliasVND*
1+| 2 | 7000
|=====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_widget"), &table()).unwrap();
        assert_eq!(plane.operations.len(), 1);
        assert_eq!(plane.operations[0].name, "OpWidgetVND");
        assert_eq!(plane.operations[0].aliases, ["OpWidgetAliasVND"]);
    }

    #[test]
    fn prefers_the_name_strings_section_over_prose() {
        // The Dependencies prose names SPV_OTHER_widget; picking the
        // first SPV_ token in the file would take the wrong one.
        let (plane, _) = extract(SPEC, None, &table()).unwrap();
        assert_eq!(plane.name, "SPV_TEST_widget");
    }

    #[test]
    fn an_explicit_name_wins_over_the_document() {
        let (plane, _) = extract(SPEC, Some("SPV_TEST_override"), &table()).unwrap();
        assert_eq!(plane.name, "SPV_TEST_override");
    }

    #[test]
    fn bold_names_in_a_description_are_not_read_as_capabilities() {
        // The description mentions *None*; only the final cell counts.
        let (plane, _) = extract(SPEC, None, &table()).unwrap();
        assert!(!plane.operations[0].capabilities.iter().any(|c| c == "None"));
    }

    #[test]
    fn a_single_line_row_reads_its_last_cell() {
        let spec = r#"
|====
2+^| BuiltIn ^| Enabling Capabilities
| 5 | *WidgetIdVND* | *WidgetVND*
|====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        let builtins = plane.kinds.iter().find(|k| k.name == "BuiltIn").unwrap();
        assert_eq!(builtins.enumerants[0].requires, ["WidgetVND"]);
    }

    #[test]
    fn rows_for_one_kind_across_two_tables_become_one_group() {
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *AEXT*
|
|====
|====
^| Capability ^| Implicitly Declares
| 2 | *BEXT*
|
|====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        assert_eq!(plane.kinds.len(), 1, "one Capability group, not two");
        assert_eq!(plane.kind("Capability").len(), 2);
    }

    #[test]
    fn a_repeated_token_is_deduped_and_a_clash_is_reported() {
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *AEXT*
|
| 9 | *AEXT*
|
|====
"#;
        let (plane, notes) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        assert_eq!(plane.kind("Capability").len(), 1);
        assert_eq!(plane.kind("Capability")[0].value, 1);
        assert!(notes.iter().any(|n| n.contains("conflicting values")));
    }

    #[test]
    fn enumerant_names_with_one_value_become_aliases() {
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *AEXT*
|
| 1 | *AliasAEXT*
|
|====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        assert_eq!(plane.kind("Capability").len(), 1);
        assert_eq!(plane.kind("Capability")[0].aliases, ["AliasAEXT"]);
    }

    #[test]
    fn an_instruction_without_an_encoding_row_is_reported_not_defaulted() {
        // Paired with a readable capability table, so extraction succeeds and
        // the note is what carries the problem.
        let spec = r#"
|====
^| Capability ^| Implicitly Declares
| 1 | *AEXT*
|
|====
|=====
4+|[[OpMysteryEXT]]*OpMysteryEXT* +
Some prose with no encoding row.
|=====
"#;
        let (plane, notes) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        assert!(
            plane.operations.is_empty(),
            "an opcode must never be invented"
        );
        assert!(
            notes.iter().any(|n| n.contains("OpMysteryEXT")),
            "got: {notes:?}"
        );
    }

    #[test]
    fn an_unclassifiable_table_is_skipped_not_guessed() {
        let spec = r#"
|====
| Last Modified Date | 2025-12-18
| Revision           | 1
|====
|====
^| Capability ^| Implicitly Declares
| 1 | *AEXT*
|
|====
"#;
        let (plane, _) = extract(spec, Some("SPV_TEST_x"), &table()).unwrap();
        // The version table has no recognisable header, so nothing from it
        // becomes an enumerant.
        assert_eq!(plane.kind("Capability").len(), 1);
    }

    #[test]
    fn a_spec_with_nothing_recognisable_says_what_to_do() {
        let err = extract("Just prose.\n", Some("SPV_TEST_x"), &table())
            .unwrap_err()
            .to_string();
        assert!(err.contains("by hand"), "got: {err}");
    }

    #[test]
    fn a_missing_name_is_an_error() {
        let spec = "|====\n^| Capability ^| x\n| 1 | *AEXT*\n|\n|====\n";
        let err = extract(spec, None, &table()).unwrap_err().to_string();
        assert!(err.contains("pass the extension name"), "got: {err}");
    }
}
