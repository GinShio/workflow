//! The data model: what a project *is*, parsed from TOML but not yet resolved.
//!
//! These types stay deliberately close to the file on disk. Templated fields
//! (`repos.*.build_dir`, an `[environment]` map, …) are kept as *raw* strings and
//! `toml::Value`s here; turning them into concrete paths and command lines is
//! [`super::resolve`]'s job, and only once a [`Profile`] is supplied. Keeping the
//! two apart is the whole reason a read-only `info` never has to run a build
//! planner.
//!
//! One thing is inferred rather than declared: a repo's *kind*. A path that is
//! nested under `repos.main` and carries its own `main_branch` is a submodule; a
//! nested path without one is a subtree; anything else is standalone. Declaring
//! it would just be a fourth thing to keep consistent with the other three.
//!
//! Two fields describe a repo whose checkout is *not* wholly this project's own.
//! [`RawRepo::from`] borrows another project's repo as this one, so a component
//! several projects consume is declared once where it lives; [`RawRepo::skip`]
//! names the paths this checkout never materialises, which is what makes the
//! borrow usable — the borrower's own copy of the component stays unmaterialised
//! rather than shadowing the one it borrowed.
//!
//! Quoted section names below are from `docs/reference/project-design.rst`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use crate::remote::Role;

/// A whole config file, parsed. Every section is optional so one file may carry
/// a project, toolchains, and an org at once ("One project per file; registries
/// merge"). Unknown keys are rejected so a typo like `[toolchian]` fails loudly
/// instead of being silently ignored.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawFile {
    pub project: Option<RawProject>,
    #[serde(default)]
    pub repos: BTreeMap<String, RawRepo>,
    pub org: Option<RawOrg>,
    #[serde(default)]
    pub toolchains: BTreeMap<String, RawToolchain>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawProject {
    pub org: Option<String>,
    pub focus: Option<String>,
    pub build_system: Option<BuildSystem>,
    pub toolchain: Option<String>,
    pub generator: Option<String>,
    #[serde(default)]
    pub default_presets: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub devenv: RawDevenvTable,
    #[serde(default)]
    pub extra_config_args: Vec<String>,
    #[serde(default)]
    pub extra_build_args: Vec<String>,
    #[serde(default)]
    pub extra_install_args: Vec<String>,
    #[serde(default)]
    pub presets: BTreeMap<String, RawPreset>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawRepo {
    /// On-disk location (root / standalone) or a subpath relative to
    /// `repos.main` (nested). Absent exactly when [`from`](Self::from) borrows
    /// the repo from another project, which supplies the path instead; the
    /// loader fills it in, so everything downstream sees a plain path.
    pub path: Option<String>,
    /// Borrow another project's repo as this one: `[<org>/]<project>[:<repo>]`,
    /// defaulting to that project's `repos.main`. What travels is the repo's
    /// *git identity* (path, main branch, remotes, hooks, branch strategy,
    /// worktree paths, `skip`); what stays local is how *this* build uses it
    /// (`anchor`, `source_dir`, `build_dir`, `install_dir`, `presets`). A borrow
    /// may not itself be borrowed.
    pub from: Option<String>,
    /// Which project this checkout is currently developed *as*, as a project
    /// reference (`[<org>/]<project>`), when several projects reference it. It is
    /// the **owner's** statement: the borrowers have no grounds to prefer between
    /// themselves. Path lookups then answer for that project's borrow of this
    /// repo instead of for its owner — see
    /// [`Workspace::developed_as`](super::workspace::Workspace::developed_as).
    pub developed_as: Option<String>,
    pub main_branch: Option<String>,
    pub anchor: Option<String>,
    pub branch_strategy: Option<String>,
    pub worktree_dir: Option<String>,
    /// The fixed initial checkout created after a bare clone. Required by the
    /// hybrid strategy; optional for worktree, which otherwise renders
    /// `worktree_dir` for `main_branch`. A relative value is resolved beside
    /// that rendered main worktree path.
    pub bootstrap_worktree_dir: Option<String>,
    /// Templated path of the build system's source (where the top-level
    /// `CMakeLists.txt`/`meson.build`/… lives) when it is not the checkout root.
    /// Resolved from the build repo; defaults to its resolved `workdir`.
    pub source_dir: Option<String>,
    /// Where the backend writes its build tree. Templated; a focus declaration
    /// overrides its anchor's default. Absent on both means there is nothing to
    /// build.
    pub build_dir: Option<String>,
    /// Install prefix. Templated; the focus-over-anchor precedence matches
    /// [`build_dir`](Self::build_dir).
    pub install_dir: Option<String>,
    /// Paths this checkout never materialises, as an ordered gitignore-style
    /// pattern list where `!` re-includes (see [`super::skip`]). Not templated:
    /// these are git patterns, and mixing `{{ }}` into `*`/`!` would only make
    /// them harder to read.
    #[serde(default)]
    pub skip: Vec<String>,
    #[serde(default)]
    pub remotes: RawRemotes,
    #[serde(default)]
    pub hooks: RawHooks,
    #[serde(default)]
    pub presets: BTreeMap<String, RawPreset>,
}

impl RawRepo {
    /// The fields a borrower may not declare. Two reasons, one rule: a
    /// [`from`](Self::from) borrow *supplies* the component's git identity, and a
    /// local override would quietly reintroduce the duplication the borrow
    /// removes; [`developed_as`](Self::developed_as) is the *owner's* answer about
    /// a checkout this entry does not own, so a borrower declaring it would only
    /// be writing a value nothing reads. Both are rejected by the loader rather
    /// than silently picked between or ignored.
    pub fn borrowed_field_conflicts(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.path.is_some() {
            out.push("path");
        }
        if self.main_branch.is_some() {
            out.push("main_branch");
        }
        if self.branch_strategy.is_some() {
            out.push("branch_strategy");
        }
        if self.worktree_dir.is_some() {
            out.push("worktree_dir");
        }
        if self.bootstrap_worktree_dir.is_some() {
            out.push("bootstrap_worktree_dir");
        }
        if !self.skip.is_empty() {
            out.push("skip");
        }
        if !self.remotes.is_empty() {
            out.push("remotes");
        }
        if self.hooks != RawHooks::default() {
            out.push("hooks");
        }
        if self.developed_as.is_some() {
            out.push("developed_as");
        }
        out
    }
}

/// A parsed [`RawRepo::from`] reference.
#[derive(Debug, PartialEq, Eq)]
pub struct BorrowRef<'a> {
    /// A project reference as [`super::workspace::Workspace::project`] takes it
    /// — a bare name or `org/name`.
    pub project: &'a str,
    /// Which of that project's repos to borrow; `main` unless spelled out.
    pub repo: &'a str,
}

