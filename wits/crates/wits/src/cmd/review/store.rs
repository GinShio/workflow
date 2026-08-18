//! The local store: three JSON files per MR, and how they are addressed.
//!
//! Each MR gets a directory holding `info.json` (metadata + diff state),
//! `comments.json` (the forge's discussion, a cache), and `local.json` (your
//! unsubmitted actions — the one file you edit). The root is resolved on the
//! `WITS_REVIEW_DIR` → `$XDG_STATE_HOME/wits/review` → `$GIT_DIR/wits/review`
//! ladder, then keyed by the repo's `host/owner/repo` so one central root can
//! hold many repos and a store migrates cleanly between roots.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use wits_util::forge::RemoteInfo;
use wits_util::git::Repository;

use super::model::{Comments, Info, Local};

/// The per-repo root under which one repo's review state lives.
pub struct Store {
    root: PathBuf,
}

fn base_dir(repo: &Repository) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WITS_REVIEW_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state).join("wits").join("review"));
    }
    // The *common* git dir, so the store is shared across linked worktrees — a
    // `checkout` worktree and the main clone resolve to the same store. (Older
    // git without `--path-format` falls back to the plain git dir.)
    let git_dir = repo
        .git_common_dir()
        .or_else(|| repo.git_dir())
        .context("not inside a git repository (no .git dir)")?;
    Ok(git_dir.join("wits").join("review"))
}

impl Store {
    pub fn open(repo: &Repository, target: &RemoteInfo) -> Result<Store> {
        let root = base_dir(repo)?
            .join(&target.host)
            .join(&target.owner)
            .join(&target.repo);
        Ok(Store { root })
    }

    fn mr_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    // -- info (metadata + diff state) ----------------------------------------

    pub fn load_info(&self, id: &str) -> Option<Info> {
        read_json(&self.mr_dir(id).join("info.json"))
    }

    pub fn save_info(&self, id: &str, info: &Info) -> Result<()> {
        write_json(&self.mr_dir(id).join("info.json"), info)
    }

    // -- comments (remote discussion cache) ----------------------------------

    pub fn load_comments(&self, id: &str) -> Comments {
        read_json(&self.mr_dir(id).join("comments.json")).unwrap_or_default()
    }

    pub fn save_comments(&self, id: &str, comments: &Comments) -> Result<()> {
        write_json(&self.mr_dir(id).join("comments.json"), comments)
    }

    // -- local (the editable draft) ------------------------------------------

    /// The editable draft — the one *precious* file, the only one that would
    /// be lost. Its absence is a legitimate empty draft; a present-but-
    /// unparseable file is a real error we surface rather than silently treating
    /// as empty (a hand-edit typo must never erase your in-progress review).
    pub fn load_local(&self, id: &str) -> Result<Local> {
        let path = self.mr_dir(id).join("local.json");
        if !path.exists() {
            return Ok(Local::default());
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let local: Local = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} — fix the JSON or delete it", path.display()))?;
        Ok(local)
    }

    /// Persist the draft, or delete the file once it has emptied — an empty
    /// draft and no draft are the same thing.
    pub fn save_local(&self, id: &str, local: &Local) -> Result<()> {
        let path = self.mr_dir(id).join("local.json");
        if local.is_empty() {
            return remove_if_present(&path);
        }
        write_json(&path, local)
    }

    // -- in-flight cleanup (deferred, id-keyed) ------------------------------

    /// The forge-side ids a prior failed `submit` left unpublished (a GitHub
    /// pending-review id, GitLab draft-note ids), to be cleaned up at the start of
    /// the next `submit`. Empty (or absent) means nothing to clean.
    pub fn load_inflight(&self, id: &str) -> Vec<String> {
        read_json(&self.mr_dir(id).join("inflight.json")).unwrap_or_default()
    }

    /// Record the in-flight ids to clean next time, or delete the file once there
    /// are none — a clean, fully-published submit leaves nothing behind.
    pub fn save_inflight(&self, id: &str, ids: &[String]) -> Result<()> {
        let path = self.mr_dir(id).join("inflight.json");
        if ids.is_empty() {
            return remove_if_present(&path);
        }
        write_json(&path, &ids)
    }

    // -- enumeration & removal -----------------------------------------------

    /// Every MR's `info`, for the inbox and stack reconstruction.
    pub fn list_infos(&self) -> Vec<Info> {
        self.mr_ids()
            .iter()
            .filter_map(|id| self.load_info(id))
            .collect()
    }

