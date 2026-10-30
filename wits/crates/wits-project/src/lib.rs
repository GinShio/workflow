//! `wits-project` — the project registry: what a project declares, how it
//! resolves, and the questions consumers ask about it.
//!
//! The declaration model stays deliberately close to the file on disk: templated
//! fields (`repos.*.build_dir`, an `[environment]` map, …) are kept as raw
//! strings and `toml::Value`s, and loading does no path rendering and no git
//! queries. Turning declarations into concrete paths and command lines is the
//! resolution stage's job: [`resolve_paths`] for a checkout question, and
//! [`resolve_plan`] for a build.

pub mod hook;
pub mod layout;
pub mod remotes;
pub mod skip;

mod check;
mod context;
mod decl;
mod load;
mod paths;
mod plan;
mod presets;
mod profile;
mod target;
mod toolchain;
mod workspace;

pub use check::{check_all, check_one, Issue};
pub use context::value_to_string;
pub use decl::{
    BorrowRef, BranchStrategy, BuildSystem, DevenvDecl, DevenvOp, HooksDecl, Kind, OrgDecl,
    PresetDecl, RemoteDecl, RepoDecl, SettingsDecl, ToolchainDecl, DEVENV_SEPARATOR,
};
pub use hook::HookPhase;
pub use paths::{branch_build_dirs, resolve_paths, BranchBuildDir, PathInput, ResolvedPaths};
pub use plan::{
    resolve_plan, BuildInput, BuildPlan, DevenvEntry, LogicalConfig, Toolchain, ToolchainInjector,
};
pub use profile::{
    anchor_of, identity_repo, target_branch, BranchAbsence, BranchIdentity, Profile,
};
pub use remotes::{
    declared_for_checkout, desired, for_checkout, roles_in_force, roles_of, Declared, DesiredRemote,
};
pub use target::{looks_like_path, resolve_target, Target};
pub use wits_core::remote::{RemoteRoles, Role};
pub use workspace::{Project, Repo, Workspace};
