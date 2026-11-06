//! Content hashing — the one vocabulary identity is built from.
//!
//! Everything here is canonical: paths are relative and sorted by bytes, a
//! tree records entry type, executable bit and symlink target, and every
//! combination is length-prefixed so no two different inputs can spell the same
//! byte stream. There are no mtimes, uids or inodes.

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use wits_actions::{Env, Overlay};

const FILE_DOMAIN: &[u8] = b"wits-gputest-file-v1\0";
const TREE_DOMAIN: &[u8] = b"wits-gputest-tree-v1\0";
const SET_DOMAIN: &[u8] = b"wits-gputest-set-v1\0";
const IDENTITY_DOMAIN: &[u8] = b"wits-gputest-identity-v1\0";
const ARGV_DOMAIN: &[u8] = b"wits-gputest-argv-v1\0";
const ENV_DOMAIN: &[u8] = b"wits-gputest-env-v2\0";

/// A run's identity and the seven component hashes it is made of. The archive
/// records all of them, so two runs can be compared component by component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    pub identity: String,
    /// The installed suite tree, from its install stamp.
    pub suite: String,
    /// The installed runner tree, from its install stamp.
    pub runner: String,
    /// The selected ICD manifests and the libraries they name.
    pub driver_icd: String,
    /// What the driver's `libs` lists.
    pub driver_libs: String,
    /// What the driver's `config` lists.
    pub driver_config: String,
    /// The runner's argv without the run directory and the baseline.
    pub argv: String,
    /// What the composed environment changed relative to the caller.
    pub env: String,
}

impl Content {
    /// The identity over the seven components.
    pub fn digest(&self) -> String {
        hash_identity(&[
            ("suite", &self.suite),
            ("runner", &self.runner),
            ("driver.icd", &self.driver_icd),
            ("driver.libs", &self.driver_libs),
            ("driver.config", &self.driver_config),
            ("argv", &self.argv),
            ("env", &self.env),
        ])
    }
}

/// A file's content hash, following a symlink to the file it names.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("opening {} to hash it", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(FILE_DOMAIN);
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("reading {} to hash it", path.display()))?;
    Ok(digest(hasher))
}

