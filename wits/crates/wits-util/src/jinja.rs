//! The Jinja dialect every renderer in the tree shares.
//!
//! This module holds no rendering logic — only the definition of the language:
//! which filters and functions exist, how an undefined value behaves, and how
//! trailing whitespace is treated. A template therefore means the same thing
//! wherever it is written, whether that is a project config value or a scaffold
//! catalogue body.
//!
//! Two entry points, for the two ways callers need it. A renderer that adds
//! filters of its own takes a fresh [`environment`] and extends it; a renderer
//! that does not takes [`shared`] and pays nothing, since building an
//! `Environment` means populating MiniJinja's whole builtin table.
//!
//! ## Undefined paths are errors
//!
//! [`UndefinedBehavior::Strict`] is the load-bearing setting. A misspelled path
//! must not splice an empty hole into a generated file or into a resolved build
//! path, so it fails instead of rendering "". A caller that publishes a
//! collection therefore publishes it even when empty, so strictness distinguishes
//! "no entries" from "no such name".

use std::sync::OnceLock;

use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior, Value};

/// The shared environment, built once for the process.
///
/// `Environment` is `Send + Sync`, and a `'static` one still parses a
/// short-lived template string, so callers that do not extend the dialect can
/// borrow one instance for the whole run.
pub fn shared() -> &'static Environment<'static> {
    static SHARED: OnceLock<Environment<'static>> = OnceLock::new();
    SHARED.get_or_init(environment)
}

/// A fresh environment, for a caller that adds filters of its own.
pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Jinja strips one trailing newline from a template by default, which would
    // quietly break a verbatim-body contract: a body that is a line list would
    // emit its last line unterminated and run into the text below it. Bodies are
    // exact bytes, not prose, so that behaviour is off.
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
    env.add_filter("required", |value: Value, message: String| {
        if value.is_undefined() {
            return Err(Error::new(ErrorKind::InvalidOperation, message));
        }
        Ok(value)
    });
    env.add_function("fail", |message: String| -> Result<String, Error> {
        Err(Error::new(ErrorKind::InvalidOperation, message))
    });
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    fn render(template: &str, ctx: Value) -> Result<String, Error> {
        shared().render_str(template, ctx)
    }

    #[test]
    fn pad_fills_to_the_column_and_never_truncates() {
        assert_eq!(
            render("[{{ 'ab' | pad(4) }}]", Value::from(())).unwrap(),
            "[ab  ]"
        );
        assert_eq!(
            render("[{{ 'abcdef' | pad(4) }}]", Value::from(())).unwrap(),
            "[abcdef]"
        );
    }

    #[test]
    fn prefix_and_suffix_decorate_every_element() {
        let ctx = Value::from_serialize(BTreeMap::from([("xs", vec!["A", "B"])]));
        assert_eq!(
            render("{{ xs | prefix('K') | join(', ') }}", ctx.clone()).unwrap(),
            "KA, KB"
        );
        assert_eq!(
            render("{{ xs | suffix('!') | join(', ') }}", ctx).unwrap(),
            "A!, B!"
        );
    }

    #[test]
    fn strip_prefix_drops_only_a_leading_match() {
        assert_eq!(
            render("{{ 'OpOpFoo' | strip_prefix('Op') }}", Value::from(())).unwrap(),
            "OpFoo"
        );
        assert_eq!(
            render("{{ 'Foo' | strip_prefix('Op') }}", Value::from(())).unwrap(),
            "Foo"
        );
    }

    #[test]
    fn required_turns_an_optional_path_into_an_error() {
        assert!(render(
            "{{ missing | required('supply missing in the overlay') }}",
            Value::from(())
        )
        .is_err());
    }

    #[test]
    fn fail_stops_a_template_with_its_message() {
        let err = render("{{ fail('bad metadata') }}", Value::from(()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad metadata"), "got: {err}");
    }

    #[test]
    fn unknown_paths_fail_but_known_empty_collections_render() {
        let ctx = Value::from_serialize(BTreeMap::from([(
            "spv",
            BTreeMap::from([("operations", Vec::<String>::new())]),
        )]));
        assert_eq!(render("{{ spv.operations }}", ctx.clone()).unwrap(), "[]");
        assert!(render("{{ spv.no_such_collection }}", ctx).is_err());
    }
}
