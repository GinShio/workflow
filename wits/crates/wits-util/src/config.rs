//! Config, at two granularities, one module.
//!
//! Both halves answer the same question — *where does this configuration come
//! from?* — at different scopes, so they live together rather than in two
//! near-adjacent modules:
//!
//! - [`Resolver`] resolves a **single setting** by precedence: an environment
//!   variable over a git-config value, optionally scoped to a *context* so a
//!   `prod` set of secrets never borrows the `default` one.
//! - [`resolve_root`] / [`discover_toml`] locate a tool's **config tree** — the
//!   directory it keeps `*.toml` under — via the env -> XDG -> HOME search.
//!
//! Both are generic OS-convention plumbing with no domain knowledge, so they sit
//! on the floor; a subsystem supplies its own [`Root`] naming or [`Resolver`]
//! prefix and routes the results however it likes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::git::Repository;

/// How a tool names its config directory across the standard search locations.
pub struct Root<'a> {
    /// An absolute override read from this environment variable; when set it
    /// must point at an existing directory.
    pub env: &'a str,
    /// The path under `$XDG_CONFIG_HOME`.
    pub xdg: &'a str,
    /// The path under `$HOME`.
    pub home: &'a str,
}

/// Resolve the single config root: `$<env>` (which must exist if set), then the
/// first existing of `$XDG_CONFIG_HOME/<xdg>` and `$HOME/<home>`.
pub fn resolve_root(spec: &Root<'_>) -> Result<PathBuf> {
    if let Some(env) = std::env::var_os(spec.env) {
        let path = PathBuf::from(env);
        if !path.is_dir() {
            bail!(
                "{} points at {} which is not a directory",
                spec.env,
                path.display()
            );
        }
        return Ok(path);
    }

    let mut candidates = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        candidates.push(PathBuf::from(xdg).join(spec.xdg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(spec.home));
    }
    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }
    bail!(
        "no config root found (looked for {})",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Every `*.toml` under `root`, recursively, in sorted order. A `root` that does
/// not exist yields an empty list rather than an error, so a tool with no config
/// installed simply sees nothing to load.
pub fn discover_toml(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    scan(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn scan(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            scan(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            out.push(path);
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Single-setting resolution — env over git-config, context-scoped.
//
// A setting like the encryption password can legitimately live in several
// places: an environment variable (handy for CI), or git config (handy for a
// checkout you return to). We want a single, predictable order of precedence
// and — importantly — no bootstrap loop, since the thing reading config here is
// the same machinery that would otherwise need config to know where to look.
//
// The one subtlety is the *context* fallback. A repository can keep several
// independent sets of secrets (the `default` set, a `prod` set, and so on).
// When a non-default context is active we deliberately do NOT fall back to the
// bare, context-less key: doing so would silently hand a `prod` operation the
// `default` password — the kind of cross-context credential bleed that encrypts
// data under the wrong key. The bare key is only consulted for the default
// context.
// ----------------------------------------------------------------------------

/// Which layer answered a lookup. Surfaced so a `status` view can tell the user
/// *why* it sees a particular value, which is usually the actual question when
/// secrets misbehave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Env,
    Git,
    Default,
}

impl std::fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Env => "env",
            Self::Git => "git",
            Self::Default => "default",
        })
    }
}

pub struct ResolvedValue {
    pub value: String,
    pub source: ConfigSource,
}

/// Resolves keys for one namespace (`prefix`) and one `context`.
///
/// The same logical key maps to an env var (`PREFIX_[CONTEXT_]KEY`, upper-cased)
/// and a git config key (`prefix.[context.]key`, lower-cased); env always wins
/// because it is the more deliberate, ephemeral override. The `prefix` is the
/// git-config spelling and may be dotted (`wits.transcrypt`); the env form
/// replaces those dots with underscores (`WITS_TRANSCRYPT_…`), so a subcommand's
/// keys land under one consistent `wits.<sub>` / `WITS_<SUB>_` namespace.
pub struct Resolver<'repo> {
    repo: Option<&'repo Repository>,
    prefix: String,
    context: Option<String>,
}

impl<'repo> Resolver<'repo> {
    pub fn new(
        repo: Option<&'repo Repository>,
        prefix: impl Into<String>,
        context: Option<impl Into<String>>,
    ) -> Self {
        Self {
            repo,
            prefix: prefix.into(),
            context: context.map(Into::into),
        }
    }

    pub fn get(&self, key: &str) -> Option<ResolvedValue> {
        // Anything other than "default" is treated as an isolated context that
        // must not borrow the bare key — see the module note on credential bleed.
        let allow_bare = self.context.as_deref().is_none_or(|c| c == "default");

        if let Some(ctx) = &self.context {
            if let Some(value) = self.get_env(Some(ctx), key) {
                return Some(ResolvedValue {
                    value,
                    source: ConfigSource::Env,
                });
            }
        }
        if allow_bare {
            if let Some(value) = self.get_env(None, key) {
                return Some(ResolvedValue {
                    value,
                    source: ConfigSource::Env,
                });
            }
        }

        if let Some(repo) = self.repo {
            if let Some(ctx) = &self.context {
                if let Some(value) = self.get_git(repo, Some(ctx), key) {
                    return Some(ResolvedValue {
                        value,
                        source: ConfigSource::Git,
                    });
                }
            }
            if allow_bare {
                if let Some(value) = self.get_git(repo, None, key) {
                    return Some(ResolvedValue {
                        value,
                        source: ConfigSource::Git,
                    });
                }
            }
        }

        None
    }

