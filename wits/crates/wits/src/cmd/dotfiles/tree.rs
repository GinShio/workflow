//! Finding the repository, and reading files out of it according to its
//! [`Layout`](super::layout).
//!
//! Everything this module hands out is a **root-relative** path. Absolute paths
//! only appear at the moment of touching the filesystem, via [`Repo::abs`]. That
//! is not a stylistic preference: generated configs refer to each other by
//! relative path, and computing those correctly requires that every path already
//! be expressed in the same frame.
//!
//! The one piece of real logic here is [`read_toml`]. A per-overlay fragment is
//! encrypted at rest, so on a machine without that overlay's key it checks out
//! as base64 rather than TOML. That is a *normal* state of the working tree, not
//! corruption — and not something to generate from either, since the output
//! would silently lack its values. So it comes back as its own outcome rather
//! than as a parse error, and the caller decides what can be built without it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;

use wits_util::config::Resolver;
use wits_util::git::Repository;

use super::layout::{Config, Resolved, CONFIG_NAME};

/// A located dotfiles repository: where it is, and how it is arranged.
#[derive(Debug)]
pub struct Repo {
    root: PathBuf,
    layout: Resolved,
}

impl Repo {
    /// Locate and read the layout declaration.
    ///
    /// `--config` names the file outright; `--root` looks for the default name
    /// in a given directory. Failing both, `$WITS_DOTFILES_CONFIG` and
    /// `wits.dotfiles.config` are consulted, and finally the current directory's
    /// ancestors are searched. The walk comes last but matters most in practice:
    /// it is what makes the command work with no configuration at all when you
    /// are standing in the repository, which is where you are when you edit a
    /// manifest.
    pub fn open(config: Option<&Path>, root: Option<&Path>) -> Result<Self> {
        if let Some(path) = config {
            return Self::at(path);
        }
        if let Some(dir) = root {
            return Self::at(&dir.join(CONFIG_NAME));
        }

        let cwd = std::env::current_dir().context("cannot read the current directory")?;
        let repo = Repository::new(&cwd);
        let resolver = Resolver::new(Some(&repo), "wits.dotfiles", None::<String>);
        if let Some(resolved) = resolver.get("config") {
            let path = expand_tilde(&resolved.value);
            return Self::at(&path).with_context(|| format!("layout from {}", resolved.source));
        }

        for dir in cwd.ancestors() {
            let candidate = dir.join(CONFIG_NAME);
            if candidate.is_file() {
                return Self::at(&candidate);
            }
        }
        bail!(
            "no dotfiles repository found: no ancestor of {} contains {CONFIG_NAME}, \
             and neither WITS_DOTFILES_CONFIG nor wits.dotfiles.config is set",
            cwd.display()
        )
    }

    fn at(config: &Path) -> Result<Self> {
        if !config.is_file() {
            bail!("no layout declaration at {}", config.display());
        }
        let text = std::fs::read_to_string(config)
            .with_context(|| format!("reading {}", config.display()))?;
        let parsed: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", config.display()))?;
        let layout = Resolved::new(parsed).with_context(|| format!("in {}", config.display()))?;

        // The declaration's own directory is the root every path in it is
        // relative to, so the two can never disagree.
        let root = config
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let composition = root.join(&layout.composition);
        if !composition.is_file() {
            bail!(
                "{} declares its composition table at {}, which does not exist",
                config.display(),
                layout.composition.display()
            );
        }
        Ok(Self { root, layout })
    }

    pub fn layout(&self) -> &Resolved {
        &self.layout
    }

    /// A root-relative path made absolute, for actually touching the disk.
    pub fn abs(&self, relative: &Path) -> PathBuf {
        self.root.join(relative)
    }

    /// Every module directory name, sorted, minus the names this layout
    /// reserves for its own files. Sorted because the order modules are layered
    /// in has to be a property of the repository, not of the filesystem.
    pub fn modules(&self) -> Result<Vec<String>> {
        let reserved = self.layout.reserved_module_names();
        let mut names = self.dirs_in(&self.layout.modules)?;
        names.retain(|name| !reserved.contains(name));
        Ok(names)
    }

