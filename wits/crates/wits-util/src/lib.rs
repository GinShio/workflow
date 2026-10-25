//! `wits-util` — crypto, in transit.
//!
//! Everything else now lives in `wits-core` (the floor and the git/forge/
//! worktree subsystems), `wits-project` (declarations, resolution, queries), or
//! `wits-build` (the build systems). The re-exports below keep the crate's old
//! module paths alive until it is deleted; they are a bridge, not API, and new
//! consumers name `wits_core`/`wits_project` directly. This crate is deleted
//! when the last module — `crypto` — has a crate of its own.

pub use wits_core::{
    config, editor, forge, git, jinja, log, pager, process, remote, system, time, worktree,
};

pub mod crypto;