/// The canonical hash of an installed tree.
///
/// Every file, directory and symlink under `root` is recorded with its relative
/// path; files carry the executable bit and their content hash, symlinks their
/// target string, and directories exist as entries so an empty one is visible.
/// `skip` is the install stamp, which is written after the hash and would
/// otherwise make every install differ from its own record.
pub fn hash_tree(root: &Path, skip: Option<&Path>) -> Result<String> {
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
    skip: Option<&Path>,
    entries: &mut Vec<(Vec<u8>, Entry)>,
) -> Result<()> {
    let dir = root.join(rel);
    let children = std::fs::read_dir(&dir)
        .and_then(|read| read.collect::<std::io::Result<Vec<_>>>())
        .with_context(|| format!("reading directory {}", dir.display()))?;

    for child in children {
        let child_rel = rel.join(child.file_name());
        let path = root.join(&child_rel);
        if skip == Some(path.as_path()) {
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
/// libraries, the files a driver lists. A label may repeat (a file listed both
/// on its own and through its directory) only with the same content.
pub fn hash_set(entries: &[(String, String)]) -> Result<String> {
    let mut map = BTreeMap::new();
    for (label, hash) in entries {
        if let Some(previous) = map.insert(label, hash) {
            if previous != hash {
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

/// The identity: a SHA-256 over the sorted `(label, component hash)` pairs.
fn hash_identity(components: &[(&str, &str)]) -> String {
    let mut pairs = components.to_vec();
    pairs.sort_unstable();
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

/// The canonical hash of what the composed environment changed relative to the
/// scrubbed caller: every set and every unset, by name.
///
/// Where a set extends a value the caller already had, only the added part is
/// hashed. Meson's developer environment prepends its build directories to
/// `PATH` (and to `PKG_CONFIG_PATH` when the caller has one), so the full value
/// embeds the caller's own search path, which differs between a login shell and
/// the service manager. The caller's value is not part of the identity, like
/// every variable the overlay leaves alone; what the devenv added is.
pub fn hash_overlay(overlay: &Overlay, caller: &Env) -> String {
    let mut sets: Vec<(&[u8], &[u8], &[u8])> = overlay
        .set
        .iter()
        .map(|(name, value)| {
            let old = caller.get(name).map(|old| old.as_bytes());
            let (form, part) = added_part(value.as_bytes(), old);
            (name.as_bytes(), form, part)
        })
        .collect();
    sets.sort_unstable();
    let mut unsets: Vec<&[u8]> = overlay.unset.iter().map(|name| name.as_bytes()).collect();
    unsets.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(ENV_DOMAIN);
    for (name, form, part) in sets {
        field(&mut hasher, form);
        field(&mut hasher, name);
        field(&mut hasher, part);
    }
    for name in unsets {
        field(&mut hasher, b"unset");
        field(&mut hasher, name);
    }
    digest(hasher)
}

/// How a set value relates to the caller's: a prefix or suffix added to it, or
/// a value of its own.
fn added_part<'a>(value: &'a [u8], old: Option<&[u8]>) -> (&'static [u8], &'a [u8]) {
    match old {
        Some(old) if !old.is_empty() && value.len() > old.len() => {
            if value.ends_with(old) {
                (b"prefix", &value[..value.len() - old.len()])
            } else if value.starts_with(old) {
                (b"suffix", &value[old.len()..])
            } else {
                (b"set", value)
            }
        }
        _ => (b"set", value),
    }
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
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn a_tree_hash_is_canonical_and_notices_each_axis() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a"), "a").unwrap();
        fs::write(root.join("sub/b"), "b").unwrap();
        let first = hash_tree(root, None).unwrap();
        assert_eq!(first, hash_tree(root, None).unwrap());

        fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o755)).unwrap();
        let executable = hash_tree(root, None).unwrap();
        assert_ne!(first, executable);

        fs::write(root.join("sub/b"), "changed").unwrap();
        let changed = hash_tree(root, None).unwrap();
        assert_ne!(executable, changed);

        symlink("a", root.join("link")).unwrap();
        assert_ne!(changed, hash_tree(root, None).unwrap());
        fs::remove_file(root.join("link")).unwrap();

        let stamp = root.join(".stamp");
        fs::write(&stamp, "stamp").unwrap();
        assert_eq!(hash_tree(root, Some(&stamp)).unwrap(), changed);
        assert_ne!(hash_tree(root, None).unwrap(), changed);
    }

    #[test]
    fn an_empty_directory_is_part_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let empty = hash_tree(root, None).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        assert_ne!(empty, hash_tree(root, None).unwrap());
    }

    fn content() -> Content {
        Content {
            identity: String::new(),
            suite: "sha256:1".into(),
            runner: "sha256:2".into(),
            driver_icd: "sha256:3".into(),
            driver_libs: "sha256:4".into(),
            driver_config: "sha256:5".into(),
            argv: "sha256:6".into(),
            env: "sha256:7".into(),
        }
    }

    #[test]
    fn the_identity_moves_with_every_component() {
        let base = content().digest();
        assert_eq!(base, content().digest());
        let mutations: [fn(&mut Content); 7] = [
            |c| c.suite.push('x'),
            |c| c.runner.push('x'),
            |c| c.driver_icd.push('x'),
            |c| c.driver_libs.push('x'),
            |c| c.driver_config.push('x'),
            |c| c.argv.push('x'),
            |c| c.env.push('x'),
        ];
        for mutate in mutations {
            let mut changed = content();
            mutate(&mut changed);
            assert_ne!(base, changed.digest());
        }
    }

    #[test]
    fn a_set_is_order_independent_and_refuses_a_conflicting_duplicate() {
        let a = hash_set(&[("a".into(), "1".into()), ("b".into(), "2".into())]).unwrap();
        let b = hash_set(&[("b".into(), "2".into()), ("a".into(), "1".into())]).unwrap();
        assert_eq!(a, b);
        let repeated = hash_set(&[
            ("a".into(), "1".into()),
            ("b".into(), "2".into()),
            ("a".into(), "1".into()),
        ])
        .unwrap();
        assert_eq!(a, repeated);
        assert!(hash_set(&[("a".into(), "1".into()), ("a".into(), "2".into())]).is_err());
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    fn overlay(set: &[(&str, &str)], unset: &[&str]) -> Overlay {
        Overlay {
            set: set
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect(),
            unset: unset.iter().map(OsString::from).collect(),
        }
    }

    #[test]
    fn an_extended_caller_value_is_hashed_by_what_was_added() {
        let shell = env(&[("PATH", "/home/u/.cargo/bin:/usr/bin")]);
        let service = env(&[("PATH", "/usr/bin")]);
        let from_shell = overlay(&[("PATH", "/b/bin:/home/u/.cargo/bin:/usr/bin")], &[]);
        let from_service = overlay(&[("PATH", "/b/bin:/usr/bin")], &[]);
        assert_eq!(
            hash_overlay(&from_shell, &shell),
            hash_overlay(&from_service, &service)
        );

        let other_build = overlay(&[("PATH", "/c/bin:/usr/bin")], &[]);
        assert_ne!(
            hash_overlay(&from_service, &service),
            hash_overlay(&other_build, &service)
        );

        let appended = overlay(&[("PATH", "/usr/bin:/b/bin")], &[]);
        assert_ne!(
            hash_overlay(&from_service, &service),
            hash_overlay(&appended, &service)
        );
    }

    #[test]
    fn a_new_or_replaced_value_and_an_unset_are_hashed_whole() {
        let caller = env(&[("KEEP", "1"), ("GONE", "x")]);
        let base = overlay(&[("ADD", "a")], &["GONE"]);
        assert_eq!(
            hash_overlay(&base, &caller),
            hash_overlay(&overlay(&[("ADD", "a")], &["GONE"]), &env(&[]))
        );
        assert_ne!(
            hash_overlay(&base, &caller),
            hash_overlay(&overlay(&[("ADD", "b")], &["GONE"]), &caller)
        );
        assert_ne!(
            hash_overlay(&base, &caller),
            hash_overlay(&overlay(&[("ADD", "a")], &[]), &caller)
        );
    }
}