    /// The overlay directories a module has on disk, sorted. Used to tell "this
    /// module has no content for that overlay" (normal) apart from "this module
    /// has content nobody deploys" (a check finding).
    pub fn overlays_of(&self, app: &str) -> Result<Vec<String>> {
        let fragments = self.layout.module_fragments.to_string_lossy().into_owned();
        let mut names = self.dirs_in(&self.layout.modules.join(app))?;
        names.retain(|name| *name != fragments);
        Ok(names)
    }

    /// Every `*.toml` directly inside a fragment directory, as
    /// `(file stem, root-relative path)`, sorted by stem.
    ///
    /// Scanned rather than probed by overlay name, because a fragment directory
    /// now holds an unknown number of files per overlay — and because scanning
    /// is what makes a file that belongs to *no* overlay visible instead of
    /// silently ignored. A directory that does not exist is simply empty: most
    /// modules have no per-overlay values at all.
    pub fn fragments_in(&self, relative: &Path) -> Result<Vec<(String, PathBuf)>> {
        let dir = self.abs(relative);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = PathBuf::from(entry.file_name());
            if name.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(stem) = name.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            found.push((stem, relative.join(entry.file_name())));
        }
        found.sort();
        Ok(found)
    }

    fn dirs_in(&self, relative: &Path) -> Result<Vec<String>> {
        let dir = self.abs(relative);
        let mut names = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Resolve an install's path within one overlay, reporting whether the
    /// target is a directory — a deployment backend distinguishes a directory
    /// source from a file source, and this is where that is known.
    pub fn overlay_target(&self, app: &str, overlay: &str, path: &str) -> Option<Target> {
        let base = self.layout.modules.join(app).join(overlay);
        let relative = if path == "." {
            base
        } else {
            base.join(path.trim_end_matches('/'))
        };
        let meta = std::fs::metadata(self.abs(&relative)).ok()?;
        Some(Target {
            is_dir: meta.is_dir(),
        })
    }

    /// Read a file every host is generated from. Unlike a fragment it has no
    /// overlay that could be skipped instead, so still being encrypted stops
    /// the run.
    pub fn read<T: DeserializeOwned>(&self, relative: &Path) -> Result<T> {
        let path = self.abs(relative);
        read_toml(&path)?.with_context(|| {
            format!(
                "{} is still encrypted — this clone has no transcrypt key for it, \
                 and every host is generated from it",
                path.display()
            )
        })
    }

    /// Read a file this clone may legitimately hold as ciphertext: `None`
    /// means it is still encrypted.
    pub fn read_unless_locked<T: DeserializeOwned>(&self, relative: &Path) -> Result<Option<T>> {
        read_toml(&self.abs(relative))
    }

    pub fn exists(&self, relative: &Path) -> bool {
        self.abs(relative).is_file()
    }
}

pub struct Target {
    pub is_dir: bool,
}

