//! Hook rendering: the one place a `[repos.*.hooks]` command is turned into a
//! concrete shell string.
//!
//! A hook template resolves against the repo's own namespace
//! ([`crate::context::context_for_repo`]), which for a borrowed repo is the
//! **borrower's** project context — the hook runs where the repo is used, even
//! though its text travelled from the owner.

use anyhow::Result;

use crate::context::{self, Ctx};
use crate::decl::HooksDecl;
use crate::workspace::{Project, Repo, Workspace};

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
    /// The rendered command for one phase, or `None` when the phase declares no
    /// hook (the built-in lifecycle then applies).
    pub fn render_hook(
        &self,
        ws: &Workspace,
        repo: &Repo,
        phase: HookPhase,
    ) -> Result<Option<String>> {
        let Some(template) = phase.command(&repo.decl.hooks) else {
            return Ok(None);
        };
        let ctx = Ctx::new(context::context_for_repo(ws, self, repo.name()));
        Ok(Some(ctx.render(template)?))
    }
}
