//! The command line surface: argument types and one function per verb.
//!
//! The flags are deliberately the ones `build` and `devenv` already use, plus
//! `--if-changed` on `run`. The plugin re-declares them because `IdentityArgs`
//! lives in the `wits` binary, not in a library; the resolution itself still
//! goes through `target_branch`/`resolve_paths` rather than a private copy.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args;

use crate::resolve::Flags;
use crate::Command;

pub mod check;
pub mod history;
pub mod install;
pub mod list;
pub mod plan;
pub mod prune;
pub mod run;
pub mod show;

pub fn dispatch(command: Command) -> Result<u8> {
    match command {
        Command::Check => check::run().map(|()| 0),
        Command::List => list::run().map(|()| 0),
        Command::Plan(args) => plan::run(&args).map(|()| 0),
        Command::Install(args) => install::run(&args).map(|()| 0),
        Command::Run(args) => run::run(&args),
        Command::History(args) => history::run(&args).map(|()| 0),
        Command::Show(args) => show::run(&args).map(|()| 0),
        Command::Prune => prune::run().map(|()| 0),
        Command::DevenvCapture { file } => wits_actions::capture(&file).map(|()| 0),
    }
}

/// The build-identity flags, mirroring `wits`'s `IdentityArgs`.
#[derive(Debug, Args, Default)]
pub struct ProfileFlags {
    /// Target branch. Default: the driver's branch, else the project's
    /// main branch.
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    /// Use the selected detached HEAD as-is, without assigning it a branch
    /// identity or switching any checkout.
    #[arg(long, conflicts_with = "branch")]
    pub detach: bool,
    /// Build type; default: the driver's `build_type` (release).
    #[arg(short = 'B', long = "build-type")]
    pub build_type: Option<String>,
    /// Select a declared toolchain.
    #[arg(short = 'T', long)]
    pub toolchain: Option<String>,
    /// Build-system generator.
    #[arg(short = 'G', long)]
    pub generator: Option<String>,
    /// Apply a preset (repeatable; accepts org/preset).
    #[arg(short = 'p', long = "preset")]
    pub presets: Vec<String>,
    /// The repo to build or describe.
    #[arg(long)]
    pub focus: Option<String>,
    /// Build the base from this checkout verbatim.
    #[arg(long = "work-dir", value_name = "DIR")]
    pub work_dir: Option<PathBuf>,
    /// Register a template variable, exposed as `{{spec.KEY}}`.
    #[arg(long = "spec", value_name = "KEY=VALUE", value_parser = parse_spec)]
    pub specs: Vec<(String, String)>,
    /// Override the resolved install prefix.
    #[arg(long = "install-dir", value_name = "DIR")]
    pub install_dir: Option<PathBuf>,
    /// Override the resolved build directory.
    #[arg(long = "build-dir", value_name = "DIR")]
    pub build_dir: Option<PathBuf>,
}

impl ProfileFlags {
    pub fn to_flags(&self) -> Flags {
        Flags {
            branch: self.branch.clone(),
            detach: self.detach,
            build_type: self.build_type.clone(),
            toolchain: self.toolchain.clone(),
            generator: self.generator.clone(),
            presets: self.presets.clone(),
            focus: self.focus.clone(),
            work_dir: self.work_dir.clone(),
            build_dir: self.build_dir.clone(),
            install_dir: self.install_dir.clone(),
            specs: self.specs.iter().cloned().collect(),
            here: std::env::current_dir().ok(),
        }
    }
}

fn parse_spec(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((key, _)) if key.trim().is_empty() => Err(format!("empty key in spec '{value}'")),
        Some((key, value)) => Ok((key.trim().to_owned(), value.to_owned())),
        None => Err(format!("expected KEY=VALUE, got '{value}'")),
    }
}

#[derive(Debug, Args)]
pub struct PlanArgs {
    /// Cases to plan.
    #[arg(value_name = "CASE", required = true)]
    pub cases: Vec<String>,
    #[command(flatten)]
    pub profile: ProfileFlags,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Cases to run.
    #[arg(value_name = "CASE", required = true)]
    pub cases: Vec<String>,
    /// Skip a build whose identity was already tested.
    #[arg(long)]
    pub if_changed: bool,
    #[command(flatten)]
    pub profile: ProfileFlags,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Suites to install (default: every declared suite).
    #[arg(value_name = "SUITE")]
    pub suites: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// The case whose archives to list.
    pub case: String,
    /// Show at most this many archives.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The case the archive belongs to.
    pub case: String,
    /// The run id.
    pub id: String,
    /// Also print `failures.csv`.
    #[arg(long)]
    pub failures: bool,
}

/// Refuse a case name that could escape the store before it is used as a path
/// component. The config loader already enforces this for declared cases; the
/// history/show verbs accept names independently so old archives stay readable.
pub fn check_case_component(case: &str) -> Result<()> {
    crate::config::validate_case_name(case)
}

/// Refuse a run id that could escape the store.
pub fn check_run_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("run id '{id}' is not a path component");
    }
    Ok(())
}
