//! `wits-util` — the project subsystem and the build-system backends, in transit.
//!
//! Everything except `project`, `build_system`, and `crypto` now lives in
//! `wits-core`. The crate-internal re-exports below keep those three compiling
//! against their original `crate::…` paths while the rest of the migration
//! proceeds; nothing here is public API, and new consumers name `wits_core`
//! directly. This crate is deleted when the last of the three has a home of its
//! own.

pub(crate) use wits_core::{config, git, jinja, remote, system};

pub mod build_system;
pub mod crypto;
pub mod project;
