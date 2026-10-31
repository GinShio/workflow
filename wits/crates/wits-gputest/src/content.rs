//! Content hashing — the one vocabulary identity is built from.
//!
//! Everything here is canonical: paths are relative and sorted by bytes, a
//! tree records entry type, executable bit and symlink target, and every
//! combination is length-prefixed so no two different inputs can spell the same
//! byte stream. There are no mtimes, uids or inodes.
//!
//! The primitives are `hash_file`, `hash_tree`, `hash_set` and `hash_identity`;
//! argv and environment have their own canonical forms beside them because
//! they are not trees or file sets.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use wits_actions::Overlay;

/// The marker the run directory is replaced with in the identity argv.
pub const RUN_DIR_MARKER: &str = "{run}";

/// The install stamp's file name, at the root of every installed tree.
pub const STAMP: &str = ".gputest.toml";

const FILE_DOMAIN: &[u8] = b"wits-gputest-file-v1\0";
const TREE_DOMAIN: &[u8] = b"wits-gputest-tree-v1\0";
const SET_DOMAIN: &[u8] = b"wits-gputest-set-v1\0";
const IDENTITY_DOMAIN: &[u8] = b"wits-gputest-identity-v1\0";
const ARGV_DOMAIN: &[u8] = b"wits-gputest-argv-v1\0";
const ENV_DOMAIN: &[u8] = b"wits-gputest-env-v1\0";

/// A file's content hash, following a symlink to the file it names.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("opening {} to hash it", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(FILE_DOMAIN);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .with_context(|| format!("reading {} to hash it", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(digest(hasher))
}

/// A per-command file-hash cache. A run composes the same driver for several
/// cases; the loader paths and ICD libraries are the same files each time, so
/// hashing them once per command is the design's stated cost.
#[derive(Debug, Default)]
pub struct HashCache {
    files: BTreeMap<PathBuf, String>,
}

impl HashCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hash_file(&mut self, path: &Path) -> Result<String> {
        if let Some(hash) = self.files.get(path) {
            return Ok(hash.clone());
        }
        let hash = hash_file(path)?;
        self.files.insert(path.to_path_buf(), hash.clone());
        Ok(hash)
    }
}

/// The canonical hash of an installed tree.
///
/// Every file, directory and symlink under `root` is recorded with its relative
/// path; files carry the executable bit and their content hash, symlinks their
/// target string, and directories exist as entries so an empty one is visible.
/// `skip` names paths that are not part of the tree — the install stamp, which
/// is written after the hash and would otherwise make every install differ from
/// its own record.
pub fn hash_tree(root: &Path, skip: &[PathBuf]) -> Result<String> {
    if !root.is_dir() {
        bail!("tree root {} is not a directory", root.display());
    }
    let mut entries = Vec::new();
    walk(root, Path::new(""), skip, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    hasher.update(TREE_DOMAIN);
    for (rel, entry) in entries {
        match entry {
            Entry::Directory => {
                hasher.update(b"d");
                field(&mut hasher, &rel);
            }
            Entry::File { executable, digest } => {
                hasher.update(b"f");
                field(&mut hasher, &rel);
                hasher.update([u8::from(executable)]);
                field(&mut hasher, digest.as_bytes());
            }
            Entry::Symlink { target } => {
                hasher.update(b"l");
                field(&mut hasher, &rel);
                field(&mut hasher, &target);
            }
        }
    }
    Ok(digest(hasher))
}

enum Entry {
    Directory,
    File { executable: bool, digest: String },
    Symlink { target: Vec<u8> },
}

fn walk(
    root: &Path,
    rel: &Path,
    skip: &[PathBuf],
    entries: &mut Vec<(Vec<u8>, Entry)>,
) -> Result<()> {
    let dir = root.join(rel);
    let mut children: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading directory {}", dir.display()))?;
    children.sort_by(|a, b| a.file_name().as_bytes().cmp(b.file_name().as_bytes()));

    for child in children {
        let child_rel = rel.join(child.file_name());
        let path = root.join(&child_rel);
        if skip
            .iter()
            .any(|skipped| path.as_path() == skipped.as_path())
        {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("reading metadata of {}", path.display()))?;
        let file_type = meta.file_type();
        let rel_bytes = child_rel.as_os_str().as_bytes().to_vec();
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path)
                .with_context(|| format!("reading symlink {}", path.display()))?;
            entries.push((
                rel_bytes,
                Entry::Symlink {
                    target: target.as_os_str().as_bytes().to_vec(),
                },
            ));
        } else if file_type.is_dir() {
            entries.push((rel_bytes, Entry::Directory));
            walk(root, &child_rel, skip, entries)?;
        } else if file_type.is_file() {
            entries.push((
                rel_bytes,
                Entry::File {
                    executable: meta.permissions().mode() & 0o111 != 0,
                    digest: hash_file(&path)?,
                },
            ));
        } else {
            bail!(
                "{} is neither a regular file, directory nor symlink; the tree cannot be hashed canonically",
                path.display()
            );
        }
    }
    Ok(())
}

