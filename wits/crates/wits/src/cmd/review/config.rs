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
//!
//! The file is read strictly: every key and value means what
//! `docs/commands/review.rst` says it does, or loading fails and names the file.
//! Read leniently, a typo is not an error but a different feed — a misspelled
//! facet is dropped, and the feed widens to every open MR.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use wits_core::forge::RemoteInfo;
use wits_core::forge::{FeedQuery, FeedStates};
use wits_core::git::Repository;

use super::StackMode;

/// How many MRs a feed pulls when it doesn't say otherwise — a cap so a large
/// repo can't flood the inbox.
const DEFAULT_LIMIT: usize = 30;

/// The whole config file: repos, each holding named feeds.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    repo: BTreeMap<String, RepoSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoSection {
    #[serde(default)]
    feed: BTreeMap<String, FeedDef>,
}

/// One feed's faceted filter as written in TOML.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedDef {
    #[serde(default)]
    state: FeedState,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default, rename = "exclude-labels")]
    exclude_labels: Vec<String>,
    author: Option<String>,
    assignee: Option<String>,
    reviewer: Option<String>,
    search: Option<String>,
    limit: Option<NonZeroUsize>,
    /// How much of each matched MR's stack to complete (`none`/`auto`/`all`).
    /// A per-feed default the `--stack` flag overrides; unset means `auto`.
    stack: Option<StackMode>,
}

/// Which lifecycle states a feed pulls, in the three spellings the guide lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
enum FeedState {
    #[default]
    #[serde(rename = "open+draft")]
    OpenAndDraft,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "draft")]
    Draft,
}

impl FeedState {
    fn states(self) -> FeedStates {
        match self {
            FeedState::OpenAndDraft => FeedStates {
                open: true,
                draft: true,
            },
            FeedState::Open => FeedStates {
                open: true,
                draft: false,
            },
            FeedState::Draft => FeedStates {
                open: false,
                draft: true,
            },
        }
    }
}

impl FeedDef {
    /// Resolve this feed definition into a query. Borrows `self` (the fields are
    /// cloned into the owned [`FeedQuery`] regardless), so the lookup needs no
    /// hand-written clone of `FeedDef`.
    fn to_query(&self) -> FeedQuery {
        FeedQuery {
            states: self.state.states(),
            labels: self.labels.clone(),
            exclude_labels: self.exclude_labels.clone(),
            author: self.author.clone(),
            assignee: self.assignee.clone(),
            reviewer: self.reviewer.clone(),
            search: self.search.clone(),
            limit: self.limit.map_or(DEFAULT_LIMIT, NonZeroUsize::get),
        }
    }

    /// The value rule the field types cannot carry: a facet the feed names must
    /// name something, since an empty one would reach the forge as a filter on
    /// nothing.
    fn check(&self) -> Result<()> {
        let facets = [
            ("author", &self.author),
            ("assignee", &self.assignee),
            ("reviewer", &self.reviewer),
            ("search", &self.search),
        ];
        for (key, value) in facets {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                bail!("`{key}` is empty");
            }
        }
        for (key, labels) in [
            ("labels", &self.labels),
            ("exclude-labels", &self.exclude_labels),
        ] {
            if labels.iter().any(|label| label.trim().is_empty()) {
                bail!("`{key}` holds an empty label");
            }
        }
        Ok(())
    }
}

/// The loaded configuration, and where it was looked for.
pub struct Config {
    /// The file the lookup settled on; `None` when there was nowhere to look.
    path: Option<PathBuf>,
    /// Whether that file exists. An absent default file is a legitimate empty
    /// config, but an error message still has to say it was absent.
    found: bool,
    file: ConfigFile,
}

/// The repo key a section is filed under.
pub fn repo_key(info: &RemoteInfo) -> String {
    format!("{}/{}/{}", info.host, info.owner, info.repo)
}

/// `__feeds`: the feed names `fetch --feed` accepts in this checkout's
/// repository, sorted, one per line. The consumer is fish completion, which
/// reads this instead of keeping its own copy of the file lookup and a parser
/// of the file — so the contract is the output shape alone: nothing else on
/// stdout.
pub fn run_names(repo: &Repository) -> Result<()> {
    let ctx = super::local(repo)?;
    let cfg = Config::load()?;
    for name in cfg.feed_names(&repo_key(&ctx.target)) {
        println!("{name}");
    }
    Ok(())
}

/// Where the file is when `WITS_REVIEW_CONFIG` does not name one: XDG's config
/// directory, of which `$HOME/.config` is the XDG default.
fn default_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("wits").join("review.toml"))
}

