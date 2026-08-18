//! Feed configuration — the RSS-style subscription layer.
//!
//! Tokens and host detection stay in git config (reused from `stack`); this is
//! the *other* axis: the structured, growing part that git config is a poor home
//! for. It lives in a single global TOML (`$XDG_CONFIG_HOME/wits/review.toml`,
//! overridable via `WITS_REVIEW_CONFIG`) with a section per repo, keyed by the
//! parsed `host/owner/repo` identity so one file holds many repos without
//! committing personal review preferences into anyone's tree.
//!
//! A repo with no section simply has no feeds — but a token alone still lets you
//! review a single MR by number, the same graceful degradation `stack` has.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use wits_util::forge::RemoteInfo;
use wits_util::forge::{FeedQuery, FeedStates};

use super::StackMode;

/// How many MRs a feed pulls when it doesn't say otherwise — a cap so a large
/// repo can't flood the inbox.
const DEFAULT_LIMIT: usize = 30;

/// The whole config file: repos, each holding named feeds.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    repo: HashMap<String, RepoSection>,
}

#[derive(Debug, Default, Deserialize)]
struct RepoSection {
    #[serde(default)]
    feed: HashMap<String, FeedDef>,
}

/// One feed's faceted filter as written in TOML.
#[derive(Debug, Default, Deserialize)]
struct FeedDef {
    /// `"open+draft"` (default), `"open"`, or `"draft"`.
    state: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default, rename = "exclude-labels")]
    exclude_labels: Vec<String>,
    author: Option<String>,
    assignee: Option<String>,
    reviewer: Option<String>,
    search: Option<String>,
    limit: Option<usize>,
    /// How much of each matched MR's stack to complete (`none`/`auto`/`all`).
    /// A per-feed default the `--stack` flag overrides; unset means `auto`.
    stack: Option<StackMode>,
}

impl FeedDef {
    /// Resolve this feed definition into a query. Borrows `self` (the fields are
    /// cloned into the owned [`FeedQuery`] regardless), so the lookup needs no
    /// hand-written clone of `FeedDef`.
    fn to_query(&self) -> FeedQuery {
        FeedQuery {
            states: parse_states(self.state.as_deref()),
            labels: self.labels.clone(),
            exclude_labels: self.exclude_labels.clone(),
            author: self.author.clone(),
            assignee: self.assignee.clone(),
            reviewer: self.reviewer.clone(),
            search: self.search.clone(),
            limit: self.limit.unwrap_or(DEFAULT_LIMIT),
        }
    }
}

/// The loaded configuration.
pub struct Config {
    file: ConfigFile,
}

/// The repo key a section is filed under.
pub fn repo_key(info: &RemoteInfo) -> String {
    format!("{}/{}/{}", info.host, info.owner, info.repo)
}

fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WITS_REVIEW_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("wits").join("review.toml"));
    }
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("wits")
            .join("review.toml")
    })
}

impl Config {
    /// Load the config, or an empty one when the file is absent (feeds are then
    /// simply unavailable — not an error).
    pub fn load() -> Result<Config> {
        let Some(path) = config_path() else {
            return Ok(Config {
                file: ConfigFile::default(),
            });
        };
        if !path.exists() {
            return Ok(Config {
                file: ConfigFile::default(),
            });
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: ConfigFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Config { file })
    }

    /// The feed named `name` for the repo keyed by `key`, resolved into a query.
    pub fn feed(&self, key: &str, name: &str) -> Option<FeedQuery> {
        let def = self.file.repo.get(key)?.feed.get(name)?;
        Some(def.to_query())
    }

    /// The feed's configured stack-completion mode, if it set one. `None` means
    /// the feed didn't say, so the caller falls back to the default (`auto`).
    pub fn feed_stack(&self, key: &str, name: &str) -> Option<StackMode> {
        self.file.repo.get(key)?.feed.get(name)?.stack
    }

    /// The names of every feed configured for a repo, for listing/help.
    pub fn feed_names(&self, key: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .file
            .repo
            .get(key)
            .map(|s| s.feed.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// Parse a `state` string into the two flags. Anything unrecognized (or absent)
/// falls back to the default open+draft rather than silently selecting nothing.
fn parse_states(s: Option<&str>) -> FeedStates {
    let Some(s) = s else {
        return FeedStates::default();
    };
    let mut open = false;
    let mut draft = false;
    for part in s.split(['+', ',', ' ']).filter(|p| !p.is_empty()) {
        match part.trim().to_lowercase().as_str() {
            "open" => open = true,
            "draft" => draft = true,
            _ => {}
        }
    }
    if !open && !draft {
        FeedStates::default()
    } else {
        FeedStates { open, draft }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_forms() {
        assert_eq!(
            parse_states(Some("open+draft")),
            FeedStates {
                open: true,
                draft: true
            }
        );
        assert_eq!(
            parse_states(Some("open")),
            FeedStates {
                open: true,
                draft: false
            }
        );
        assert_eq!(
            parse_states(Some("draft")),
            FeedStates {
                open: false,
                draft: true
            }
        );
        // Unknown or empty falls back to the default rather than selecting nothing.
        assert_eq!(parse_states(Some("bogus")), FeedStates::default());
        assert_eq!(parse_states(None), FeedStates::default());
    }

    #[test]
    fn resolves_a_named_feed_from_a_repo_section() {
        let toml = r#"
            [repo."github.com/mesa/mesa"]
            feed.mine = { reviewer = "@me", state = "open" }
            feed.vk = { labels = ["vulkan", "spirv"], exclude-labels = ["wip"], limit = 10 }
        "#;
        let cfg = Config {
            file: toml::from_str(toml).unwrap(),
        };
        let key = "github.com/mesa/mesa";

        let mine = cfg.feed(key, "mine").unwrap();
        assert_eq!(mine.reviewer.as_deref(), Some("@me"));
        assert_eq!(
            mine.states,
            FeedStates {
                open: true,
                draft: false
            }
        );
        assert_eq!(mine.limit, DEFAULT_LIMIT);

        let vk = cfg.feed(key, "vk").unwrap();
        assert_eq!(vk.labels, ["vulkan", "spirv"]);
        assert_eq!(vk.exclude_labels, ["wip"]);
        assert_eq!(vk.limit, 10);

        assert_eq!(cfg.feed_names(key), ["mine", "vk"]);
        assert!(cfg.feed(key, "absent").is_none());
        assert!(cfg.feed("other/repo/x", "mine").is_none());
    }

    #[test]
    fn feed_stack_mode_is_optional_and_parses() {
        let toml = r#"
            [repo."github.com/o/r"]
            feed.plain = { reviewer = "@me" }
            feed.whole = { reviewer = "@me", stack = "all" }
            feed.flat  = { reviewer = "@me", stack = "none" }
        "#;
        let cfg = Config {
            file: toml::from_str(toml).unwrap(),
        };
        let key = "github.com/o/r";
        // Unset means "no opinion" — the caller falls back to the default.
        assert_eq!(cfg.feed_stack(key, "plain"), None);
        assert_eq!(cfg.feed_stack(key, "whole"), Some(StackMode::All));
        assert_eq!(cfg.feed_stack(key, "flat"), Some(StackMode::None));
    }
}
