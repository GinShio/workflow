//! `wits gputest run` — the whole pipeline, one report line per case.
//!
//! Under `-n` it prints the plan `plan` would print and does not run the test;
//! composition and stamp reads still happen, because the plan cannot be printed
//! without them.

use anyhow::Result;

use wits_project::Workspace;

use crate::cmd::RunArgs;
use crate::config::Config;
use crate::pipeline;

pub fn run(args: &RunArgs) -> Result<u8> {
    let config = Config::load()?;
    // The design's step 2: scrub before the registry is loaded, so a project
    // template that reads env.* cannot see caller graphics state either.
    crate::env::scrub_process_env();
    let ws = Workspace::load()?;
    let flags = args.profile.to_flags();
    if wits_core::log::is_dry_run() {
        pipeline::plan_cases(&config, &ws, &args.cases, &flags)?;
        return Ok(0);
    }
    pipeline::run_cases(&config, &ws, &args.cases, &flags, args.if_changed)
}