/// Parse and check a config file's text. Serde refuses unknown keys and
/// misspelled values; what remains is the rule on repo keys and
/// [`FeedDef::check`].
fn parse(text: &str) -> Result<ConfigFile> {
    let file: ConfigFile = toml::from_str(text)?;
    for (key, section) in &file.repo {
        check_repo_key(key)?;
        for (name, feed) in &section.feed {
            feed.check()
                .with_context(|| format!("[repo.\"{key}\"] feed '{name}'"))?;
        }
    }
    Ok(file)
}

/// Refuse a key that can never equal a [`repo_key`]: remote parsing yields
/// whole `/`-separated segments (a GitLab subgroup adding more) and strips a
/// `.git` suffix, so a key outside that shape names no checkout and would
/// otherwise sit in the file unmatched.
fn check_repo_key(key: &str) -> Result<()> {
    let segments: Vec<&str> = key.split('/').collect();
    let well_formed = segments.len() >= 3
        && segments.iter().all(|segment| !segment.is_empty())
        && !key.chars().any(char::is_whitespace)
        && !key.ends_with(".git");
    if !well_formed {
        bail!(
            "[repo.\"{key}\"] is not a repository; write it as host/owner/repo, \
             as in [repo.\"github.com/mesa/mesa\"]"
        );
    }
    Ok(())
}

impl Config {
    /// Load the config. A default location with no file is an empty config, so
    /// feeds are simply unavailable; a `WITS_REVIEW_CONFIG` naming no file is an
    /// error, since an instruction that cannot be carried out must not read as
    /// "nothing configured".
    pub fn load() -> Result<Config> {
        if let Some(named) = std::env::var_os("WITS_REVIEW_CONFIG") {
            let path = PathBuf::from(named);
            if !path.is_file() {
                bail!(
                    "WITS_REVIEW_CONFIG points at {} which is not a file",
                    path.display()
                );
            }
            return Config::read(path);
        }
        match default_path() {
            Some(path) if path.exists() => Config::read(path),
            path => Ok(Config {
                path,
                found: false,
                file: ConfigFile::default(),
            }),
        }
    }

    fn read(path: PathBuf) -> Result<Config> {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file = parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Config {
            path: Some(path),
            found: true,
            file,
        })
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

