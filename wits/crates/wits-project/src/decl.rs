//! The typed declaration model: what a project *is*, parsed from TOML but not
//! yet resolved.
//!
//! These types stay deliberately close to the file on disk. Templated fields
//! (`repos.*.build_dir`, an `[environment]` map, …) are kept as *raw* strings and
//! `toml::Value`s here; turning them into concrete paths and command lines is the
//! resolution stage's job, and only once a profile is supplied. Keeping the two
//! apart is the whole reason a read-only query never has to run a build planner.
//!
//! One thing is inferred rather than declared: a repo's *kind*. A path that is
//! nested under `repos.main` and carries its own `main_branch` is a submodule; a
//! nested path without one is a subtree; anything else is standalone. Declaring
//! it would just be a fourth thing to keep consistent with the other three.
//!
//! Two fields describe a repo whose checkout is *not* wholly this project's own.
//! [`RepoDecl::from`] borrows another project's repo as this one, so a component
//! several projects consume is declared once where it lives; [`RepoDecl::skip`]
//! names the paths this checkout never materialises, which is what makes the
//! borrow usable — the borrower's own copy of the component stays unmaterialised
//! rather than shadowing the one it borrowed.
//!
//! Quoted section names below are from `docs/reference/project-design.rst`.

use std::collections::BTreeMap;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use wits_core::remote::Role;

/// A whole config file, parsed. Every section is optional so one file may carry
/// a project, toolchains, and an org at once ("One project per file; registries
/// merge"). Unknown keys are rejected so a typo like `[toolchian]` fails loudly
/// instead of being silently ignored.
///
/// Loader-internal: the loader routes the sections and the public model starts
/// at [`SettingsDecl`] and [`RepoDecl`].
#[derive(Debug, Default)]
pub(crate) struct FileDecl {
    pub(crate) project: Option<ProjectFileDecl>,
    pub(crate) repos: BTreeMap<String, RepoDecl>,
    pub(crate) org: Option<OrgDecl>,
    pub(crate) toolchains: BTreeMap<String, ToolchainDecl>,
}

/// The `[project]` table, split into its `org` key and the settings proper.
///
/// Loader-internal. It is not a serde type: [`RawFile`] carries a flat mirror of
/// the table, converted here — serde's `deny_unknown_fields` does not reach
/// across `#[serde(flatten)]`, and strictness is worth more than field grouping.
#[derive(Debug, Default)]
pub(crate) struct ProjectFileDecl {
    pub(crate) org: Option<String>,
    pub(crate) settings: SettingsDecl,
}

impl FileDecl {
    /// Parse one config file into declarations, rejecting unknown keys.
    pub(crate) fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let raw: RawFile = toml::from_str(text)?;
        Ok(FileDecl {
            project: raw.project.map(ProjectFileDecl::from),
            repos: raw.repos,
            org: raw.org,
            toolchains: raw.toolchains,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    project: Option<ProjectTableDecl>,
    #[serde(default)]
    repos: BTreeMap<String, RepoDecl>,
    org: Option<OrgDecl>,
    #[serde(default)]
    toolchains: BTreeMap<String, ToolchainDecl>,
}

/// The flat serde mirror of the `[project]` table: `org` plus every
/// [`SettingsDecl`] field. Adding a field to `SettingsDecl` without adding it
/// here fails to compile in [`From<ProjectTableDecl>`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTableDecl {
    org: Option<String>,
    focus: Option<String>,
    build_system: Option<BuildSystem>,
    toolchain: Option<String>,
    generator: Option<String>,
    #[serde(default)]
    default_presets: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    definitions: BTreeMap<String, toml::Value>,
    #[serde(default, deserialize_with = "devenv_map")]
    devenv: Vec<(String, DevenvDecl)>,
    #[serde(default)]
    extra_config_args: Vec<String>,
    #[serde(default)]
    extra_build_args: Vec<String>,
    #[serde(default)]
    extra_install_args: Vec<String>,
    #[serde(default)]
    presets: BTreeMap<String, PresetDecl>,
}

impl From<ProjectTableDecl> for ProjectFileDecl {
    fn from(t: ProjectTableDecl) -> Self {
        ProjectFileDecl {
            org: t.org,
            settings: SettingsDecl {
                focus: t.focus,
                build_system: t.build_system,
                toolchain: t.toolchain,
                generator: t.generator,
                default_presets: t.default_presets,
                environment: t.environment,
                definitions: t.definitions,
                devenv: t.devenv,
                extra_config_args: t.extra_config_args,
                extra_build_args: t.extra_build_args,
                extra_install_args: t.extra_install_args,
                presets: t.presets,
            },
        }
    }
}

/// The `[project]` settings: everything the table carries besides `org`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsDecl {
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
    #[serde(default, deserialize_with = "devenv_map")]
    pub devenv: Vec<(String, DevenvDecl)>,
    #[serde(default)]
    pub extra_config_args: Vec<String>,
    #[serde(default)]
    pub extra_build_args: Vec<String>,
    #[serde(default)]
    pub extra_install_args: Vec<String>,
    #[serde(default)]
    pub presets: BTreeMap<String, PresetDecl>,
}

