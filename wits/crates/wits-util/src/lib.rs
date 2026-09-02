//! `wits-util` — the shared library behind the `wits` CLI and its plugins.
//!
//! Everything a command needs that isn't the command itself lives here: the thin
//! OS/git/process floor, the Jinja and config machinery, and the larger
//! self-contained subsystems (project resolution, build systems, forge/remote).
//! The `wits` binary composes these into built-in subcommands; an out-of-tree
//! plugin binary (`wits-<name>`) can depend on this same crate to reuse the
//! floor instead of reinventing it.
//!
//! The modules are flat on purpose. There is still a rough gradient — `config`,
//! `crypto`, `git`, `jinja`, `log`, `process`, `time` are the thin floor;
//! `build_system`, `forge`, `project`, `worktree` are subsystems with real domain
//! logic — but they sit side by side so a consumer names `wits_util::process` or
//! `wits_util::forge` directly, without a grouping layer in between.
//!
//! The gradient is a real line, not a label. `jinja` is on the floor because it
//! defines a language and decides nothing; resolving a *config* against it — where
//! an entry may itself be a template naming another entry — is policy, so it lives
//! with the config it serves, in [`project::context`].
//!
//! Two pairs that were once separate modules are now unified where they belong:
//! `config` folds in the single-setting `Resolver` (both answer "where does this
//! come from?"), and the git-remote parsing that feeds forge detection lives in
//! `forge::remote` (re-exported from `forge`), beside the forge it serves.

pub mod build_system;
pub mod config;
pub mod crypto;
pub mod forge;
pub mod git;
pub mod jinja;
pub mod log;
pub mod process;
pub mod project;
pub mod system;
pub mod time;
pub mod worktree;