    /// The names of every feed configured for a repo, sorted.
    pub fn feed_names(&self, key: &str) -> Vec<String> {
        self.file
            .repo
            .get(key)
            .map(|s| s.feed.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The file the feeds were read from, when one exists.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref().filter(|_| self.found)
    }

    /// Why the repo keyed by `key` has no feeds, naming the file consulted — for
    /// an error that has to tell a missing file, a section filed under another
    /// spelling of the repo, and a section with no feed in it apart.
    pub fn why_no_feeds(&self, key: &str) -> String {
        let Some(path) = &self.path else {
            return "there is no config file to read: set WITS_REVIEW_CONFIG, \
                    XDG_CONFIG_HOME, or HOME"
                .to_owned();
        };
        if !self.found {
            return format!("{} does not exist", path.display());
        }
        if self.file.repo.contains_key(key) {
            return format!(
                "its [repo.\"{key}\"] section in {} holds no feed",
                path.display()
            );
        }
        let keys: Vec<&str> = self.file.repo.keys().map(String::as_str).collect();
        if keys.is_empty() {
            format!("{} configures no repository", path.display())
        } else {
            format!("{} configures only {}", path.display(), keys.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(text: &str) -> Config {
        Config {
            path: Some(PathBuf::from("/cfg/review.toml")),
            found: true,
            file: parse(text).unwrap(),
        }
    }

    fn refusal(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    #[test]
    fn state_takes_exactly_the_three_documented_spellings() {
        let cfg = loaded(
            r#"
            [repo."github.com/o/r"]
            feed.both  = { state = "open+draft" }
            feed.open  = { state = "open" }
            feed.draft = { state = "draft" }
            feed.unset = { reviewer = "@me" }
        "#,
        );
        let states = |name: &str| cfg.feed("github.com/o/r", name).unwrap().states;
        assert_eq!(
            states("both"),
            FeedStates {
                open: true,
                draft: true
            }
        );
        assert_eq!(
            states("open"),
            FeedStates {
                open: true,
                draft: false
            }
        );
        assert_eq!(
            states("draft"),
            FeedStates {
                open: false,
                draft: true
            }
        );
        assert_eq!(states("unset"), FeedStates::default());

        // Anything else used to fall back to open+draft in silence.
        for bad in ["opne", "draft+open", "Open", "open,draft", ""] {
            let err = refusal(&format!(
                "[repo.\"github.com/o/r\"]\nfeed.x = {{ state = \"{bad}\" }}"
            ));
            assert!(err.contains("open+draft"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn an_unknown_key_is_refused_at_every_level() {
        // A misspelled facet: read leniently, the feed would match every open MR.
        let err = refusal("[repo.\"github.com/o/r\"]\nfeed.x = { label = [\"XDL\"] }");
        assert!(err.contains("label"), "{err}");
        let err = refusal("[repo.\"github.com/o/r\"]\nfeeds.x = { labels = [\"XDL\"] }");
        assert!(err.contains("feeds"), "{err}");
        let err = refusal("[repos.\"github.com/o/r\"]\nfeed.x = { labels = [\"XDL\"] }");
        assert!(err.contains("repos"), "{err}");
    }

    #[test]
    fn a_zero_limit_is_refused() {
        let err = refusal("[repo.\"github.com/o/r\"]\nfeed.x = { limit = 0 }");
        assert!(err.contains("nonzero"), "{err}");
    }

    #[test]
    fn an_empty_facet_is_refused() {
        for (feed, key) in [
            (r#"{ author = "" }"#, "author"),
            (r#"{ reviewer = "  " }"#, "reviewer"),
            (r#"{ search = "" }"#, "search"),
            (r#"{ labels = ["XDL", ""] }"#, "labels"),
            (r#"{ exclude-labels = [" "] }"#, "exclude-labels"),
        ] {
            let err = refusal(&format!("[repo.\"github.com/o/r\"]\nfeed.x = {feed}"));
            assert!(
                err.contains(key) && err.contains("feed 'x'"),
                "{feed}: {err}"
            );
        }
    }

    #[test]
    fn a_repo_key_that_cannot_name_a_repository_is_refused() {
        for bad in [
            "https://github.com/o/r",
            "github.com/o/r.git",
            "github.com/r",
            "github.com//r",
            "github.com/o/r ",
        ] {
            let err = refusal(&format!(
                "[repo.\"{bad}\"]\nfeed.x = {{ reviewer = \"@me\" }}"
            ));
            assert!(err.contains("host/owner/repo"), "{bad:?}: {err}");
        }
        // A GitLab subgroup is more segments, not a malformed key.
        loaded("[repo.\"gitlab.com/group/sub/proj\"]\nfeed.x = { reviewer = \"@me\" }");
    }

    #[test]
    fn resolves_a_named_feed_from_a_repo_section() {
        let cfg = loaded(
            r#"
            [repo."github.com/mesa/mesa"]
            feed.mine = { reviewer = "@me", state = "open" }
            feed.vk = { labels = ["vulkan", "spirv"], exclude-labels = ["wip"], limit = 10 }
        "#,
        );
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
        let cfg = loaded(
            r#"
            [repo."github.com/o/r"]
            feed.plain = { reviewer = "@me" }
            feed.whole = { reviewer = "@me", stack = "all" }
            feed.flat  = { reviewer = "@me", stack = "none" }
        "#,
        );
        let key = "github.com/o/r";
        // Unset means "no opinion" — the caller falls back to the default.
        assert_eq!(cfg.feed_stack(key, "plain"), None);
        assert_eq!(cfg.feed_stack(key, "whole"), Some(StackMode::All));
        assert_eq!(cfg.feed_stack(key, "flat"), Some(StackMode::None));
    }

    #[test]
    fn why_no_feeds_tells_the_absences_apart() {
        let key = "github.com/o/r";
        let missing = Config {
            path: Some(PathBuf::from("/cfg/review.toml")),
            found: false,
            file: ConfigFile::default(),
        };
        assert_eq!(missing.why_no_feeds(key), "/cfg/review.toml does not exist");
        assert!(missing.path().is_none());

        let other = loaded("[repo.\"github.com/O/R\"]\nfeed.x = { reviewer = \"@me\" }");
        assert_eq!(
            other.why_no_feeds(key),
            "/cfg/review.toml configures only github.com/O/R"
        );
        assert_eq!(other.path(), Some(Path::new("/cfg/review.toml")));

        let empty = loaded("[repo.\"github.com/o/r\"]");
        assert!(
            empty.why_no_feeds(key).contains("holds no feed"),
            "{}",
            empty.why_no_feeds(key)
        );

        let nowhere = Config {
            path: None,
            found: false,
            file: ConfigFile::default(),
        };
        assert!(nowhere.why_no_feeds(key).contains("WITS_REVIEW_CONFIG"));
    }
}