/// Parse `[<org>/]<project>[:<repo>]`. The separators cannot collide: `/`
/// qualifies the org (and is left for the project lookup to interpret), `:`
/// selects the repo.
pub fn parse_borrow(spec: &str) -> Result<BorrowRef<'_>, String> {
    let (project, repo) = match spec.split_once(':') {
        Some((p, r)) => (p.trim(), r.trim()),
        None => (spec.trim(), "main"),
    };
    if project.is_empty() {
        return Err(format!("from '{spec}' names no project"));
    }
    if repo.is_empty() {
        return Err(format!("from '{spec}' has an empty repo after ':'"));
    }
    Ok(BorrowRef { project, repo })
}

/// A repo's lifecycle hooks: an optional command string per phase. A typed
/// struct rather than a free `phase -> command` map, so a mistyped phase like
/// `pre_updat` is a hard parse error (via `deny_unknown_fields`) instead of a
/// silently-ignored no-op. Each command is a template resolved against the
/// per-repo context when the phase runs (see `wits update`).
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RawHooks {
    /// Replaces the built-in clone (origin → checkout main → submodules); runs
    /// before the checkout exists, so in the current working directory.
    pub clone: Option<String>,
    /// After a fresh clone (or the `clone` override), in the checkout.
    pub post_clone: Option<String>,
    /// Before the update proper.
    pub pre_update: Option<String>,
    /// Replaces the built-in update (fetch/rebase/submodules).
    pub update: Option<String>,
    /// After the update.
    pub post_update: Option<String>,
}

/// A repo's remotes, keyed by the git remote name to create. Free-form: the two
/// privileged names carry a role implicitly, anything else is a plain extra
/// remote unless it declares a [`role`](RawRemote::role).
pub type RawRemotes = BTreeMap<String, RawRemote>;

