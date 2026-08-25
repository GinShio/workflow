//! The operand-kind table: which SPIR-V operand kinds to extract.
//!
//! The specification contributes the kind names. A private catalogue may attach
//! arbitrary metadata for its templates; the engine copies that data without
//! assigning meaning to any key.
//!
//! The `aliases` are the exception that proves the split: they exist solely to
//! recognise a hand-written table header, so they are a reading concern. They live
//! here anyway because they are per-kind data, and splitting one small table
//! across two files to honour a taxonomy would help nobody.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One operand kind, as a target tree understands it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kind {
    /// The specification's spelling, which is also how the grammar and the prose
    /// tables identify the kind.
    pub name: String,
    /// Lower-cased spellings seen in the prose spec's table headers.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Opaque values copied into the descriptor as `kind.meta.*`.
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindTable {
    #[serde(default, rename = "kind")]
    pub kinds: Vec<Kind>,
}

impl KindTable {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Load `<config root>/kinds.toml`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("kinds.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn by_name(&self, name: &str) -> Option<&Kind> {
        self.kinds.iter().find(|kind| kind.name == name)
    }

    /// Match an asciidoc table header to a kind.
    ///
    /// Anchored at the start of the normalised header so that an unrelated table
    /// carrying a trailing `Enabling Capabilities` column is not mistaken for a
    /// capability table.
    pub fn by_header(&self, header: &str) -> Option<&Kind> {
        let normalised = normalise_header(header);
        self.kinds.iter().find(|kind| {
            kind.aliases
                .iter()
                .any(|alias| normalised.starts_with(alias))
        })
    }
}

/// Strip a header row down to comparable words: `2+^| Capability ^| Implicitly
/// Declares` becomes `capability implicitly declares`.
fn normalise_header(header: &str) -> String {
    let cleaned: String = header
        .chars()
        .map(|c| if "|+^*".contains(c) { ' ' } else { c })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    // A leading column-span count (`2+`) survives as a bare number.
    let words = match words.split_first() {
        Some((first, rest)) if first.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => &words[..],
    };
    words.join(" ").to_lowercase()
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
qualified = false

[[kind]]
name = "BuiltIn"
aliases = ["builtin", "builtins", "built-in", "built-ins"]
[kind.meta]
label = "B"
qualified = true

[[kind]]
name = "ExecutionMode"
aliases = ["execution mode", "execution modes"]
[kind.meta]
label = "C"
"#;

    fn table() -> KindTable {
        KindTable::parse(TABLE).unwrap()
    }

    #[test]
    fn metadata_comes_out_of_the_file_without_engine_knowledge() {
        let table = table();
        let meta = &table.by_name("BuiltIn").unwrap().meta;
        assert_eq!(meta["label"].as_str(), Some("B"));
        assert_eq!(meta["qualified"].as_bool(), Some(true));
    }

    #[test]
    fn recognises_a_spec_table_header() {
        let table = table();
        let header = "2+^| Capability ^| Implicitly Declares";
        assert_eq!(table.by_header(header).unwrap().name, "Capability");
    }

    #[test]
    fn a_multi_word_alias_matches() {
        let table = table();
        assert_eq!(
            table
                .by_header("| Execution Mode | Enabling Capabilities")
                .unwrap()
                .name,
            "ExecutionMode"
        );
    }

    #[test]
    fn a_trailing_capability_column_does_not_hijack_the_table() {
        // This is a BuiltIn table that happens to list enabling capabilities;
        // anchoring at the start is what keeps it from reading as a Capability
        // table.
        let table = table();
        assert_eq!(
            table
                .by_header("2+^| BuiltIn ^| Enabling Capabilities")
                .unwrap()
                .name,
            "BuiltIn"
        );
    }

    #[test]
    fn an_unrelated_header_matches_nothing() {
        assert!(table().by_header("| Instruction | Operands").is_none());
    }

    #[test]
    fn a_misspelled_field_is_rejected() {
        let err = KindTable::parse("[[kind]]\nname='X'\nunknown_field=true\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown_field"), "got: {err}");
    }
}