/// One repo as declared.
///
/// [`strategy`](Self::strategy) defaults to `in-place`; the raw mirror behind
/// [`Deserialize`] keeps the TOML key optional so that the loader can still tell
/// an *explicitly declared* `branch_strategy` from an omitted one when it
/// rejects a borrowing repo's travelling fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(try_from = "RawRepoDecl")]
pub struct RepoDecl {
    /// On-disk location (root / standalone) or a subpath relative to
    /// `repos.main` (nested). Absent exactly when [`from`](Self::from) borrows
    /// the repo from another project: the declaration keeps it `None`, and the
    /// loader resolves the source checkout into the resolved repo instead. Use
    /// `Project::repo_abs_path` for the resolved location; do not write that
    /// path back here.
    pub path: Option<String>,
    /// Borrow another project's repo as this one: `[<org>/]<project>[:<repo>]`,
    /// defaulting to that project's `repos.main`. What travels is the repo's
    /// *git identity* (main branch, remotes, hooks, branch strategy, worktree
    /// paths, `skip`); what stays local is how *this* build uses it (`anchor`,
    /// `source_dir`, `build_dir`, `install_dir`, `presets`). A borrow may not
    /// itself be borrowed.
    pub from: Option<BorrowRef>,
    /// Which project this checkout is currently developed *as*, as a project
    /// reference (`[<org>/]<project>`), when several projects reference it. It is
    /// the **owner's** statement: the borrowers have no grounds to prefer between
    /// themselves. Path lookups then answer for that project's borrow of this
    /// repo instead of for its owner.
    pub developed_as: Option<String>,
    pub main_branch: Option<String>,
    pub anchor: Option<String>,
    /// The TOML key is `branch_strategy` (see the struct docs).
    pub strategy: BranchStrategy,
    /// Whether the file wrote `branch_strategy` at all. [`strategy`](Self::strategy)
    /// carries the default either way; this distinguishes a repo that *declares*
    /// `in-place` from one that says nothing, which a report wants to show while
    /// resolution must not care.
    pub strategy_declared: bool,
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
    /// pattern list where `!` re-includes. Not templated: these are git patterns,
    /// and mixing `{{ }}` into `*`/`!` would only make them harder to read.
    pub skip: Vec<String>,
    pub remotes: BTreeMap<String, RemoteDecl>,
    pub hooks: HooksDecl,
    pub presets: BTreeMap<String, PresetDecl>,
}

/// The raw serde mirror of [`RepoDecl`]. It keeps `branch_strategy` optional so
/// an explicitly declared value is visible to the borrow-conflict rule; the
/// logical model carries the defaulted enum.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepoDecl {
    path: Option<String>,
    from: Option<BorrowRef>,
    developed_as: Option<String>,
    main_branch: Option<String>,
    anchor: Option<String>,
    #[serde(rename = "branch_strategy")]
    branch_strategy: Option<BranchStrategy>,
    worktree_dir: Option<String>,
    bootstrap_worktree_dir: Option<String>,
    source_dir: Option<String>,
    build_dir: Option<String>,
    install_dir: Option<String>,
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    remotes: BTreeMap<String, RemoteDecl>,
    #[serde(default)]
    hooks: HooksDecl,
    #[serde(default)]
    presets: BTreeMap<String, PresetDecl>,
}

