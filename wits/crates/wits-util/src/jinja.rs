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
    // Abbreviating `a_b_c` to `ABC` is a shape several kinds of table want. The
    // filter knows only how to take first letters and then fill from what is
    // left; *which* words it sees is the caller's rule and stays in the caller's
    // config, so no tree's naming convention is spelled here.
    env.add_filter(
        "initials",
        |value: String, width: usize| -> Result<String, Error> {
            let words: Vec<&str> = value.split('_').filter(|word| !word.is_empty()).collect();
            let mut tag: Vec<char> = words
                .iter()
                .filter_map(|word| word.chars().next())
                .map(|ch| ch.to_ascii_uppercase())
                .collect();
            // Too few words to give one letter each: keep going through the letters
            // already passed over, in the order they appear.
            if tag.len() < width {
                for ch in words.iter().flat_map(|word| word.chars().skip(1)) {
                    tag.push(ch.to_ascii_uppercase());
                    if tag.len() == width {
                        break;
                    }
                }
            }
            if tag.len() < width {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!(
                        "'{value}' has too few characters for a {width}-character abbreviation"
                    ),
                ));
            }
            tag.truncate(width);
            Ok(tag.into_iter().collect())
        },
    );
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
    fn initials_takes_one_letter_per_word() {
        assert_eq!(
            render("{{ 'shader_soft_widget' | initials(3) }}", Value::from(())).unwrap(),
            "SSW"
        );
    }

    #[test]
    fn initials_fills_from_the_letters_it_passed_over() {
        // Two words cannot give three letters, so the rest comes from what is
        // left of them, in order.
        assert_eq!(
            render("{{ 'hdr_metadata' | initials(3) }}", Value::from(())).unwrap(),
            "HMD"
        );
    }

    #[test]
    fn initials_refuses_rather_than_returning_a_short_tag() {
        // A short tag would collide with another entry's, and the table it goes
        // into has no way to notice.
        assert!(render("{{ 'ab' | initials(3) }}", Value::from(())).is_err());
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
