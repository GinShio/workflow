//! The template context for the resolution pipeline: how it is built, and how it
//! resolves.
//!
//! Every `{{ … }}` a project config can reference resolves against a [`Ctx`]
//! assembled here, and this is the *single* place that tree is built — the full
//! pipeline context (via `resolve::plan`), the Profile-free per-repo context
//! ([`context_for_repo`], for `update`), and the minimal path context
//! ([`path_context`], for `repo.path` templates in `workspace`) all share
//! `system_facts`/`repo_value`/the `env.*` snapshot, so the namespaces can't
//! drift between callers. The layer-application helpers
//! ([`apply_env_map`]/[`apply_def_map`]/…) live here too, since folding a config
//! layer into `Ctx` + [`LogicalConfig`] is the same context concern.
//!
//! ## Why the resolver lives here and not in the Jinja floor
//!
//! Jinja renders a *finished* context. Project config's contract is the opposite:
//! a config value may itself be a template naming another config value, in an
//! order nobody declared — `alive2` has an `env` entry reading a sibling that
//! sorts *after* it. Bridging those two is a project-config policy, not a
//! property of the template language, so it sits beside the config it serves
//! while [`crate::jinja`] stays a dialect definition with no policy in it.
//!
//! [`Ctx`] is that bridge. It resolves *by dotted path, on demand*: asked for a
//! template, it discovers which paths the template reads, resolves each of those
//! recursively, and hands Jinja a context holding only them. Resolved paths are
//! memoised, and a path stack turns a self-reference cycle into an error rather
//! than a stack overflow. Resolving on demand is not only an optimisation — the
//! context carries the whole process environment, and eagerly running Jinja over
//! every environment variable would both cost a great deal and choke on the first
//! value that happens to contain a brace.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use minijinja::value::ValueKind;
use minijinja::{Error, ErrorKind, Value};
use thiserror::Error as ThisError;

use super::model::{infer_kind, LogicalConfig};
use super::workspace::{ProjectData, Workspace};

/// The names a template can see: values nested through maps, built
/// programmatically with [`insert_path`] and handed to [`Ctx::new`].
///
/// Spelled `Bindings` rather than `Context` because every module that assembles
/// one also imports `anyhow::Context`.
pub type Bindings = BTreeMap<String, Value>;

#[derive(Debug, ThisError, PartialEq)]
pub enum TemplateError {
    #[error("cannot resolve path '{0}' in template context")]
    UnknownPath(String),
    #[error("circular reference: {0}")]
    Cycle(String),
    #[error("template {template:?} resolved to a non-string: {value}")]
    NotAString { template: String, value: String },
    #[error("{0}")]
    Render(String),
}

// --- the pipeline context ------------------------------------------------------

/// A template context, and the resolver over it.
///
/// Mutation and resolution are one type because they share one invariant: the
/// memo describes `root`, so a write has to drop it. Splitting them would mean
/// re-establishing that handshake across the seam on every mutation, for no
/// caller that wants only one half — the throwaway contexts in `workspace` and
/// `resolve` build one and never write to it, which costs nothing.
pub struct Ctx {
    root: Bindings,
    /// Fully resolved values, by the dotted path that produced them.
    ///
    /// Any write clears the whole memo rather than the entries that actually
    /// depended on the changed path. Tracking that would need a reverse
    /// dependency map, and the pipeline writes in bursts before it reads, so the
    /// memo it discards is usually empty.
    cache: RefCell<HashMap<String, Value>>,
}

impl Clone for Ctx {
    fn clone(&self) -> Self {
        // A clone starts cold: the memo is a pure function of `root`, so dropping
        // it is always safe.
        Ctx::new(self.root.clone())
    }
}

