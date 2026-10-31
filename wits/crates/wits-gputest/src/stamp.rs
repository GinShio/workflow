//! The install stamp: `.gputest.toml` at the root of every installed suite and
//! runner tree.
//!
//! It is the suite's analogue of CMake's `install_manifest.txt`. `install` is
//! the only writer; `run` reads the hash and the commits and never re-walks the
//! tree. A missing or unreadable stamp is an error, not a first run: it means
//! the tree was not installed by `wits gputest install`.
//!
//! The stamp is written atomically after the build and the asset check, and it
//! is removed before a new build starts, so a failed install cannot leave a
//! half-updated tree looking installed.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::content::STAMP;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub hash: String,
    pub installed: String,
    pub project: StampProject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StampProject {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

/// The stamp path under an install root.
pub fn path(root: &Path) -> PathBuf {
    root.join(STAMP)
}

/// Read and verify a stamp names `expected_project`.
pub fn read(root: &Path, expected_project: &str) -> Result<Stamp> {
    let path = path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => bail!(
            "install stamp {} is missing; the tree was not installed by \
             `wits gputest install` (or that install failed before stamping)",
            path.display()
        ),
        Err(err) => {
            return Err(err).with_context(|| format!("reading install stamp {}", path.display()))
        }
    };
    let stamp: Stamp = toml::from_str(&text)
        .with_context(|| format!("reading install stamp {}", path.display()))?;
    if stamp.hash.is_empty() || !stamp.hash.starts_with("sha256:") {
        bail!(
            "install stamp {} has no usable tree hash (got {:?})",
            path.display(),
            stamp.hash
        );
    }
    if stamp.project.key != expected_project {
        bail!(
            "install stamp {} names project '{}', not '{}'; reinstall with `wits gputest install`",
            path.display(),
            stamp.project.key,
            expected_project
        );
    }
    Ok(stamp)
}

/// Write a stamp atomically: a temporary file beside it, then a rename.
pub fn write(root: &Path, stamp: &Stamp) -> Result<()> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("creating install root {}", root.display()))?;
    let text = toml::to_string(stamp).context("serializing the install stamp")?;
    let mut temp = tempfile::NamedTempFile::new_in(root)
        .with_context(|| format!("creating a temporary stamp in {}", root.display()))?;
    temp.write_all(text.as_bytes())
        .with_context(|| format!("writing a temporary stamp in {}", root.display()))?;
    temp.flush()
        .with_context(|| format!("flushing a temporary stamp in {}", root.display()))?;
    temp.persist(path(root))
        .map_err(|err| err.error)
        .with_context(|| format!("renaming the install stamp into {}", root.display()))?;
    Ok(())
}

/// Remove an old stamp before a new install starts. A missing stamp is fine.
pub fn remove(root: &Path) -> Result<()> {
    match std::fs::remove_file(path(root)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("removing the old install stamp in {}", root.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            hash: "sha256:abc".to_owned(),
            installed: "20261026T213012Z".to_owned(),
            project: StampProject {
                key: "khronos/deqp".to_owned(),
                branch: Some("main".to_owned()),
                commits: BTreeMap::from([("main".to_owned(), "deadbeef".to_owned())]),
            },
        }
    }

    #[test]
    fn a_stamp_round_trips_and_is_verified_by_project_key() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &stamp()).unwrap();
        let read_back = read(dir.path(), "khronos/deqp").unwrap();
        assert_eq!(read_back.hash, "sha256:abc");
        assert_eq!(read_back.project.commits["main"], "deadbeef");
        assert!(read(dir.path(), "khronos/piglit").is_err());
    }

    #[test]
    fn a_missing_stamp_names_the_install_command() {
        let dir = tempfile::tempdir().unwrap();
        let err = read(dir.path(), "khronos/deqp").unwrap_err();
        assert!(format!("{err:#}").contains("wits gputest install"));
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        remove(dir.path()).unwrap();
        write(dir.path(), &stamp()).unwrap();
        remove(dir.path()).unwrap();
        assert!(!path(dir.path()).exists());
    }
}