/// One declared remote.
///
/// Written either as a bare URL (`origin = "git@…"`) or as a table when it needs
/// more than that — the same string-or-struct courtesy [`StringList`] extends to
/// `extends`. The common case, a URL under a conventional name, stays one line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RawRemote {
    pub url: String,
    /// Which role this remote holds. Omitted for the privileged names, whose name
    /// already says it; **required** for any other name that is to hold a role,
    /// since a name is the only signal a bare checkout has (see [`crate::remote`]).
    pub role: Option<Role>,
    /// Extra push URLs, so one push fans out to copies of the same repository.
    ///
    /// An attribute rather than a role: a role answers "which remote does X" and
    /// so has at most one holder, while any number of remotes may each fan out.
    /// Unrelated to `git clone --mirror`, which copies a whole ref space; these
    /// are only additional push destinations.
    pub mirrors: Vec<String>,
}

impl<'de> Deserialize<'de> for RawRemote {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The table form, carrying the same `deny_unknown_fields` strictness as
        // the rest of the file model so a mistyped `mirror` fails loudly.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Table {
            url: String,
            role: Option<Role>,
            #[serde(default)]
            mirrors: Vec<String>,
        }

        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawRemote;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a URL string or a remote table")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<RawRemote, E> {
                Ok(RawRemote {
                    url: s.to_owned(),
                    ..Default::default()
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<RawRemote, A::Error> {
                let t = Table::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(RawRemote {
                    url: t.url,
                    role: t.role,
                    mirrors: t.mirrors,
                })
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawPreset {
    #[serde(default)]
    pub extends: StringList,
    pub applies_when: Option<BTreeMap<String, toml::Value>>,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub devenv: RawDevenvTable,
    #[serde(default)]
    pub extra_config_args: Vec<String>,
    #[serde(default)]
    pub extra_build_args: Vec<String>,
    #[serde(default)]
    pub extra_install_args: Vec<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawToolchain {
    pub cc: Option<String>,
    pub cxx: Option<String>,
    pub rustc: Option<String>,
    pub ar: Option<String>,
    pub nm: Option<String>,
    pub ranlib: Option<String>,
    pub strip: Option<String>,
    pub linker: Option<String>,
    pub launcher: Option<String>,
    #[serde(default)]
    pub c_flags: Vec<String>,
    #[serde(default)]
    pub cxx_flags: Vec<String>,
    #[serde(default)]
    pub link_flags: Vec<String>,
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawOrg {
    pub name: String,
    #[serde(default)]
    pub presets: BTreeMap<String, RawPreset>,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub devenv: RawDevenvTable,
}

/// What a `devenv` entry does to its variable — Meson's `environment()`
/// vocabulary, since Meson's own developer environment is the base the
/// operations land on for a meson build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevenvOp {
    Set,
    Prepend,
    Append,
}

impl DevenvOp {
    /// The config spelling, which is also how a report names the operation.
    pub fn as_str(self) -> &'static str {
        match self {
            DevenvOp::Set => "set",
            DevenvOp::Prepend => "prepend",
            DevenvOp::Append => "append",
        }
    }
}

/// The separator a `prepend`/`append` joins with unless one is declared: the
/// path-list separator, as Meson's `environment()` defaults to.
pub const DEVENV_SEPARATOR: &str = ":";

/// One `devenv` entry as written. A bare scalar or list sets the variable; a
/// table names exactly one of `set`/`prepend`/`append`, with an optional
/// `separator` that joins a list and, for prepend/append, the existing value.
/// The values are templates, resolved by the pipeline like `environment`'s.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDevenv {
    pub op: DevenvOp,
    pub values: Vec<toml::Value>,
    pub separator: String,
}

impl RawDevenv {
    fn from_value(value: toml::Value) -> Result<Self, String> {
        let table = match value {
            toml::Value::Table(table) => table,
            bare => {
                return Ok(RawDevenv {
                    op: DevenvOp::Set,
                    values: devenv_parts(bare)?,
                    separator: DEVENV_SEPARATOR.to_owned(),
                })
            }
        };
        let mut operation = None;
        let mut separator = None;
        for (key, value) in table {
            let op = match key.as_str() {
                "separator" => {
                    let text = value.as_str().ok_or("separator must be a string")?;
                    separator = Some(text.to_owned());
                    continue;
                }
                "set" => DevenvOp::Set,
                "prepend" => DevenvOp::Prepend,
                "append" => DevenvOp::Append,
                other => {
                    return Err(format!(
                        "unknown key '{other}' (use one of set, prepend, append, and optionally separator)"
                    ))
                }
            };
            if operation.replace((op, value)).is_some() {
                return Err("names more than one of set, prepend, append".to_owned());
            }
        }
        let (op, value) = operation.ok_or("names none of set, prepend, append")?;
        Ok(RawDevenv {
            op,
            values: devenv_parts(value)?,
            separator: separator.unwrap_or_else(|| DEVENV_SEPARATOR.to_owned()),
        })
    }
}

/// A `devenv` value's parts: one scalar, or a list of them. A table has no
/// spelling as part of an environment variable.
fn devenv_parts(value: toml::Value) -> Result<Vec<toml::Value>, String> {
    let parts = match value {
        toml::Value::Array(items) => items,
        scalar => vec![scalar],
    };
    if parts
        .iter()
        .any(|part| matches!(part, toml::Value::Table(_) | toml::Value::Array(_)))
    {
        return Err("a value must be a scalar or a list of scalars".to_owned());
    }
    Ok(parts)
}

impl<'de> Deserialize<'de> for RawDevenv {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawDevenv::from_value(toml::Value::deserialize(d)?).map_err(de::Error::custom)
    }
}

/// A `devenv` table's entries, in key order.
///
/// A list rather than a map because a preset merged across org/project/repo
/// carries every level's operation on the same variable, in level order: the
/// operations accumulate, as Meson's do, rather than the nearest replacing the
/// rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawDevenvTable(pub Vec<(String, RawDevenv)>);

impl<'de> Deserialize<'de> for RawDevenvTable {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let entries = BTreeMap::<String, RawDevenv>::deserialize(d)?;
        Ok(RawDevenvTable(entries.into_iter().collect()))
    }
}

