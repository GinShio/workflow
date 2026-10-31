//! `wits gputest plan` — the run pipeline without the run.

use anyhow::Result;

use wits_project::Workspace;

use crate::cmd::PlanArgs;
use crate::config::Config;
use crate::pipeline;

pub fn run(args: &PlanArgs) -> Result<()> {
    let config = Config::load()?;
    // The design's step 2: scrub before the registry is loaded, so a project
    // template that reads env.* cannot see caller graphics state either.
    crate::env::scrub_process_env();
    let ws = Workspace::load()?;
    pipeline::plan_cases(&config, &ws, &args.cases, &args.profile.to_flags())
}
