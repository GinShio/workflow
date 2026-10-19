//! Cross-checking a project's declarations against each other and the registry.
//!
//! These are the checks that need the whole registry or the disk: `--check` is
//! where a `developed_as` typo, a `skip` list that is not in force, or a dry
//! resolve failure is reported. File-local facts (unknown keys, remote roles,
//! strategy fields) are already rejected by the loader.

use wits_core::git::Repository;

use crate::layout;
use crate::paths::PathInput;
use crate::plan::{resolve_plan, BuildInput};
use crate::profile::Profile;
use crate::skip;
use crate::workspace::{Project, Workspace};

/// One problem found by [`check_one`]. `repo` names the repo when the problem is
/// repo-scoped; `message` is the human-readable detail, with the repo already
/// prefixed where it reads naturally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The owning project's canonical key.
    pub project: String,
    /// The repo the issue names, if any.
    pub repo: Option<String>,
    pub message: String,
}

/// Check every project in the registry.
pub fn check_all(ws: &Workspace) -> Vec<Issue> {
    ws.projects()
        .flat_map(|project| check_one(ws, project))
        .collect()
}

/// Check one project.
pub fn check_one(ws: &Workspace, project: &Project) -> Vec<Issue> {
    let mut issues = Vec::new();
    let key = project.key();
    let mut push = |repo: Option<&str>, message: String| {
        issues.push(Issue {
            project: key.clone(),
            repo: repo.map(str::to_owned),
            message,
        });
    };

    for (name, repo) in project.repos() {
        if repo.kind().has_own_git() && repo.decl().main_branch.is_none() {
            push(
                Some(name),
                format!("repo '{name}' has its own git but no main_branch"),
            );
        }
        // A `developed_as` that resolves to nothing is silent everywhere else:
        // path resolution keeps the structural answer rather than failing, so
        // that one registry file deployed without the project it names cannot
        // break every query. Judged here instead, where the whole registry is in
        // hand and the verdict is the caller's to act on.
        if let Some(spec) = &repo.decl().developed_as {
            if ws.project(spec).is_err() {
                push(
                    Some(name),
                    format!("repo '{name}': developed_as '{spec}' names no known project"),
                );
            } else if ws.developed_as(project, name).is_none() {
                push(
                    Some(name),
                    format!("repo '{name}': developed_as '{spec}' does not borrow this repo"),
                );
            }
        }
        // A declared `skip` that is not in force is the one config fact whose
        // truth lives on disk rather than in the file. Only a cloned checkout can
        // answer, and it is the same one `update` verifies.
        if !repo.decl().skip.is_empty() {
            if let Some(checkout) = layout::primary_checkout(ws, project, repo)
                .ok()
                .flatten()
                .map(Repository::new)
            {
                for v in skip::violations(&checkout, &repo.decl().skip) {
                    push(Some(name), format!("repo '{name}': {v}"));
                }
                if wits_core::log::is_verbose() {
                    for cmd in skip::remedy(&checkout, &repo.decl().skip) {
                        log::debug!(
                            "repo '{name}' fix: (cd {} && {cmd})",
                            checkout.path().display()
                        );
                    }
                }
            }
        }
    }

    let settings = project.settings();
    let has_build_dir = project
        .repos()
        .any(|(_, repo)| repo.decl().build_dir.is_some());
    if has_build_dir && settings.build_system.is_none() {
        push(None, "build_dir is set but build_system is not".into());
    }
    // Whether a declared `build_system` actually has a backend is `wits build`'s
    // concern; here we only cross-check the *declared* facts: a toolchain's own
    // `supports` list against `build_system`.
    if let Some(bs) = settings.build_system {
        if let Some(tc) = &settings.toolchain {
            if let Some(def) = ws.toolchain(tc) {
                if !def.supports.is_empty() && !def.supports.iter().any(|s| s == bs.as_str()) {
                    push(
                        None,
                        format!("toolchain '{tc}' does not support '{}'", bs.as_str()),
                    );
                }
            }
        }
    }
    if let Some(tc) = &settings.toolchain {
        if ws.toolchain(tc).is_none() {
            push(None, format!("unknown toolchain '{tc}'"));
        }
    }

    // A dry resolve catches template errors, preset cycles, unknown presets.
    // Validation has no backend, so this is a path-only resolve — toolchain
    // selection runs (and can fail), but there is nothing to inject.
    let profile = Profile {
        toolchain: settings.toolchain.clone(),
        ..Default::default()
    };
    let input = BuildInput {
        paths: PathInput {
            profile: &profile,
            branch: Some("main"),
            work_dir: None,
            build_dir: None,
            install_dir: None,
        },
        inject_toolchain: false,
        injector: None,
        extra_config_args: &[],
        extra_build_args: &[],
        extra_install_args: &[],
    };
    if let Err(e) = resolve_plan(ws, project, &input) {
        push(None, format!("resolution: {e:#}"));
    }
    issues
}
