//! What the last completed run of one case on one device tested, and what the
//! next run compares against.
//!
//! `run` and `baseline` are separate because a rerun of the same identity must
//! compare against the stored baseline, not against itself.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// The identity the last completed run tested.
    pub hash: String,
    /// The archive that run produced.
    pub run: String,
    /// The archive that run compared against, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

/// `$XDG_STATE_HOME/wits/gputest`, else `$HOME/.local/state/wits/gputest`.
fn root() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|xdg| !xdg.is_empty()) {
        return Ok(PathBuf::from(xdg).join("wits/gputest"));
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
    toml::from_str(&text)
        .map(Some)
        .with_context(|| format!("state file {} cannot be read", path.display()))
}

pub fn write(case: &str, device: &str, state: &State) -> Result<()> {
    let text = toml::to_string(state).context("serializing state")?;
    util::write_atomic(&path(case, device)?, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_round_trips_and_a_malformed_one_is_an_error() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::testutil::EnvGuard::set("XDG_STATE_HOME", dir.path());
        assert_eq!(read("radv-vk", "1002-150e").unwrap(), None);

        let state = State {
            hash: "sha256:abc".to_owned(),
            run: "20261026T213012Z".to_owned(),
            baseline: None,
        };
        write("radv-vk", "1002-150e", &state).unwrap();
        assert_eq!(read("radv-vk", "1002-150e").unwrap(), Some(state.clone()));
        let text = std::fs::read_to_string(path("radv-vk", "1002-150e").unwrap()).unwrap();
        assert!(!text.contains("baseline"), "{text}");

        let with_baseline = State {
            baseline: Some("20261023T000000Z".to_owned()),
            ..state
        };
        write("radv-vk", "1002-150e", &with_baseline).unwrap();
        assert_eq!(read("radv-vk", "1002-150e").unwrap(), Some(with_baseline));

        std::fs::write(path("radv-vk", "1002-150e").unwrap(), "hash = \"x\"\n").unwrap();
        let err = read("radv-vk", "1002-150e").unwrap_err();
        assert!(format!("{err:#}").contains("cannot be read"), "{err:#}");
    }
}
