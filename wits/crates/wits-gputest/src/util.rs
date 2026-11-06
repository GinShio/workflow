//! Small helpers shared by the pipeline and the verbs.

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use wits_actions::Env;

/// One environment variable's value as text. The variables identity reads are
/// paths and globs, and a non-UTF-8 value there cannot be interpreted.
pub fn env_text<'a>(env: &'a Env, key: &str) -> Result<Option<&'a str>> {
    env.get(OsStr::new(key))
        .map(|value| {
            value
                .to_str()
                .with_context(|| format!("environment entry {key} is not UTF-8"))
        })
        .transpose()
}

/// A path-list variable split the way the loader splits it, minus empty
/// components.
///
/// An empty component is the *current directory* to the loader, not a named
/// path; the identity is built from named paths, so `LD_LIBRARY_PATH` is the
/// build's list and `VK_DRIVER_FILES=""` names no manifest rather than a
/// phantom one.
pub fn env_paths(env: &Env, key: &str) -> Vec<PathBuf> {
    match env.get(OsStr::new(key)) {
        Some(value) => std::env::split_paths(value)
            .filter(|path| !path.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Whether any execute bit is set.
pub fn is_executable(path: &Path) -> Result<bool> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("reading metadata of {}", path.display()))?;
    Ok(meta.permissions().mode() & 0o111 != 0)
}

/// Replace `path` with `bytes` so that a crash or a power loss leaves the old
/// file or the new one, never a torn one: a temporary file beside it, synced,
/// then renamed over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating a temporary file in {}", dir.display()))?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .with_context(|| format!("writing a temporary file in {}", dir.display()))?;
    temp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("renaming a temporary file to {}", path.display()))?;
    Ok(())
}
