//! Building and rendering the template context for the resolution pipeline.
//!
//! Every `{{ … }}` a project config can reference resolves against a [`Value`]
//! tree assembled here, and this is the *single* place that tree is built — the
//! full pipeline context ([`Ctx`], via `resolve::plan`), the Profile-free
//! per-repo context ([`context_for_repo`], for `update`/`context`), and the
//! minimal path context ([`path_context`], for `repo.path` templates in
//! `workspace`) all share `system_facts`/`repo_value`/the `env.*` snapshot, so
//! the namespaces can't drift between callers. The layer-application helpers
//! ([`apply_env_map`]/[`apply_def_map`]/…) live here too, since folding a config
//! layer into `Ctx` + [`LogicalConfig`] is the same context concern.

use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;

use anyhow::Result;

use crate::template::{Engine, Value};

use super::model::{infer_kind, LogicalConfig};
use super::workspace::{ProjectData, Workspace};

// --- the mutable pipeline context ---------------------------------------------

/// A mutable template context plus rendering helpers.
///
/// A [`Engine`] memoises the paths it resolves, so a run of renders against an
/// unchanged context should share one. We hold it lazily and rebuild it only
/// when the context actually changes: [`engine`](Ctx::engine) populates the slot
/// on first use, and every mutation ([`set`](Ctx::set)/[`set_env`](Ctx::set_env))
/// drops it so a stale snapshot (and its memo) can never answer against an
/// out-of-date tree.
pub(crate) struct Ctx {
    root: Value,
    engine: RefCell<Option<Engine>>,
}

impl Clone for Ctx {
    fn clone(&self) -> Self {
        // A clone starts with a cold engine: the cache is a pure optimisation
        // over `root`, so it is always safe to drop and rebuild on demand.
        Ctx {
            root: self.root.clone(),
            engine: RefCell::new(None),
        }
    }
}

impl Ctx {
    pub(crate) fn new(root: Value) -> Self {
        Ctx {
            root,
            engine: RefCell::new(None),
        }
    }

    /// The memoising engine for the current context, built once and reused until
    /// the next mutation invalidates it.
    pub(crate) fn engine(&self) -> Ref<'_, Engine> {
        if self.engine.borrow().is_none() {
            *self.engine.borrow_mut() = Some(Engine::new(self.root.clone()));
        }
        Ref::map(self.engine.borrow(), |slot| {
            slot.as_ref().expect("just populated")
        })
    }

    fn invalidate(&mut self) {
        *self.engine.get_mut() = None;
    }

    pub(crate) fn set(&mut self, path: &str, value: Value) {
        self.root.insert_path(path, value);
        self.invalidate();
    }

    pub(crate) fn set_env(&mut self, key: &str, value: String) {
        self.root
            .insert_path(&format!("env.{key}"), Value::Str(value));
        self.invalidate();
    }

    /// The accumulated context tree, consumed at the end of a plan so it can be
    /// handed back for arbitrary template resolution.
    pub(crate) fn into_value(self) -> Value {
        self.root
    }

    /// Render a template string to a plain string (embedded scalars stringified).
    pub(crate) fn render(&self, s: &str) -> Result<String> {
        Ok(value_to_string(&self.engine().resolve_str(s)?))
    }

    /// Resolve a value, returning it as a string (for env values).
    pub(crate) fn render_value(&self, v: &Value) -> Result<String> {
        Ok(value_to_string(&self.engine().resolve(v)?))
    }
}

// --- layer application --------------------------------------------------------

/// Fold the accumulated environment into the context's `env.*` so later layers
/// can reference values earlier ones produced (`{{env.CC}}`, …).
pub(crate) fn fold_env(ctx: &mut Ctx, logical: &LogicalConfig) {
    for (k, v) in &logical.environment {
        ctx.set_env(k, v.clone());
    }
}