/// A field that may be written as a single string or a list of strings
/// (`extends = "base"` or `extends = ["a", "b"]`).
#[derive(Debug, Default, Clone)]
pub struct StringList(pub Vec<String>);

impl<'de> Deserialize<'de> for StringList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = StringList;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string or a list of strings")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<StringList, E> {
                Ok(StringList(vec![s.to_owned()]))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<StringList, A::Error> {
                let mut out = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    out.push(item);
                }
                Ok(StringList(out))
            }
        }
        d.deserialize_any(V)
    }
}

/// standalone / submodule / subtree — inferred, never declared (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Standalone,
    Submodule,
    Subtree,
}

impl Kind {
    /// A subtree has no git of its own — it lives inside its anchor's checkout.
    pub fn has_own_git(self) -> bool {
        !matches!(self, Kind::Subtree)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Standalone => "standalone",
            Kind::Submodule => "submodule",
            Kind::Subtree => "subtree",
        }
    }
}

/// Infer a repo's kind from its name, path, and whether it declares a main
/// branch. `repos.main` is always standalone; a nested (relative) path is a
/// submodule when it has its own `main_branch`, a subtree otherwise.
///
/// A borrowed repo resolves to the *absolute* path of the checkout it borrowed,
/// so it lands on `Standalone` — which is what it is from this project's side,
/// however it is nested in the project that owns it. That is the property the
/// rest of the tool needs: an external checkout, with its own git, that this
/// project's submodule refresh must keep its hands off.
pub fn infer_kind(name: &str, repo: &RawRepo) -> Kind {
    // Before the loader resolves a borrow there is no path to judge; the answer
    // is the same one the resolved form gives.
    let Some(path) = repo.path.as_deref() else {
        return Kind::Standalone;
    };
    if name == "main" || !is_nested(path) {
        Kind::Standalone
    } else if repo.main_branch.is_some() {
        Kind::Submodule
    } else {
        Kind::Subtree
    }
}

/// A path is "nested" (a subpath of `repos.main`) when it is relative — not
/// absolute and not `~`-rooted (shells usually expand `~`, but a quoted path
/// might reach us intact).
pub fn is_nested(path: &str) -> bool {
    !(path.starts_with('/') || path.starts_with('~') || std::path::Path::new(path).is_absolute())
}

/// The build systems wits knows how to drive. Declared once per project as
/// `build_system`; every variant maps to a backend in [`crate::build_system`],
/// so unlike a free string the mapping is total and an unknown value fails at
/// *parse* time (via the custom [`Deserialize`]) rather than deep inside a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    Cmake,
    Meson,
    Cargo,
}

