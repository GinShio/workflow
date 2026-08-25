//! The site catalogue: one TOML file per target tree, describing every place an
//! extension has to be registered.
//!
//! This is the configurable half of the tool. Ingest knows the specification
//! schemas, which are fixed and few; the catalogue knows a *tree's* conventions,
//! which drift with the tree and differ between trees. Keeping the second half as
//! data means following someone else's refactor is a one-line pattern edit, and
//! adding another tree is a new file rather than a new code path.
//!
//! ## What a rule says
//!
//! ```toml
//! [[rule]]
//! what   = "{{ kind.name }} entries"              # label, for the report
//! path   = "data/items.txt"
//! repeat = { over = "spv.kinds", as = "kind" }    # one edit per element
//! scope  = ['^section {{ kind.name }}$']
//! before = '^end$'
//! body   = """
//! {%- for e in kind.enumerants %}
//! {{ e.name }} = {{ e.value }}
//! {%- endfor %}
//! """
//! ```
//!
//! Every string is a template. `body` is inserted **verbatim** as rendered — no
//! newline is added or trimmed — because the sites disagree about it: a line list
//! needs a trailing newline, while an entry spliced into a JSON array has to
//! arrive as `,\n    { … }` with none. Jinja's `{%- -%}` already gives exact
//! control, so a normalisation step here would only fight the author.
//!
//! ## Two iteration axes, deliberately different words
//!
//! `repeat` fans a rule out into **several edits**, each with its own anchor.
//! Iterating *within* one edit is a `{% for %}` in the body. Anchors are
//! placement and Jinja cannot reach them, which is why the outer axis lives here.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::model::Plane;

/// How this tool names its config directory, following the same env -> XDG ->
/// HOME search every other wits subsystem uses.
pub const CONFIG_ROOT: wits_util::config::Root<'static> = wits_util::config::Root {
    env: "WITS_SCAFFOLD_CONFIG",
    xdg: "wits/scaffold",
    home: ".config/wits/scaffold",
};

/// One target tree's catalogue.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub target: TargetSpec,
    /// Applied in file order; several rules touching one file compose.
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    /// What `--target` selects this catalogue by.
    pub name: String,
    /// Where the tree is checked out, as a template with `~` expansion. Every
    /// rule's `path` is relative to it. See [`crate::root`] for why it is a
    /// template rather than a plain path.
    pub root: String,
    /// Which descriptor plane this tree consumes. A descriptor lacking it means
    /// the target is skipped, not partly applied.
    pub plane: Plane,
    /// Values that surround a rule by default. Each is rendered as a template;
    /// empty values are ignored.
    #[serde(default)]
    pub wrap: Vec<String>,
    /// How a non-empty wrapper value is written around a rule body.
    #[serde(default)]
    pub wrapper: Option<WrapperSpec>,
    /// Free-form defaults this tree's templates read as `var.<name>`. They live
    /// here, not as CLI flags, because they are facts about one tree; `--var`
    /// overrides one for a run. Each value is itself a template.
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
}

/// How a tree surrounds text with one wrapper value.
///
/// `open` is rendered outermost first and `close` in reverse order. They receive
/// the normal rule context plus the current value as `{{ item }}`.
/// `open_pattern` optionally identifies existing opener lines that belong to the
/// entry below them; sorted insertion moves above such lines.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrapperSpec {
    pub open: String,
    pub close: String,
    #[serde(default)]
    pub open_pattern: Option<String>,
}

/// Fan a rule out into one edit per element of a context collection.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repeat {
    /// Dotted path into the render context, e.g. `spv.kinds`.
    pub over: String,
    /// The name each element is bound to while rendering this rule.
    #[serde(rename = "as")]
    pub binding: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub what: String,
    pub path: String,
    pub body: String,

    // --- Anchor. Exactly one shape; `validate` rejects the rest. ---
    /// Append at end of file.
    #[serde(default)]
    pub eof: bool,
    /// Insert before the first match; with `scope`, the block's terminator.
    #[serde(default)]
    pub before: Option<String>,
    /// Insert after the last match of a repeated block.
    #[serde(default)]
    pub after_last: Option<String>,
    /// Successively nested block openers.
    #[serde(default)]
    pub scope: Vec<String>,
    /// Sorted insertion; capture group 1 is an entry's sort key.
    #[serde(default)]
    pub sorted: Option<String>,
    /// Capture group 1 of a key is its section; only keys in the new entry's own
    /// section are compared against.
    #[serde(default)]
    pub section: Option<String>,
    /// Create the file. `body` is then the whole content.
    #[serde(default)]
    pub create: bool,

    /// Override the target's default wrappers. An empty list means no wrapper.
    #[serde(default)]
    pub wrap: Option<Vec<String>>,
    #[serde(default)]
    pub repeat: Option<Repeat>,

    /// Apply this rule only when the template renders to something truthy.
    ///
    /// This decides whether the rule exists at all; wrapping only changes the
    /// text of a rule that already exists.
    #[serde(default)]
    pub when: Option<String>,
}

