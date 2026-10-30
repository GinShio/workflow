//! `wits-actions` — the mutating actions behind the command verbs.
//!
//! The CLI is a composition root: it parses arguments and formats output, then
//! hands a resolved request to one of these actions. Keeping build, update, and
//! devenv here rather than under `cmd/` lets an in-process orchestrator reuse
//! them without a command line, and keeps their complexity out of both the CLI
//! and the read-only `wits-project` core.
//!
//! An [`Identity`] is resolved by the caller (`target_branch`) before an action
//! runs, so branch selection is not repeated inside the action, and no action
//! reads the process cwd: where the caller stands arrives as
//! [`Identity::here`], which a detached build takes its checkout from.

use std::path::PathBuf;

use wits_project::Profile;

pub mod build;
pub mod devenv;
pub mod update;

pub use build::{build, BuildOptions, BuildRequest};
pub use devenv::{capture, compose, run, DevenvRequest, Overlay};
pub use update::{update, UpdateOptions};
// `BuildMode`'s source of truth is `wits-build`; re-exported so a consumer of
// the actions can name the mode without adding the backends as a direct
// dependency.
pub use wits_build::BuildMode;

/// An environment in OS-string form: a process's own, or the one [`compose`]
/// diffs an [`Overlay`] against.
pub type Env = std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>;

/// A build identity resolved before an action runs: the profile axes, the branch
/// (`None` = an explicit detached HEAD), the output-path overrides, and where
/// the caller stands.
pub struct Identity {
    pub profile: Profile,
    pub branch: Option<String>,
    pub work_dir: Option<PathBuf>,
    pub build_dir: Option<PathBuf>,
    pub install_dir: Option<PathBuf>,
    /// The target path, or the directory the command runs in: a detached build
    /// takes each repo's checkout here before its primary checkout.
    pub here: Option<PathBuf>,
}
