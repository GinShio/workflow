//! Where a target tree is checked out.
//!
//! A catalogue names its tree by **path**, not through any project registry, so a
//! catalogue is self-contained: it describes a tree and says where that tree is,
//! and nothing else has to be installed for it to resolve.
//!
//! The path is a template because one tree commonly has more than one checkout —
//! a per-branch worktree layout is the usual reason — and which one to scaffold
//! into is a per-run choice. Writing `{{ var.<name> }}` and defaulting it under
//! `[target.vars]` lets `--var` redirect a single run; leaving it out of
//! `[target.vars]` instead makes `--var` mandatory, for a tree whose location
//! nobody should be guessing.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use minijinja::{Environment, Value};

use crate::render;

/// Render a target's `root` template and expand a leading `~`.
///
/// Existence is not checked here. Whether a missing tree is fatal depends on how
/// the target was selected, which is the caller's knowledge: naming a target and
/// finding no tree is an error, while sweeping every catalogue on a machine that
/// holds only some of the trees is not.
pub fn resolve(template: &str, ctx: &Value, env: &Environment<'_>) -> Result<PathBuf> {
    let rendered = render::one(env, template, ctx).context("target root")?;
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        bail!("target root '{template}' rendered empty");
    }
    Ok(expand_tilde(trimmed))
}

/// `~` and `~/…` against `$HOME`. A bare `~user` form is left alone: resolving
/// another user's home needs the password database, and no catalogue wants it.
fn expand_tilde(path: &str) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::from(path);
    };
    match rest {
        "" => PathBuf::from(home),
        _ if rest.starts_with('/') => PathBuf::from(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx(vars: &[(&str, &str)]) -> Value {
        Value::from_serialize(BTreeMap::from([(
            "var",
            vars.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        )]))
    }

    #[test]
    fn a_literal_path_is_the_common_case() {
        let env = render::environment();
        assert_eq!(
            resolve("/fixtures/tree-a/main", &ctx(&[]), &env).unwrap(),
            PathBuf::from("/fixtures/tree-a/main")
        );
    }

    #[test]
    fn a_var_redirects_the_checkout() {
        let env = render::environment();
        assert_eq!(
            resolve(
                "{{ var.tree }}",
                &ctx(&[("tree", "/fixtures/tree-a/topic")]),
                &env
            )
            .unwrap(),
            PathBuf::from("/fixtures/tree-a/topic")
        );
    }

    #[test]
    fn a_leading_tilde_expands() {
        std::env::set_var("HOME", "/fixtures/home");
        let env = render::environment();
        assert_eq!(
            resolve("~/tree-a", &ctx(&[]), &env).unwrap(),
            PathBuf::from("/fixtures/home/tree-a")
        );
        assert_eq!(
            resolve("~", &ctx(&[]), &env).unwrap(),
            PathBuf::from("/fixtures/home")
        );
    }

    #[test]
    fn a_tilde_inside_a_path_is_not_a_home_reference() {
        let env = render::environment();
        assert_eq!(
            resolve("/fixtures/~backup/x", &ctx(&[]), &env).unwrap(),
            PathBuf::from("/fixtures/~backup/x")
        );
    }

    #[test]
    fn a_root_that_renders_to_nothing_is_refused() {
        // Far better than resolving to the process's working directory and
        // writing a scaffold into whatever tree happens to be there.
        let env = render::environment();
        assert!(resolve("{{ var.absent }}", &ctx(&[]), &env).is_err());
    }
}
