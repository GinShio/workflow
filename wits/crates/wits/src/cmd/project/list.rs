//! `project list` — one summary line per registered project.

use anyhow::Result;

use wits_project::{Project, Workspace};

/// `list`: one summary line per registered project. Its own verb rather than
/// `info` with no target, so `info` describes exactly one project on every path
/// and its omitted positional means what it means for `build`/`update` — the
/// project owning the current directory.
pub(super) fn run(ws: &Workspace) -> Result<()> {
    for project in ws.projects() {
        println!("{}", summary_line(project));
    }
    Ok(())
}

fn summary_line(project: &Project) -> String {
    let bs = project
        .settings()
        .build_system
        .map(|b| b.as_str())
        .unwrap_or("-");
    let focus = project.focus_name(None);
    format!("{:<24} focus={:<8} build={}", project.key(), focus, bs)
}
