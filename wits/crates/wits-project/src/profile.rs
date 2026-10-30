//! The axes that affect resolution — identity, focus, and the branch a build is
//! for.
//!
//! A [`Profile`] is built from CLI flags and never from a file; it is separated
//! from action options such as `mode`/`install`/`target` on purpose. The branch
//! and work-dir axes are *not* on the profile: they are [`crate::paths::PathInput`]
//! fields, because that is the stage that consumes them.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use crate::layout;
use crate::workspace::{Project, Workspace};

/// The axes that affect *resolution* (paths, identity).
///
/// Deliberately not `build::BuildOptions`: these change what `build_dir`/repo
/// `workdir` resolve to; those change only the commands, and are the build
/// action's own business.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub build_type: Option<String>,
    pub toolchain: Option<String>,
    pub generator: Option<String>,
    pub presets: Vec<String>,
    /// `--focus` override; falls back to `project.focus`, then `"main"`.
    pub focus: Option<String>,
    /// CLI-registered template variables, exposed as `{{spec.*}}` (from
    /// `--spec K=V`). Purely referenceable, never applied on its own: a template
    /// that mentions `{{spec.mr}}` *requires* the caller to supply it, so an
    /// out-of-band value (an MR number, a variant tag) enters resolution without
    /// being baked into the file model or guessed.
    pub specs: BTreeMap<String, String>,
}

/// A branch-backed build identity. A detached build has no value here: it uses
/// the selected checkout's `HEAD` without inventing a branch name for templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchIdentity {
    pub raw: String,
    pub slug: String,
}

/// The repo the build sources from: the focus's `anchor`, or the focus itself.
pub fn anchor_of(project: &Project, focus: &str) -> String {
    project
        .repos
        .get(focus)
        .and_then(|r| r.decl.anchor.clone())
        .unwrap_or_else(|| focus.to_owned())
}

/// The nearest own-git repo starting from the focus: the focus itself if it has
/// its own git, otherwise its anchor (a subtree shares its anchor's git). A
/// cycle yields `None`.
pub fn identity_repo(project: &Project, focus: &str) -> Option<String> {
    let mut name = focus.to_owned();
    for _ in 0..project.repos.len() + 1 {
        let repo = project.repos.get(&name)?;
        if repo.kind.has_own_git() {
            return Some(name);
        }
        name = repo.decl.anchor.clone()?;
    }
    None
}

/// What a caller can suggest when the focus's identity repo has no branch
/// checked out. A build action owns `--detach`; a read-only query does not, and
/// naming a flag the command lacks would send its reader to the wrong help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchAbsence {
    /// `wits build` / `wits devenv`: `--branch` or `--detach`.
    Action,
    /// A read-only query: `--branch` only.
    Query,
}

/// The single branch funnel: decide which branch a command resolves for `focus`.
///
/// `focus` is the repo whose identity is wanted — the project's focus for
/// `build`/`devenv`, or the path-derived repo for `info`/`hash`/
/// `branch-build-dirs`. Callers must never turn a failed discovery into an
/// implicit detach: `--detach` is explicit and conflicts with `--branch`.
/// `absent` picks the hint for a checkout with no branch; it is the caller's
/// flag surface, not the resolution itself.
pub fn target_branch(
    ws: &Workspace,
    project: &Project,
    focus: &str,
    explicit_branch: Option<&str>,
    detach: bool,
    absent: BranchAbsence,
) -> Result<Option<String>> {
    if detach && explicit_branch.is_some() {
        bail!("--branch and --detach cannot be used together");
    }
    if detach {
        return Ok(None);
    }
    if let Some(branch) = explicit_branch {
        return Ok(Some(branch.to_owned()));
    }
    let name = identity_repo(project, focus)
        .context("no own-git repo to take a branch from; pass --branch")?;
    layout::current_branch(ws, project, &name)?
        .map(Some)
        .with_context(|| match absent {
            BranchAbsence::Action => format!(
                "repo '{name}' has no branch checked out; pass --branch to select a branch, \
                 or --detach to use HEAD as-is"
            ),
            BranchAbsence::Query => "could not determine a branch; pass --branch".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The absence hint names only flags the caller actually has: `build` and
    /// `devenv` own `--detach`, a read-only query does not.
    #[test]
    fn the_absence_hint_names_the_callers_own_flags() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("p.toml"),
            "[project]\n[repos.main]\npath = \"/nonexistent/repo\"\nmain_branch = \"main\"\n",
        )
        .unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        let project = ws.project("p").unwrap();

        let action =
            target_branch(&ws, project, "main", None, false, BranchAbsence::Action).unwrap_err();
        let action = format!("{action:#}");
        assert!(action.contains("--detach"), "{action}");

        let query =
            target_branch(&ws, project, "main", None, false, BranchAbsence::Query).unwrap_err();
        let query = format!("{query:#}");
        assert!(query.contains("pass --branch"), "{query}");
        assert!(!query.contains("--detach"), "{query}");
    }
}