impl Ctx {
    pub fn new(root: Bindings) -> Self {
        Ctx {
            root,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn set(&mut self, path: &str, value: Value) {
        insert_path(&mut self.root, path, value);
        self.cache.get_mut().clear();
    }

    pub(crate) fn set_env(&mut self, key: &str, value: String) {
        self.set(&format!("env.{key}"), Value::from(value));
    }

    /// The accumulated context tree, consumed at the end of a plan so it can be
    /// handed back for arbitrary template resolution.
    pub(crate) fn into_value(self) -> Value {
        Value::from(self.root)
    }

    /// Resolve a template string, keeping the type of a whole-expression result.
    pub fn resolve_str(&self, s: &str) -> Result<Value, TemplateError> {
        self.resolve_string(s, &mut Vec::new())
    }

    /// Resolve an arbitrary value: strings have their templates expanded, lists
    /// and maps are walked element-wise, scalars pass through.
    pub fn resolve(&self, raw: &Value) -> Result<Value, TemplateError> {
        self.resolve_value(raw, &mut Vec::new())
    }

    /// Look up (and fully resolve) a dotted context path.
    pub fn get(&self, path: &str) -> Result<Value, TemplateError> {
        self.resolve_path(path, &mut Vec::new())
    }

    /// Render a template to the single string an environment variable or a
    /// command-line argument has to be.
    pub fn render(&self, s: &str) -> Result<String, TemplateError> {
        Ok(value_to_string(&self.resolve_str(s)?))
    }

    /// Like [`render`](Ctx::render), but for a template that names a *path*: a
    /// list or a map flattening into one is never what a path template meant, so
    /// anything but a string is refused instead of silently joined.
    pub fn render_path(&self, s: &str) -> Result<String, TemplateError> {
        let resolved = self.resolve_str(s)?;
        match resolved.as_str() {
            Some(text) => Ok(text.to_owned()),
            None => Err(TemplateError::NotAString {
                template: s.to_owned(),
                value: resolved.to_string(),
            }),
        }
    }

    /// Resolve a value, returning it as a string (for env values).
    pub(crate) fn render_value(&self, v: &Value) -> Result<String, TemplateError> {
        Ok(value_to_string(&self.resolve(v)?))
    }

    fn resolve_value(&self, v: &Value, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        match v.kind() {
            ValueKind::String => {
                self.resolve_string(v.as_str().expect("string kind has a str"), stack)
            }
            ValueKind::Seq => {
                let items = v
                    .try_iter()
                    .map_err(walk_error)?
                    .map(|item| self.resolve_value(&item, stack))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::from(items))
            }
            ValueKind::Map => {
                let mut out = Bindings::new();
                for key in v.try_iter().map_err(walk_error)? {
                    let item = v.get_item(&key).map_err(walk_error)?;
                    out.insert(key.to_string(), self.resolve_value(&item, stack)?);
                }
                Ok(Value::from(out))
            }
            _ => Ok(v.clone()),
        }
    }

    fn resolve_string(&self, s: &str, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        // Most config values are plain text. Skipping the parse for those is only
        // sound if every opening delimiter is listed, comments included: a string
        // holding just `{# … #}` must be stripped the same way one holding
        // `{{ x }}{# … #}` is.
        if !s.contains("{{") && !s.contains("{%") && !s.contains("{#") {
            return Ok(Value::from(s));
        }
        let env = crate::jinja::shared();
        // Discovering the paths and then rendering are two passes over the same
        // source, so each form compiles once and both passes use that. Jinja has
        // no way to read the names off already-compiled instructions, so the
        // discovery pass reparses internally; that is the one cost left here.
        match whole_expression(s) {
            // One complete `{{ … }}` is the typed form: evaluating it as an
            // expression is what lets an integer stay an integer instead of
            // arriving as its own decimal spelling. It needs no template at all.
            Some(source) => {
                let expression = env
                    .compile_expression(source)
                    .map_err(|error| parse_error(error, s))?;
                let (context, missing) = self.bind(expression.undeclared_variables(true), stack)?;
                expression
                    .eval(context)
                    .map_err(|error| render_error(error, s, &missing))
            }
            None => {
                let template = env
                    .template_from_str(s)
                    .map_err(|error| parse_error(error, s))?;
                let (context, missing) = self.bind(template.undeclared_variables(true), stack)?;
                template
                    .render(context)
                    .map(Value::from)
                    .map_err(|error| render_error(error, s, &missing))
            }
        }
    }

    /// Build the context one template needs: every path it names, resolved.
    ///
    /// Resolving only what the template reads is what keeps resolution lazy — an
    /// unrelated entry that is cyclic or unresolvable stays that way, exactly as
    /// it would if nothing ever asked for it.
    ///
    /// A path the context does not hold is left *out* of the returned context
    /// rather than reported here, so `{{ maybe | default('x') }}` still works;
    /// strict mode then fails the render, and the names collected here are what
    /// turn that failure back into the [`TemplateError::UnknownPath`] callers
    /// match on.
    fn bind(
        &self,
        paths: std::collections::HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(Value, Vec<String>), TemplateError> {
        let mut context = Bindings::new();
        let mut missing = Vec::new();
        // Sorted, so a shorter path is bound before the longer ones that refine it
        // and the reported name does not vary run to run.
        for path in paths.into_iter().collect::<BTreeSet<_>>() {
            match self.resolve_path(&path, stack) {
                Ok(value) => insert_path(&mut context, &path, value),
                // What could not be resolved is the path the *error* names, not the
                // one asked for: resolving `env.PATH` may fail on the `env.TOOLS`
                // its value references, and that is the name worth reporting.
                Err(TemplateError::UnknownPath(unresolved)) => missing.push(unresolved),
                Err(other) => return Err(other),
            }
        }
        Ok((Value::from(context), missing))
    }

    fn resolve_path(&self, path: &str, stack: &mut Vec<String>) -> Result<Value, TemplateError> {
        if let Some(hit) = self.cache.borrow().get(path) {
            return Ok(hit.clone());
        }
        if stack.iter().any(|p| p == path) {
            stack.push(path.to_owned());
            return Err(TemplateError::Cycle(stack.join(" -> ")));
        }
        let raw = lookup_raw(&self.root, path)?;
        stack.push(path.to_owned());
        let resolved = self.resolve_value(&raw, stack);
        stack.pop();
        let resolved = resolved?;
        self.cache
            .borrow_mut()
            .insert(path.to_owned(), resolved.clone());
        Ok(resolved)
    }
}

/// Insert `value` at a dotted `path`, creating the intermediate maps.
///
/// An intermediate component that is not a map is replaced by one: the
/// alternative is dropping the insert, and a context built from a silently
/// discarded write is harder to debug than one that overwrites.
pub fn insert_path(context: &mut Bindings, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            context.insert(path.to_owned(), value);
        }
        Some((head, rest)) => {
            let mut child = context.remove(head).map(entries).unwrap_or_default();
            insert_path(&mut child, rest, value);
            context.insert(head.to_owned(), Value::from(child));
        }
    }
}

