//! `project exists` — exit successfully when a named project's main repository
//! is cloned.

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_core::git;
use wits_project::{layout, Workspace};

use super::common::canonical;

#[derive(Debug, Args)]
pub struct ExistsArgs {
    /// Project name, either bare or fully qualified as `org/name`.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// A quiet checkout query: the name must resolve uniquely, then `repos.main`
/// must be the root of either a working-tree checkout or a bare clone.
pub(super) fn run(ws: &Workspace, args: &ExistsArgs) -> Result<()> {
    let project = ws.project(&args.name)?;
    let path = layout::primary_path(ws, project, project.repo("main").context("repo not found")?)
        .context("cannot resolve path of repos.main")?;
    let repo = git::Repository::new(&path);
    let root = repo
        .toplevel()
        .or_else(|| repo.is_bare().then(|| repo.git_common_dir()).flatten());
    if root.is_some_and(|root| canonical(&root) == canonical(&path)) {
        Ok(())
    } else {
        bail!(
            "project '{}' is registered but repos.main is not cloned at {}",
            project.key(),
            path.display()
        )
    }
}
