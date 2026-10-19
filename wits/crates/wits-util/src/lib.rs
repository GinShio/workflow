//! `wits-util` — the build-system backends and crypto, in transit.
//!
//! Everything else now lives in `wits-core` (the floor and the git/forge/
//! worktree subsystems) or `wits-project` (declarations, resolution, queries).
//! The re-exports below keep the modules still here compiling against their
//! original `crate::…` paths while the rest of the migration proceeds; they are
//! a bridge, not API, and new consumers name `wits_core`/`wits_project`
//! directly. This crate is deleted when the last of these has a home of its own.

pub use wits_core::{
    config, editor, forge, git, jinja, log, pager, process, remote, system, time, worktree,
};

pub mod build_system;
pub mod crypto;