impl BuildSystem {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildSystem::Cmake => "cmake",
            BuildSystem::Meson => "meson",
            BuildSystem::Cargo => "cargo",
        }
    }
}

impl std::str::FromStr for BuildSystem {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cmake" => Ok(BuildSystem::Cmake),
            "meson" => Ok(BuildSystem::Meson),
            "cargo" => Ok(BuildSystem::Cargo),
            other => Err(format!(
                "unknown build_system '{other}' (use cmake|meson|cargo)"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for BuildSystem {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchStrategy {
    #[default]
    InPlace,
    Worktree,
    Hybrid,
}

impl BranchStrategy {
    pub fn parse(s: Option<&str>) -> anyhow::Result<Self> {
        match s {
            None | Some("in-place") => Ok(BranchStrategy::InPlace),
            Some("worktree") => Ok(BranchStrategy::Worktree),
            Some("hybrid") => Ok(BranchStrategy::Hybrid),
            Some(other) => {
                anyhow::bail!("unknown branch_strategy '{other}' (use in-place|worktree|hybrid)")
            }
        }
    }

    /// The config spelling, the exact inverse of [`parse`](Self::parse) — so what
    /// a report prints is what a file may be written with.
    pub fn as_str(self) -> &'static str {
        match self {
            BranchStrategy::InPlace => "in-place",
            BranchStrategy::Worktree => "worktree",
            BranchStrategy::Hybrid => "hybrid",
        }
    }

    /// Worktree and hybrid repositories use a bare common repository plus
    /// linked working trees; in-place keeps a conventional clone.
    pub fn is_bare_backed(self) -> bool {
        matches!(self, BranchStrategy::Worktree | BranchStrategy::Hybrid)
    }
}

/// The axes that affect *resolution* (paths, identity). Built from CLI flags,
/// never from a file. Separated from `build::BuildOptions` on purpose: these
/// change what `build_dir`/repo `workdir` resolve to; those change only the
/// commands, and are the build action's own business ("The read/act split") —
/// the core neither defines nor reads them.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub build_type: Option<String>,
    pub toolchain: Option<String>,
    pub generator: Option<String>,
    pub branch: Option<String>,
    pub presets: Vec<String>,
    /// `--focus` override; falls back to `project.focus`, then `"main"`.
    pub focus: Option<String>,
    /// `--work-dir` override: use this checkout verbatim as the build base,
    /// bypassing the branch strategy's `worktree_dir`/in-place resolution. This
    /// is the seam that lets a checkout materialised *elsewhere* — a `review`
    /// worktree of an MR, say — be built through the project machinery without
    /// the two commands touching in code. `None` resolves the build repo's
    /// `workdir` as usual.
    pub work_dir: Option<PathBuf>,
    /// CLI-registered template variables, exposed as `{{spec.*}}` (from
    /// `--spec K=V`). Purely referenceable, never applied on its own: a template
    /// that mentions `{{spec.mr}}` *requires* the caller to supply it, so an
    /// out-of-band value (an MR number, a variant tag) enters resolution without
    /// being baked into the file model or guessed.
    pub specs: BTreeMap<String, String>,
}

/// A toolchain after selection: canonical fields plus verbatim pass-through
/// blocks. Backends translate the canonical fields into native form ("Single
/// source of truth for compilers, realised by the backend"); the pass-through
/// blocks are applied as-is.
#[derive(Debug, Clone, Default)]
pub struct Toolchain {
    pub name: String,
    pub cc: Option<String>,
    pub cxx: Option<String>,
    pub rustc: Option<String>,
    pub ar: Option<String>,
    pub nm: Option<String>,
    pub ranlib: Option<String>,
    pub strip: Option<String>,
    pub linker: Option<String>,
    pub launcher: Option<String>,
    pub c_flags: Vec<String>,
    pub cxx_flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub definitions: Vec<(String, minijinja::Value)>,
}

/// The accumulated, resolved build configuration produced by the pipeline
/// ("Build configuration layering").
/// `definitions` keep their type (bool/int/string) so a backend can spell each
/// one the way its tool expects.
///
/// `devenv` is the runtime twin of `environment`, kept apart because the two
/// must never mix: a build-time value (`CC`, `CFLAGS`) has no business in the
/// program under test, and a runtime selector none in a configure step.
#[derive(Debug, Clone, Default)]
pub struct LogicalConfig {
    pub environment: Vec<(String, String)>,
    pub definitions: Vec<(String, minijinja::Value)>,
    pub devenv: Vec<DevenvEntry>,
    pub extra_config_args: Vec<String>,
    pub extra_build_args: Vec<String>,
    pub extra_install_args: Vec<String>,
}

/// One resolved `devenv` operation, applied at run time on top of the
/// backend's base environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevenvEntry {
    pub name: String,
    pub op: DevenvOp,
    /// The rendered parts, already joined with `separator`.
    pub value: String,
    pub separator: String,
}

impl DevenvEntry {
    /// Apply this operation to `env` with Meson's run-mode semantics: a
    /// prepend/append onto a variable that is absent yields the value alone,
    /// while one onto a variable that is present — even empty — joins with the
    /// separator.
    pub fn apply(&self, env: &mut BTreeMap<OsString, OsString>) {
        let key = OsString::from(&self.name);
        let joined = match (self.op, env.get(&key)) {
            (DevenvOp::Set, _) | (_, None) => OsString::from(&self.value),
            (DevenvOp::Prepend, Some(current)) => {
                let mut out = OsString::from(&self.value);
                out.push(&self.separator);
                out.push(current);
                out
            }
            (DevenvOp::Append, Some(current)) => {
                let mut out = current.clone();
                out.push(&self.separator);
                out.push(&self.value);
                out
            }
        };
        env.insert(key, joined);
    }
}

impl LogicalConfig {
    /// Accumulate a `devenv` operation. A `set` discards the operations on its
    /// variable that came before it, since it overwrites whatever they built;
    /// a prepend/append repeating one still in force is not added again, so a
    /// preset reached twice through `extends` cannot add the same entry twice
    /// (two copies of one ICD in `VK_DRIVER_FILES` load the driver twice).
    pub fn add_devenv(&mut self, entry: DevenvEntry) {
        if entry.op == DevenvOp::Set {
            self.devenv.retain(|e| e.name != entry.name);
        } else if self.devenv.contains(&entry) {
            return;
        }
        self.devenv.push(entry);
    }

