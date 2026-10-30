//! `wits update` — the CLI shell over [`wits_actions::update`](fn@wits_actions::update).
//!
//! The action — clone what is missing, update what is there, never switch a
//! branch — is `wits-actions`'; this module only parses the flags and resolves
//! the target.

use anyhow::Result;
use clap::Args;

use wits_actions::UpdateOptions;
use wits_project::{resolve_target, Workspace};

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Project name or path (default: the project owning the current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// Also refresh repos borrowed from another project (`from`). Off by default:
    /// the project that owns a shared component updates it, so a component used
    /// by five projects is fetched once, not five times.
    #[arg(long = "with-borrowed")]
    pub with_borrowed: bool,
}

pub fn run(args: &UpdateArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let project = resolve_target(&ws, args.target.as_deref())?.project;
    wits_actions::update(
        &ws,
        project,
        &UpdateOptions {
            with_borrowed: args.with_borrowed,
        },
    )
}
