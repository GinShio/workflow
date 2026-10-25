//! Cross-checking a project's declarations against each other and the registry.
//!
//! These are the checks that need the whole registry or the disk: `project
//! check` is where a `developed_as` typo, a `skip` list that is not in force,
//! or a dry resolve failure is reported. File-local facts (unknown keys, remote roles,
//! strategy fields) are already rejected by the loader.

use wits_core::git::Repository;

use crate::decl::BuildSystem;
use crate::layout;
use crate::paths::PathInput;
use crate::plan::{resolve_plan, BuildInput};
use crate::profile::Profile;
use crate::skip;
use crate::workspace::{Project, Workspace};

/// One problem found by [`check_one`]. `message` is the human-readable detail,
/// naming the repo when the problem is repo-scoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The owning project's canonical key.
    pub project: String,
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
    let mut push = |message: String| {
        issues.push(Issue {
            project: key.clone(),
            message,
        });
    };

    let default_focus = project.focus_name(None).to_owned();
    for (name, repo) in project.repos() {
        let hooks = &repo.decl().hooks;
        if (hooks.install.is_some() || hooks.post_install.is_some()) && name != default_focus {
            log::warn!(
                "repo '{name}' declares an install hook, but only the focus repo \
                 '{default_focus}' is installed; it runs only with --focus {name}"
            );
        }
        if repo.kind().has_own_git() && repo.decl().main_branch.is_none() {
            push(format!("repo '{name}' has its own git but no main_branch"));
        }
        // A `developed_as` that resolves to nothing is silent everywhere else:
        // path resolution keeps the structural answer rather than failing, so
        // that one registry file deployed without the project it names cannot
        // break every query. Judged here instead, where the whole registry is in
        // hand and the verdict is the caller's to act on.
        if let Some(spec) = &repo.decl().developed_as {
            if ws.project(spec).is_err() {
                push(format!(
                    "repo '{name}': developed_as '{spec}' names no known project"
                ));
            } else if ws.developed_as(project, name).is_none() {
                push(format!(
                    "repo '{name}': developed_as '{spec}' does not borrow this repo"
                ));
            }
        }
        // A declared `skip` that is not in force is the one config fact whose
        // truth lives on disk rather than in the file. Only a cloned checkout can
        // answer, and it is the same one `update` verifies.
        if !repo.decl().skip.is_empty() {
            if let Some(checkout) = layout::primary_checkout(ws, project, repo.name())
                .ok()
                .flatten()
                .map(Repository::new)
            {
                for v in skip::violations(&checkout, &repo.decl().skip) {
                    push(format!("repo '{name}': {v}"));
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
        push("build_dir is set but build_system is not".into());
    }
    // Whether a declared `build_system` actually has a backend is `wits build`'s
    // concern; here we only cross-check the *declared* facts: a toolchain's own
    // `supports` list against `build_system`.
    if let Some(bs) = settings.build_system {
        if let Some(tc) = &settings.toolchain {
            if let Some(def) = ws.toolchain(tc) {
                if !def.supports.is_empty() && !def.supports.iter().any(|s| s == bs.as_str()) {
                    push(format!(
                        "toolchain '{tc}' does not support '{}'",
                        bs.as_str()
                    ));
                }
            }
        }
    }
    if let Some(tc) = &settings.toolchain {
        if ws.toolchain(tc).is_none() {
            push(format!("unknown toolchain '{tc}'"));
        }
    }
    // A post-install hook can only add to an install step. cargo has none, so
    // --install would fail before the hook ran; the install hook is the way to
    // give it one.
    if settings.build_system == Some(BuildSystem::Cargo) {
        if let Some(repo) = project.repo(project.focus_name(None)) {
            let hooks = &repo.decl().hooks;
            if hooks.post_install.is_some() && hooks.install.is_none() {
                push(format!(
                    "repo '{}': post_install without install: cargo has no install step; \
                     declare an install hook",
                    repo.name()
                ));
            }
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
            here: None,
        },
        inject_toolchain: false,
        injector: None,
        extra_config_args: &[],
        extra_build_args: &[],
        extra_install_args: &[],
    };
    match resolve_plan(ws, project, &input) {
        Err(e) => push(format!("resolution: {e:#}")),
        Ok(plan) => {
            // The install hooks render against the *plan* context, so a broken
            // `{{...}}` in one is the same class of error as a broken path
            // template — caught here, where no build has to run first.
            if let Some(repo) = project.repo(&plan.paths.focus) {
                let hooks = &repo.decl().hooks;
                if hooks.install.is_some() || hooks.post_install.is_some() {
                    // A hook runs from `install_dir` (or the source dir) and
                    // stages from `build_dir`; a relative path or a missing
                    // build dir cannot do that. Report it here, before a build.
                    if let Err(e) = plan.install_hook_paths() {
                        push(format!("repo '{}': install hook paths: {e:#}", repo.name()));
                    }
                }
                for (phase, template) in [
                    ("install", hooks.install.as_deref()),
                    ("post_install", hooks.post_install.as_deref()),
                ] {
                    if let Some(template) = template {
                        if let Err(e) = plan.render(template) {
                            push(format!("repo '{}': {phase} hook: {e:#}", repo.name()));
                        }
                    }
                }
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(body: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.toml"), body).unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        (dir, ws)
    }

    /// The issue text, one line per issue, so a test can assert on the whole set
    /// without depending on issue order.
    fn messages(ws: &Workspace) -> String {
        check_one(ws, ws.project("p").unwrap())
            .into_iter()
            .map(|issue| issue.message)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_toolchain_support_mismatch_is_reported() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cmake"
            toolchain = "t"

            [repos.main]
            path = "/src/p"
            main_branch = "main"

            [toolchains.t]
            supports = ["meson"]
            "#,
        );
        let text = messages(&ws);
        assert!(
            text.contains("toolchain 't' does not support 'cmake'"),
            "{text}"
        );
    }

    #[test]
    fn an_unknown_toolchain_is_reported_by_name() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            toolchain = "ghost"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("unknown toolchain 'ghost'"), "{text}");
        // The dry resolve selects the same toolchain, so it fails too rather
        // than silently planning around the typo.
        assert!(text.contains("resolution:"), "{text}");
    }

    #[test]
    fn a_dry_resolve_reports_a_broken_template() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            build_dir = "{{missing.path}}/_build"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("resolution:"), "{text}");
        assert!(text.contains("missing.path"), "{text}");
    }

    /// A value that is one bare unknown name is a broken template too, not an
    /// empty variable for the build to run with.
    #[test]
    fn a_dry_resolve_reports_a_bare_unknown_name() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            environment = { VK_ICD_FILENAMES = "{{icd_json}}" }

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            "#,
        );
        let text = messages(&ws);
        assert!(
            text.contains(
                "resolution: resolving project.environment.VK_ICD_FILENAMES: \
                 cannot resolve path 'icd_json'"
            ),
            "{text}"
        );
    }

    #[test]
    fn an_install_hook_without_a_build_dir_is_reported() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            install_dir = "{{repos.main.workdir}}/_install"
            [repos.main.hooks]
            install = "echo install"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("require a build_dir"), "{text}");
    }

    #[test]
    fn a_relative_install_hook_path_is_reported() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build"
            install_dir = "_install"
            [repos.main.hooks]
            install = "echo install"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("absolute install_dir"), "{text}");
    }

    #[test]
    fn a_cargo_post_install_without_install_is_reported() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cargo"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build"
            install_dir = "{{repos.main.workdir}}/_install"
            [repos.main.hooks]
            post_install = "echo fixup"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("post_install without install"), "{text}");
    }

    #[test]
    fn a_broken_install_hook_template_is_reported() {
        let (_d, ws) = ws_with(
            r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/p"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build"
            install_dir = "{{repos.main.workdir}}/_install"
            [repos.main.hooks]
            install = "sh {{missing.path}}"
            "#,
        );
        let text = messages(&ws);
        assert!(text.contains("install hook:"), "{text}");
        assert!(text.contains("missing.path"), "{text}");
    }
}
