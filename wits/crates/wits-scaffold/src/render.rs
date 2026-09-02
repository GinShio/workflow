//! The scaffold's dialect of the shared template engine.
//!
//! Every string a catalogue holds is a template, so the engine is shared rather
//! than owned by whichever stage happens to render first: a target's tree root is
//! resolved before any edit is planned, and both go through here.
//!
//! The dialect is [`wits_util::jinja`], the same one project config resolves
//! against, so a template reads the same wherever it is written. What this module
//! adds is the two filters only a Vulkan/SPIR-V catalogue needs, and the checks
//! that make a catalogue's mistakes legible.
//!
//! Strict undefined behaviour comes from that shared environment. What follows
//! from it here is that modeled collections are always published, using an empty
//! list when they contain no entries, so strictness distinguishes an empty
//! collection from an unknown path.
//!
//! Variables receive an earlier, more actionable check: a `var.*` must have a
//! default under `[target.vars]` or arrive as `--var`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use minijinja::{Environment, Error, ErrorKind, Value};
use sha2::{Digest, Sha256};

/// The shared dialect, plus the filters only a specification catalogue needs.
pub fn environment() -> Environment<'static> {
    let mut env = wits_util::jinja::environment();
    env.add_filter("extension_tag", extension_tag);
    env.add_filter("sha256", sha256_prefix);
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
