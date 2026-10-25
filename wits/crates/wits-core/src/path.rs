//! Filesystem path rules shared across the tree.
//!
//! Three spellings have to agree wherever they appear: a branch's path
//! component, a leading tilde, and whether two paths name one place. They live
//! here rather than beside each caller because the worktree policy, the project
//! layer, and the CLI all use them; a second implementation would drift with
//! nothing to catch it.
//!
//! Nothing here touches the filesystem except [`real`] and [`same_path`], which
//! ask it only to answer "is this the same place?".

use std::path::{Path, PathBuf};

/// A branch name reduced to one path component: every character outside
/// `[A-Za-z0-9._-]` becomes `_`.
///
/// The worktree policy and the project layer must spell one branch the same
/// way: a branch's worktree directory and its `build_dir`/`workdir` template
/// are two views of one tree. This is not `stack::slice`'s branch-name minter,
/// which lowercases and collapses to `-` when it *invents* a name; this only
/// makes an existing name safe to drop into a path.
pub fn slug(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Expand a leading `~` or `~/…` against `$HOME`.
///
/// A bare `~user` form is left alone: resolving another user's home needs the
/// password database, and no path wits resolves wants it. A `~` anywhere but
/// the first component is literal, as in a shell.
pub fn expand_tilde(path: &str) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::from(path);
    };
    match rest {
        "" => PathBuf::from(home),
        _ if rest.starts_with('/') => PathBuf::from(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// The path with symlinks and `.`/`..` resolved, or the path as written when it
/// does not exist (or cannot be resolved).
///
/// Two callers compare paths that may not exist yet — a worktree about to be
/// created, a build directory not yet made — so the fallback is the contract,
/// not an error: an unresolvable path is compared literally.
pub fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether two spellings name the same filesystem path: symlinks and `.`/`..`
/// do not create a second checkout. A path that cannot be canonicalised is
/// compared as written, via [`real`].
pub fn same_path(a: &Path, b: &Path) -> bool {
    real(a) == real(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_path_safe_characters_and_folds_the_rest() {
        assert_eq!(slug("main"), "main");
        assert_eq!(slug("feature/add-thing"), "feature_add-thing");
        assert_eq!(slug("v1.2.3"), "v1.2.3");
        assert_eq!(slug("weird name?"), "weird_name_");
    }

    /// A leading `~` is the only one that expands; `~user` needs a passwd
    /// database this tool deliberately does not read.
    #[test]
    fn tilde_expands_only_as_a_leading_component() {
        let _guard = crate::log::test_flag_guard();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        assert_eq!(expand_tilde("~/tree"), home.path().join("tree"));
        assert_eq!(expand_tilde("~"), home.path());
        assert_eq!(expand_tilde("~user/tree"), PathBuf::from("~user/tree"));
        assert_eq!(expand_tilde("/a/~/b"), PathBuf::from("/a/~/b"));
    }

    #[test]
    fn same_path_resolves_a_symlink_and_a_missing_path_stays_literal() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::create_dir(&target).unwrap();

        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(same_path(&target, &link));
        }

        assert!(!same_path(&target, &dir.path().join("other")));
        // A path that cannot be canonicalised is compared as written, which is
        // what lets a not-yet-created worktree be compared at all.
        let missing = dir.path().join("missing").join("..").join("real");
        assert_eq!(real(&missing), missing);
    }

    /// Restores `HOME` on unwind, so a failing assertion cannot leak a test
    /// directory into the next test that reads it.
    struct HomeGuard(Option<std::ffi::OsString>);

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let saved = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            HomeGuard(saved)
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
