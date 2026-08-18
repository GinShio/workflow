//! Subcommand implementations, one module per `wits <command>`.
//!
//! The umbrella binary is kept deliberately thin so that a new command is just
//! a new module here plus one arm in `main`'s dispatch — there is no plugin
//! machinery to learn, and nothing speculative is carried around for commands
//! that don't exist yet.

pub mod build;
pub mod dotfiles;
pub mod project;
pub mod review;
pub mod stack;
pub mod transcrypt;
pub mod update;
pub mod worktree;