    /// The ids of MRs with pending work: a non-empty local draft, or an in-flight
    /// cleanup a prior failed submit deferred. `submit --all` uses this, so a
    /// deferred cleanup is retried even after the draft that spawned it is gone.
    pub fn local_ids(&self) -> Vec<String> {
        self.mr_ids()
            .into_iter()
            .filter(|id| {
                let dir = self.mr_dir(id);
                dir.join("local.json").exists() || dir.join("inflight.json").exists()
            })
            .collect()
    }

    /// Drop an MR's whole directory — for `prune`.
    pub fn delete_mr(&self, id: &str) -> Result<()> {
        let dir = self.mr_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }

    /// The immediate subdirectory names of the repo root — one per MR.
    fn mr_ids(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        ids.sort();
        ids
    }

    // -- current-checkout pointer --------------------------------------------

    pub fn current(&self) -> Option<String> {
        fs::read_to_string(self.root.join("current"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn set_current(&self, id: &str) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        fs::write(self.root.join("current"), id).context("recording current review")?;
        Ok(())
    }

    /// Forget the current-checkout pointer — used when the MR it names is pruned,
    /// so a later `--next`/`--prev` can't navigate from a store that's gone.
    pub fn clear_current(&self) -> Result<()> {
        remove_if_present(&self.root.join("current"))
    }
}

/// The git-ref namespace that pins a reviewed snapshot's objects alive.
pub mod refs {
    pub fn pin(mr: &str, sha: &str) -> String {
        format!("refs/wits/review/{mr}/{sha}")
    }

    pub fn base_pin(mr: &str, sha: &str) -> String {
        format!("refs/wits/review/{mr}/{sha}-base")
    }

    pub fn mr_prefix(mr: &str) -> String {
        format!("refs/wits/review/{mr}/")
    }
}

/// Read a JSON cache file, or `None` when it is absent. A file that *exists* but
/// won't parse is corruption, not absence: these are all refetchable caches
/// (`info`/`comments`/`inflight`), so we degrade to `None` rather than error —
/// but we warn, so a corrupt cache is never silently invisible (unlike the
/// precious `local.json`, which surfaces a hard error in `load_local`).
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(e) => {
            log::warn!(
                "{}: ignoring unparseable cache ({e}); re-run `wits review fetch`",
                path.display()
            );
            None
        }
    }
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::review::model::{Action, Local, SCHEMA};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        (dir, Store { root })
    }

    #[test]
    fn info_round_trips_and_lists() {
        let (_g, store) = store();
        let info = crate::cmd::review::stub_info("7", "main", "feat");
        assert!(store.load_info("7").is_none());
        store.save_info("7", &info).unwrap();
        assert_eq!(store.load_info("7").unwrap().mr.id, "7");
        assert_eq!(store.list_infos().len(), 1);

        store.delete_mr("7").unwrap();
        assert!(store.load_info("7").is_none());
        assert!(store.list_infos().is_empty());
    }

    #[test]
    fn local_persists_and_vanishes_when_empty() {
        let (_g, store) = store();
        let mut local = Local::default();
        local.actions.push(Action::Comment {
            id: None,
            file: Some("a.c".into()),
            line: Some(3),
            side: None,
            start_line: None,
            start_side: None,
            body: "hi".into(),
            commit: None,
        });
        store.save_local("7", &local).unwrap();
        assert_eq!(store.load_local("7").unwrap().actions.len(), 1);
        assert_eq!(store.local_ids(), ["7"]);

        store
            .save_local(
                "7",
                &Local {
                    schema: SCHEMA,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.local_ids().is_empty());
    }

    #[test]
    fn inflight_round_trips_and_counts_as_pending_work() {
        let (_g, store) = store();
        assert!(store.load_inflight("7").is_empty());
        assert!(store.local_ids().is_empty());

        // A deferred cleanup (no draft) still counts as pending work, so
        // `submit --all` retries it even after the draft is gone.
        store.save_inflight("7", &["PRR_abc".into()]).unwrap();
        assert_eq!(store.load_inflight("7"), ["PRR_abc"]);
        assert_eq!(store.local_ids(), ["7"]);

        // Clearing it removes the file and the pending-work marker.
        store.save_inflight("7", &[]).unwrap();
        assert!(store.load_inflight("7").is_empty());
        assert!(store.local_ids().is_empty());
    }

    #[test]
    fn current_pointer_round_trips() {
        let (_g, store) = store();
        assert!(store.current().is_none());
        store.set_current("42").unwrap();
        assert_eq!(store.current().as_deref(), Some("42"));
    }
}