pub(crate) fn apply_env_map(
    ctx: &mut Ctx,
    logical: &mut LogicalConfig,
    ns: &str,
    raw: &BTreeMap<String, toml::Value>,
) -> Result<()> {
    if raw.is_empty() {
        return Ok(());
    }
    // Overlay the raw entries under both the namespace and env.* so entries may
    // reference each other in any order, then resolve each.
    for (k, v) in raw {
        let val = Value::from(v);
        ctx.set(&format!("{ns}.{k}"), val.clone());
        ctx.set_env(k, value_to_string(&val));
    }
    // Resolve every entry against one engine, then release the borrow before
    // writing back — the writes mutate `ctx`, which the engine borrow forbids
    // (and which would in any case invalidate the snapshot it read from).
    let resolved: Vec<(String, String)> = {
        let engine = ctx.engine();
        raw.keys()
            .map(|k| {
                Ok((
                    k.clone(),
                    value_to_string(&engine.get(&format!("env.{k}"))?),
                ))
            })
            .collect::<Result<_>>()?
    };
    for (k, v) in resolved {
        logical.set_env(&k, v.clone());
        ctx.set_env(&k, v);
    }
    Ok(())
}

pub(crate) fn apply_def_map(
    ctx: &mut Ctx,
    logical: &mut LogicalConfig,
    ns: &str,
    raw: &BTreeMap<String, toml::Value>,
) -> Result<()> {
    if raw.is_empty() {
        return Ok(());
    }
    for (k, v) in raw {
        ctx.set(&format!("{ns}.{k}"), Value::from(v));
    }
    let engine = ctx.engine();
    for k in raw.keys() {
        let value = engine.get(&format!("{ns}.{k}"))?;
        logical.set_definition(k, value);
    }
    Ok(())
}

pub(crate) fn resolve_args(ctx: &Ctx, raw: &[String], out: &mut Vec<String>) -> Result<()> {
    for arg in raw {
        out.push(ctx.render(arg)?);
    }
    Ok(())
}

/// Like [`resolve_args`], but skips a value already present — presets replace
/// what earlier layers set for the *same* preset, while distinct presets still
/// accumulate in order.
pub(crate) fn resolve_replace(ctx: &Ctx, raw: &[String], out: &mut Vec<String>) -> Result<()> {
    for arg in raw {
        let rendered = ctx.render(arg)?;
        if !out.contains(&rendered) {
            out.push(rendered);
        }
    }
    Ok(())
}

// --- context builders ---------------------------------------------------------

/// The `system.*` template namespace: the shared host-facts tree from
/// [`crate::system`].
pub fn system_facts() -> Value {
    crate::system::facts()
}

/// The process environment as an `env.*` map — the base every context layers on.
fn env_snapshot() -> Value {
    let mut env_map = BTreeMap::new();
    for (k, v) in std::env::vars() {
        env_map.insert(k, Value::Str(v));
    }
    Value::Map(env_map)
}

/// Expose an org's shared values under `org.environment.*` / `org.definitions.*`
/// so templates can name them directly. This only populates the namespace —
/// folding them into a build's logical config is the pipeline's own layer, which
/// contexts built for `update`/`context` never run.
fn insert_org_namespace(root: &mut Value, ws: &Workspace, org: Option<&str>) {
    let Some(org) = org else { return };
    let Some(org_data) = ws.org_base(org) else {
        return;
    };
    for (k, v) in &org_data.environment {
        root.insert_path(&format!("org.environment.{k}"), Value::from(v));
    }
    for (k, v) in &org_data.definitions {
        root.insert_path(&format!("org.definitions.{k}"), Value::from(v));
    }
}

