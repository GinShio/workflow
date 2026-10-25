//! `wits project` — the CLI shell over the read-only project core, one verb per
//! file.
//!
//! Describes projects (the default), validates their configuration (`--check`),
//! and answers machine-readable status, path, or identity queries.
//! Everything about what a project *is* — the model, the workspace registry,
//! resolution, and the project-shaped git surface — lives in the read-only core
//! at [`wits_project`]; this module is one of its consumers, alongside the
//! separate `wits build` and `wits update` commands. See
//! `docs/reference/project-design.rst`, "Library shape — core plus actions";
//! quoted section names below are from the same document.
//!
//! It deliberately owns **no** worktree management. That was once `project
//! context`, which created a branch's worktree and tore down its build dir;
//! worktrees are now [`wits worktree`](crate::cmd::worktree)'s, which does the
//! job for any repository rather than only a registered one. `project` and
//! worktrees meet at a path — see `build`'s `--work-dir` — and at the remote
//! roles `wits worktree` asks the registry for, like every other command.
//!
//! The verbs live in their own modules. [`common`] holds the resolution they
//! share — which repo a target names, which branch a query defaults to, and the
//! plan assembly `info`/`--get` use — and every other file answers one shape of
//! question.

use anyhow::Result;
use clap::{Args, Subcommand};

use wits_project::Workspace;

use crate::cmd::identity::ProfileArgs;

mod branch_build_dirs;
mod check;
mod common;
mod exists;
mod hash;
mod info;
mod list;
mod names;

/// `wits project` — the read-only half of the tool: one verb per shape of
/// question it answers.
#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectSub,
    /// The profile axes (branch / build-type / toolchain / …) that shape
    /// resolution. Declared once here as **global** flags, so every `project`
    /// subcommand accepts them uniformly — the way `-v`/`-n` are inherited from
    /// the process layer ("CLI surface") — and so a machine-readable path query
    /// resolves the *same* dir a build would (the one shared `Profile`, "Profile
    /// vs BuildOptions"). Being global, they may be written on either side of
    /// the subcommand.
    #[command(flatten)]
    pub profile: ProfileArgs,
}

/// The verbs, and why each is its own rather than a key of `info --get`.
///
/// `info` projects one *value* out of a resolved plan, which is a shape three of
/// these do not have: `exists` answers with an exit status and must work on a
/// project that is not cloned (so it never resolves a plan at all);
/// `branch-build-dirs` is a search across the registry returning 0..N rows
/// spanning projects, not a field of this project; `hash` carries its own flags
/// and walks git objects rather than resolving config. Folding any of them into
/// `--get` would cost it the single-line-scalar contract that makes `--get`
/// repeatable and safe to read back positionally.
#[derive(Debug, Subcommand)]
pub enum ProjectSub {
    /// Summarise every registered project, one line each.
    List,
    /// Describe one project in full — or, with `--get`, print one resolved value.
    Info(info::InfoArgs),
    /// Validate configuration legality (no target: every project, for CI).
    Check(common::TargetArgs),
    /// Exit successfully when a named project's main repository is cloned.
    Exists(exists::ExistsArgs),
    /// Print every existing build directory that a branch of one checkout
    /// identifies, across all projects — what a branch deletion orphans.
    BranchBuildDirs(common::TargetArgs),
    /// Print a repo's commit hash for a branch, optionally with its submodules'
    /// pinned hashes — read from the tree, so no checkout or branch switch.
    Hash(hash::HashArgs),
    /// Print the name lists shell completion needs. Hidden like the top-level
    /// plumbing (`__applets`, `__trunk`): fish calls this at TAB time, so
    /// completion asks the registry instead of keeping its own TOML parser of
    /// it — the same one-question-one-command bargain `__trunk` made for the
    /// git hooks.
    #[command(name = "__names", hide = true)]
    Names(names::NamesArgs),
}

pub fn run(args: &ProjectArgs) -> Result<()> {
    let ws = Workspace::load()?;
    // The profile axes live on the parent as global flags, so they are read here
    // once and handed to whichever subcommand ran — the value lands on the parent
    // regardless of which side of the subcommand it was written on.
    let profile = &args.profile;
    match &args.command {
        ProjectSub::List => list::run(&ws),
        ProjectSub::Info(a) => info::run(&ws, a, profile),
        ProjectSub::Check(a) => check::run(&ws, a.target.as_deref()),
        ProjectSub::Exists(a) => exists::run(&ws, a),
        ProjectSub::BranchBuildDirs(a) => branch_build_dirs::run(&ws, a, profile),
        ProjectSub::Hash(a) => hash::run(&ws, a, profile),
        ProjectSub::Names(a) => names::run(&ws, a),
    }
}
