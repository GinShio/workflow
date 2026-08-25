//! Turn a catalogue plus a descriptor into a list of edits.
//!
//! This stage is where the two configurable halves meet: the catalogue says
//! *where*, the descriptor says *what*, and the template engine renders one into
//! the other. It reads no files and writes none — every edit it produces carries
//! a compiled anchor that [`crate::emit`] resolves later — so the whole stage is
//! testable without a target tree on disk.
//!
//! Nothing here knows what any particular tree wants said. Wrapper spelling
//! comes from the catalogue, per-kind metadata rides on the descriptor, and the
//! engine's own vocabulary lives in [`crate::render`].

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use minijinja::{Environment, Value};

use crate::anchor::{self, Anchor, AnchorSpec};
use crate::catalog::{Catalog, RuleSpec, WrapperSpec};
use crate::model::Extension;
use crate::render;

/// One pending change to one file.
#[derive(Debug)]
pub struct Edit {
    /// Path relative to the target tree's root.
    pub path: String,
    /// Human label for the report, already rendered.
    pub what: String,
    /// Exactly the bytes to insert, wrapper text included.
    pub text: String,
    pub action: Action,
}

#[derive(Debug)]
pub enum Action {
    /// Create the file; `text` is the whole content.
    Create,
    /// Splice `text` in at the anchor. `sort_line` is the first line of the
    /// unwrapped body, which a sorted anchor reads the new key from.
    Insert {
        anchor: Anchor,
        sort_line: Option<String>,
    },
}

/// Per-run knobs that override the catalogue.
#[derive(Debug, Default)]
pub struct Options {
    /// `--var name=value`, layered over the catalogue's `[target.vars]`. A name
    /// present in neither is an error wherever a template reads it — see
    /// [`crate::render::require_vars`].
    pub vars: BTreeMap<String, String>,
    pub overlay: crate::overlay::Overlay,
}

/// Every `var.*` this run has a value for: the catalogue's defaults with `--var`
/// layered on top.
///
/// A name defined here only by `--var` is exactly the "no sensible default, the
/// caller must choose" case, so it needs no separate namespace: leave it out of
/// the catalogue and the run cannot proceed without the flag.
fn defined_vars(catalog: &Catalog, opts: &Options) -> BTreeMap<String, String> {
    let mut vars = catalog.target.vars.clone();
    vars.extend(opts.vars.clone());
    vars
}

/// Everything one build holds constant, so the stages below take one parameter
/// instead of five and cannot be handed a mismatched set.
struct Build<'a> {
    catalog: &'a Catalog,
    env: &'a Environment<'a>,
    /// Every `var.*` this run has a value for, for the requirement check. The
    /// same map the context is built from, so the check and the render cannot
    /// disagree about what is defined.
    vars: BTreeMap<String, String>,
}

/// Build every edit one catalogue asks for.
pub fn build(catalog: &Catalog, ext: &Extension, opts: &Options) -> Result<Vec<Edit>> {
    let ext = opts
        .overlay
        .apply(&catalog.target.name, ext)
        .with_context(|| format!("target '{}' overlay", catalog.target.name))?;
    if !ext.has_plane(catalog.target.plane) {
        bail!(
            "target '{}' consumes the {} plane, which this descriptor does not have",
            catalog.target.name,
            catalog.target.plane
        );
    }

    let env = render::environment();
    let base = context(catalog, &ext, opts, &env)?;
    let build = Build {
        catalog,
        env: &env,
        vars: defined_vars(catalog, opts),
    };

    let mut edits = Vec::new();
    for rule in &catalog.rules {
        edits.extend(expand(&build, rule, &base).with_context(|| format!("rule '{}'", rule.what))?);
    }
    Ok(edits)
}