pub fn repo_value(project: &ProjectData, name: &str) -> Value {
    let repo = &project.repos[name];
    let kind = infer_kind(name, repo);
    let abs = project
        .repo_abs_path(name)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let origin = repo.remotes.origin.clone().unwrap_or_default();
    let upstream = repo
        .remotes
        .upstream
        .clone()
        .unwrap_or_else(|| origin.clone());
    let mut m = BTreeMap::new();
    m.insert("name".into(), Value::str(name));
    m.insert("path".into(), Value::str(abs));
    m.insert("kind".into(), Value::str(kind.as_str()));
    m.insert(
        "main_branch".into(),
        Value::str(repo.main_branch.clone().unwrap_or_default()),
    );
    m.insert(
        "anchor".into(),
        Value::str(repo.anchor.clone().unwrap_or_default()),
    );
    m.insert("origin".into(), Value::str(origin));
    m.insert("upstream".into(), Value::str(upstream));
    m.insert(
        "mirrors".into(),
        Value::List(repo.remotes.mirrors.iter().map(Value::str).collect()),
    );
    Value::Map(m)
}

/// The base context every full plan and per-repo render shares: `project.*`,
/// every `repos.<name>.*`, the `org.*` namespace, `system.*`, and `env.*`. `repo.*` and
/// any Profile-specific bindings (branch, build_type, toolchain, and per-repo
/// workdirs) are layered on by the caller.
fn base_context(ws: &Workspace, project: &ProjectData, focus: &str) -> Value {
    let mut root = Value::Map(BTreeMap::new());
    root.insert_path("project.name", Value::str(&project.name));
    root.insert_path(
        "project.org",
        Value::str(project.org.clone().unwrap_or_default()),
    );
    root.insert_path("project.focus", Value::str(focus));
    for name in project.repos.keys() {
        root.insert_path(&format!("repos.{name}"), repo_value(project, name));
    }
    insert_org_namespace(&mut root, ws, project.org.as_deref());
    root.insert_path("system", system_facts());
    root.insert_path("env", env_snapshot());
    root
}

/// A [`Ctx`] seeded with the shared base context (`project.*`, `repos.*`, `org.*`,
/// `system.*`, `env.*`) plus `repo.*` bound to `focus`. The pipeline
/// then layers branch/build_type/toolchain and the repos' resolved `workdir`s
/// onto it.
pub(crate) fn plan_base(ws: &Workspace, project: &ProjectData, focus: &str) -> Ctx {
    let mut root = base_context(ws, project, focus);
    root.insert_path("repo", repo_value(project, focus));
    Ctx::new(root)
}

/// A context sufficient to resolve a repo-scoped template (a hook, a
/// `worktree_dir`): the shared base plus `repo.*` = `repo_name`. No Profile or
/// resolved `workdir` is available, so this is safe for `update`/`context`, which
/// don't build a full plan.
pub fn context_for_repo(ws: &Workspace, project: &ProjectData, repo_name: &str) -> Value {
    let mut root = base_context(ws, project, project.focus_name(None));
    root.insert_path("repo", repo_value(project, repo_name));
    root
}

/// The minimal Profile-free context for resolving `repo.path` templates:
/// `project.name`, `project.org`, `system.*`, `env.*`. No `repos.*` (that would
/// be circular — a repo's path is what we are computing), and no Profile, so
/// `workspace` can answer `repo_abs_path` / `project_for_path` without a plan.
pub fn path_context(name: &str, org: Option<&str>) -> Value {
    let mut root = Value::Map(BTreeMap::new());
    root.insert_path("project.name", Value::str(name));
    root.insert_path("project.org", Value::str(org.unwrap_or_default()));
    root.insert_path("system", system_facts());
    root.insert_path("env", env_snapshot());
    root
}

// --- misc ---------------------------------------------------------------------

pub(crate) fn value_to_string(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::List(items) => items
            .iter()
            .map(value_to_string)
            .collect::<Vec<_>>()
            .join(" "),
        Value::Map(_) => String::new(),
    }
}

/// A filesystem-safe path component from a branch name: every character outside
/// `[A-Za-z0-9._-]` → `_`. Distinct from `stack::slice`'s branch-name slug,
/// which *mints* a new branch name (lowercasing, collapsing to `-`); this only
/// makes an existing branch safe to drop into a `build_dir`/repo `workdir` path.
pub fn path_slug(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}