/// The canonical hash of a labelled set — the loader's ICD manifests and
/// libraries, the loader paths' shared objects, the loader configuration files.
/// Duplicate labels must agree; paths are deduplicated by the caller or by this
/// map.
pub fn hash_set(entries: &[(String, String)]) -> Result<String> {
    let mut map = BTreeMap::new();
    for (label, hash) in entries {
        if let Some(previous) = map.insert(label.clone(), hash.clone()) {
            if previous != *hash {
                bail!("content set names {label:?} twice with different content");
            }
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(SET_DOMAIN);
    for (label, hash) in map {
        field(&mut hasher, label.as_bytes());
        field(&mut hasher, hash.as_bytes());
    }
    Ok(digest(hasher))
}

/// The identity: a SHA-256 over the sorted `(label, component hash)` pairs, with
/// a domain separator so a changed component set cannot collide with an old
/// state value.
pub fn hash_identity(components: &[(&str, String)]) -> String {
    let mut pairs: Vec<_> = components.iter().collect();
    pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_DOMAIN);
    for (label, hash) in pairs {
        field(&mut hasher, label.as_bytes());
        field(&mut hasher, hash.as_bytes());
    }
    digest(hasher)
}

/// The canonical hash of an argv: every element length-prefixed, in order.
pub fn hash_argv(argv: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ARGV_DOMAIN);
    for arg in argv {
        field(&mut hasher, arg.as_bytes());
    }
    digest(hasher)
}

/// The canonical hash of the final applied overlay: sets and unsets relative to
/// the scrubbed caller, with both names and values carried as their exact
/// bytes.
pub fn hash_overlay(overlay: &Overlay) -> String {
    let mut sets: Vec<(&OsString, &OsString)> = overlay.set.iter().map(|(k, v)| (k, v)).collect();
    sets.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut unsets: Vec<&OsString> = overlay.unset.iter().collect();
    unsets.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

    let mut hasher = Sha256::new();
    hasher.update(ENV_DOMAIN);
    for (name, value) in sets {
        hasher.update(b"S");
        field(&mut hasher, name.as_bytes());
        field(&mut hasher, value.as_bytes());
    }
    for name in unsets {
        hasher.update(b"U");
        field(&mut hasher, name.as_bytes());
    }
    digest(hasher)
}

fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn digest(hasher: Sha256) -> String {
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(7 + bytes.len() * 2);
    out.push_str("sha256:");
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn a_tree_hash_is_canonical_and_notices_each_axis() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a"), "a").unwrap();
        fs::write(root.join("sub/b"), "b").unwrap();
        let first = hash_tree(root, &[]).unwrap();
        let second = hash_tree(root, &[]).unwrap();
        assert_eq!(first, second);

        fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o755)).unwrap();
        let executable = hash_tree(root, &[]).unwrap();
        assert_ne!(first, executable);

        fs::write(root.join("sub/b"), "changed").unwrap();
        let changed = hash_tree(root, &[]).unwrap();
        assert_ne!(executable, changed);

        symlink("a", root.join("link")).unwrap();
        let linked = hash_tree(root, &[]).unwrap();
        assert_ne!(changed, linked);
        fs::remove_file(root.join("link")).unwrap();

        fs::write(root.join(STAMP), "stamp").unwrap();
        assert_eq!(hash_tree(root, &[root.join(STAMP)]).unwrap(), changed);
        assert_ne!(hash_tree(root, &[]).unwrap(), changed);
    }

    #[test]
    fn an_empty_directory_is_part_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let empty = hash_tree(root, &[]).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        assert_ne!(empty, hash_tree(root, &[]).unwrap());
    }

    #[test]
    fn an_identity_is_order_independent_and_fixed_by_its_labels() {
        let a = hash_identity(&[("suite", "sha256:1".into()), ("runner", "sha256:2".into())]);
        let b = hash_identity(&[("runner", "sha256:2".into()), ("suite", "sha256:1".into())]);
        assert_eq!(a, b);
        let changed = hash_identity(&[("suite", "sha256:1".into()), ("runner", "sha256:3".into())]);
        assert_ne!(a, changed);
        let fewer = hash_identity(&[("suite", "sha256:1".into())]);
        assert_ne!(a, fewer);
    }

    #[test]
    fn a_set_is_order_independent_and_refuses_a_conflicting_duplicate() {
        let a = hash_set(&[
            ("a".into(), "sha256:1".into()),
            ("b".into(), "sha256:2".into()),
        ])
        .unwrap();
        let b = hash_set(&[
            ("b".into(), "sha256:2".into()),
            ("a".into(), "sha256:1".into()),
        ])
        .unwrap();
        assert_eq!(a, b);
        assert!(hash_set(&[
            ("a".into(), "sha256:1".into()),
            ("a".into(), "sha256:2".into())
        ])
        .is_err());
    }

    #[test]
    fn an_overlay_records_sets_and_unsets() {
        use std::ffi::OsString;
        let overlay = Overlay {
            set: vec![(OsString::from("A"), OsString::from("1"))],
            unset: vec![OsString::from("B")],
        };
        let a = hash_overlay(&overlay);
        let reordered = Overlay {
            set: vec![(OsString::from("A"), OsString::from("1"))],
            unset: vec![OsString::from("B")],
        };
        assert_eq!(a, hash_overlay(&reordered));
        let changed = Overlay {
            set: vec![(OsString::from("A"), OsString::from("2"))],
            unset: vec![],
        };
        assert_ne!(a, hash_overlay(&changed));
    }
}
