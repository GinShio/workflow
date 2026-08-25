//! Reading the specifications.
//!
//! This half of the tool is **code, not configuration**, and the asymmetry with
//! the output half is deliberate. The set of sources is closed and fixed by the
//! bodies that publish them: a SPIR-V grammar, a Vulkan registry, and the
//! per-extension prose spec. No fourth source exists that anyone would plug in, so
//! a selector language here would be a parameter serving no caller. The set of
//! *targets*, by contrast, is open and each one's conventions drift with its own
//! refactors, which is why the catalogue is data.
//!
//! The one piece of reading knowledge that *is* configuration lives in
//! [`crate::kinds`]: how a tree spells each operand kind is its own convention.
//!
//! Two of the three sources are machine-generated and exact; the prose spec is
//! hand-written and only ever parsed heuristically. That is the whole reason the
//! descriptor exists as a reviewable intermediate — see [`crate::model`].

pub mod spv_grammar;
pub mod spv_spec;
pub mod vk_registry;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Where the specification files live on this machine.
///
/// Files, not repositories: these registries are published documents rather than
/// anything this machine develops, and one of the checkouts lives in a tmpfs that
/// does not survive a reboot. Naming the exact files keeps a moved checkout to a
/// one-line fix, and lets a stale copy be pointed at deliberately.
///
/// One name per source, spelled the same way here, in the module that reads it,
/// and on the command line — so a source has one name rather than three.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    /// The SPIR-V core grammar, `spirv.core.grammar.json`.
    #[serde(default)]
    pub spv_grammar: Option<PathBuf>,
    /// The directory holding the per-extension SPIR-V specifications, searched
    /// recursively because the registry files them in per-vendor subdirectories.
    #[serde(default)]
    pub spv_spec: Option<PathBuf>,
    /// The Vulkan registry, `vk.xml`.
    #[serde(default)]
    pub vk_registry: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvFile {
    #[serde(default)]
    source: Sources,
}

impl Sources {
    /// Load `<config root>/env.toml`. A missing file is not an error: every
    /// path can also be given on the command line.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("env.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let parsed: EnvFile =
            toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
        Ok(parsed.source)
    }

    /// Locate one extension's specification under the spec directory.
    pub fn find_spec(&self, name: &str) -> Result<PathBuf> {
        let root = self
            .spv_spec
            .as_ref()
            .context("no SPIR-V spec directory configured (env.toml: source.spv_spec)")?;
        let wanted = format!("{name}.asciidoc");
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().is_some_and(|f| f == wanted.as_str()) {
                    return Ok(path);
                }
            }
        }
        bail!("no {wanted} under {}", root.display())
    }
}

/// A value that the grammar spells as an integer for value enums and a hex
/// string for bit enums.
pub(crate) fn number(value: &serde_json::Value) -> Result<i64> {
    match value {
        serde_json::Value::Number(n) => n
            .as_i64()
            .with_context(|| format!("value {n} is not an integer")),
        serde_json::Value::String(s) => {
            let text = s.trim();
            let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => i64::from_str_radix(hex, 16),
                None => text.parse(),
            };
            parsed.with_context(|| format!("value '{text}' is not a number"))
        }
        other => bail!("value {other} is neither a number nor a string"),
    }
}

/// Keep the first definition of each name, reporting any disagreement.
///
/// Specs repeat a token in a prose table and again in a summary appendix, and
/// the grammar lists aliases; the duplicate is normally identical, and when it
/// is not the user has to know rather than get an arbitrary winner.
pub(crate) fn dedupe<T>(
    entries: Vec<T>,
    name_of: impl Fn(&T) -> String,
    value_of: impl Fn(&T) -> i64,
    what: &str,
    notes: &mut Vec<String>,
) -> Vec<T> {
    let mut seen: BTreeMap<String, i64> = BTreeMap::new();
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = name_of(&entry);
        let value = value_of(&entry);
        match seen.get(&name) {
            None => {
                seen.insert(name, value);
                kept.push(entry);
            }
            Some(&previous) if previous != value => notes.push(format!(
                "{what} {name}: conflicting values {previous} and {value}, kept the first"
            )),
            Some(_) => {}
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_parse_from_both_spellings() {
        assert_eq!(number(&serde_json::json!(5454)).unwrap(), 5454);
        assert_eq!(number(&serde_json::json!("0x0004")).unwrap(), 4);
        assert_eq!(number(&serde_json::json!("12")).unwrap(), 12);
        assert!(number(&serde_json::json!("nope")).is_err());
        assert!(number(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn dedupe_keeps_the_first_and_reports_a_clash() {
        let mut notes = Vec::new();
        let entries = vec![("A", 1), ("B", 2), ("A", 1), ("B", 9)];
        let kept = dedupe(
            entries,
            |e| e.0.to_owned(),
            |e| e.1,
            "capability",
            &mut notes,
        );
        assert_eq!(kept, vec![("A", 1), ("B", 2)]);
        assert_eq!(notes.len(), 1, "only the disagreement is worth a note");
        assert!(notes[0].contains("B: conflicting values 2 and 9"));
    }

    #[test]
    fn a_missing_env_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources::load(dir.path()).unwrap();
        assert!(sources.vk_registry.is_none());
    }

    #[test]
    fn env_file_reads_the_source_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("env.toml"),
            "[source]\nvk_registry = \"/r/vk.xml\"\nspv_grammar = \"/r/g.json\"\n",
        )
        .unwrap();
        let sources = Sources::load(dir.path()).unwrap();
        assert_eq!(sources.vk_registry.unwrap(), PathBuf::from("/r/vk.xml"));
        assert_eq!(sources.spv_grammar.unwrap(), PathBuf::from("/r/g.json"));
    }

    #[test]
    fn spec_lookup_recurses_into_vendor_directories() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("extensions/TEST");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(vendor.join("SPV_TEST_widget.asciidoc"), "").unwrap();
        let sources = Sources {
            spv_spec: Some(dir.path().join("extensions")),
            ..Default::default()
        };
        assert_eq!(
            sources.find_spec("SPV_TEST_widget").unwrap(),
            vendor.join("SPV_TEST_widget.asciidoc")
        );
        assert!(sources.find_spec("SPV_TEST_absent").is_err());
    }
}
