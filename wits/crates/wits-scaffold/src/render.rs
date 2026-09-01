//! The template engine, and the one contract it enforces.
//!
//! Every string a catalogue holds is a template, so the engine is shared rather
//! than owned by whichever stage happens to render first: a target's tree root is
//! resolved before any edit is planned, and both go through here.
//!
//! ## Why a real Jinja and not `wits_util::template`
//!
//! The shared template engine resolves a *config context*: dotted paths, and
//! expressions only as a whole value. It has no iteration, which generated text
//! needs. Growing loops into the config resolver to serve one plugin would have
//! been the wrong place for them, so text generation gets a real Jinja engine.
//!
//! ## Undefined paths are errors
//!
//! A misspelled path must not splice an empty hole into generated source.
//! MiniJinja therefore runs in strict mode. Modeled collections are always
//! published, using an empty list when they contain no entries, so strictness
//! distinguishes an empty collection from an unknown path.
//!
//! Variables receive an earlier, more actionable check: a `var.*` must have a
//! default under `[target.vars]` or arrive as `--var`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior, Value};
use sha2::{Digest, Sha256};

/// The engine, with the filters catalogues need beyond Jinja's built-ins.
pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Jinja strips one trailing newline from a template by default, which would
    // quietly break the verbatim-body contract: a rule whose body is a line list
    // would emit its last line unterminated and run into the text below it. The
    // bodies here are exact bytes, not prose, so that behaviour is off.
    env.set_keep_trailing_newline(true);
    // Built-in `join` concatenates, but generated lists often need each element
    // prefixed or suffixed first.
    env.add_filter(
        "prefix",
        |values: Vec<String>, with: String| -> Vec<String> {
            values
                .into_iter()
                .map(|value| format!("{with}{value}"))
                .collect()
        },
    );
    env.add_filter(
        "suffix",
        |values: Vec<String>, with: String| -> Vec<String> {
            values
                .into_iter()
                .map(|value| format!("{value}{with}"))
                .collect()
        },
    );
    // Jinja's `replace` has no occurrence limit; generated identifiers sometimes
    // need one leading prefix removed and no other occurrence touched.
    env.add_filter("strip_prefix", |value: String, prefix: String| -> String {
        value.strip_prefix(&prefix).unwrap_or(&value).to_owned()
    });
    // Padding must never truncate: fitting a column cannot be allowed to corrupt
    // an entry.
    env.add_filter("pad", |value: String, width: usize| -> String {
        let mut padded = value;
        while padded.chars().count() < width {
            padded.push(' ');
        }
        padded
    });
    env.add_filter("extension_tag", extension_tag);
    env.add_filter("sha256", sha256_prefix);
    env.add_filter("required", required_value);
    env.add_function(
        "fail",
        |message: String| -> std::result::Result<String, Error> {
            Err(Error::new(ErrorKind::InvalidOperation, message))
        },
    );
    env
}

fn extension_tag(value: String) -> std::result::Result<String, Error> {
    let parts: Vec<&str> = value.split('_').collect();
    if parts.len() < 3
        || parts[0] != "VK"
        || parts[1..]
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_alphanumeric()))
    {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "extension_tag expects VK_<VENDOR>_<name>",
        ));
    }
    let vendor = parts[1];
    let payload = &parts[2..];
    let mut tag: Vec<char> = payload
        .iter()
        .filter_map(|word| word.chars().next())
        .map(|ch| ch.to_ascii_uppercase())
        .collect();
    if tag.len() < 3 {
        tag.insert(
            0,
            vendor
                .chars()
                .next()
                .expect("non-empty vendor")
                .to_ascii_uppercase(),
        );
    }
    if tag.len() < 3 {
        for ch in payload
            .iter()
            .flat_map(|word| word.chars().skip(1))
            .chain(vendor.chars().skip(1))
        {
            tag.push(ch.to_ascii_uppercase());
            if tag.len() == 3 {
                break;
            }
        }
    }
    if tag.len() < 3 {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "extension name is too short to form a three-character tag",
        ));
    }
    tag.truncate(3);
    Ok(tag.into_iter().collect())
}

fn sha256_prefix(value: String, length: usize) -> std::result::Result<String, Error> {
    if length == 0 || length > 64 {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "sha256 prefix length must be between 1 and 64",
        ));
    }
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    Ok(digest[..length].to_owned())
}

