//! `project check` — validate configuration legality.

use anyhow::{bail, Result};
use clap::Args;

use wits_project::{resolve_target, Workspace};

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Project name, or a path inside a checkout (default: every project).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
}

pub(super) fn run(ws: &Workspace, args: &CheckArgs) -> Result<()> {
    let issues: Vec<wits_project::Issue> = match args.target.as_deref() {
        Some(target) => wits_project::check_one(ws, resolve_target(ws, Some(target))?.project),
        None => wits_project::check_all(ws),
    };
    let problems: Vec<String> = issues
        .iter()
        .map(|issue| format!("[{}] {}", issue.project, issue.message))
        .collect();
    if problems.is_empty() {
        println!("ok");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("{p}");
        }
        bail!("{} configuration problem(s)", problems.len())
    }
}