    pub fn get_or_default(&self, key: &str, default_val: &str) -> ResolvedValue {
        self.get(key).unwrap_or_else(|| ResolvedValue {
            value: default_val.to_owned(),
            source: ConfigSource::Default,
        })
    }

    fn get_env(&self, ctx: Option<&str>, key: &str) -> Option<String> {
        let name = match ctx {
            Some(c) => format!("{}_{}_{}", self.prefix, c, key),
            None => format!("{}_{}", self.prefix, key),
        };
        // A dotted prefix (`wits.transcrypt`) is the git-config spelling; in an
        // env var the dots become underscores, so `wits.transcrypt`/`password`
        // reads `WITS_TRANSCRYPT_PASSWORD`.
        std::env::var(name.replace('.', "_").to_uppercase()).ok()
    }

    fn get_git(&self, repo: &Repository, ctx: Option<&str>, key: &str) -> Option<String> {
        let name = match ctx {
            Some(c) => format!("{}.{}.{}", self.prefix, c, key),
            None => format!("{}.{}", self.prefix, key),
        };
        repo.get_config(&name.to_lowercase()).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Command;

    #[test]
    fn discovers_toml_recursively_and_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("z.toml"), "").unwrap();
        std::fs::write(root.join("a/m.toml"), "").unwrap();
        std::fs::write(root.join("a/b/c.toml"), "").unwrap();
        std::fs::write(root.join("a/skip.txt"), "").unwrap();

        let found = discover_toml(root).unwrap();

        // Recursive (nested c.toml found), extension-filtered (skip.txt absent),
        // and returned in sorted path order.
        assert_eq!(
            found,
            vec![
                root.join("a/b/c.toml"),
                root.join("a/m.toml"),
                root.join("z.toml"),
            ]
        );
    }

    #[test]
    fn missing_root_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("does-not-exist");
        assert!(discover_toml(&absent).unwrap().is_empty());
    }

    #[test]
    fn falls_back_to_caller_default() {
        let resolver = Resolver::new(None::<&Repository>, "wftest", None::<String>);
        let resolved = resolver.get_or_default("missing", "fallback");
        assert_eq!(resolved.value, "fallback");
        assert_eq!(resolved.source, ConfigSource::Default);
    }

    #[test]
    fn reads_environment() {
        let resolver = Resolver::new(None::<&Repository>, "wftest", None::<String>);
        std::env::set_var("WFTEST_MYKEY", "env_value");
        let resolved = resolver.get("mykey");
        std::env::remove_var("WFTEST_MYKEY");
        let resolved = resolved.expect("env var should resolve");
        assert_eq!(resolved.value, "env_value");
        assert_eq!(resolved.source, ConfigSource::Env);
    }

    #[test]
    fn context_specific_key_wins_over_bare() {
        let resolver = Resolver::new(None::<&Repository>, "wftest2", Some("prod"));
        std::env::set_var("WFTEST2_MYKEY", "bare");
        std::env::set_var("WFTEST2_PROD_MYKEY", "scoped");
        let resolved = resolver.get("mykey");
        std::env::remove_var("WFTEST2_MYKEY");
        std::env::remove_var("WFTEST2_PROD_MYKEY");
        assert_eq!(resolved.unwrap().value, "scoped");
    }

    #[test]
    fn non_default_context_does_not_borrow_bare_key() {
        let resolver = Resolver::new(None::<&Repository>, "wftest3", Some("prod"));
        std::env::set_var("WFTEST3_MYKEY", "bare");
        let resolved = resolver.get("mykey");
        std::env::remove_var("WFTEST3_MYKEY");
        assert!(resolved.is_none());
    }

    // The git layer is the half that needs a real repository, so it earns its
    // own test: with no matching env var, a value set in git config must come
    // back tagged as coming from git.
    #[test]
    fn reads_git_config_when_env_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        // force_run: tests share the global dry-run flag and run in parallel, so
        // these setup commands must execute even if another test has it toggled on.
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();
        Command::new("git")
            .args(["config", "wfgittest.password", "from-git"])
            .current_dir(dir.path())
            .force_run()
            .exec()
            .unwrap();

        let repo = Repository::new(dir.path());
        let resolver = Resolver::new(Some(&repo), "wfgittest", Some("default"));
        let resolved = resolver.get("password").expect("git config should resolve");
        assert_eq!(resolved.value, "from-git");
        assert_eq!(resolved.source, ConfigSource::Git);
    }
}
