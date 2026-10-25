//! `project check` — validate configuration legality.

use anyhow::{bail, Result};

use wits_project::{resolve_target, Workspace};

pub(super) fn run(ws: &Workspace, target: Option<&str>) -> Result<()> {
    let issues: Vec<wits_project::Issue> = match target {
        Some(_) => wits_project::check_one(ws, resolve_target(ws, target)?),
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