/// The context for anything resolved before a descriptor is involved — a target's
/// tree root, which must not depend on which extension is being added.
///
/// Only the variables the root itself reads are rendered. Rendering all of them
/// would fail on values that legitimately read the descriptor, which is not in
/// scope at this point.
pub fn target_context(catalog: &Catalog, opts: &Options, env: &Environment<'_>) -> Result<Value> {
    let root = catalog.target.root.as_str();
    let vars = defined_vars(catalog, opts);
    render::require_vars(env, &[root], &vars)
        .with_context(|| format!("target '{}' root", catalog.target.name))?;

    let mut ctx = BTreeMap::new();
    let wanted = render::referenced(env, root, "var");
    publish_vars(&mut ctx, &vars, env, &Value::from(()), Some(&wanted))?;
    Ok(Value::from(ctx))
}

/// One rule becomes one edit, or one per element of its `repeat` collection.
fn expand(build: &Build<'_>, rule: &RuleSpec, base: &BTreeMap<String, Value>) -> Result<Vec<Edit>> {
    // Check `var.*` first so a missing run input gets the dedicated catalogue/
    // command-line diagnostic rather than a generic strict-template error.
    if let Some(condition) = &rule.when {
        render::require_vars(build.env, &[condition.as_str()], &build.vars)?;
    }
    if !applies(rule, build.env, base)? {
        return Ok(Vec::new());
    }
    // Only once the rule is known to be live: one that `when` dropped must not
    // demand values for text it will never emit.
    render::require_vars(build.env, &rule.templates(), &build.vars)?;

    let Some(repeat) = &rule.repeat else {
        return Ok(emit_one(build, rule, base)?.into_iter().collect());
    };

    let collection = lookup(base, &repeat.over).with_context(|| {
        format!(
            "'repeat.over = {}' names nothing in the context",
            repeat.over
        )
    })?;
    let items = collection
        .try_iter()
        .with_context(|| format!("'repeat.over = {}' is not a collection", repeat.over))?;

    let mut edits = Vec::new();
    for item in items {
        let mut scoped = base.clone();
        scoped.insert(repeat.binding.clone(), item);
        edits.extend(emit_one(build, rule, &scoped)?);
    }
    Ok(edits)
}

/// Whether a rule's `when` condition holds.
///
/// Empty strings, empty lists, `false`, and `0` are false; everything else is
/// true.
fn applies(rule: &RuleSpec, env: &Environment<'_>, ctx: &BTreeMap<String, Value>) -> Result<bool> {
    let Some(condition) = &rule.when else {
        return Ok(true);
    };
    render::truthy(env, condition, &Value::from(ctx.clone()))
}

/// Render one rule against one context, or nothing if it has nothing to say.
///
/// An empty body is not an error and not an empty insertion: it means this rule
/// contributes nothing here. A fan-out reaches that case routinely — a rule that
/// emits a line only for entries declaring a dependency renders to nothing for a
/// group where none do, while its siblings render normally. Emitting it anyway
/// would wrap nothing around no text.
fn emit_one(
    build: &Build<'_>,
    rule: &RuleSpec,
    ctx: &BTreeMap<String, Value>,
) -> Result<Option<Edit>> {
    let value = Value::from(ctx.clone());
    let text = |template: &str| -> Result<String> { render::one(build.env, template, &value) };

    let what = text(&rule.what)?;
    let path = text(&rule.path)?;
    let body = text(&rule.body)?;
    if body.trim().is_empty() {
        return Ok(None);
    }

    let wrapper_templates = rule.wrap.as_ref().unwrap_or(&build.catalog.target.wrap);
    let wrapper_refs: Vec<&str> = wrapper_templates.iter().map(String::as_str).collect();
    render::require_vars(build.env, &wrapper_refs, &build.vars)?;
    let wrappers = render_wrappers(wrapper_templates, build.env, &value)?;
    if !wrappers.is_empty() {
        if let Some(wrapper) = build.catalog.target.wrapper.as_ref() {
            render::require_vars(
                build.env,
                &[wrapper.open.as_str(), wrapper.close.as_str()],
                &build.vars,
            )?;
        }
    }
    let wrapped = surround(&body, &wrappers, build.catalog.target.wrapper.as_ref(), ctx)
        .with_context(|| format!("rule '{what}' cannot be wrapped"))?;

    if rule.create {
        return Ok(Some(Edit {
            path,
            what,
            text: wrapped,
            action: Action::Create,
        }));
    }

    let attached_before = if rule.sorted.is_some() {
        build
            .catalog
            .target
            .wrapper
            .as_ref()
            .and_then(|wrapper| wrapper.open_pattern.as_deref())
            .map(|pattern| {
                render::require_vars(build.env, &[pattern], &build.vars)?;
                text(pattern)
            })
            .transpose()
            .context("target wrapper opener pattern")?
    } else {
        None
    };
    let spec = AnchorSpec {
        eof: rule.eof,
        scope: rule
            .scope
            .iter()
            .map(|p| text(p))
            .collect::<Result<Vec<_>>>()?,
        close: rule.before.as_deref().map(text).transpose()?,
        after_last: rule.after_last.as_deref().map(text).transpose()?,
        key: rule.sorted.as_deref().map(text).transpose()?,
        group: rule.section.as_deref().map(text).transpose()?,
        attached_before,
    };
    let sort_line = rule
        .sorted
        .is_some()
        .then(|| first_line(&body))
        .flatten()
        .map(str::to_owned);

    Ok(Some(Edit {
        path,
        what,
        text: wrapped,
        action: Action::Insert {
            anchor: anchor::compile(&spec)?,
            sort_line,
        },
    }))
}

