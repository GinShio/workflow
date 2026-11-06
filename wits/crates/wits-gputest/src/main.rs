//! `wits-gputest` — run a GPU test suite against a wits build, compare the
//! result with the previous build, and keep one self-describing archive.
//!
//! The guide is `docs/commands/gputest.rst`. This binary is a plugin: `wits
//! gputest …` reaches it through the git-style `$PATH` dispatch, and `wits`
//! forwards its global `-n`/`-v` ahead of the plugin's arguments.
//!
//! A *driver* is a wits project plus a profile plus an ICD selector; a *suite*
//! is one runner invocation and the named groups of tests it offers; a *case*
//! is a driver × suite pair and the groups it runs, which names the comparison
//! and the store; a *run* is one execution, one archive, and at most one state
//! update.

mod archive;
mod cmd;
mod config;
mod content;
mod cores;
mod device;
mod env;
mod icd;
mod pipeline;
mod report;
mod resolve;
mod stamp;
mod state;
mod store;
mod suite;
#[cfg(test)]
mod testutil;
mod time;
mod util;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "wits-gputest",
    version,
    about = "Run GPU test suites against a wits build, compare, and archive the evidence.",
    arg_required_else_help = true
)]
struct Cli {
    /// Show what is happening as it happens.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Print what a mutating verb would do instead of doing it; `run -n`
    /// prints each case's plan.
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// One line per case: its newest archive, and whether it can run now.
    /// Exits 1 when a case cannot.
    List,
    /// Install the test suites and runners the config names.
    Install(cmd::InstallArgs),
    /// Run cases and compare each against its baseline (`-n`: print the plan).
    Run(Box<cmd::RunArgs>),
    /// The archives for one case, newest first.
    History(cmd::HistoryArgs),
    /// Print one archive's `run.toml`, and optionally its failures.
    Show(cmd::ShowArgs),
    /// Remove expired archives, stale `.part` files and empty directories.
    Prune,

    /// The program a build system's developer environment runs to read that
    /// environment back as data. Hidden plumbing, not a workflow verb.
    #[command(name = "__devenv-capture", hide = true)]
    DevenvCapture {
        /// Where to write the inherited environment.
        file: std::path::PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    wits_core::log::init(cli.verbose, cli.dry_run);

    let running = matches!(cli.command, Command::Run(_));
    match cmd::dispatch(cli.command) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("Error: {err:#}");
            // `run`'s exit status is its machine interface: 2 means a case
            // could not run, which is also what a command-level failure means.
            ExitCode::from(if running { 2 } else { 1 })
        }
    }
}
