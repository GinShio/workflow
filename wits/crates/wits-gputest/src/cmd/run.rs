//! `wits gputest run` — the whole pipeline, one report line per case. Under
//! `-n` it prints each case's plan instead: composition and stamp reads still
//! happen, because the plan cannot be printed without them.

use anyhow::Result;

use wits_project::Workspace;

use crate::cmd::RunArgs;
use crate::config::Config;
use crate::resolve::Session;
use crate::{device, env, pipeline};

pub fn run(args: &RunArgs) -> Result<u8> {
    let config = Config::load()?;
    // Before the registry loads: a project template that reads `env.*` must
    // not see caller graphics state either.
    let caller = env::scrub();
    let ws = Workspace::load()?;
    let session = Session {
        config: &config,
        ws: &ws,
        profile: &args.profile,
        groups: &args.groups,
        here: std::env::current_dir().ok(),
        caller,
        device: device::probe()?,
    };
    if wits_core::log::is_dry_run() {
        pipeline::plan_cases(&session, &args.cases)?;
        return Ok(0);
    }
    Ok(pipeline::run_cases(&session, &args.cases, args.if_changed))
}