/// Render wrapper values, dropping empty ones.
fn render_wrappers(
    templates: &[String],
    env: &Environment<'_>,
    ctx: &Value,
) -> Result<Vec<String>> {
    let mut values = Vec::with_capacity(templates.len());
    for template in templates {
        let rendered = render::one(env, template, ctx)?;
        if !rendered.is_empty() {
            values.push(rendered);
        }
    }
    Ok(values)
}

/// Surround a body with nested wrappers, outermost value first.
fn surround(
    body: &str,
    values: &[String],
    wrapper: Option<&WrapperSpec>,
    ctx: &BTreeMap<String, Value>,
) -> Result<String> {
    if values.is_empty() {
        return Ok(body.to_owned());
    }
    let Some(wrapper) = wrapper else {
        bail!(
            "this rule has wrapper values {}, but the target declares no [target.wrapper]",
            values.join(", ")
        );
    };
    if !body.ends_with('\n') {
        bail!("a wrapped body must end with a newline");
    }

    let env = render::environment();
    let bind = |item: &String| {
        let mut scoped = ctx.clone();
        scoped.insert("item".to_owned(), Value::from(item.clone()));
        Value::from(scoped)
    };
    let mut out = String::with_capacity(body.len() + values.len() * 32);
    for item in values {
        out.push_str(&render::one(&env, &wrapper.open, &bind(item))?);
    }
    out.push_str(body);
    for item in values.iter().rev() {
        out.push_str(&render::one(&env, &wrapper.close, &bind(item))?);
    }
    Ok(out)
}

/// Assemble the render context: the descriptor's planes, plus the target's own
/// facts under separate names so a template never has to guess which tree it is
/// rendering for.
fn context(
    catalog: &Catalog,
    ext: &Extension,
    opts: &Options,
    env: &Environment<'_>,
) -> Result<BTreeMap<String, Value>> {
    let mut ctx = BTreeMap::new();
    if let Some(spv) = &ext.spv {
        ctx.insert("spv".to_owned(), Value::from_serialize(spv));
    }
    if let Some(vk) = &ext.vk {
        ctx.insert("vk".to_owned(), Value::from_serialize(vk));
    }

    // Variables may themselves be templates over the descriptor, so they are
    // rendered before being published. The context they see deliberately lacks
    // `var`, which both prevents a self-reference and keeps the resolution order
    // one pass instead of a fixpoint.
    let vars = defined_vars(catalog, opts);
    let planes = Value::from(ctx.clone());
    publish_vars(&mut ctx, &vars, env, &planes, None)?;
    ctx.insert(
        "target".to_owned(),
        Value::from_serialize(BTreeMap::from([
            ("name", catalog.target.name.clone()),
            ("plane", catalog.target.plane.to_string()),
        ])),
    );
    Ok(ctx)
}