impl RuleSpec {
    /// Every template this rule will render, `when` excluded.
    ///
    /// `when` is left out because it decides whether the rest exists at all, so it
    /// is checked on its own beforehand — see [`crate::plan`]. Anything added to
    /// this struct that a rule renders belongs in this list too, or a `var.*` it
    /// reads would go unchecked and render as a hole.
    pub fn templates(&self) -> Vec<&str> {
        let mut all = vec![self.what.as_str(), self.path.as_str(), self.body.as_str()];
        all.extend(self.scope.iter().map(String::as_str));
        all.extend(
            [&self.before, &self.after_last, &self.sorted, &self.section]
                .into_iter()
                .flatten()
                .map(String::as_str),
        );
        if let Some(wrappers) = &self.wrap {
            all.extend(wrappers.iter().map(String::as_str));
        }
        all
    }

    /// Which anchor keys this rule set, for error messages.
    fn anchor_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.eof {
            keys.push("eof");
        }
        if self.create {
            keys.push("create");
        }
        if self.after_last.is_some() {
            keys.push("after_last");
        }
        if self.sorted.is_some() {
            keys.push("sorted");
        }
        if self.before.is_some() && self.sorted.is_none() {
            keys.push("before");
        }
        keys
    }

    /// Reject a rule that names no position or more than one.
    ///
    /// Worth failing on rather than picking a winner: the shapes differ only in
    /// which keys are present, so a leftover key from an edit is indistinguishable
    /// from a deliberate choice, and guessing would land the edit somewhere
    /// plausible but wrong.
    fn validate(&self) -> Result<()> {
        // Structural pairings first, then the count. A rule missing the partner
        // of a key it did set trips both checks, and the pairing is the more
        // specific diagnosis — `scope` without `before` is a forgotten
        // terminator, not an author who named no position at all.
        if self.create && !self.scope.is_empty() {
            bail!(
                "rule '{}': a created file has no block to anchor within",
                self.what
            );
        }
        if !self.scope.is_empty() && self.before.is_none() {
            bail!(
                "rule '{}': 'scope' opens a block, so it needs 'before' to close it",
                self.what
            );
        }
        if self.section.is_some() && self.sorted.is_none() {
            bail!(
                "rule '{}': 'section' partitions a sort key, so it needs 'sorted'",
                self.what
            );
        }

        let keys = self.anchor_keys();
        match keys.len() {
            1 => Ok(()),
            0 => bail!(
                "rule '{}' names no position: set one of eof, before, after_last, sorted, create",
                self.what
            ),
            _ => bail!(
                "rule '{}' names {} positions ({}); exactly one is allowed",
                self.what,
                keys.len(),
                keys.join(", ")
            ),
        }
    }
}