impl RawRepoDecl {
    /// The fields a borrower may not declare. Two reasons, one rule: a
    /// [`from`](RepoDecl::from) borrow *supplies* the component's git identity,
    /// and a local override would quietly reintroduce the duplication the borrow
    /// removes; `developed_as` is the *owner's* answer about a checkout this
    /// entry does not own, so a borrower declaring it would only be writing a
    /// value nothing reads. Both are rejected rather than silently picked
    /// between or ignored.
    fn borrowed_field_conflicts(&self) -> Vec<&'static str> {
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
        if self.hooks != HooksDecl::default() {
            out.push("hooks");
        }
        if self.developed_as.is_some() {
            out.push("developed_as");
        }
        out
    }
}

impl TryFrom<RawRepoDecl> for RepoDecl {
    type Error = String;

    fn try_from(raw: RawRepoDecl) -> Result<Self, String> {
        if raw.from.is_some() {
            let conflicts = raw.borrowed_field_conflicts();
            if !conflicts.is_empty() {
                return Err(format!(
                    "a borrow may not declare {} — they belong to the project that owns the repo",
                    conflicts.join(", ")
                ));
            }
        }
        Ok(RepoDecl {
            path: raw.path,
            from: raw.from,
            developed_as: raw.developed_as,
            main_branch: raw.main_branch,
            anchor: raw.anchor,
            strategy: raw.branch_strategy.unwrap_or_default(),
            strategy_declared: raw.branch_strategy.is_some(),
            worktree_dir: raw.worktree_dir,
            bootstrap_worktree_dir: raw.bootstrap_worktree_dir,
            source_dir: raw.source_dir,
            build_dir: raw.build_dir,
            install_dir: raw.install_dir,
            skip: raw.skip,
            remotes: raw.remotes,
            hooks: raw.hooks,
            presets: raw.presets,
        })
    }
}

/// A parsed [`RepoDecl::from`] reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowRef {
    /// A project reference as `Workspace::project` takes it — a bare name or
    /// `org/name`.
    pub project: String,
    /// Which of that project's repos to borrow; `main` unless spelled out.
    pub repo: String,
}

/// Parse `[<org>/]<project>[:<repo>]`. The separators cannot collide: `/`
/// qualifies the org (and is left for the project lookup to interpret), `:`
/// selects the repo.
pub(crate) fn parse_borrow(spec: &str) -> Result<BorrowRef, String> {
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
    Ok(BorrowRef {
        project: project.to_owned(),
        repo: repo.to_owned(),
    })
}

impl<'de> Deserialize<'de> for BorrowRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let spec = String::deserialize(d)?;
        parse_borrow(&spec).map_err(de::Error::custom)
    }
}

/// A repo's lifecycle hooks: an optional command string per phase. A typed
/// struct rather than a free `phase -> command` map, so a mistyped phase like
/// `pre_updat` is a hard parse error (via `deny_unknown_fields`) instead of a
/// silently-ignored no-op. Each command is a template resolved against the
/// per-repo context when the phase runs.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HooksDecl {
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

/// One declared remote.
///
/// Written either as a bare URL (`origin = "git@…"`) or as a table when it needs
/// more than that — the same string-or-struct courtesy `extends` extends to
/// lists. The common case, a URL under a conventional name, stays one line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RemoteDecl {
    pub url: String,
    /// Which role this remote holds. Omitted for the privileged names, whose name
    /// already says it; **required** for any other name that is to hold a role,
    /// since a name is the only signal a bare checkout has (see
    /// [`wits_core::remote`]).
    pub role: Option<Role>,
    /// Extra push URLs, so one push fans out to copies of the same repository.
    ///
    /// An attribute rather than a role: a role answers "which remote does X" and
    /// so has at most one holder, while any number of remotes may each fan out.
    /// Unrelated to `git clone --mirror`, which copies a whole ref space; these
    /// are only additional push destinations.
    pub mirrors: Vec<String>,
}