/// Render the defined variables and publish them as `var.*`.
///
/// `seen` is the context they may read, which never includes `var` itself: that
/// both prevents a self-reference and keeps resolution one pass instead of a
/// fixpoint. `only` restricts the set to those names, for a caller that cannot
/// yet render the rest.
fn publish_vars(
    ctx: &mut BTreeMap<String, Value>,
    defined: &BTreeMap<String, String>,
    env: &Environment<'_>,
    seen: &Value,
    only: Option<&std::collections::BTreeSet<String>>,
) -> Result<()> {
    let mut vars = defined.clone();
    if let Some(only) = only {
        vars.retain(|name, _| only.contains(name));
    }
    for (name, raw) in vars.iter_mut() {
        *raw = render::one(env, raw, seen).with_context(|| format!("target variable '{name}'"))?;
    }
    ctx.insert("var".to_owned(), Value::from_serialize(&vars));
    Ok(())
}

/// Resolve a dotted path in the context, or `None` if any step is missing.
fn lookup(ctx: &BTreeMap<String, Value>, path: &str) -> Option<Value> {
    let mut parts = path.split('.');
    let mut current = ctx.get(parts.next()?)?.clone();
    for part in parts {
        let next = current.get_attr(part).ok()?;
        if next.is_undefined() {
            return None;
        }
        current = next;
    }
    Some(current)
}

