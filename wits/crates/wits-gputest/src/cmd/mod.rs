//! The command line surface: argument types and one module per verb.

use anyhow::Result;
use clap::Args;

use crate::resolve::ProfileFlags;
use crate::Command;

pub mod history;
pub mod install;
pub mod list;
pub mod prune;
pub mod run;
pub mod show;

pub fn dispatch(command: Command) -> Result<u8> {
    match command {
        Command::List => list::run(),
        Command::Install(args) => install::run(&args).map(|()| 0),
        Command::Run(args) => run::run(&args),
        Command::History(args) => history::run(&args).map(|()| 0),
        Command::Show(args) => show::run(&args).map(|()| 0),
        Command::Prune => prune::run().map(|()| 0),
        Command::DevenvCapture { file } => wits_actions::capture(&file).map(|()| 0),
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Cases to run.
    #[arg(value_name = "CASE", required = true)]
    pub cases: Vec<String>,
    /// Skip a case whose identity was already tested.
    #[arg(long)]
    pub if_changed: bool,
    /// Run only this group of each case's suite (repeatable). The groups must
    /// lie within what the case runs: the run compares with the case's
    /// baseline and leaves its state alone.
    #[arg(long = "group", value_name = "GROUP", conflicts_with = "if_changed")]
    pub groups: Vec<String>,
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
    /// Also print the runner's `failures.csv`.
    #[arg(long)]
    pub failures: bool,
}