impl<'de> Deserialize<'de> for RemoteDecl {
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
            type Value = RemoteDecl;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a URL string or a remote table")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<RemoteDecl, E> {
                Ok(RemoteDecl {
                    url: s.to_owned(),
                    ..Default::default()
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<RemoteDecl, A::Error> {
                let t = Table::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(RemoteDecl {
                    url: t.url,
                    role: t.role,
                    mirrors: t.mirrors,
                })
            }
        }
        d.deserialize_any(V)
    }
}

/// A named preset: the layer a project or repo applies on top of its own
/// settings, and the unit `extends` and `--preset` select.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct PresetDecl {
    #[serde(default, deserialize_with = "string_list")]
    pub extends: Vec<String>,
    pub applies_when: Option<BTreeMap<String, toml::Value>>,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
    #[serde(default, deserialize_with = "devenv_map")]
    pub devenv: Vec<(String, DevenvDecl)>,
    #[serde(default)]
    pub extra_config_args: Vec<String>,
    #[serde(default)]
    pub extra_build_args: Vec<String>,
    #[serde(default)]
    pub extra_install_args: Vec<String>,
}

/// A named toolchain: canonical compiler/linker fields plus verbatim
/// environment and definition blocks. Backends translate the canonical fields
/// into their native spelling; nothing here names a backend.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct ToolchainDecl {
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

/// An org declaration: the shared namespace its member projects inherit, plus
/// its own presets (which apply only when named).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct OrgDecl {
    pub name: String,
    #[serde(default)]
    pub environment: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub definitions: BTreeMap<String, toml::Value>,
    #[serde(default, deserialize_with = "devenv_map")]
    pub devenv: Vec<(String, DevenvDecl)>,
    #[serde(default)]
    pub presets: BTreeMap<String, PresetDecl>,
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
pub struct DevenvDecl {
    pub op: DevenvOp,
    pub values: Vec<toml::Value>,
    pub separator: String,
}

