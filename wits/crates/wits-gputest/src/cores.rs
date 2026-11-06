//! The physical-core count `jobs = "cores"` resolves to.
//!
//! Physical cores, not logical CPUs: SMT siblings add no shader-compilation
//! throughput and only contend for the GPU. Every `cpuN/topology` carries
//! `physical_package_id` and `core_id`, and the unique pairs are the cores. A
//! system that hides the topology files (a container without sysfs) gets the
//! logical count.

use std::collections::BTreeSet;
use std::path::Path;

/// Physical cores, never less than one.
pub fn physical_cores() -> u32 {
    from_topology(Path::new("/sys/devices/system/cpu")).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    })
}

fn from_topology(root: &Path) -> Option<u32> {
    let mut cores = BTreeSet::new();
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("cpu")) else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let topology = entry.path().join("topology");
        // An offline CPU has no topology files; the online ones still count.
        let read = |file: &str| std::fs::read_to_string(topology.join(file)).ok();
        if let (Some(package), Some(core)) = (read("physical_package_id"), read("core_id")) {
            cores.insert((package.trim().to_owned(), core.trim().to_owned()));
        }
    }
    u32::try_from(cores.len()).ok().filter(|&n| n > 0)
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
        fs::create_dir_all(dir.path().join("cpu5")).unwrap();
        assert_eq!(from_topology(dir.path()), Some(3));
    }

    #[test]
    fn an_empty_topology_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(from_topology(dir.path()), None);
    }
}
