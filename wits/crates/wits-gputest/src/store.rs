//! The archive store and its sweep.
//!
//! One archive per run at `<root>/<case>/<device>/<id>.tar.zst`. The store is a
//! person's result collection, not an index: state lives elsewhere, `prune`
//! only deletes archives, and an archive that ages out never resurrects a run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::Store;

/// One archive found in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRef {
    pub device: String,
    pub id: String,
    pub path: PathBuf,
}

/// The store path for one archive.
pub fn archive_path(store: &Store, case: &str, device: &str, id: &str) -> PathBuf {
    store
        .root
        .join(case)
        .join(device)
        .join(format!("{id}.tar.zst"))
}

/// The directory an archive lives in.
pub fn archive_dir(store: &Store, case: &str, device: &str) -> PathBuf {
    store.root.join(case).join(device)
}

/// Copy the archive written in the run directory into the store: a `.part`
/// beside its destination, then a rename. The rename stays inside the store's
/// filesystem, so it is atomic even when the runtime directory is another one.
pub fn install_archive(
    store: &Store,
    case: &str,
    device: &str,
    id: &str,
    source: &Path,
) -> Result<PathBuf> {
    let dir = archive_dir(store, case, device);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating archive directory {}", dir.display()))?;
    let destination = archive_path(store, case, device, id);
    let part = dir.join(format!("{id}.tar.zst.part"));
    std::fs::copy(source, &part).with_context(|| {
        format!(
            "copying archive {} into the store as {}",
            source.display(),
            part.display()
        )
    })?;
    std::fs::rename(&part, &destination)
        .with_context(|| format!("renaming {} to {}", part.display(), destination.display()))?;
    Ok(destination)
}

/// Every archive for one case, across devices, newest id first.
pub fn archives(store: &Store, case: &str) -> Result<Vec<ArchiveRef>> {
    let case_dir = store.root.join(case);
    let mut out = Vec::new();
    let devices = match std::fs::read_dir(&case_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading archive directory {}", case_dir.display()))
        }
    };
    for device in devices {
        let device = device?;
        if !device.file_type()?.is_dir() {
            continue;
        }
        let device_name = device.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(device.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".tar.zst") else {
                continue;
            };
            out.push(ArchiveRef {
                device: device_name.clone(),
                id: id.to_owned(),
                path: entry.path(),
            });
        }
    }
    out.sort_by(|a, b| {
        let (a_ts, a_suffix) = crate::time::run_order(&a.id);
        let (b_ts, b_suffix) = crate::time::run_order(&b.id);
        b_ts.cmp(&a_ts)
            .then_with(|| b_suffix.cmp(&a_suffix))
            .then_with(|| a.device.cmp(&b.device))
    });
    Ok(out)
}

/// Find one archive by id across the case's devices. A repeated id on two
/// devices is ambiguous and named as such.
pub fn find(store: &Store, case: &str, id: &str) -> Result<ArchiveRef> {
    let matches: Vec<ArchiveRef> = archives(store, case)?
        .into_iter()
        .filter(|archive| archive.id == id)
        .collect();
    match matches.len() {
        0 => bail!(
            "no archive {id} for case '{case}' under {}",
            store.root.display()
        ),
        1 => Ok(matches.into_iter().next().expect("length 1")),
        _ => bail!(
            "archive {id} exists on more than one device ({}); the store path needs a device selector",
            matches
                .iter()
                .map(|archive| archive.device.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// What a prune did or would do.
#[derive(Debug, Default)]
pub struct PruneReport {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<String>,
}

/// Remove archives older than `store.archive_days`, every `.part` (the crash
/// leftover of an interrupted install; `prune` assumes no run is writing one),
/// and empty case/device directories. State is left alone: it is the fact that
/// a build was tested, not the evidence.
pub fn prune(store: &Store, dry_run: bool) -> Result<PruneReport> {
    let cutoff = crate::time::now_secs().saturating_sub(store.archive_days.saturating_mul(86_400));
    let mut report = PruneReport::default();
    let cases = match std::fs::read_dir(&store.root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(err) => {
            return Err(err).with_context(|| format!("reading store root {}", store.root.display()))
        }
    };

    for case in cases {
        let case = case?;
        if !case.file_type()?.is_dir() {
            continue;
        }
        let case_dir = case.path();
        let devices = std::fs::read_dir(&case_dir)?;
        for device in devices {
            let device = device?;
            if !device.file_type()?.is_dir() {
                continue;
            }
            let device_dir = device.path();
            for entry in std::fs::read_dir(&device_dir)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let expired = if name.ends_with(".part") {
                    true
                } else if let Some(id) = name.strip_suffix(".tar.zst") {
                    match crate::time::parse_run_id(id) {
                        Some(timestamp) => timestamp < cutoff,
                        None => false,
                    }
                } else {
                    false
                };
                if !expired {
                    continue;
                }
                if dry_run {
                    report.removed.push(path);
                } else {
                    match std::fs::remove_file(&path) {
                        Ok(()) => report.removed.push(path),
                        Err(err) => report
                            .failed
                            .push(format!("removing {}: {err}", path.display())),
                    }
                }
            }
            remove_if_empty(&device_dir, dry_run, &mut report);
        }
        remove_if_empty(&case_dir, dry_run, &mut report);
    }
    Ok(report)
}

fn remove_if_empty(dir: &Path, dry_run: bool, report: &mut PruneReport) {
    let empty = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    };
    if !empty {
        return;
    }
    if dry_run {
        report.removed.push(dir.to_path_buf());
    } else {
        match std::fs::remove_dir(dir) {
            Ok(()) => report.removed.push(dir.to_path_buf()),
            Err(err) => report
                .failed
                .push(format!("removing {}: {err}", dir.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn store(root: &Path) -> Store {
        Store {
            root: root.to_path_buf(),
            archive_days: 1,
        }
    }

    #[test]
    fn archives_are_listed_newest_first_and_found_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        for (device, id) in [
            ("1002-150e", "20261026T213012Z"),
            ("1002-150e", "20261027T213012Z"),
            ("10de-1234", "20261025T213012Z"),
        ] {
            let path = archive_path(&store, "c", device, id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, id).unwrap();
        }
        let list = archives(&store, "c").unwrap();
        assert_eq!(list[0].id, "20261027T213012Z");
        assert_eq!(list.len(), 3);
        assert_eq!(
            find(&store, "c", "20261025T213012Z").unwrap().device,
            "10de-1234"
        );
    }

    #[test]
    fn a_prune_removes_expired_archives_parts_and_empty_directories_but_not_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let old = archive_path(&store, "c", "d", "20200101T000000Z");
        let fresh = archive_path(&store, "c", "d", &crate::time::now_run_id());
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "old").unwrap();
        fs::write(&fresh, "fresh").unwrap();
        let part = old.with_file_name("x.tar.zst.part");
        fs::write(&part, "part").unwrap();
        let report = prune(&store, false).unwrap();
        assert!(!old.exists());
        assert!(!part.exists());
        assert!(fresh.exists());
        assert!(report.removed.contains(&old));
        assert!(report.failed.is_empty());
    }

    #[test]
    fn a_dry_run_reports_and_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let old = archive_path(&store, "c", "d", "20200101T000000Z");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "old").unwrap();
        let report = prune(&store, true).unwrap();
        assert!(old.exists());
        assert!(report.removed.contains(&old));
    }
}