/// A map value's entries, or an empty map for anything else. Guarding on the kind
/// matters: Jinja iterates a string by character, so an unguarded walk would turn
/// a scalar into a map of letters.
fn entries(value: Value) -> Bindings {
    if value.kind() != ValueKind::Map {
        return Bindings::new();
    }
    let Ok(keys) = value.try_iter() else {
        return Bindings::new();
    };
    keys.filter_map(|key| Some((key.to_string(), value.get_item(&key).ok()?)))
        .collect()
}

/// Convert parsed TOML into a template value.
pub fn from_toml(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::from(s.as_str()),
        toml::Value::Integer(n) => Value::from(*n),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::from(*b),
        // Jinja has no datetime, and a config that mentions one wants to print it;
        // its TOML spelling is that text.
        toml::Value::Datetime(d) => Value::from(d.to_string()),
        toml::Value::Array(items) => Value::from(items.iter().map(from_toml).collect::<Vec<_>>()),
        toml::Value::Table(table) => Value::from(
            table
                .iter()
                .map(|(k, v)| (k.clone(), from_toml(v)))
                .collect::<Bindings>(),
        ),
    }
}

/// If `s` is exactly `{{ … }}` and nothing else, the expression inside it. This is
/// what distinguishes the typed form from a template that merely contains a
/// placeholder.
fn whole_expression(s: &str) -> Option<&str> {
    let inner = s.trim().strip_prefix("{{")?.strip_suffix("}}")?;
    // Reject a second opener, so `{{a}}{{b}}` renders as text rather than being
    // mistaken for one expression.
    if inner.contains("{{") || inner.contains("{%") {
        None
    } else {
        Some(inner)
    }
}

fn parse_error(error: Error, source: &str) -> TemplateError {
    TemplateError::Render(format!("{error} in '{source}'"))
}

