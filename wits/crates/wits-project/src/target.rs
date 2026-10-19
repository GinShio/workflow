//! Turning a CLI positional (a name or a path) into one project.

use anyhow::{Context, Result};

use crate::workspace::{expand_tilde, Project, Workspace};

/// Classify a CLI positional as a filesystem path rather than a name: `.`/`..`
/// or a leading `.`, `/`, or `~`. Everything else is a name.
pub fn looks_like_path(token: &str) -> bool {
    token == "."
        || token == ".."
        || token.starts_with('.')
        || token.starts_with('/')
        || token.starts_with('~')
}

/// Resolve a name/path positional (or the current directory) to one project.
///
/// The core's public entry point for turning a `--target`-shaped positional into
/// a project: `wits project` (info/path queries), `wits build`, and `wits update`
/// all funnel through here, so the name-vs-path rules stay in one place.
pub fn resolve_target<'a>(ws: &'a Workspace, target: Option<&str>) -> Result<&'a Project> {
    match target {
        Some(t) if looks_like_path(t) => {
            let path = expand_tilde(t);
            ws.project_for_path(&path)
                .with_context(|| format!("no project owns the path {}", path.display()))
        }
        Some(t) => ws.project(t),
        None => {
            let cwd = std::env::current_dir()?;
            ws.project_for_path(&cwd).context(
                "not inside any known project; pass a name or run from inside a project's checkout",
            )
        }
    }
}