fn required_value(value: Value, message: String) -> std::result::Result<Value, Error> {
    if value.is_undefined() {
        return Err(Error::new(ErrorKind::InvalidOperation, message));
    }
    Ok(value)
}

/// Render one template against one context.
pub fn one(env: &Environment<'_>, template: &str, ctx: &Value) -> Result<String> {
    env.template_from_str(template)
        .and_then(|t| t.render(ctx))
        .map_err(|error| {
            let excerpt: String = template.chars().take(120).collect();
            // MiniJinja's own message plus the template that failed, since a
            // catalogue has many templates and the message alone rarely says which.
            anyhow::anyhow!("{error} while rendering `{excerpt}`")
        })
}

/// Evaluate a condition with Jinja's native truthiness when it is one complete
/// expression. Other templates retain their rendered-string behavior.
pub fn truthy(env: &Environment<'_>, template: &str, ctx: &Value) -> Result<bool> {
    let trimmed = template.trim();
    if let Some(expression) = trimmed
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
    {
        let value = env
            .compile_expression(expression.trim())
            .and_then(|expression| expression.eval(ctx.clone()))
            .map_err(anyhow::Error::from);
        let value = value?;
        if value.is_undefined() {
            bail!(
                "condition `{}` evaluated to an unknown path",
                expression.trim()
            );
        }
        return Ok(value.is_true());
    }
    let rendered = one(env, template, ctx)?;
    let rendered = rendered.trim();
    Ok(!matches!(
        rendered,
        "" | "false" | "0" | "[]" | "{}" | "none"
    ))
}

/// The keys `template` reads out of one namespace, e.g. `var` for `{{ var.tree }}`.
///
/// Static analysis, so it over-reports rather than under-reports: a key mentioned
/// in a branch that will not be taken still comes back. Callers use it to render
/// a *subset* of something, where over-reporting only costs extra work.
pub fn referenced(env: &Environment<'_>, template: &str, namespace: &str) -> BTreeSet<String> {
    let prefix = format!("{namespace}.");
    let Ok(parsed) = env.template_from_str(template) else {
        return BTreeSet::new();
    };
    parsed
        .undeclared_variables(true)
        .iter()
        .filter_map(|path| path.strip_prefix(&prefix).map(str::to_owned))
        .collect()
}