impl Catalog {
    /// Parse and check one catalogue. Every rule is validated up front so a
    /// malformed one is reported before any tree is touched.
    pub fn parse(text: &str) -> Result<Self> {
        let catalog: Catalog = toml::from_str(text)?;
        for rule in &catalog.rules {
            rule.validate()?;
        }
        Ok(catalog)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read catalogue {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in catalogue {}", path.display()))
    }
}

/// Every catalogue under `<config root>/target/`, keyed by its target name.
///
/// Discovery is by directory rather than a registry file: dropping in a fourth
/// tree's catalogue should not also mean editing a list of catalogues.
pub fn load_all(root: &Path) -> Result<Vec<(PathBuf, Catalog)>> {
    let dir = root.join("target");
    let mut found = Vec::new();
    for path in wits_util::config::discover_toml(&dir)
        .with_context(|| format!("cannot scan {}", dir.display()))?
    {
        found.push((path.clone(), Catalog::load(&path)?));
    }
    if found.is_empty() {
        bail!(
            "no target catalogues found under {} — this tool is all configuration, so it needs at \
             least one",
            dir.display()
        );
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r##"
[target]
name = "alpha"
root = "/fixtures/alpha"
plane = "spv"
wrap = ["{{ var.frame }}"]

[target.wrapper]
open = "BEGIN {{ item }}\n"
close = "END {{ item }}\n"
open_pattern = "^BEGIN "

[target.vars]
frame = "FRAME"

[[rule]]
what = "name string"
path = "list.txt"
eof = true
body = "{{ spv.name }}\n"
"##;

    fn rule_of(extra: &str) -> Result<Catalog> {
        Catalog::parse(&format!(
            "[target]\nname=\"t\"\nroot=\"/t\"\nplane=\"spv\"\n\n[[rule]]\nwhat=\"w\"\npath=\"f\"\nbody=\"b\"\n{extra}"
        ))
    }

    #[test]
    fn parses_a_minimal_catalogue() {
        let catalog = Catalog::parse(MINIMAL).unwrap();
        assert_eq!(catalog.target.name, "alpha");
        assert_eq!(catalog.target.root, "/fixtures/alpha");
        assert_eq!(catalog.target.plane, Plane::Spv);
        let wrapper = catalog.target.wrapper.as_ref().expect("a wrapper spelling");
        assert_eq!(wrapper.close, "END {{ item }}\n");
        assert_eq!(catalog.target.wrap, ["{{ var.frame }}"]);
        assert_eq!(catalog.rules.len(), 1);
        assert!(catalog.rules[0].eof);
    }

    #[test]
    fn a_misspelled_key_is_rejected_not_ignored() {
        // The whole point of deny_unknown_fields: `sortd` would otherwise leave
        // the rule with no anchor at all and no complaint.
        let err = rule_of("sortd = 'x'").unwrap_err().to_string();
        assert!(err.contains("sortd"), "got: {err}");
    }

    #[test]
    fn a_rule_with_no_position_is_rejected() {
        let err = rule_of("").unwrap_err().to_string();
        assert!(err.contains("names no position"), "got: {err}");
    }

    #[test]
    fn two_positions_are_rejected_rather_than_ranked() {
        let err = rule_of("eof = true\nafter_last = 'x'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("names 2 positions"), "got: {err}");
    }

    #[test]
    fn a_sorted_list_need_not_sit_inside_a_block() {
        // Some of these lists are a whole file of sorted entries with nothing
        // enclosing them, so `sorted` alone is a complete anchor.
        let catalog = rule_of("sorted = '^([A-Z_]+)\\s'").unwrap();
        assert!(catalog.rules[0].sorted.is_some());
        assert!(catalog.rules[0].scope.is_empty());
    }

    #[test]
    fn section_without_sorted_is_rejected() {
        let err = rule_of("eof = true\nsection = '^(x)'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs 'sorted'"), "got: {err}");
    }

    #[test]
    fn scope_without_a_terminator_is_rejected() {
        let err = rule_of("scope = ['^open$']").unwrap_err().to_string();
        assert!(err.contains("needs 'before'"), "got: {err}");
    }

    #[test]
    fn a_created_file_cannot_also_be_scoped() {
        let err = rule_of("create = true\nscope = ['^open$']\nbefore = '^close$'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no block to anchor"), "got: {err}");
    }

    #[test]
    fn wrap_accepts_both_an_empty_override_and_a_nested_list() {
        let off = rule_of("eof = true\nwrap = []").unwrap();
        assert_eq!(off.rules[0].wrap.as_deref(), Some([].as_slice()));

        let nested = rule_of("eof = true\nwrap = ['A', 'B']").unwrap();
        assert_eq!(
            nested.rules[0].wrap.as_deref(),
            Some(["A".to_owned(), "B".to_owned()].as_slice())
        );
    }

    #[test]
    fn repeat_binds_a_name_for_the_body_to_use() {
        let catalog = rule_of("eof = true\nrepeat = { over = 'spv.kinds', as = 'kind' }").unwrap();
        let repeat = catalog.rules[0].repeat.as_ref().unwrap();
        assert_eq!(repeat.over, "spv.kinds");
        assert_eq!(repeat.binding, "kind");
    }

    #[test]
    fn a_sorted_rule_that_is_fully_specified_passes() {
        let catalog = rule_of(
            "scope = ['^enum Id$']\nbefore = '^\\};$'\nsorted = '^\\s*([A-Z_]+),'\nsection = '^([A-Z]+)_'",
        )
        .unwrap();
        assert!(catalog.rules[0].sorted.is_some());
    }

    #[test]
    fn an_empty_target_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_all(dir.path()).is_err());
    }
}
