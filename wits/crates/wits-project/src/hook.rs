//! Hook rendering: the one place a `[repos.*.hooks]` command is turned into a
//! concrete shell string.
//!
//! A hook template resolves against the repo's own namespace
//! ([`crate::context::context_for_repo`]), which for a borrowed repo is the
//! **borrower's** project context — the hook runs where the repo is used, even
//! though its text travelled from the owner.

use anyhow::{bail, Result};

use crate::context::{self, Ctx};
use crate::decl::HooksDecl;
use crate::workspace::{Project, Workspace};

/// The lifecycle phase a hook belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Clone,
    PostClone,
    PreUpdate,
    Update,
    PostUpdate,
}

impl HookPhase {
    /// The phase's spelling in a `[repos.*.hooks]` table, which is also the
    /// label a hook log line uses.
    pub fn as_str(self) -> &'static str {
        match self {
            HookPhase::Clone => "clone",
            HookPhase::PostClone => "post_clone",
            HookPhase::PreUpdate => "pre_update",
            HookPhase::Update => "update",
            HookPhase::PostUpdate => "post_update",
        }
    }

    fn command(self, hooks: &HooksDecl) -> Option<&str> {
        match self {
            HookPhase::Clone => hooks.clone.as_deref(),
            HookPhase::PostClone => hooks.post_clone.as_deref(),
            HookPhase::PreUpdate => hooks.pre_update.as_deref(),
            HookPhase::Update => hooks.update.as_deref(),
            HookPhase::PostUpdate => hooks.post_update.as_deref(),
        }
    }
}

impl Project {
    /// The rendered command for one phase of this project's repo `repo`, or
    /// `None` when the phase declares no hook (the built-in lifecycle then
    /// applies).
    pub fn render_hook(
        &self,
        ws: &Workspace,
        repo: &str,
        phase: HookPhase,
    ) -> Result<Option<String>> {
        let Some(declared) = self.repo(repo) else {
            bail!("project '{}' has no repo '{repo}'", self.key());
        };
        let Some(template) = phase.command(&declared.decl.hooks) else {
            return Ok(None);
        };
        let ctx = Ctx::new(context::context_for_repo(ws, self, repo));
        Ok(Some(ctx.render(template)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let ws = Workspace::load_from(dir.path()).unwrap();
        (dir, ws)
    }

    fn fixture() -> (tempfile::TempDir, Workspace) {
        ws_with(&[
            (
                "engine.toml",
                r#"
                [project]
                [repos.main]
                path = "/src/engine"
                main_branch = "main"
                [repos.main.hooks]
                clone = "{{project.name}}:{{repo.name}}:clone"
                post_clone = "{{project.name}}:{{repo.name}}:post_clone"
                pre_update = "{{project.name}}:{{repo.name}}:pre_update"
                update = "{{project.name}}:{{repo.name}}:update"
                post_update = "{{project.name}}:{{repo.name}}:post_update"
                "#,
            ),
            (
                "viewer.toml",
                r#"
                [project]
                [repos.main]
                path = "/src/viewer"
                main_branch = "main"
                [repos.engine]
                from = "engine"
                anchor = "main"
                "#,
            ),
            (
                "bad.toml",
                r#"
                [project]
                [repos.main]
                path = "/src/bad"
                main_branch = "main"
                [repos.main.hooks]
                pre_update = "{{missing.path}}"
                "#,
            ),
        ])
    }

    #[test]
    fn every_phase_renders_its_own_hook() {
        let (_d, ws) = fixture();
        let project = ws.project("engine").unwrap();
        for (phase, suffix) in [
            (HookPhase::Clone, "clone"),
            (HookPhase::PostClone, "post_clone"),
            (HookPhase::PreUpdate, "pre_update"),
            (HookPhase::Update, "update"),
            (HookPhase::PostUpdate, "post_update"),
        ] {
            assert_eq!(phase.as_str(), suffix);
            assert_eq!(
                project.render_hook(&ws, "main", phase).unwrap(),
                Some(format!("engine:main:{suffix}")),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn a_borrowed_hook_renders_in_the_borrowers_namespace() {
        let (_d, ws) = fixture();
        let project = ws.project("viewer").unwrap();
        assert_eq!(
            project
                .render_hook(&ws, "engine", HookPhase::PostClone)
                .unwrap(),
            Some("viewer:engine:post_clone".to_owned())
        );
    }

    #[test]
    fn an_unknown_path_in_a_hook_is_an_error() {
        let (_d, ws) = fixture();
        let project = ws.project("bad").unwrap();
        let err = project
            .render_hook(&ws, "main", HookPhase::PreUpdate)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing.path"), "{err}");
    }

    #[test]
    fn a_repo_the_project_does_not_declare_is_refused() {
        let (_d, ws) = fixture();
        let viewer = ws.project("viewer").unwrap();
        let err = viewer
            .render_hook(&ws, "nope", HookPhase::PreUpdate)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no repo 'nope'"), "{err}");
    }
}