fn first_line(body: &str) -> Option<&str> {
    body.lines().find(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Enumerant, KindGroup, SpvOpcode, SpvPlane};

    /// A descriptor with two operand kinds so a fan-out has more than one
    /// element, and with one entry that declares a dependency and one that does
    /// not, which is the shape the filtering rules are written against.
    fn descriptor() -> Extension {
        let mut spv = SpvPlane::new("SPV_TEST_widget");
        spv.operations.push(SpvOpcode {
            name: "OpWidgetLoadTEST".into(),
            aliases: Vec::new(),
            value: 5451,
            class: "Arithmetic".into(),
            operands: Vec::new(),
            capabilities: vec!["WidgetTEST".into()],
            encoding: crate::model::SpvEncoding::default(),
            meta: Default::default(),
        });
        spv.kinds.push(KindGroup {
            name: "Alpha".into(),
            meta: BTreeMap::from([
                ("label".into(), toml::Value::String("A".into())),
                ("scope".into(), toml::Value::String("First".into())),
                ("qualified".into(), toml::Value::Boolean(false)),
            ]),
            enumerants: vec![
                Enumerant {
                    name: "WidgetTEST".into(),
                    aliases: Vec::new(),
                    value: 5454,
                    requires: vec![],
                },
                Enumerant {
                    name: "WidgetPlusTEST".into(),
                    aliases: Vec::new(),
                    value: 5455,
                    requires: vec!["WidgetTEST".into(), "BaseTEST".into()],
                },
            ],
        });
        spv.kinds.push(KindGroup {
            name: "Bravo".into(),
            meta: BTreeMap::from([
                ("label".into(), toml::Value::String("B".into())),
                ("scope".into(), toml::Value::String("Second".into())),
                ("qualified".into(), toml::Value::Boolean(true)),
            ]),
            enumerants: vec![Enumerant {
                name: "WidgetIdTEST".into(),
                aliases: Vec::new(),
                value: 5460,
                requires: vec!["WidgetTEST".into()],
            }],
        });
        Extension {
            spv: Some(spv),
            ..Default::default()
        }
    }

    const TARGET: &str = concat!(
        "[target]\nname = \"alpha\"\nroot = \"/fixtures/alpha\"\nplane = \"spv\"\n",
        "wrap = [\"{{ var.frame }}\"]\n",
        "[target.wrapper]\nopen = \"BEGIN {{ item }}\\n\"\nclose = \"END {{ item }}\\n\"\n",
        "open_pattern = \"^BEGIN \"\n",
        "[target.vars]\nframe = \"FRAME_{{ spv.feature }}\"\ndefault = \"ON\"\n",
    );

    fn catalog_of(rules: &str) -> Catalog {
        Catalog::parse(&format!("{TARGET}{rules}")).unwrap()
    }

    fn only(catalog: &Catalog, ext: &Extension, opts: &Options) -> Edit {
        let mut edits = build(catalog, ext, opts).unwrap();
        assert_eq!(edits.len(), 1, "expected exactly one edit");
        edits.pop().unwrap()
    }

    #[test]
    fn a_plain_rule_uses_the_target_wrapper() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"name string\"\npath = \"list.txt\"\neof = true\n\
             body = \"{{ spv.name }}\\n\"\n",
        );
        let edit = only(&catalog, &descriptor(), &Options::default());
        assert_eq!(edit.path, "list.txt");
        assert_eq!(
            edit.text,
            "BEGIN FRAME_TEST_WIDGET\nSPV_TEST_widget\nEND FRAME_TEST_WIDGET\n"
        );
    }

    #[test]
    fn wrapper_spelling_comes_from_the_target_not_the_engine() {
        let catalog = Catalog::parse(
            "[target]\nname = \"alpha\"\nroot = \"/t\"\nplane = \"spv\"\nwrap = [\"FRAME\"]\n\
             [target.wrapper]\nopen = \"; begin {{ target.name }} {{ item }}\\n\"\n\
             close = \"; end {{ item }}\\n\"\n\
             [[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"x\\n\"\n",
        )
        .unwrap();
        let edit = only(&catalog, &descriptor(), &Options::default());
        assert_eq!(edit.text, "; begin alpha FRAME\nx\n; end FRAME\n");
    }

    #[test]
    fn wrapper_values_without_a_spelling_are_refused() {
        let catalog = Catalog::parse(
            "[target]\nname = \"alpha\"\nroot = \"/t\"\nplane = \"spv\"\nwrap = [\"FRAME\"]\n\
             [[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"x\\n\"\n",
        )
        .unwrap();
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("[target.wrapper]"), "got: {err}");
    }

    #[test]
    fn an_empty_variable_drops_the_default_wrapper() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"name string\"\npath = \"f\"\neof = true\nbody = \"{{ spv.name }}\\n\"\n",
        );
        let opts = Options {
            vars: BTreeMap::from([("frame".to_owned(), String::new())]),
            ..Default::default()
        };
        assert_eq!(
            only(&catalog, &descriptor(), &opts).text,
            "SPV_TEST_widget\n"
        );
    }

    #[test]
    fn an_empty_override_leaves_a_rule_unwrapped() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"entry\"\npath = \"f.json\"\neof = true\nwrap = []\n\
             body = \"{{ spv.feature }}\\n\"\n",
        );
        assert_eq!(
            only(&catalog, &descriptor(), &Options::default()).text,
            "TEST_WIDGET\n"
        );
    }

    #[test]
    fn an_explicit_wrapper_list_nests_outermost_first() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"arm\"\npath = \"f.txt\"\neof = true\n\
             wrap = [\"{{ var.frame }}\", \"SECOND\"]\nbody = \"x\\n\"\n",
        );
        assert_eq!(
            only(&catalog, &descriptor(), &Options::default()).text,
            "BEGIN FRAME_TEST_WIDGET\nBEGIN SECOND\nx\nEND SECOND\nEND FRAME_TEST_WIDGET\n"
        );
    }

    #[test]
    fn an_empty_wrapper_value_drops_out_of_an_explicit_list() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"arm\"\npath = \"f.txt\"\neof = true\n\
             wrap = [\"{{ var.frame }}\", \"SECOND\"]\nbody = \"x\\n\"\n",
        );
        let opts = Options {
            vars: BTreeMap::from([("frame".to_owned(), String::new())]),
            ..Default::default()
        };
        assert_eq!(
            only(&catalog, &descriptor(), &opts).text,
            "BEGIN SECOND\nx\nEND SECOND\n"
        );
    }

    #[test]
    fn repeat_produces_one_edit_per_element_with_its_own_anchor() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"{{ kind.name }} section\"\npath = \"items.txt\"\n\
             repeat = { over = \"spv.kinds\", as = \"kind\" }\n\
             scope = ['^section {{ kind.name }}$']\nbefore = '^end$'\n\
             body = \"\"\"\n{%- for e in kind.enumerants %}\n  add({{ e.name }});\n{%- endfor %}\n\"\"\"\n",
        );
        let edits = build(&catalog, &descriptor(), &Options::default()).unwrap();
        assert_eq!(edits.len(), 2, "one per operand kind");
        assert_eq!(edits[0].what, "Alpha section");
        assert_eq!(edits[1].what, "Bravo section");
        // Two entries in the first group, one in the second.
        assert_eq!(edits[0].text.matches("add(").count(), 2);
        assert_eq!(edits[1].text.matches("add(").count(), 1);
    }

    #[test]
    fn per_kind_metadata_stays_opaque_to_the_engine() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"m\"\npath = \"items.txt\"\n\
             repeat = { over = \"spv.kinds\", as = \"kind\" }\n\
             scope = ['x']\nbefore = '^end$'\n\
             body = \"\"\"\n{%- for e in kind.enumerants %}\n  add(\"{% if kind.meta.qualified %}{{ kind.meta.label }}{% endif %}{{ e.name }}\");\n{%- endfor %}\n\"\"\"\n",
        );
        let edits = build(&catalog, &descriptor(), &Options::default()).unwrap();
        assert!(edits[0].text.contains(r#"add("WidgetTEST")"#));
        assert!(edits[1].text.contains(r#"add("BWidgetIdTEST")"#));
    }

    #[test]
    fn the_prefix_filter_builds_a_list_initialiser() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"reqs\"\npath = \"items.txt\"\n\
             repeat = { over = \"spv.kinds\", as = \"kind\" }\n\
             scope = ['^section {{ kind.meta.scope }}$']\nbefore = '^end$'\n\
             body = \"\"\"\n{%- for e in kind.enumerants if e.requires %}\n  REQ({{ e.name }}, {{ '{' }}{{ e.requires | prefix('Alpha') | join(', ') }}{{ '}' }});\n{%- endfor %}\n\"\"\"\n",
        );
        let edits = build(&catalog, &descriptor(), &Options::default()).unwrap();
        assert!(edits[0]
            .text
            .contains("REQ(WidgetPlusTEST, {AlphaWidgetTEST, AlphaBaseTEST});"));
    }

    #[test]
    fn strip_prefix_drops_only_a_leading_match() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"op\"\npath = \"op.h\"\neof = true\n\
             body = \"{% for i in spv.operations %}{{ i.name | strip_prefix('Op') }}\\n{% endfor %}\"\n",
        );
        assert!(only(&catalog, &descriptor(), &Options::default())
            .text
            .contains("WidgetLoadTEST"));
    }

    #[test]
    fn target_vars_are_readable_and_overridable() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"entry\"\npath = \"settings.txt\"\neof = true\n\
             body = \"entry({{ var.frame }} {{ var.default }})\\n\"\n",
        );
        assert!(only(&catalog, &descriptor(), &Options::default())
            .text
            .contains("entry(FRAME_TEST_WIDGET ON)"));

        let opts = Options {
            vars: BTreeMap::from([("default".to_owned(), "OFF".to_owned())]),
            ..Default::default()
        };
        assert!(only(&catalog, &descriptor(), &opts).text.contains(" OFF)"));
    }

    #[test]
    fn a_target_variable_may_itself_be_a_template() {
        let catalog = Catalog::parse(
            "[target]\nname = \"alpha\"\nroot = \"/t\"\nplane = \"spv\"\n\
             [target.vars]\nblurb = \"Support {{ spv.name }}\"\n\
             [[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"{{ var.blurb }}\\n\"\n",
        )
        .unwrap();
        assert_eq!(
            only(&catalog, &descriptor(), &Options::default()).text,
            "Support SPV_TEST_widget\n"
        );
    }

    #[test]
    fn a_var_with_no_catalogue_default_is_readable_once_passed() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nwrap = []\n\
             body = \"limit = {{ var.limit }}\\n\"\n",
        );
        let opts = Options {
            vars: BTreeMap::from([("limit".to_owned(), "64".to_owned())]),
            ..Default::default()
        };
        assert_eq!(only(&catalog, &descriptor(), &opts).text, "limit = 64\n");
    }

    #[test]
    fn a_rule_reading_an_undefined_variable_stops_the_run() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nwrap = []\n\
             body = \"limit = {{ var.limit }}\\n\"\n",
        );
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("--var limit="), "got: {err}");
    }

    #[test]
    fn a_dropped_rule_does_not_demand_values_for_text_it_will_never_emit() {
        // The check runs after `when`, which is the whole reason it runs per rule
        // rather than over the catalogue up front.
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nwrap = []\n\
             when = \"{{ var.frame }}\"\nbody = \"{{ var.limit }}\\n\"\n",
        );
        let disabled = Options {
            vars: BTreeMap::from([("frame".to_owned(), String::new())]),
            ..Default::default()
        };
        assert!(build(&catalog, &descriptor(), &disabled)
            .unwrap()
            .is_empty());
        // Live again, and now the value really is needed.
        assert!(build(&catalog, &descriptor(), &Options::default()).is_err());
    }

    #[test]
    fn a_dropped_rule_does_not_render_its_default_wrapper() {
        let catalog = Catalog::parse(
            "[target]\nname = \"alpha\"\nroot = \"/t\"\nplane = \"spv\"\n\
             wrap = [\"{{ var.missing_value }}\"]\n\
             [target.wrapper]\nopen = \"{{ var.missing_open }} {{ item }}\\n\"\nclose = \"END\\n\"\n\
             [target.vars]\nenabled = \"\"\n\
             [[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\n\
             when = \"{{ var.enabled }}\"\nbody = \"x\\n\"\n",
        )
        .unwrap();
        assert!(build(&catalog, &descriptor(), &Options::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_when_reading_an_undefined_variable_reports_the_required_flag() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\n\
             when = \"{{ var.enable }}\"\nbody = \"x\\n\"\n",
        );
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("--var enable="), "got: {err}");
    }

    #[test]
    fn a_when_reading_an_unknown_descriptor_path_is_an_error() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\n\
             when = \"{{ spv.no_such_collection }}\"\nbody = \"x\\n\"\n",
        );
        assert!(build(&catalog, &descriptor(), &Options::default()).is_err());
    }

    #[test]
    fn a_sorted_rule_carries_the_unwrapped_first_line() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"id\"\npath = \"ids.h\"\n\
             scope = ['^enum Id$']\nbefore = '^\\};$'\n\
             sorted = '^\\s*([A-Z0-9_]+),'\nsection = '^([A-Z]+)_'\n\
             body = \"        {{ spv.feature }},\\n\"\n",
        );
        let edit = only(&catalog, &descriptor(), &Options::default());
        let Action::Insert { sort_line, .. } = &edit.action else {
            panic!("expected an insertion");
        };
        // Wrapper lines must not become the sort key.
        assert_eq!(sort_line.as_deref(), Some("        TEST_WIDGET,"));
    }

    #[test]
    fn target_open_pattern_reaches_a_sorted_anchor() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"id\"\npath = \"items.txt\"\n\
             scope = ['^enum Id$']\nbefore = '^\\};$'\n\
             sorted = '^\\s*([A-Z0-9_]+),'\n\
             body = \"        BRAVO,\\n\"\n",
        );
        let edit = only(&catalog, &descriptor(), &Options::default());
        let Action::Insert { anchor, sort_line } = &edit.action else {
            panic!("expected an insertion");
        };
        let text = "enum Id\n        ALPHA,\nBEGIN FRAME_OLD\n        MIKE,\nEND FRAME_OLD\n};\n";
        let placed = anchor.locate(text, sort_line.as_deref()).unwrap();
        assert!(text[..placed.offset].ends_with("        ALPHA,\n"));
    }

    #[test]
    fn create_makes_the_body_the_whole_file() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"document\"\npath = \"generated/{{ spv.snake }}.txt\"\n\
             create = true\nwrap = []\nbody = \"begin\\nend\\n\"\n",
        );
        let edit = only(&catalog, &descriptor(), &Options::default());
        assert_eq!(edit.path, "generated/spv_test_widget.txt");
        assert!(matches!(edit.action, Action::Create));
        assert_eq!(edit.text, "begin\nend\n");
    }

    #[test]
    fn a_missing_plane_stops_the_target_rather_than_half_applying_it() {
        let catalog = Catalog::parse(
            "[target]\nname = \"beta\"\nroot = \"/t\"\nplane = \"vk\"\n\
             [[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"x\\n\"\n",
        )
        .unwrap();
        let err = build(&catalog, &descriptor(), &Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("vk plane"), "got: {err}");
    }

    #[test]
    fn a_fan_out_element_with_nothing_to_say_produces_no_edit() {
        // One entry per group declares a requirement, so both groups render; clear
        // them and each rule has no lines, and then no edit at all rather than an
        // empty wrapped edit.
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"reqs\"\npath = \"enum.h\"\n\
             repeat = { over = \"spv.kinds\", as = \"kind\" }\n\
             scope = ['x']\nbefore = '^\\}$'\n\
             body = \"\"\"\n{%- for e in kind.enumerants if e.requires %}  X({{ e.name }});\n{% endfor -%}\n\"\"\"\n",
        );
        let mut ext = descriptor();
        assert_eq!(build(&catalog, &ext, &Options::default()).unwrap().len(), 2);

        for kind in &mut ext.spv.as_mut().unwrap().kinds {
            for enumerant in &mut kind.enumerants {
                enumerant.requires.clear();
            }
        }
        assert!(build(&catalog, &ext, &Options::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_known_empty_repeat_collection_produces_no_edits() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"kind\"\npath = \"items.txt\"\n\
             repeat = { over = \"spv.kinds\", as = \"kind\" }\n\
             eof = true\nbody = \"{{ kind.name }}\\n\"\n",
        );
        let mut ext = descriptor();
        ext.spv.as_mut().unwrap().kinds.clear();
        assert!(build(&catalog, &ext, &Options::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_wrapped_body_without_a_trailing_newline_is_refused() {
        let catalog =
            catalog_of("[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"no newline\"\n");
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("must end with a newline"), "got: {err}");
    }

    #[test]
    fn an_unwrapped_body_may_splice_mid_line() {
        // A JSON list entry needs exactly `,\n  { … }` with no trailing newline,
        // which is why a rendered body is never normalised.
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f.json\"\neof = true\nwrap = []\n\
             body = \",\\n    { \\\"Name\\\": \\\"{{ spv.feature }}\\\" }\"\n",
        );
        assert_eq!(
            only(&catalog, &descriptor(), &Options::default()).text,
            ",\n    { \"Name\": \"TEST_WIDGET\" }"
        );
    }

    #[test]
    fn when_drops_a_rule_when_its_variable_is_empty() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"entry\"\npath = \"items.txt\"\neof = true\nwrap = []\n\
             when = \"{{ var.frame }}\"\nbody = \"entry({{ var.frame }})\\n\"\n",
        );
        assert_eq!(
            build(&catalog, &descriptor(), &Options::default())
                .unwrap()
                .len(),
            1
        );

        let disabled = Options {
            vars: BTreeMap::from([("frame".to_owned(), String::new())]),
            ..Default::default()
        };
        assert!(build(&catalog, &descriptor(), &disabled)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn when_reads_an_empty_collection_as_false() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"opcodes\"\npath = \"op.h\"\neof = true\n\
             when = \"{{ spv.operations }}\"\nbody = \"x\\n\"\n",
        );
        let mut ext = descriptor();
        assert_eq!(build(&catalog, &ext, &Options::default()).unwrap().len(), 1);

        ext.spv.as_mut().unwrap().operations.clear();
        assert!(build(&catalog, &ext, &Options::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_repeat_over_nothing_is_reported_against_its_rule() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\n\
             repeat = { over = \"spv.nosuchthing\", as = \"k\" }\nbody = \"x\\n\"\n",
        );
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("names nothing"), "got: {err}");
    }

    #[test]
    fn a_bad_template_names_the_rule_it_came_from() {
        let catalog = catalog_of(
            "[[rule]]\nwhat = \"w\"\npath = \"f\"\neof = true\nbody = \"{{ unclosed \"\n",
        );
        let err = format!(
            "{:#}",
            build(&catalog, &descriptor(), &Options::default()).unwrap_err()
        );
        assert!(err.contains("rule 'w'"), "got: {err}");
    }
}