/// Attribute the failure of a render to the missing path that best explains it.
///
/// Strict mode reports an undefined value without naming the path that produced
/// it, and callers key real decisions on that name, so the paths [`Ctx::bind`]
/// could not resolve are what answer it.
fn render_error(error: Error, source: &str, missing: &[String]) -> TemplateError {
    match (error.kind(), missing.first()) {
        (ErrorKind::UndefinedError, Some(path)) => TemplateError::UnknownPath(path.clone()),
        _ => TemplateError::Render(format!("{error} in '{source}'")),
    }
}

/// A container that reports a kind it then refuses to be walked as. Nothing this
/// module builds can do that, but [`Ctx::resolve`] takes any `Value`, including
/// one a caller backed with a dynamic object, so it is reported rather than
/// silently skipped.
fn walk_error(error: Error) -> TemplateError {
    TemplateError::Render(format!("walking template context: {error}"))
}

fn lookup_raw(root: &Bindings, path: &str) -> Result<Value, TemplateError> {
    let (head, rest) = match path.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (path, None),
    };
    let mut current = root
        .get(head)
        .cloned()
        .ok_or_else(|| TemplateError::UnknownPath(path.to_owned()))?;
    for part in rest.into_iter().flat_map(|rest| rest.split('.')) {
        let next = match current.kind() {
            ValueKind::Seq => part
                .parse::<usize>()
                .ok()
                .and_then(|index| current.get_item_by_index(index).ok()),
            ValueKind::Map => current.get_attr(part).ok(),
            _ => None,
        };
        match next {
            Some(value) if !value.is_undefined() => current = value,
            _ => return Err(TemplateError::UnknownPath(path.to_owned())),
        }
    }
    Ok(current)
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
        let val = from_toml(v);
        ctx.set(&format!("{ns}.{k}"), val.clone());
        ctx.set_env(k, value_to_string(&val));
    }
    // Resolve every entry before writing any back: a write clears the memo, so
    // interleaving would discard the resolutions of the entries still to come.
    let resolved: Vec<(String, String)> = raw
        .keys()
        .map(|k| Ok((k.clone(), value_to_string(&ctx.get(&format!("env.{k}"))?))))
        .collect::<Result<_>>()?;
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
        ctx.set(&format!("{ns}.{k}"), from_toml(v));
    }
    for k in raw.keys() {
        let value = ctx.get(&format!("{ns}.{k}"))?;
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
    Value::from(
        std::env::vars()
            .map(|(k, v)| (k, Value::from(v)))
            .collect::<Bindings>(),
    )
}

