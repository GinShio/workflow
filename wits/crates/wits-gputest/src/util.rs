//! Small helpers shared by the pipeline and the verbs.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use wits_actions::Env;

/// One environment variable's raw value.
pub fn env_os<'a>(env: &'a Env, key: &str) -> Option<&'a OsStr> {
    env.get(OsStr::new(key)).map(|value| value.as_os_str())
}

/// One environment variable's value as text. The identity's fixed variable set
/// is path and glob syntax, and a non-UTF-8 value there cannot be interpreted.
pub fn env_text<'a>(env: &'a Env, key: &str) -> Result<Option<&'a str>> {
    let Some(value) = env_os(env, key) else {
        return Ok(None);
    };
    value
        .to_str()
        .map(Some)
        .with_context(|| format!("environment entry {key} is not UTF-8"))
}

/// A path-list variable split the way the loader splits it, minus empty
/// components.
///
/// An empty component is the *current directory* to the loader, not a named
/// path; the identity is built from named paths, so `LD_LIBRARY_PATH` is the
/// build's list and `VK_DRIVER_FILES=""` names no manifest rather than a
/// phantom one.
pub fn env_paths(env: &Env, key: &str) -> Vec<PathBuf> {
    match env_os(env, key) {
        Some(value) => std::env::split_paths(value)
            .filter(|path| !path.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Read a file's text, with the path in the error.
pub fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Read a whole file, with the path in the error.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// Whether any execute bit is set.
pub fn is_executable(path: &Path) -> Result<bool> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("reading metadata of {}", path.display()))?;
    Ok(meta.permissions().mode() & 0o111 != 0)
}
