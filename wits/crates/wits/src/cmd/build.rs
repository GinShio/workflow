//! `wits build` — the CLI shell over [`wits_actions::build`](fn@wits_actions::build).
//!
//! The flags are parsed here; planning, the branch guard, and the backend steps
//! are the action's, so the same build runs from an in-process orchestrator
//! without a command line.

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_actions::{build, BuildMode, BuildOptions, BuildRequest};
use wits_project::{resolve_target, Workspace};

use crate::cmd::identity::IdentityArgs;

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Project name or path (default: the project owning the current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Configure only; do not compile.
    #[arg(long = "config-only", conflicts_with_all = ["build_only", "reconfig", "uninstall"])]
    pub config_only: bool,
    /// Compile only; assume already configured.
    #[arg(long = "build-only", conflicts_with_all = ["reconfig", "uninstall"])]
    pub build_only: bool,
    /// Delete the build dir and configure fresh.
    #[arg(long, conflicts_with = "uninstall")]
    pub reconfig: bool,
    /// Reverse an install (backend-driven).
    #[arg(long)]
    pub uninstall: bool,
    /// Install after building.
    #[arg(long)]
    pub install: bool,
    /// Build a specific target.
    #[arg(short = 't', long = "target")]
    pub build_target: Option<String>,

    /// Raw args appended to the configure command (verbatim).
    #[arg(long = "extra-config-args", num_args = 1.., value_name = "ARG")]
    pub extra_config_args: Vec<String>,
    /// Raw args appended to the build command (verbatim).
    #[arg(long = "extra-build-args", num_args = 1.., value_name = "ARG")]
    pub extra_build_args: Vec<String>,
    /// Raw args appended to the install command (verbatim).
    #[arg(long = "extra-install-args", num_args = 1.., value_name = "ARG")]
    pub extra_install_args: Vec<String>,
    /// Shorthand: -Xconfig,ARG / -Xbuild,ARG / -Xinstall,ARG (repeatable).
    #[arg(short = 'X', value_name = "SCOPE,ARG")]
    pub extra: Vec<String>,
}

/// `wits build` — its own top-level command, over the shared `project` core.
pub fn run(args: &BuildArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let target = resolve_target(&ws, args.target.as_deref())?;
    // Validate the flag-only layer before resolving the identity: a malformed
    // `-X` is the caller's mistake and names itself without touching git.
    let options = build_options(args)?;
    let request = BuildRequest {
        identity: args.identity.to_identity(&ws, &target)?,
        options,
    };
    build(&ws, target.project, &request)
}

fn build_options(a: &BuildArgs) -> Result<BuildOptions> {
    let mode = if a.config_only {
        BuildMode::ConfigOnly
    } else if a.build_only {
        BuildMode::BuildOnly
    } else if a.reconfig {
        BuildMode::Reconfig
    } else if a.uninstall {
        BuildMode::Uninstall
    } else {
        BuildMode::Auto
    };

    let (mut cfg, mut build, mut install) = (
        a.extra_config_args.clone(),
        a.extra_build_args.clone(),
        a.extra_install_args.clone(),
    );
    for x in &a.extra {
        let (scope, arg) = x
            .split_once(',')
            .with_context(|| format!("-X expects SCOPE,ARG (got '{x}')"))?;
        match scope {
            "config" => cfg.push(arg.to_owned()),
            "build" => build.push(arg.to_owned()),
            "install" => install.push(arg.to_owned()),
            other => bail!("-X scope must be config|build|install (got '{other}')"),
        }
    }

    Ok(BuildOptions {
        mode,
        install: a.install,
        target: a.build_target.clone(),
        extra_config_args: cfg,
        extra_build_args: build,
        extra_install_args: install,
    })
}