/// Expose an org's shared values under `org.environment.*` / `org.definitions.*`
/// so templates can name them directly. This only populates the namespace —
/// folding them into a build's logical config is the pipeline's own layer, which
/// contexts built for `update`/`context` never run.
fn insert_org_namespace(root: &mut Bindings, ws: &Workspace, org: Option<&str>) {
    let Some(org) = org else { return };
    let Some(org_data) = ws.org_base(org) else {
        return;
    };
    for (k, v) in &org_data.environment {
        insert_path(root, &format!("org.environment.{k}"), from_toml(v));
    }
    for (k, v) in &org_data.definitions {
        insert_path(root, &format!("org.definitions.{k}"), from_toml(v));
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
    Value::from(BTreeMap::from([
        ("name", Value::from(name)),
        ("path", Value::from(abs)),
        ("kind", Value::from(kind.as_str())),
        (
            "main_branch",
            Value::from(repo.main_branch.clone().unwrap_or_default()),
        ),
        (
            "anchor",
            Value::from(repo.anchor.clone().unwrap_or_default()),
        ),
        ("origin", Value::from(origin)),
        ("upstream", Value::from(upstream)),
        ("mirrors", Value::from(repo.remotes.mirrors.clone())),
    ]))
}

/// The base context every full plan and per-repo render shares: `project.*`,
/// every `repos.<name>.*`, the `org.*` namespace, `system.*`, and `env.*`. `repo.*` and
/// any Profile-specific bindings (branch, build_type, toolchain, and per-repo
/// workdirs) are layered on by the caller.
fn base_context(ws: &Workspace, project: &ProjectData, focus: &str) -> Bindings {
    let mut root = Bindings::new();
    insert_path(&mut root, "project.name", Value::from(project.name.clone()));
    insert_path(
        &mut root,
        "project.org",
        Value::from(project.org.clone().unwrap_or_default()),
    );
    insert_path(&mut root, "project.focus", Value::from(focus));
    for name in project.repos.keys() {
        insert_path(
            &mut root,
            &format!("repos.{name}"),
            repo_value(project, name),
        );
    }
    insert_org_namespace(&mut root, ws, project.org.as_deref());
    insert_path(&mut root, "system", system_facts());
    insert_path(&mut root, "env", env_snapshot());
    root
}

/// A [`Ctx`] seeded with the shared base context (`project.*`, `repos.*`, `org.*`,
/// `system.*`, `env.*`) plus `repo.*` bound to `focus`. The pipeline
/// then layers branch/build_type/toolchain and the repos' resolved `workdir`s
/// onto it.
pub(crate) fn plan_base(ws: &Workspace, project: &ProjectData, focus: &str) -> Ctx {
    let mut root = base_context(ws, project, focus);
    insert_path(&mut root, "repo", repo_value(project, focus));
    Ctx::new(root)
}

/// A context sufficient to resolve a repo-scoped template (a hook, a
/// `worktree_dir`): the shared base plus `repo.*` = `repo_name`. No Profile or
/// resolved `workdir` is available, so this is safe for `update`/`context`, which
/// don't build a full plan.
pub fn context_for_repo(ws: &Workspace, project: &ProjectData, repo_name: &str) -> Bindings {
    let mut root = base_context(ws, project, project.focus_name(None));
    insert_path(&mut root, "repo", repo_value(project, repo_name));
    root
}

/// The minimal Profile-free context for resolving `repo.path` templates:
/// `project.name`, `project.org`, `system.*`, `env.*`. No `repos.*` (that would
/// be circular — a repo's path is what we are computing), and no Profile, so
/// `workspace` can answer `repo_abs_path` / `project_for_path` without a plan.
pub fn path_context(name: &str, org: Option<&str>) -> Bindings {
    let mut root = Bindings::new();
    insert_path(&mut root, "project.name", Value::from(name));
    insert_path(
        &mut root,
        "project.org",
        Value::from(org.unwrap_or_default()),
    );
    insert_path(&mut root, "system", system_facts());
    insert_path(&mut root, "env", env_snapshot());
    root
}

// --- misc ---------------------------------------------------------------------

/// Flatten a resolved value into the single string an environment variable or a
/// command-line argument has to be.
///
/// A list becomes its space-joined elements, which is what a flag list or a
/// `PATH`-shaped value wants; Jinja's own rendering (`["a", "b"]`) is for display.
/// A map has no such form, so it flattens to nothing rather than to a guess.
pub(crate) fn value_to_string(v: &Value) -> String {
    match v.kind() {
        ValueKind::Seq => v
            .try_iter()
            .map(|items| {
                items
                    .map(|item| value_to_string(&item))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        ValueKind::Map => String::new(),
        _ => v.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings() -> Bindings {
        let mut root = Bindings::new();
        insert_path(&mut root, "project.name", Value::from("mesa"));
        insert_path(&mut root, "build_type", Value::from("debug"));
        insert_path(&mut root, "system.memory.total_gb", Value::from(16));
        insert_path(&mut root, "branch.slug", Value::from("feature_x"));
        insert_path(&mut root, "repo.path", Value::from("/src/mesa"));
        // self-referential environment map
        insert_path(&mut root, "env.TOOLS", Value::from("/opt/tools"));
        insert_path(&mut root, "env.BIN", Value::from("{{env.TOOLS}}/bin"));
        insert_path(&mut root, "env.PATH", Value::from("{{env.BIN}}:/usr/bin"));
        root
    }

    fn ctx() -> Ctx {
        Ctx::new(bindings())
    }

    #[test]
    fn whole_placeholder_keeps_type() {
        assert_eq!(
            ctx().resolve_str("{{ system.memory.total_gb }}").unwrap(),
            Value::from(16)
        );
    }

    #[test]
    fn embedded_placeholder_stringifies() {
        assert_eq!(
            ctx()
                .resolve_str("{{repo.path}}/_build/{{build_type}}")
                .unwrap(),
            Value::from("/src/mesa/_build/debug")
        );
    }

    /// The reason this resolver exists: an entry whose value is a template naming
    /// another entry, which `alive2`'s config does across a whole chain.
    #[test]
    fn lazy_self_reference_resolves() {
        assert_eq!(
            ctx().get("env.PATH").unwrap(),
            Value::from("/opt/tools/bin:/usr/bin")
        );
    }

    /// Order must not matter, so a reference to an entry that sorts *later* has
    /// to resolve — the shape `alive2.toml` relies on today.
    #[test]
    fn an_entry_may_reference_one_that_sorts_after_it() {
        let mut root = Bindings::new();
        insert_path(&mut root, "env.A_FIRST", Value::from("{{env.Z_LAST}}/bin"));
        insert_path(&mut root, "env.Z_LAST", Value::from("/opt"));
        assert_eq!(
            Ctx::new(root).get("env.A_FIRST").unwrap(),
            Value::from("/opt/bin")
        );
    }

    #[test]
    fn cycle_is_detected() {
        let mut root = Bindings::new();
        insert_path(&mut root, "a", Value::from("{{b}}"));
        insert_path(&mut root, "b", Value::from("{{a}}"));
        assert!(matches!(
            Ctx::new(root).get("a"),
            Err(TemplateError::Cycle(_))
        ));
    }

    #[test]
    fn unknown_path_is_hard_error() {
        assert_eq!(
            ctx().resolve_str("{{nope.missing}}"),
            Err(TemplateError::UnknownPath("nope.missing".into()))
        );
    }

    /// The detached-worktree fallback in `resolve` keys on the exact path, so a
    /// partially-present prefix must still name what was asked for.
    #[test]
    fn unknown_path_names_the_full_path_that_was_asked_for() {
        assert_eq!(
            ctx().resolve_str("{{ branch.raw }}/x"),
            Err(TemplateError::UnknownPath("branch.raw".into()))
        );
    }

    /// A lookup that fails inside another entry reports the name that actually
    /// went missing, not the entry that referenced it.
    #[test]
    fn an_unknown_path_reached_through_another_entry_names_the_real_culprit() {
        let mut root = Bindings::new();
        insert_path(&mut root, "env.PATH", Value::from("{{env.TOOLS}}/bin"));
        assert_eq!(
            Ctx::new(root).resolve_str("{{ env.PATH }}"),
            Err(TemplateError::UnknownPath("env.TOOLS".into()))
        );
    }

    /// The no-template fast path has to agree with Jinja on every delimiter, or a
    /// value would render differently for having a placeholder beside it.
    #[test]
    fn a_comment_is_stripped_with_or_without_a_placeholder_beside_it() {
        assert_eq!(ctx().resolve_str("a{# c #}b").unwrap(), Value::from("ab"));
        assert_eq!(
            ctx().resolve_str("{{build_type}}{# c #}").unwrap(),
            Value::from("debug")
        );
    }

    /// Only the paths a template reads are resolved, so an unrelated broken entry
    /// stays dormant — which is also what keeps the process environment out of
    /// the parser.
    #[test]
    fn an_unread_cyclic_entry_does_not_poison_a_render() {
        let mut root = bindings();
        insert_path(&mut root, "loop.a", Value::from("{{loop.b}}"));
        insert_path(&mut root, "loop.b", Value::from("{{loop.a}}"));
        assert_eq!(
            Ctx::new(root).resolve_str("{{build_type}}").unwrap(),
            Value::from("debug")
        );
    }

    #[test]
    fn expressions_cover_what_the_old_bracket_sublanguage_did() {
        let ctx = ctx();
        assert_eq!(
            ctx.resolve_str("{{ [1, system.memory.total_gb // 4] | max }}")
                .unwrap(),
            Value::from(4)
        );
        assert_eq!(ctx.resolve_str("{{ 1 + 2 * 3 }}").unwrap(), Value::from(7));
        assert_eq!(
            ctx.resolve_str("{{ (1 + 2) * 3 }}").unwrap(),
            Value::from(9)
        );
        assert_eq!(ctx.resolve_str("{{ 7 % 3 }}").unwrap(), Value::from(1));
        assert_eq!(
            ctx.resolve_str("{{ build_type == \"debug\" }}").unwrap(),
            Value::from(true)
        );
        assert_eq!(ctx.resolve_str("{{ 3.9 | int }}").unwrap(), Value::from(3));
    }

    #[test]
    fn division_by_zero_is_error() {
        assert!(matches!(
            ctx().resolve_str("{{ 1 // 0 }}"),
            Err(TemplateError::Render(_))
        ));
    }

    #[test]
    fn loops_and_conditionals_reach_config_too() {
        let mut root = Bindings::new();
        insert_path(&mut root, "repo.mirrors", Value::from(vec!["a", "b", "c"]));
        assert_eq!(
            Ctx::new(root)
                .resolve_str("{% for m in repo.mirrors %}{{ m }};{% endfor %}")
                .unwrap(),
            Value::from("a;b;c;")
        );
    }

    /// `repos.my-repo` is a subtraction to Jinja, so a hyphenated key is reached
    /// by subscript. Repo names take hyphens routinely, so this has to work.
    #[test]
    fn a_hyphenated_key_is_reached_by_subscript() {
        let mut root = Bindings::new();
        insert_path(&mut root, "repos.my-repo.workdir", Value::from("/w"));
        assert_eq!(
            Ctx::new(root)
                .resolve_str("{{ repos['my-repo'].workdir }}")
                .unwrap(),
            Value::from("/w")
        );
    }

    /// The one invariant that made merging the resolver into the context worth
    /// doing: a write has to retire the memo that described the old tree.
    #[test]
    fn a_write_retires_the_memo_it_invalidates() {
        let mut ctx = ctx();
        assert_eq!(ctx.render("{{build_type}}").unwrap(), "debug");
        ctx.set("build_type", Value::from("release"));
        assert_eq!(ctx.render("{{build_type}}").unwrap(), "release");
        // Including a memo reached indirectly, through another entry.
        assert_eq!(
            ctx.render("{{env.PATH}}").unwrap(),
            "/opt/tools/bin:/usr/bin"
        );
        ctx.set_env("TOOLS", "/usr/local".into());
        assert_eq!(
            ctx.render("{{env.PATH}}").unwrap(),
            "/usr/local/bin:/usr/bin"
        );
    }

    #[test]
    fn a_path_template_refuses_to_flatten_a_collection() {
        let mut root = Bindings::new();
        insert_path(&mut root, "repo.mirrors", Value::from(vec!["a", "b"]));
        let ctx = Ctx::new(root);
        // `render` joins, because an argument list legitimately wants that…
        assert_eq!(ctx.render("{{repo.mirrors}}").unwrap(), "a b");
        // …but a path built from a list is always a mistake.
        assert!(matches!(
            ctx.render_path("{{repo.mirrors}}"),
            Err(TemplateError::NotAString { .. })
        ));
    }

    #[test]
    fn insert_path_replaces_a_scalar_standing_where_a_map_belongs() {
        let mut root = Bindings::new();
        insert_path(&mut root, "a", Value::from("scalar"));
        insert_path(&mut root, "a.b", Value::from(1));
        assert_eq!(Ctx::new(root).get("a.b").unwrap(), Value::from(1));
    }

    #[test]
    fn from_toml_carries_the_scalar_types_across() {
        let table: toml::Value =
            toml::from_str("i = 1\nf = 1.5\nb = true\ns = \"x\"\nl = [1, 2]").expect("valid toml");
        let mut root = Bindings::new();
        insert_path(&mut root, "t", from_toml(&table));
        let ctx = Ctx::new(root);
        assert_eq!(ctx.get("t.i").unwrap(), Value::from(1));
        assert_eq!(ctx.get("t.f").unwrap(), Value::from(1.5));
        assert_eq!(ctx.get("t.b").unwrap(), Value::from(true));
        assert_eq!(ctx.get("t.s").unwrap(), Value::from("x"));
        assert_eq!(ctx.get("t.l").unwrap(), Value::from(vec![1, 2]));
    }
}
