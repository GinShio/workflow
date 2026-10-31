//! What the last completed run of one case on one device tested, and what the
//! next run compares against.
//!
//! The file is three lines: the identity hash, the last archive id, and the
//! baseline archive id (absent on a first run). `run` and `baseline` are
//! separate because a rerun of the same identity must compare against the
//! stored baseline, not against itself.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// The identity the last completed run tested.
    pub hash: String,
    /// The archive that run produced.
    pub run: String,
    /// The archive that run compared against, when it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

/// `$XDG_STATE_HOME/wits/gputest`, else `$HOME/.local/state/wits/gputest`.
pub fn root() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("wits/gputest"));
        }
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .context("neither XDG_STATE_HOME nor HOME is set; cannot locate the state directory")?;
    Ok(PathBuf::from(home).join(".local/state/wits/gputest"))
}

/// The state file for one case and device.
pub fn path(case: &str, device: &str) -> Result<PathBuf> {
    Ok(root()?.join(case).join(format!("{device}.toml")))
}

/// Read a state file. Missing is `None`; unreadable or malformed is an error,
/// because treating it as a first run would silently drop the baseline.
pub fn read(case: &str, device: &str) -> Result<Option<State>> {
    let path = path(case, device)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("state file {} cannot be read", path.display()))
        }
    };
    let state: State = toml::from_str(&text)
        .with_context(|| format!("state file {} cannot be read", path.display()))?;
    if state.hash.is_empty() {
        bail!("state file {} is incomplete", path.display());
    }
    if !crate::time::is_run_id(&state.run)
        || state
            .baseline
            .as_deref()
            .is_some_and(|baseline| !crate::time::is_run_id(baseline))
    {
        bail!(
            "state file {} does not name run ids; refusing to use it as store paths",
            path.display()
        );
    }
    Ok(Some(state))
}

/// Write a state file with a temporary file and a rename, so a crash leaves the
/// previous state or none.
pub fn write(case: &str, device: &str, state: &State) -> Result<()> {
    let path = path(case, device)?;
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;
    let text = toml::to_string(state).context("serializing state")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating a temporary state file in {}", dir.display()))?;
    temp.write_all(text.as_bytes())
        .with_context(|| format!("writing a temporary state file in {}", dir.display()))?;
    temp.flush()
        .with_context(|| format!("flushing a temporary state file in {}", dir.display()))?;
    temp.persist(&path)
        .map_err(|err| err.error)
        .with_context(|| format!("renaming the state file into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_round_trips_and_keeps_an_absent_baseline_absent() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::testutil::EnvGuard::set("XDG_STATE_HOME", dir.path());
        let case = "radv-vk";
        let device = "1002-150e";
        assert_eq!(read(case, device).unwrap(), None);
        let state = State {
            hash: "sha256:abc".to_owned(),
            run: "20261026T213012Z".to_owned(),
            baseline: None,
        };
        write(case, device, &state).unwrap();
        assert_eq!(read(case, device).unwrap(), Some(state.clone()));
        let text = std::fs::read_to_string(path(case, device).unwrap()).unwrap();
        assert!(!text.contains("baseline"), "{text}");

        let with_baseline = State {
            baseline: Some("20261023T000000Z".to_owned()),
            ..state
        };
        write(case, device, &with_baseline).unwrap();
        assert_eq!(read(case, device).unwrap(), Some(with_baseline));
    }

    #[test]
    fn a_malformed_state_is_an_error_not_a_first_run() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::testutil::EnvGuard::set("XDG_STATE_HOME", dir.path());
        let path = path("c", "d").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "hash = \"x\"\n").unwrap();
        let err = read("c", "d").unwrap_err();
        assert!(format!("{err:#}").contains("cannot be read"));

        std::fs::write(&path, "hash = \"sha256:x\"\nrun = \"../../escape\"\n").unwrap();
        let err = read("c", "d").unwrap_err();
        assert!(format!("{err:#}").contains("refusing"), "{err:#}");
    }
}
