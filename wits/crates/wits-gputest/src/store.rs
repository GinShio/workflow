//! The archive store and its sweep.
//!
//! One archive per run at `<root>/<case>/<device>/<id>.tar.zst`. The store is a
//! person's result collection, not an index: state lives elsewhere, `prune`
//! only deletes archives, and an archive that ages out never resurrects a run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive::{self, RunRecord};
use crate::config::Store;

/// One archive found in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRef {
    pub device: String,
    pub id: String,
    pub path: PathBuf,
}

/// The directory one case's archives on one device live in.
pub fn archive_dir(store: &Store, case: &str, device: &str) -> PathBuf {
    store.root.join(case).join(device)
}

/// The store path for one archive.
pub fn archive_path(store: &Store, case: &str, device: &str, id: &str) -> PathBuf {
    archive_dir(store, case, device).join(format!("{id}.tar.zst"))
}

/// Write a run's archive into the store: a `.part` beside its destination,
/// then a rename inside the store's filesystem, so a crash never leaves a
/// truncated archive under a run id.
pub fn save(
    store: &Store,
    case: &str,
    device: &str,
    record: &RunRecord,
    run_dir: &Path,
) -> Result<PathBuf> {
    let dir = archive_dir(store, case, device);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating the archive directory {}", dir.display()))?;
    let destination = dir.join(format!("{}.tar.zst", record.started));
    let part = dir.join(format!("{}.tar.zst.part", record.started));
    archive::write(&part, record, run_dir)?;
    std::fs::rename(&part, &destination)
        .with_context(|| format!("renaming {} to {}", part.display(), destination.display()))?;
    Ok(destination)
}

/// Every archive for one case, across devices, newest first.
pub fn archives(store: &Store, case: &str) -> Result<Vec<ArchiveRef>> {
    let mut out = Vec::new();
    for device_dir in subdirectories(&store.root.join(case))? {
        let device = device_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for entry in std::fs::read_dir(&device_dir)
            .with_context(|| format!("reading archive directory {}", device_dir.display()))?
        {
            let path = entry
                .with_context(|| format!("reading archive directory {}", device_dir.display()))?
                .path();
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".tar.zst"))
            else {
                continue;
            };
            out.push(ArchiveRef {
                device: device.clone(),
                id: id.to_owned(),
                path: path.clone(),
            });
        }
    }
    // Run ids are fixed-width UTC timestamps: text order is time order.
    out.sort_by(|a, b| b.id.cmp(&a.id).then_with(|| a.device.cmp(&b.device)));
    Ok(out)
}

/// Find one archive by id among the case's devices.
pub fn find(store: &Store, case: &str, id: &str) -> Result<ArchiveRef> {
    match archives(store, case)?
        .into_iter()
        .find(|archive| archive.id == id)
    {
        Some(archive) => Ok(archive),
        None => bail!(
            "no archive {id} for case '{case}' under {}",
            store.root.display()
        ),
    }
}

/// What a prune did or would do.
#[derive(Debug, Default)]
pub struct PruneReport {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<String>,
}

/// Remove archives older than `store.archive_days`, every `.part` (the leftover
/// of a run that crashed while writing its archive; `prune` assumes no run is
/// writing one), and the case and device directories that leaves empty. State
/// is left alone: it is the fact that a build was tested, not the evidence.
pub fn prune(store: &Store, dry_run: bool) -> Result<PruneReport> {
    let cutoff = wits_core::time::now_secs() - i64::from(store.archive_days) * 86_400;
    let mut report = PruneReport::default();
    for case_dir in subdirectories(&store.root)? {
        for device_dir in subdirectories(&case_dir)? {
            for entry in std::fs::read_dir(&device_dir)
                .with_context(|| format!("reading archive directory {}", device_dir.display()))?
            {
                let path = entry
                    .with_context(|| format!("reading archive directory {}", device_dir.display()))?
                    .path();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let expired = name.ends_with(".part")
                    || name
                        .strip_suffix(".tar.zst")
                        .and_then(crate::time::parse_run_id)
                        .is_some_and(|started| started < cutoff);
                if expired {
                    remove(&path, dry_run, &mut report);
                }
            }
            remove_if_empty(&device_dir, dry_run, &mut report);
        }
        remove_if_empty(&case_dir, dry_run, &mut report);
    }
    Ok(report)
}

/// The directories directly inside `dir`; none when `dir` does not exist.
fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        if entry
            .file_type()
            .with_context(|| format!("reading {}", entry.path().display()))?
            .is_dir()
        {
            out.push(entry.path());
        }
    }
    Ok(out)
}

fn remove_if_empty(dir: &Path, dry_run: bool, report: &mut PruneReport) {
    // A directory this prune emptied of archives still lists them under -n.
    let empty = std::fs::read_dir(dir).is_ok_and(|mut entries| {
        entries
            .all(|entry| entry.is_ok_and(|entry| dry_run && report.removed.contains(&entry.path())))
    });
    if empty {
        remove(dir, dry_run, report);
    }
}

fn remove(path: &Path, dry_run: bool, report: &mut PruneReport) {
    if dry_run {
        report.removed.push(path.to_path_buf());
        return;
    }
    let removed = if path.is_dir() {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => report.removed.push(path.to_path_buf()),
        Err(err) => report
            .failed
            .push(format!("removing {}: {err}", path.display())),
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

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
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
            touch(&archive_path(&store, "c", device, id));
        }
        let ids: Vec<_> = archives(&store, "c")
            .unwrap()
            .into_iter()
            .map(|archive| archive.id)
            .collect();
        assert_eq!(
            ids,
            ["20261027T213012Z", "20261026T213012Z", "20261025T213012Z"]
        );
        assert_eq!(
            find(&store, "c", "20261025T213012Z").unwrap().device,
            "10de-1234"
        );
        assert!(find(&store, "c", "20200101T000000Z").is_err());
        assert!(archives(&store, "unknown").unwrap().is_empty());
    }

    #[test]
    fn a_saved_archive_lands_under_its_run_id_without_a_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir.path().join("store"));
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        let record = crate::archive::tests::record();
        let saved = save(&store, "c", "d", &record, &run).unwrap();
        assert_eq!(saved, archive_path(&store, "c", "d", &record.started));
        assert_eq!(archive::read_run(&saved).unwrap().started, record.started);
        let names: Vec<_> = fs::read_dir(saved.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn a_prune_removes_expired_archives_parts_and_empty_directories_but_not_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let old = archive_path(&store, "c", "d", "20200101T000000Z");
        let fresh = archive_path(&store, "c", "d", &crate::time::now_run_id());
        let part = old.with_file_name("x.tar.zst.part");
        let gone = archive_path(&store, "gone", "d", "20200101T000000Z");
        for path in [&old, &fresh, &part, &gone] {
            touch(path);
        }

        let planned = prune(&store, true).unwrap();
        assert!(old.exists() && gone.exists());
        assert!(planned.removed.contains(&old));
        assert!(planned.removed.contains(&dir.path().join("gone")));

        let report = prune(&store, false).unwrap();
        assert!(!old.exists() && !part.exists() && fresh.exists());
        assert!(!dir.path().join("gone").exists());
        assert!(report.removed.contains(&old));
        assert!(report.failed.is_empty());
    }
}