    /// Set an environment variable, replacing any earlier value for the key.
    /// Order is preserved by keeping the first insertion position.
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        set_kv(&mut self.environment, key.into(), value.into());
    }

    #[allow(dead_code)] // part of the read-only query surface; used in tests
    pub fn env_entry(&self, key: &str) -> Option<&str> {
        self.environment
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_definition(&mut self, key: impl Into<String>, value: minijinja::Value) {
        set_kv(&mut self.definitions, key.into(), value);
    }

    pub fn has_definition(&self, key: &str) -> bool {
        self.definitions.iter().any(|(k, _)| k == key)
    }
}

fn set_kv<V>(list: &mut Vec<(String, V)>, key: String, value: V) {
    if let Some(slot) = list.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        list.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_inference() {
        let sub = RawRepo {
            path: Some("subprojects/inner".into()),
            main_branch: Some("develop".into()),
            ..Default::default()
        };
        let subtree = RawRepo {
            path: Some("src/plugins/demo".into()),
            ..Default::default()
        };
        let standalone = RawRepo {
            path: Some("~/src/sibling".into()),
            main_branch: Some("main".into()),
            ..Default::default()
        };
        let borrow = RawRepo {
            from: Some("acme/engine".into()),
            ..Default::default()
        };
        assert_eq!(infer_kind("inner", &sub), Kind::Submodule);
        assert_eq!(infer_kind("demo", &subtree), Kind::Subtree);
        assert_eq!(infer_kind("side", &standalone), Kind::Standalone);
        // main is always standalone even with a relative path
        assert_eq!(infer_kind("main", &subtree), Kind::Standalone);
        // An unresolved borrow answers the same as its resolved (absolute) form.
        assert_eq!(infer_kind("engine", &borrow), Kind::Standalone);
    }

    #[test]
    fn borrow_reference_defaults_to_main() {
        assert_eq!(
            parse_borrow("acme/engine").unwrap(),
            BorrowRef {
                project: "acme/engine",
                repo: "main"
            }
        );
        assert_eq!(
            parse_borrow("acme/viewer:engine").unwrap(),
            BorrowRef {
                project: "acme/viewer",
                repo: "engine"
            }
        );
        assert_eq!(
            parse_borrow("engine").unwrap(),
            BorrowRef {
                project: "engine",
                repo: "main"
            }
        );
        assert!(parse_borrow(":engine").is_err());
        assert!(parse_borrow("acme/engine:").is_err());
    }

    /// The borrow is the single source for a repo's git identity, so a locally
    /// declared travelling field is a conflict the loader must reject — and so is
    /// `developed_as`, which answers for a checkout the borrower does not own.
    #[test]
    fn borrowed_field_conflicts_are_reported() {
        let repo: RawRepo = toml::from_str(
            r#"
            from = "acme/engine"
            main_branch = "mine"
            bootstrap_worktree_dir = "/mine"
            developed_as = "acme/other"
            anchor = "main"
            "#,
        )
        .unwrap();
        assert_eq!(
            repo.borrowed_field_conflicts(),
            vec!["main_branch", "bootstrap_worktree_dir", "developed_as"]
        );

        // `anchor` / `source_dir` / `build_dir` / `install_dir` / `presets` are
        // the borrower's own business.
        let clean: RawRepo = toml::from_str(
            r#"
            from = "acme/engine"
            anchor = "main"
            source_dir = "{{repos.main.workdir}}/src"
            build_dir = "{{repos.main.workdir}}/_build"
            install_dir = "{{repos.main.workdir}}/_install"
            [presets.x]
            definitions = { A = 1 }
            "#,
        )
        .unwrap();
        assert!(clean.borrowed_field_conflicts().is_empty());
    }

    #[test]
    fn hooks_are_typed_and_reject_a_mistyped_phase() {
        let repo: RawRepo = toml::from_str(
            r#"
            path = "~/src/x"
            [hooks]
            pre_update = "echo hi"
            "#,
        )
        .unwrap();
        assert_eq!(repo.hooks.pre_update.as_deref(), Some("echo hi"));
        assert!(repo.hooks.update.is_none());

        // A mistyped phase is a hard parse error, not a silently-dropped no-op.
        let err = toml::from_str::<RawRepo>(
            r#"
            path = "~/src/x"
            [hooks]
            pre_updat = "oops"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("pre_updat"));
    }

    #[test]
    fn build_system_parses_known_and_rejects_unknown_at_parse_time() {
        use std::str::FromStr;
        assert_eq!(BuildSystem::from_str("meson").unwrap(), BuildSystem::Meson);
        assert!(BuildSystem::from_str("bazel").is_err());
        // An unknown value is a hard parse error, not a silently-ignored string.
        let err = toml::from_str::<RawProject>(r#"build_system = "bazel""#).unwrap_err();
        assert!(err.to_string().contains("bazel"));
    }

    #[test]
    fn extends_accepts_string_or_list() {
        let one: RawPreset = toml::from_str(r#"extends = "base""#).unwrap();
        assert_eq!(one.extends.0, vec!["base"]);
        let many: RawPreset = toml::from_str(r#"extends = ["a", "b"]"#).unwrap();
        assert_eq!(many.extends.0, vec!["a", "b"]);
    }

    #[test]
    fn parses_a_project_file() {
        let file: RawFile = toml::from_str(
            r#"
            [project]
            focus = "main"
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "~/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{build_type}}"
            [repos.main.remotes]
            origin = "git@github.com:me/hello.git"
            "#,
        )
        .unwrap();
        let project = file.project.unwrap();
        assert_eq!(project.build_system, Some(BuildSystem::Cmake));
        assert_eq!(file.repos["main"].path.as_deref(), Some("~/src/hello"));
        assert_eq!(
            file.repos["main"].build_dir.as_deref(),
            Some("{{repos.main.workdir}}/_build/{{build_type}}")
        );
        // A bare URL is the shorthand for a table carrying only `url`.
        assert_eq!(
            file.repos["main"].remotes["origin"],
            RawRemote {
                url: "git@github.com:me/hello.git".into(),
                role: None,
                mirrors: Vec::new(),
            }
        );
    }

    #[test]
    fn build_dir_on_the_project_table_is_rejected() {
        let err = toml::from_str::<RawProject>(r#"build_dir = "/tmp""#).unwrap_err();
        assert!(
            err.to_string().contains("build_dir"),
            "unknown field should name build_dir: {err}"
        );
    }

    fn devenv(body: &str) -> Result<RawDevenvTable, String> {
        toml::from_str::<RawProject>(&format!("[devenv]\n{body}"))
            .map(|p| p.devenv)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_bare_devenv_value_sets_and_a_table_names_its_operation() {
        let table = devenv(
            r#"
            A = "one"
            B = 1
            C = ["x", "y"]
            D = { prepend = "{{build_dir}}/bin" }
            E = { append = ["p", "q"], separator = ";" }
            F = { set = "s" }
            "#,
        )
        .unwrap();
        let op = |name: &str| {
            let (_, raw) = table.0.iter().find(|(n, _)| n == name).unwrap();
            (raw.op, raw.values.len(), raw.separator.clone())
        };
        assert_eq!(op("A"), (DevenvOp::Set, 1, ":".into()));
        assert_eq!(op("B"), (DevenvOp::Set, 1, ":".into()));
        assert_eq!(op("C"), (DevenvOp::Set, 2, ":".into()));
        assert_eq!(op("D"), (DevenvOp::Prepend, 1, ":".into()));
        assert_eq!(op("E"), (DevenvOp::Append, 2, ";".into()));
        assert_eq!(op("F"), (DevenvOp::Set, 1, ":".into()));
    }

    #[test]
    fn a_devenv_table_names_exactly_one_known_operation() {
        for (body, needle) in [
            (r#"A = { prepend = "a", append = "b" }"#, "more than one"),
            (r#"A = { separator = ";" }"#, "none of"),
            (r#"A = { prepnd = "a" }"#, "prepnd"),
            (
                r#"A = { set = "a", separator = 1 }"#,
                "separator must be a string",
            ),
            (r#"A = { set = { x = 1 } }"#, "scalar"),
            (r#"A = [["nested"]]"#, "scalar"),
        ] {
            let err = devenv(body).unwrap_err();
            assert!(err.contains(needle), "{body}: {err}");
        }
    }

    fn entry(name: &str, op: DevenvOp, value: &str) -> DevenvEntry {
        DevenvEntry {
            name: name.into(),
            op,
            value: value.into(),
            separator: ":".into(),
        }
    }

    /// The semantics of Meson's own run mode, which is the base these land on:
    /// absent means the value alone, present — even empty — means joined.
    #[test]
    fn devenv_operations_apply_with_mesons_run_mode_semantics() {
        let mut env = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("EMPTY"), OsString::new()),
        ]);
        entry("PATH", DevenvOp::Prepend, "/b").apply(&mut env);
        entry("PATH", DevenvOp::Append, "/z").apply(&mut env);
        entry("FRESH", DevenvOp::Append, "/only").apply(&mut env);
        entry("EMPTY", DevenvOp::Prepend, "/e").apply(&mut env);
        entry("SET", DevenvOp::Set, "v").apply(&mut env);
        let get = |k: &str| env[&OsString::from(k)].to_str().unwrap().to_owned();
        assert_eq!(get("PATH"), "/b:/usr/bin:/z");
        assert_eq!(get("FRESH"), "/only");
        assert_eq!(get("EMPTY"), "/e:");
        assert_eq!(get("SET"), "v");
    }

    #[test]
    fn a_set_discards_earlier_operations_and_a_repeat_in_force_is_not_added() {
        let mut cfg = LogicalConfig::default();
        cfg.add_devenv(entry("P", DevenvOp::Prepend, "/a"));
        cfg.add_devenv(entry("Q", DevenvOp::Set, "q"));
        // A preset reached twice through `extends` repeats its prepend.
        cfg.add_devenv(entry("P", DevenvOp::Prepend, "/a"));
        assert_eq!(cfg.devenv.len(), 2);

        // A set wipes what came before it, so a later prepend that matches one
        // the set discarded is in force again rather than a repeat.
        cfg.add_devenv(entry("P", DevenvOp::Set, "/base"));
        cfg.add_devenv(entry("P", DevenvOp::Prepend, "/a"));
        let on_p: Vec<_> = cfg.devenv.iter().filter(|e| e.name == "P").collect();
        assert_eq!(
            on_p,
            vec![
                &entry("P", DevenvOp::Set, "/base"),
                &entry("P", DevenvOp::Prepend, "/a")
            ]
        );

        // Last writer wins for a set, however many times the value recurs.
        cfg.add_devenv(entry("Q", DevenvOp::Set, "other"));
        cfg.add_devenv(entry("Q", DevenvOp::Set, "q"));
        let on_q: Vec<_> = cfg.devenv.iter().filter(|e| e.name == "Q").collect();
        assert_eq!(on_q, vec![&entry("Q", DevenvOp::Set, "q")]);
    }
}
