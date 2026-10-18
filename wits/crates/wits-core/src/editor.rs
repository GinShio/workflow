//! Opening a file in the editor git would use, the way git opens one.

use std::path::Path;

use anyhow::Context as _;

use crate::git::Repository;
use crate::process::Command;

/// Open `path` in git's editor and wait for it. An editor that exits non-zero
/// is an abort, and an error here.
pub fn edit(repo: &Repository, path: &Path) -> anyhow::Result<()> {
    let editor = repo
        .editor()
        .context("git has no editor to open (set core.editor)")?;
    let script = format!("{editor} \"$@\"");
    let path = path.display().to_string();
    let code = Command::new("sh")
        .args(["-c", script.as_str(), editor.as_str(), path.as_str()])
        .force_run()
        .status()?;
    anyhow::ensure!(code == 0, "the editor exited with status {code}");
    Ok(())
}
