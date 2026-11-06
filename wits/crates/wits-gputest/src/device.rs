//! The machine's GPU, read from sysfs.
//!
//! The device keys the state and the store path, and it is deliberately the
//! *machine's* GPU, not the driver under test: probing through the driver would
//! mean a broken driver also breaks identification, and lavapipe would need a
//! special case. Sysfs answers before the test starts, driver-independently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The first DRM card with a PCI identity, and the kernel driving it. The
/// archive records it as `[device]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The PCI slot, `0000:c4:00.0`.
    pub pci: String,
    /// `vendor:device`, lower case so the path component is stable across the
    /// kernel's `PCI_ID` spelling.
    pub id: String,
    /// The kernel driver bound to the card.
    pub drm: String,
    /// The kernel release.
    pub kernel: String,
}

impl Device {
    /// The path component: `vendor:device` as `vendor-device`.
    pub fn key(&self) -> String {
        self.id.replace(':', "-")
    }
}

/// Probe `/sys/class/drm`.
pub fn probe() -> Result<Device> {
    probe_at(Path::new("/sys/class/drm"))
}

/// Probe an explicit DRM class root; the test entry point.
pub fn probe_at(root: &Path) -> Result<Device> {
    let mut cards: Vec<(u32, PathBuf)> = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let number = entry
                .file_name()
                .to_str()?
                .strip_prefix("card")?
                .parse()
                .ok()?;
            Some((number, entry.path()))
        })
        .collect();
    cards.sort_by_key(|(number, _)| *number);

    for (_, card) in &cards {
        let Ok(text) = std::fs::read_to_string(card.join("device/uevent")) else {
            continue;
        };
        let values = parse_uevent(&text);
        let Some(id) = values
            .get("PCI_ID")
            .map(|id| id.trim().to_ascii_lowercase())
        else {
            continue;
        };
        if !valid_pci_id(&id) {
            continue;
        }
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .context("reading the kernel release from /proc/sys/kernel/osrelease")?;
        return Ok(Device {
            pci: values.get("PCI_SLOT_NAME").cloned().unwrap_or_default(),
            id,
            drm: values.get("DRIVER").cloned().unwrap_or_default(),
            kernel: kernel.trim().to_owned(),
        });
    }

    bail!(
        "no DRM card with a PCI id under {}; the store is keyed by device",
        root.display()
    )
}

/// Parse a sysfs `uevent` file: `KEY=VALUE` lines, last value wins.
fn parse_uevent(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn valid_pci_id(id: &str) -> bool {
    let Some((vendor, device)) = id.split_once(':') else {
        return false;
    };
    vendor.len() == 4
        && device.len() == 4
        && vendor
            .chars()
            .chain(device.chars())
            .all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_first_pci_card_is_the_device_and_the_key_is_normalised() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("card1-DP-1/device")).unwrap();
        fs::write(
            dir.path().join("card1-DP-1/device/uevent"),
            "PCI_ID=9999:9999\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("card0/device")).unwrap();
        fs::write(
            dir.path().join("card0/device/uevent"),
            "DRIVER=amdgpu\nPCI_ID=1002:150E\nPCI_SLOT_NAME=0000:c4:00.0\n",
        )
        .unwrap();
        let device = probe_at(dir.path()).unwrap();
        assert_eq!(device.id, "1002:150e");
        assert_eq!(device.key(), "1002-150e");
        assert_eq!(device.pci, "0000:c4:00.0");
        assert_eq!(device.drm, "amdgpu");
        assert!(!device.kernel.is_empty());
    }

    #[test]
    fn a_card_without_a_pci_id_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("card0/device")).unwrap();
        fs::write(dir.path().join("card0/device/uevent"), "DRIVER=vgem\n").unwrap();
        assert!(probe_at(dir.path()).is_err());
    }

    #[test]
    fn uevent_parsing_keeps_the_last_value() {
        let values = parse_uevent("A=1\nB=x=y\nA=2\nnoise\n");
        assert_eq!(values["A"], "2");
        assert_eq!(values["B"], "x=y");
        assert!(!values.contains_key("noise"));
    }
}
