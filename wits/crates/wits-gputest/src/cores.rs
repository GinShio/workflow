//! The physical-core count.
//!
//! `jobs = "cores"` means physical cores, not logical CPUs: SMT siblings add no
//! shader-compilation throughput and only contend for the GPU. The count is
//! plugin-local because the core floor does not expose it.
//!
//! Sysfs is the first answer: every `cpuN/topology` carries `physical_package_id`
//! and `core_id`, and the unique pairs are the cores. `/proc/cpuinfo` is the
//! fallback for a kernel that does not expose topology, and the logical count is
//! the final fallback.

use std::collections::BTreeSet;
use std::path::Path;

/// Physical cores, never less than one.
pub fn physical_cores() -> u32 {
    count()
}

fn count() -> u32 {
    if let Some(n) = from_topology(Path::new("/sys/devices/system/cpu")) {
        return n;
    }
    if let Some(n) = from_cpuinfo() {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1)
}

fn from_topology(root: &Path) -> Option<u32> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut cores = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix("cpu") else {
            continue;
        };
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let topology = entry.path().join("topology");
        // One CPU without the topology files (a partial offline state) must not
        // discard the cores the others already proved.
        let (Some(package), Some(core)) = (
            read_trim(&topology.join("physical_package_id")),
            read_trim(&topology.join("core_id")),
        ) else {
            continue;
        };
        cores.insert((package, core));
    }
    (!cores.is_empty()).then_some(cores.len() as u32)
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

fn from_cpuinfo() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores = BTreeSet::new();
    let mut package = None;
    let mut core = None;
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if let (Some(package), Some(core)) = (package.take(), core.take()) {
                cores.insert((package, core));
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "physical id" => package = Some(value.trim().to_owned()),
            "core id" => core = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    (!cores.is_empty()).then_some(cores.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn topology_counts_unique_package_core_pairs() {
        let dir = tempfile::tempdir().unwrap();
        for (cpu, package, core) in [
            ("cpu0", "0", "0"),
            ("cpu1", "0", "1"),
            ("cpu2", "1", "0"),
            ("cpu3", "0", "0"),      // SMT sibling of cpu0
            ("cpu4-DP-1", "9", "9"), // not a CPU entry
        ] {
            let topology = dir.path().join(cpu).join("topology");
            fs::create_dir_all(&topology).unwrap();
            fs::write(topology.join("physical_package_id"), package).unwrap();
            fs::write(topology.join("core_id"), core).unwrap();
        }
        // A CPU whose topology is incomplete falls back to the other entries,
        // not to the whole-scan failure path.
        fs::create_dir_all(dir.path().join("cpu5")).unwrap();
        assert_eq!(from_topology(dir.path()), Some(3));
    }

    #[test]
    fn an_empty_topology_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(from_topology(dir.path()), None);
    }
}
