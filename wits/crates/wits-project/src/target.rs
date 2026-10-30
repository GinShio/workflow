//! Turning a CLI positional (a name, a path, or nothing) into what it names.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use wits_core::path::expand_tilde;

use crate::workspace::{Project, Workspace};

/// Classify a CLI positional as a filesystem path rather than a name: `.`/`..`
/// or a leading `.`, `/`, or `~`. Everything else is a name.
pub fn looks_like_path(token: &str) -> bool {
    token == "."
        || token == ".."
        || token.starts_with('.')
        || token.starts_with('/')
        || token.starts_with('~')
}

/// What a command's positional names: a project, where the command stands,
/// and — for a path or the current directory — the repo whose checkout holds
/// that place.
///
/// Every command resolves its positional here, so the name/path/cwd rule and
/// `--focus` mean the same thing everywhere. A command about a *build* asks
/// [`build_focus`](Self::build_focus); one about a single *checkout* asks
/// [`checkout_focus`](Self::checkout_focus).
pub struct Target<'a> {
    pub project: &'a Project,
    /// The repo whose checkout holds the path, after the owner's
    /// `developed_as`; `None` for a name.
    pub checkout_repo: Option<String>,
    /// Where the command stands: the path given, else the current directory.
    /// A branch is discovered from the checkout here, and a detached build
    /// takes its checkout from here.
    pub here: PathBuf,
}

impl Target<'_> {
    /// The repo a command about a build resolves for — `info`, `build`,
    /// `devenv`: `--focus`, else the project's focus. A path picks the project
    /// and where the command stands, never the focus, so a description and
    /// the build it describes agree from anywhere.
    pub fn build_focus(&self, focus: Option<&str>) -> Result<String> {
        self.checked(
            focus.unwrap_or_else(|| self.project.focus_name(None)),
            focus,
        )
    }

    /// The repo a command about one checkout resolves for — `hash`,
    /// `branch-build-dirs`: `--focus`, else the repo whose checkout holds the
    /// path, else (for a name) the project's focus.
    pub fn checkout_focus(&self, focus: Option<&str>) -> Result<String> {
        let name = focus
            .or(self.checkout_repo.as_deref())
            .unwrap_or_else(|| self.project.focus_name(None));
        self.checked(name, focus)
    }

    fn checked(&self, name: &str, flag: Option<&str>) -> Result<String> {
        if self.project.repo(name).is_none() {
            bail!(
                "project '{}' has no repo '{name}'{}",
                self.project.key(),
                if flag.is_some() { " (--focus)" } else { "" }
            );
        }
        Ok(name.to_owned())
    }
}

/// Resolve a name/path positional, or the current directory, to a [`Target`].
///
/// A name selects the project and stands in the current directory. A path —
/// or no positional, meaning the current directory — selects the project that
/// owns it, by deepest checkout and the owner's `developed_as`, and stands
/// there.
pub fn resolve_target<'a>(ws: &'a Workspace, target: Option<&str>) -> Result<Target<'a>> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (path, missing) = match target {
        Some(name) if !looks_like_path(name) => {
            return Ok(Target {
                project: ws.project(name)?,
                checkout_repo: None,
                here: cwd,
            });
        }
        Some(path) => {
            let path = cwd.join(expand_tilde(path));
            let missing = format!("no project owns the path {}", path.display());
            (path, missing)
        }
        None => (
            cwd,
            "not inside any known project; pass a name or run from inside a project's checkout"
                .to_owned(),
        ),
    };
    let (project, repo) = ws.repo_for_path(&path).context(missing)?;
    Ok(Target {
        project,
        checkout_repo: Some(repo.name().to_owned()),
        here: path,
    })
}
