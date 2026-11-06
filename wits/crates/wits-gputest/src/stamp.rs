//! The install stamp: `.gputest.toml` at the root of every installed suite and
//! runner tree.
//!
//! It is the suite's analogue of CMake's `install_manifest.txt`. `install` is
//! the only writer, and a run reads the hash and the source instead of walking
//! the tree again. A missing stamp means the tree was not installed by
//! `wits gputest install`, or that install failed: `install` removes the old
//! stamp before it builds and writes the new one only after the layout check.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util;

/// The stamp's file name, at the root of the installed tree.
const FILE: &str = ".gputest.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    /// The canonical hash of the installed tree, the stamp itself excluded.
    pub hash: String,
    /// The run-id-shaped instant the tree was installed.
    pub installed: String,
    pub source: Source,
}

/// Where an installed tree came from. The archive records it as `[suite]` and
/// `[runner]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The project's registry key.
    pub project: String,
    /// The branch `install` built: the project's main branch.
    pub branch: String,
    /// Each repo's commit on that branch when it was built.
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

/// The stamp path under an install root.
pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Read the stamp under `root` and check it names `project`.
pub fn read(root: &Path, project: &str) -> Result<Stamp> {
    let path = path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("install stamp {} is missing", path.display())
        }
        Err(err) => {
            return Err(err).with_context(|| format!("reading install stamp {}", path.display()))
        }
    };
    let stamp: Stamp = toml::from_str(&text)
        .with_context(|| format!("reading install stamp {}", path.display()))?;
    if stamp.source.project != project {
        bail!(
            "install stamp {} was written for project '{}', not '{project}'",
            path.display(),
            stamp.source.project
        );
    }
    Ok(stamp)
}

pub fn write(root: &Path, stamp: &Stamp) -> Result<()> {
    let text = toml::to_string(stamp).context("serializing the install stamp")?;
    util::write_atomic(&path(root), text.as_bytes())
}

/// Remove the stamp before a new install starts. A missing stamp is fine.
pub fn remove(root: &Path) -> Result<()> {
    match std::fs::remove_file(path(root)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err)
            .with_context(|| format!("removing the old install stamp in {}", root.display())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            hash: "sha256:abc".to_owned(),
            installed: "20261026T213012Z".to_owned(),
            source: Source {
                project: "khronos/deqp".to_owned(),
                branch: "main".to_owned(),
                commits: BTreeMap::from([("main".to_owned(), "deadbeef".to_owned())]),
            },
        }
    }

    #[test]
    fn a_stamp_round_trips_and_is_checked_against_its_project() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &stamp()).unwrap();
        assert_eq!(read(dir.path(), "khronos/deqp").unwrap(), stamp());
        let err = read(dir.path(), "fd.o/piglit").unwrap_err();
        assert!(
            format!("{err:#}").contains("written for project"),
            "{err:#}"
        );
    }

    #[test]
    fn a_missing_stamp_says_so_and_removal_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let err = read(dir.path(), "khronos/deqp").unwrap_err();
        assert!(format!("{err:#}").contains("is missing"), "{err:#}");
        remove(dir.path()).unwrap();
        write(dir.path(), &stamp()).unwrap();
        remove(dir.path()).unwrap();
        assert!(!path(dir.path()).exists());
    }
}
