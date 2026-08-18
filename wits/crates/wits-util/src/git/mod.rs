//! Talking to git by shelling out to the `git` binary.
//!
//! Everything here drives the real `git` the user's shell would, not libgit2: a
//! user's true git behaviour is the sum of their config includes, conditional
//! includes, credential helpers, and SSH setup, and libgit2 reimplements only a
//! subset that drifts in exactly the corners (includes, helpers, remote
//! resolution) we care about. The cost is a process spawn per call, which is
//! nothing next to the network round-trips these tools spend their time on.
//!
//! There is **one** handle, [`Repository`] — a cheap wrapper over a repo path
//! that every `git` invocation runs against. It carries the whole git surface
//! the tools need, which comes in three flavours that differ only in *how* the
//! child is run (the [`query`](Repository::query) / [`capture`](Repository::capture)
//! / [`stream`](Repository::stream) primitives below):
//!
//! - **reads** — captured and `force_run`, so they still answer under a dry-run
//!   (control flow depends on them; a dry-run that can't see the world tells you
//!   nothing), either for their output ([`query`](Repository::query)) or, for
//!   git's silent predicates, for their exit status
//!   ([`succeeds`](Repository::succeeds));
//! - **captured mutations** — refs and pushes, whose stderr we keep so a failure
//!   reports *why* (a stale lease, a missing base), not a bare exit code;
//! - **streamed mutations** — the working-tree porcelain (worktrees, stashes,
//!   submodules, clone), which inherit the terminal so progress streams live and
//!   in colour, and which a dry-run prints instead of running.
//!
//! The surface is split across two files purely by concern — [`floor`] holds the
//! reads and the ref/push plumbing; [`worktree`] holds the working-tree porcelain
//! and its helpers ([`RestoreGuard`], [`clone`]) — but they are one type, so a
//! caller never has to decide which of two overlapping handles it wants.
//!
//! This module is the *porcelain* — one call, one `git` invocation, no policy.
//! Decisions built on top of it (where a worktree belongs, when one may be
//! reclaimed) live in [`crate::worktree`].

mod floor;
mod worktree;

use thiserror::Error;

use crate::process::{Command, ProcessError};

pub use floor::{BranchStatus, Commit, FileChange, StatusCounts};
pub use worktree::{
    clone, clone_reference_store, create_reference_store, init_bare_host, is_object_store,
    RestoreGuard, Submodule, Worktree,
};

/// The environment variables git uses to *pin* a repository/worktree location.
/// We always run against an explicit `current_dir`, so any of these inherited
/// from the caller (git exports them for aliases and hooks) would silently
/// override that — scrubbed on every invocation in [`Repository::git`].
const GIT_LOCATION_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command failed: {0}")]
    Process(#[from] ProcessError),
    #[error("git {operation} failed: {message}")]
    Failed { operation: String, message: String },
}

/// A handle to a repository on disk. Holds no resources and is cheap to clone;
/// it's really just the path every `git` invocation runs against.
#[derive(Debug, Clone)]
pub struct Repository {
    path: std::path::PathBuf,
}

impl Repository {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The repository's path on disk.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn git(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.path);
        // Discover the repository from `current_dir`, never from an inherited
        // location env var. When `wits` is spawned by git itself — a `git`
        // alias, or a hook — git exports `GIT_DIR`/`GIT_WORK_TREE` (etc.) pinned
        // to *that* invocation's repo/worktree; left in place they override our
        // cwd and make a `git` we run against another worktree operate on the
        // wrong one (notably `git submodule`, which then dies with "cannot be
        // used without a working tree"). Scrubbing them is a no-op when unset.
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(*var);
        }
        cmd
    }

    /// Run a read-only query and return its trimmed stdout, or `None` when the
    /// command exits non-zero or prints nothing. The whole read surface is built
    /// on this, so the dry-run/force decision lives in exactly one place: reads
    /// always `force_run`, since control flow depends on them.
    fn query(&self, args: &[&str]) -> Option<String> {
        let result = self
            .git()
            .args(args.iter().copied())
            .force_run()
            .exec()
            .ok()?;
        if result.is_success() {
            let out = result.stdout_trimmed();
            (!out.is_empty()).then(|| out.to_owned())
        } else {
            None
        }
    }

    /// A read that answers with its **exit status** instead of its output, for
    /// git's predicate commands — `merge-base --is-ancestor` prints nothing and
    /// says yes or no in the exit code alone, which [`query`](Self::query) cannot
    /// express (it reads empty output as failure). `force_run` like every read,
    /// so a dry-run can still ask.
    fn succeeds(&self, args: &[&str]) -> bool {
        self.git()
            .args(args.iter().copied())
            .force_run()
            .exec()
            .map(|result| result.is_success())
            .unwrap_or(false)
    }

    /// A **captured** mutation: run it capturing stdout/stderr, so a failure can
    /// report git's own message. `force` marks the operation as safe to run even
    /// under a dry-run (our own local bookkeeping — a `refs/wits/*` pin, an
    /// object fetch — as opposed to a push or branch delete, which a `-n` run
    /// must only preview).
    fn capture(&self, operation: String, args: &[&str], force: bool) -> Result<(), GitError> {
        let mut cmd = self.git();
        cmd.args(args.iter().copied());
        if force {
            cmd.force_run();
        }
        let result = cmd.exec()?;
        if result.is_success() {
            Ok(())
        } else {
            Err(GitError::Failed {
                operation,
                message: result.stderr.trim().to_owned(),
            })
        }
    }

    /// A **streamed** mutation: run it inheriting the terminal so progress shows
    /// live and in colour (git detects the tty). The child prints its own error,
    /// so a non-zero exit needs only a terse tag. Honours dry-run — a `-n` run
    /// prints the command instead of performing it.
    fn stream(&self, operation: &str, args: &[&str]) -> Result<(), GitError> {
        let code = self.git().args(args.iter().copied()).status()?;
        if code == 0 {
            Ok(())
        } else {
            Err(GitError::Failed {
                operation: operation.to_owned(),
                message: format!("exit {code}"),
            })
        }
    }
}