impl DevenvDecl {
    fn from_value(value: toml::Value) -> Result<Self, String> {
        let table = match value {
            toml::Value::Table(table) => table,
            bare => {
                return Ok(DevenvDecl {
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
        Ok(DevenvDecl {
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

impl<'de> Deserialize<'de> for DevenvDecl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        DevenvDecl::from_value(toml::Value::deserialize(d)?).map_err(de::Error::custom)
    }
}

/// A `devenv` table's entries, in key order.
///
/// A list rather than a map because a preset merged across org/project/repo
/// carries every level's operation on the same variable, in level order: the
/// operations accumulate, as Meson's do, rather than the nearest replacing the
/// rest.
fn devenv_map<'de, D>(d: D) -> Result<Vec<(String, DevenvDecl)>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries = BTreeMap::<String, DevenvDecl>::deserialize(d)?;
    Ok(entries.into_iter().collect())
}

/// A field that may be written as a single string or a list of strings
/// (`extends = "base"` or `extends = ["a", "b"]`).
fn string_list<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or a list of strings")
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Vec<String>, E> {
            Ok(vec![s.to_owned()])
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<String>, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.push(item);
            }
            Ok(out)
        }
    }
    d.deserialize_any(V)
}

/// The build systems wits knows how to drive. Declared once per project as
/// `build_system`; every variant maps to a backend, so unlike a free string the
/// mapping is total and an unknown value fails at *parse* time (via the custom
/// [`Deserialize`]) rather than deep inside a build.
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

/// How a repo keeps its checkouts: one working tree, or a bare common
/// repository with per-branch working trees. The TOML key is `branch_strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchStrategy {
    #[default]
    InPlace,
    Worktree,
    Hybrid,
}

impl BranchStrategy {
    /// The config spelling, the exact inverse of the parse — so what a report
    /// prints is what a file may be written with.
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

impl std::str::FromStr for BranchStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "in-place" => Ok(BranchStrategy::InPlace),
            "worktree" => Ok(BranchStrategy::Worktree),
            "hybrid" => Ok(BranchStrategy::Hybrid),
            other => Err(format!(
                "unknown branch_strategy '{other}' (use in-place|worktree|hybrid)"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for BranchStrategy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
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
/// A borrowed repo is `Standalone` from the borrower's side — which is what it
/// is there, however it is nested in the project that owns it. That is the
/// property the rest of the tool needs: an external checkout, with its own git,
/// that this project's submodule refresh must keep its hands off.
pub(crate) fn infer_kind(name: &str, repo: &RepoDecl) -> Kind {
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
pub(crate) fn is_nested(path: &str) -> bool {
    !(path.starts_with('/') || path.starts_with('~') || std::path::Path::new(path).is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_inference() {
        let sub = RepoDecl {
            path: Some("subprojects/inner".into()),
            main_branch: Some("develop".into()),
            ..Default::default()
        };
        let subtree = RepoDecl {
            path: Some("src/plugins/demo".into()),
            ..Default::default()
        };
        let standalone = RepoDecl {
            path: Some("~/src/sibling".into()),
            main_branch: Some("main".into()),
            ..Default::default()
        };
        let borrow = RepoDecl {
            from: Some(BorrowRef {
                project: "acme/engine".into(),
                repo: "main".into(),
            }),
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
                project: "acme/engine".into(),
                repo: "main".into()
            }
        );
        assert_eq!(
            parse_borrow("acme/viewer:engine").unwrap(),
            BorrowRef {
                project: "acme/viewer".into(),
                repo: "engine".into()
            }
        );
        assert_eq!(
            parse_borrow("engine").unwrap(),
            BorrowRef {
                project: "engine".into(),
                repo: "main".into()
            }
        );
        assert!(parse_borrow(":engine").is_err());
        assert!(parse_borrow("acme/engine:").is_err());
    }

    /// The borrow is the single source for a repo's git identity, so a locally
    /// declared travelling field is a conflict the parser must reject — and so is
    /// `developed_as`, which answers for a checkout the borrower does not own.
    #[test]
    fn borrowed_field_conflicts_are_reported() {
        let err = toml::from_str::<RepoDecl>(
            r#"
            from = "acme/engine"
            main_branch = "mine"
            bootstrap_worktree_dir = "/mine"
            developed_as = "acme/other"
            anchor = "main"
            "#,
        )
        .unwrap_err();
        let msg = err.to_string();
        for field in ["main_branch", "bootstrap_worktree_dir", "developed_as"] {
            assert!(msg.contains(field), "{field} missing from: {msg}");
        }

        // An explicitly declared strategy is a travelling field too, even when it
        // spells the default — the reason the raw mirror keeps it optional.
        let err = toml::from_str::<RepoDecl>(
            r#"
            from = "acme/engine"
            branch_strategy = "in-place"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("branch_strategy"));

        // `anchor` / `source_dir` / `build_dir` / `install_dir` / `presets` are
        // the borrower's own business.
        toml::from_str::<RepoDecl>(
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
    }

    #[test]
    fn from_parses_into_an_owned_reference() {
        let repo: RepoDecl = toml::from_str(
            r#"
            from = "acme/engine"
            "#,
        )
        .unwrap();
        let reference = repo.from.unwrap();
        assert_eq!(reference.project, "acme/engine");
        assert_eq!(reference.repo, "main");
        assert!(toml::from_str::<RepoDecl>(r#"from = ":engine""#).is_err());
        assert!(toml::from_str::<RepoDecl>(r#"from = "acme/engine:""#).is_err());
    }

    #[test]
    fn hooks_are_typed_and_reject_a_mistyped_phase() {
        let repo: RepoDecl = toml::from_str(
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
        let err = toml::from_str::<RepoDecl>(
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
        let err = toml::from_str::<SettingsDecl>(r#"build_system = "bazel""#).unwrap_err();
        assert!(err.to_string().contains("bazel"));
    }

    #[test]
    fn strategy_defaults_to_in_place_and_rejects_unknown_at_parse_time() {
        let repo: RepoDecl = toml::from_str(r#"path = "/src/x""#).unwrap();
        assert_eq!(repo.strategy, BranchStrategy::InPlace);
        assert!(
            !repo.strategy_declared,
            "an omitted key is not a declaration"
        );
        let repo: RepoDecl =
            toml::from_str("path = \"/src/x\"\nbranch_strategy = \"hybrid\"").unwrap();
        assert_eq!(repo.strategy, BranchStrategy::Hybrid);
        assert!(repo.strategy_declared);
        // Spelling the default is still a declaration: a report shows it, and
        // resolution reads the same value either way.
        let repo: RepoDecl =
            toml::from_str("path = \"/src/x\"\nbranch_strategy = \"in-place\"").unwrap();
        assert_eq!(repo.strategy, BranchStrategy::InPlace);
        assert!(repo.strategy_declared);
        let err = toml::from_str::<RepoDecl>(
            r#"
            path = "/src/x"
            branch_strategy = "bogus"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn extends_accepts_string_or_list() {
        let one: PresetDecl = toml::from_str(r#"extends = "base""#).unwrap();
        assert_eq!(one.extends, vec!["base"]);
        let many: PresetDecl = toml::from_str(r#"extends = ["a", "b"]"#).unwrap();
        assert_eq!(many.extends, vec!["a", "b"]);
    }

    #[test]
    fn remotes_accept_a_url_string_or_a_table() {
        let repo: RepoDecl = toml::from_str(
            r#"
            path = "/src/x"
            [remotes]
            origin = "git@example.com:me/x.git"
            [remotes.mirror]
            url = "https://example.com/x.git"
            mirrors = ["https://second.example.com/x.git"]
            "#,
        )
        .unwrap();
        assert_eq!(repo.remotes["origin"].url, "git@example.com:me/x.git");
        assert_eq!(repo.remotes["origin"].role, None);
        assert!(repo.remotes["origin"].mirrors.is_empty());
        assert_eq!(repo.remotes["mirror"].mirrors.len(), 1);

        // A mistyped key inside a remote table fails loudly.
        let err = toml::from_str::<RepoDecl>(
            r#"
            path = "/src/x"
            [remotes.origin]
            url = "https://example.com/x.git"
            mirror = ["https://example.com/mirror.git"]
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mirror"));
    }

    #[test]
    fn parses_a_project_file() {
        let file = FileDecl::parse(
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
        assert_eq!(project.org, None);
        assert_eq!(project.settings.build_system, Some(BuildSystem::Cmake));
        assert_eq!(file.repos["main"].path.as_deref(), Some("~/src/hello"));
        assert_eq!(
            file.repos["main"].build_dir.as_deref(),
            Some("{{repos.main.workdir}}/_build/{{build_type}}")
        );
        // A bare URL is the shorthand for a table carrying only `url`.
        assert_eq!(
            file.repos["main"].remotes["origin"],
            RemoteDecl {
                url: "git@github.com:me/hello.git".into(),
                role: None,
                mirrors: Vec::new(),
            }
        );
    }

    #[test]
    fn build_dir_on_the_project_table_is_rejected() {
        let err = FileDecl::parse(
            r#"
            [project]
            build_dir = "/tmp"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("build_dir"),
            "unknown field should name build_dir: {err}"
        );
    }

    #[test]
    fn unknown_keys_in_repo_and_toolchain_tables_are_rejected() {
        let err = FileDecl::parse(
            r#"
            [project]
            [repos.main]
            path = "/src/x"
            mian_branch = "main"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mian_branch"), "{err}");

        let err = FileDecl::parse(
            r#"
            [toolchains.cc]
            cc = "clang"
            exra = 1
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("exra"), "{err}");
    }

    fn devenv(body: &str) -> Result<Vec<(String, DevenvDecl)>, String> {
        toml::from_str::<SettingsDecl>(&format!("[devenv]\n{body}"))
            .map(|s| s.devenv)
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
            let (_, raw) = table.iter().find(|(n, _)| n == name).unwrap();
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
}