/// Refuse the templates in `templates` if any of them reads a `var.*` that
/// `defined` does not hold.
///
/// Static analysis, so it sees a reference inside a branch that will not be taken
/// — which is why callers check a rule's templates only once the rule is known to
/// be live, rather than sweeping a whole catalogue up front. A rule dropped by
/// `when` must not demand values for text it will never emit.
pub fn require_vars(
    env: &Environment<'_>,
    templates: &[&str],
    defined: &BTreeMap<String, String>,
) -> Result<()> {
    let mut missing = BTreeSet::new();
    for template in templates {
        // A template that does not parse is not this check's business; the render
        // that follows reports it with a far better message.
        let Ok(parsed) = env.template_from_str(template) else {
            continue;
        };
        for path in parsed.undeclared_variables(true) {
            if let Some(key) = path.strip_prefix("var.") {
                if !defined.contains_key(key) {
                    missing.insert(key.to_owned());
                }
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let list: Vec<&str> = missing.iter().map(String::as_str).collect();
    bail!(
        "reads {}, which the catalogue does not define and this run did not supply — add \
         it under [target.vars] or pass {}",
        list.iter()
            .map(|key| format!("var.{key}"))
            .collect::<Vec<_>>()
            .join(", "),
        list.iter()
            .map(|key| format!("--var {key}=…"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defined(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn pad_fills_to_the_column_and_never_truncates() {
        let env = environment();
        let ctx = Value::from(());
        assert_eq!(one(&env, "[{{ 'ab' | pad(4) }}]", &ctx).unwrap(), "[ab  ]");
        assert_eq!(
            one(&env, "[{{ 'abcdef' | pad(4) }}]", &ctx).unwrap(),
            "[abcdef]"
        );
    }

    #[test]
    fn prefix_and_join_build_a_list_initialiser() {
        let env = environment();
        let ctx = Value::from_serialize(BTreeMap::from([("xs", vec!["A", "B"])]));
        assert_eq!(
            one(&env, "{{ xs | prefix('K') | join(', ') }}", &ctx).unwrap(),
            "KA, KB"
        );
    }

    #[test]
    fn strip_prefix_drops_only_a_leading_match() {
        let env = environment();
        let ctx = Value::from(());
        assert_eq!(
            one(&env, "{{ 'OpOpFoo' | strip_prefix('Op') }}", &ctx).unwrap(),
            "OpFoo"
        );
        assert_eq!(
            one(&env, "{{ 'Foo' | strip_prefix('Op') }}", &ctx).unwrap(),
            "Foo"
        );
    }

    #[test]
    fn extension_tags_have_a_stable_three_character_shape() {
        let env = environment();
        let ctx = Value::from(());
        assert_eq!(
            one(&env, "{{ 'VK_TEST_shader_widget' | extension_tag }}", &ctx).unwrap(),
            "TSW"
        );
        assert_eq!(
            one(
                &env,
                "{{ 'VK_TEST_shader_soft_widget' | extension_tag }}",
                &ctx
            )
            .unwrap(),
            "SSW"
        );
        assert!(one(&env, "{{ 'VK_TEST__bad' | extension_tag }}", &ctx).is_err());
    }

    #[test]
    fn sha256_hashes_the_exact_string_and_truncates_in_hex() {
        let env = environment();
        let ctx = Value::from(());
        assert_eq!(
            one(&env, "{{ 'VK_TEST_shader_widget' | sha256(7) }}", &ctx).unwrap(),
            "546a53e"
        );
        assert!(one(&env, "{{ 'x' | sha256(65) }}", &ctx).is_err());
    }

    #[test]
    fn required_turns_an_optional_path_into_an_error() {
        let env = environment();
        let ctx = Value::from(());
        assert!(one(
            &env,
            "{{ missing | required('supply missing in the overlay') }}",
            &ctx
        )
        .is_err());
    }

    #[test]
    fn fail_stops_a_template_with_its_message() {
        let env = environment();
        let err = one(&env, "{{ fail('bad metadata') }}", &Value::from(()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad metadata"), "got: {err}");
    }

    #[test]
    fn an_undefined_var_is_refused_and_the_message_says_both_ways_to_fix_it() {
        let env = environment();
        let err = require_vars(&env, &["root is {{ var.tree }}"], &defined(&[]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("var.tree"), "got: {err}");
        assert!(err.contains("[target.vars]"), "got: {err}");
        assert!(err.contains("--var tree="), "got: {err}");
    }

    #[test]
    fn a_defined_var_passes() {
        let env = environment();
        assert!(require_vars(&env, &["{{ var.tree }}"], &defined(&[("tree", "/t")])).is_ok());
    }

    #[test]
    fn every_undefined_var_is_named_in_one_go() {
        let env = environment();
        let err = require_vars(
            &env,
            &["{{ var.a }}", "{{ var.b }}{{ var.c }}"],
            &defined(&[("b", "x")]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("var.a") && err.contains("var.c"), "got: {err}");
        assert!(!err.contains("--var b="), "b was defined: {err}");
    }

    #[test]
    fn a_template_default_does_not_excuse_an_undefined_var() {
        // The catalogue is where a fallback belongs: `[target.vars]` already gives
        // one, and it is visible to everyone reading the catalogue rather than
        // buried in whichever body happened to mention the name first.
        let env = environment();
        assert!(require_vars(&env, &["{{ var.tree | default('/t') }}"], &defined(&[])).is_err());
    }

    #[test]
    fn unknown_paths_fail_but_known_empty_collections_render() {
        let env = environment();
        let ctx = Value::from_serialize(BTreeMap::from([(
            "spv",
            BTreeMap::from([("operations", Vec::<String>::new())]),
        )]));
        assert_eq!(one(&env, "{{ spv.operations }}", &ctx).unwrap(), "[]");
        assert!(one(&env, "{{ spv.no_such_collection }}", &ctx).is_err());
    }

    #[test]
    fn conditions_use_native_collection_truthiness() {
        let env = environment();
        let ctx = Value::from_serialize(BTreeMap::from([
            ("empty", Vec::<String>::new()),
            ("present", vec!["x".to_owned()]),
        ]));
        assert!(!truthy(&env, "{{ empty }}", &ctx).unwrap());
        assert!(truthy(&env, "{{ present }}", &ctx).unwrap());
    }
}