/// Parse a TOML file, telling "still encrypted" (`None`) apart from
/// "malformed" (an error).
///
/// They are different states with different fixes: ciphertext is what a clone
/// without the file's transcrypt key normally holds, while a parse error is a
/// bug in the file. A TOML parse error on a wall of base64 would tell you
/// neither.
fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if wits_util::crypto::is_encrypted(&bytes) {
        return Ok(None);
    }
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    toml::from_str(&text)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Reject a path that would reach outside its overlay. Manifests are trusted
/// input, but `dst`/`path` end up in generated config that may run with elevated
/// privilege, so the cheap structural check is worth it.
pub fn is_contained(path: &str) -> bool {
    !Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(raw),
        },
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository whose layout is deliberately *not* the defaults, so that
    /// anything still hard-coded fails here.
    fn scaffold() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_NAME),
            "[layout]\nmodules = 'apps'\nmodule_manifest = 'app.toml'\n\
             module_fragments = 'private'\ncomposition = 'etc/machines.toml'\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(dir.path().join("etc/machines.toml"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("apps")).unwrap();
        dir
    }

    #[test]
    fn a_missing_or_empty_directory_is_not_a_repository() {
        let empty = tempfile::tempdir().unwrap();
        assert!(Repo::open(None, Some(empty.path())).is_err());

        let real = scaffold();
        assert!(Repo::open(None, Some(real.path())).is_ok());
    }

    /// The composition table is named by the declaration, so a declaration that
    /// points at nothing is a broken repository rather than an empty one.
    #[test]
    fn a_declaration_pointing_nowhere_says_which_path_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_NAME),
            "[layout]\ncomposition = 'etc/machines.toml'\n",
        )
        .unwrap();

        let err = Repo::open(None, Some(dir.path())).unwrap_err().to_string();
        assert!(err.contains("etc/machines.toml"), "got: {err}");
    }

    #[test]
    fn the_directory_holding_the_declared_files_is_not_a_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_NAME),
            "[layout]\ncomposition = 'apps/meta/machines.toml'\nmodules = 'apps'\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("apps/meta")).unwrap();
        std::fs::write(dir.path().join("apps/meta/machines.toml"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("apps/git")).unwrap();

        let repo = Repo::open(None, Some(dir.path())).unwrap();
        assert_eq!(repo.modules().unwrap(), vec!["git".to_owned()]);
    }

    #[test]
    fn overlays_exclude_the_declared_fragment_directory() {
        let dir = scaffold();
        for sub in ["common", "personal", "private"] {
            std::fs::create_dir_all(dir.path().join("apps/git").join(sub)).unwrap();
        }
        let repo = Repo::open(None, Some(dir.path())).unwrap();
        assert_eq!(
            repo.overlays_of("git").unwrap(),
            vec!["common".to_owned(), "personal".to_owned()]
        );
    }

    #[test]
    fn a_dot_path_targets_the_overlay_root() {
        let dir = scaffold();
        let overlay = dir.path().join("apps/git/common");
        std::fs::create_dir_all(&overlay).unwrap();
        std::fs::write(overlay.join("config"), "").unwrap();
        let repo = Repo::open(None, Some(dir.path())).unwrap();

        assert!(repo.overlay_target("git", "common", ".").unwrap().is_dir);
        assert!(
            !repo
                .overlay_target("git", "common", "config")
                .unwrap()
                .is_dir
        );
        assert!(repo.overlay_target("git", "personal", ".").is_none());
    }

    /// base64 of `Salted__ciphertext-goes-here` — the salt header is what marks
    /// a transcrypt packet, and everything after it is opaque.
    const LOCKED: &str = "U2FsdGVkX19jaXBoZXJ0ZXh0LWdvZXMtaGVyZQ==";

    /// A locked file is base64, which is also valid UTF-8 and invalid TOML;
    /// without the header check it would surface as a bewildering parse error,
    /// and a caller could not tell it from a file that is genuinely broken.
    #[test]
    fn a_locked_file_reads_as_locked_and_a_broken_one_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("personal.toml");
        std::fs::write(&locked, LOCKED).unwrap();
        assert!(read_toml::<toml::Table>(&locked).unwrap().is_none());

        let broken = dir.path().join("common.toml");
        std::fs::write(&broken, "[variables\n").unwrap();
        let err = read_toml::<toml::Table>(&broken).unwrap_err();
        assert!(format!("{err:#}").contains("parsing"), "got: {err:#}");
    }

    /// Only a fragment has an overlay that can be skipped instead. Everything
    /// else feeds every host, so a locked one still stops the run, and says
    /// why rather than failing to parse.
    #[test]
    fn a_locked_shared_input_stops_the_run() {
        let dir = scaffold();
        std::fs::write(dir.path().join("etc/machines.toml"), LOCKED).unwrap();
        let repo = Repo::open(None, Some(dir.path())).unwrap();
        let composition = Path::new("etc/machines.toml");

        let err = repo
            .read::<toml::Table>(composition)
            .unwrap_err()
            .to_string();
        assert!(err.contains("still encrypted"), "got: {err}");
        assert!(
            repo.read_unless_locked::<toml::Table>(composition)
                .unwrap()
                .is_none(),
            "the same file, read as one that may be locked"
        );
    }

    #[test]
    fn parent_components_are_rejected() {
        assert!(is_contained("agents/"));
        assert!(is_contained("."));
        assert!(!is_contained("../other"));
        assert!(!is_contained("a/../../b"));
    }
}
