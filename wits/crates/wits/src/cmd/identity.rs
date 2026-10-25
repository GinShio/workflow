//! The flags that name *which* build an action acts on, shared by the commands
//! that consume one.
//!
//! [`ProfileArgs`] is the resolution profile; it is flattened into `project`'s
//! global flags and into the [`IdentityArgs`] that `build`/`devenv` flatten, so
//! a query and an action resolve the same tree from the same spellings.
//! [`IdentityArgs`] adds the detached-HEAD selector and the two output-path
//! overrides and turns the whole group into a resolved [`Identity`] — the
//! caller's job, so an action never looks at the process cwd to learn which
//! branch it acts on.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use wits_actions::Identity;
use wits_project::{target_branch, BranchAbsence, Profile, Project, Workspace};

/// The profile axes shared by `project` (all subcommands, via global flags) and
/// the actions that take an identity. Each field is `global` so it propagates
/// to every `project` subcommand; positionals cannot be global, which is why
/// the `NAME|PATH` target stays per-subcommand.
#[derive(Debug, Args, Default)]
pub struct ProfileArgs {
    /// Target branch (the build identity). Default: the focus repo's current branch.
    #[arg(short = 'b', long, global = true)]
    pub branch: Option<String>,
    /// Build type — lowercase, meson-aligned (debug, release, …).
    #[arg(short = 'B', long = "build-type", global = true)]
    pub build_type: Option<String>,
    /// Select a declared toolchain.
    #[arg(short = 'T', long, global = true)]
    pub toolchain: Option<String>,
    /// Build-system generator (e.g. Ninja).
    #[arg(short = 'G', long, global = true)]
    pub generator: Option<String>,
    /// Apply a preset (repeatable; accepts org/preset).
    #[arg(short = 'p', long = "preset", global = true)]
    pub presets: Vec<String>,
    /// Override which repo is the focus.
    #[arg(long, global = true)]
    pub focus: Option<String>,
    /// Build the base from this checkout verbatim, bypassing the branch
    /// strategy's `worktree_dir`/in-place resolution — e.g. a `wits review
    /// checkout` worktree. Everything else (`build_dir`/`source_dir`/…) still
    /// anchors on it.
    #[arg(long = "work-dir", value_name = "DIR", global = true)]
    pub work_dir: Option<PathBuf>,
    /// Register a template variable, exposed as `{{spec.KEY}}` (repeatable;
    /// `KEY=VALUE`). A project template that references `{{spec.KEY}}` requires
    /// it — this is how an out-of-band value (an MR number, a variant tag)
    /// enters resolution without being baked into the project file.
    #[arg(long = "spec", value_name = "KEY=VALUE", global = true, value_parser = parse_spec)]
    pub specs: Vec<(String, String)>,
}

/// Parse a `--spec KEY=VALUE` pair. Split on the *first* `=` so a value may
/// itself contain `=`; the key must be non-empty. Validated at parse time so a
/// malformed pair errors on the command line, not mid-resolve.
fn parse_spec(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, _)) if k.trim().is_empty() => Err(format!("empty key in spec '{s}'")),
        Some((k, v)) => Ok((k.trim().to_owned(), v.to_owned())),
        None => Err(format!("expected KEY=VALUE, got '{s}'")),
    }
}

impl ProfileArgs {
    pub fn to_profile(&self) -> Profile {
        Profile {
            build_type: self.build_type.clone(),
            toolchain: self.toolchain.clone(),
            generator: self.generator.clone(),
            presets: self.presets.clone(),
            focus: self.focus.clone(),
            specs: self.specs.iter().cloned().collect(),
        }
    }
}

/// Which build a command means: the resolution profile, the detached-HEAD
/// selector, and the two output-path overrides. `build` and `devenv` flatten the
/// same group, so a program run in a build's environment names exactly the tree
/// `build` produces for the same flags.
#[derive(Debug, Args)]
pub struct IdentityArgs {
    #[command(flatten)]
    pub profile: ProfileArgs,
    /// Use the selected detached HEAD as-is, without assigning it a branch
    /// identity or switching any checkout.
    #[arg(long, conflicts_with = "branch")]
    pub detach: bool,
    /// Override the install prefix, ignoring the resolved focus/anchor
    /// `install_dir` (the backend's install-prefix, e.g. cmake's
    /// `CMAKE_INSTALL_PREFIX`; `build` passes it to configure as well as install).
    #[arg(long = "install-dir", value_name = "DIR")]
    pub install_dir: Option<PathBuf>,
    /// Override the resolved build directory, ignoring the focus/anchor
    /// `build_dir` template — e.g. for a `review checkout` built in an isolated
    /// dir without touching config. The symmetric partner of `--install-dir`;
    /// highest priority, verbatim ("The CLI override layer and the review
    /// interaction").
    #[arg(long = "build-dir", value_name = "DIR")]
    pub build_dir: Option<PathBuf>,
}

impl IdentityArgs {
    /// Resolve the branch and hand the whole group to the actions as one owned
    /// [`Identity`]. Branch discovery goes through the shared `target_branch`
    /// funnel with the project's focus, so a program run in a build's
    /// environment and the build itself cannot disagree about which branch is
    /// meant.
    pub fn to_identity(&self, ws: &Workspace, project: &Project) -> Result<Identity> {
        let profile = self.profile.to_profile();
        let focus = project.focus_name(profile.focus.as_deref()).to_owned();
        let branch = target_branch(
            ws,
            project,
            &focus,
            self.profile.branch.as_deref(),
            self.detach,
            BranchAbsence::Action,
        )?;
        Ok(Identity {
            profile,
            branch,
            work_dir: self.profile.work_dir.clone(),
            build_dir: self.build_dir.clone(),
            install_dir: self.install_dir.clone(),
        })
    }
}
