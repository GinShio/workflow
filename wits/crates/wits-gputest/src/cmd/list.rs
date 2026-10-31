//! `wits gputest list` — one line per case: last run, verdict and asset state.
//!
//! Deliberately cheap: it reads stamps and asset metadata, never composes a
//! driver. The exit comes from the last archive's `run.toml`.

use anyhow::Result;

use wits_project::Workspace;

use crate::archive;
use crate::config::{Config, Suite};
use crate::device;
use crate::resolve;
use crate::stamp;
use crate::state;
use crate::store;
use crate::util;

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let ws = Workspace::load()?;
    let device = device::probe()?;
    let device_key = device.key();

    for case in config.cases.values() {
        let suite = config.suite(&case.suite)?;
        let assets = match asset_state(&ws, suite) {
            Ok(()) => "ok".to_owned(),
            Err(err) => format!("missing: {}", one_line(&format!("{err:#}"))),
        };

        let state = match state::read(&case.name, &device_key) {
            Ok(state) => state,
            Err(err) => {
                println!(
                    "{:<20} {:<24} {:<6} assets={} state error: {}",
                    case.name,
                    "-",
                    "-",
                    assets,
                    one_line(&format!("{err:#}"))
                );
                continue;
            }
        };
        let Some(state) = state else {
            println!("{:<20} {:<24} {:<6} assets={assets}", case.name, "-", "-");
            continue;
        };
        let exit = match store::find(&config.store, &case.name, &state.run) {
            Ok(archive) => match archive::read_run(&archive.path) {
                Ok(record) => record.exit.to_string(),
                Err(err) => format!("? ({})", one_line(&format!("{err:#}"))),
            },
            Err(_) => "gone".to_owned(),
        };
        println!(
            "{:<20} {:<24} {:<6} assets={assets}",
            case.name, state.run, exit
        );
    }
    Ok(())
}

fn asset_state(ws: &Workspace, suite: &Suite) -> Result<()> {
    let suite_project = ws.project(&suite.project)?;
    let runner_project = ws.project(&suite.runner)?;
    let suite_root = resolve::install_root(ws, suite_project)?;
    let runner_root = resolve::install_root(ws, runner_project)?;
    resolve::ensure_separate_roots(&suite_root, &runner_root)?;
    stamp::read(&suite_root, &suite_project.key())?;
    stamp::read(&runner_root, &runner_project.key())?;
    let runner = runner_root.join(suite.kind.runner_bin());
    if !runner.is_file() {
        anyhow::bail!("runner binary {} is missing", runner.display());
    }
    if !util::is_executable(&runner)? {
        anyhow::bail!("runner binary {} is not executable", runner.display());
    }
    suite.kind.check_assets(suite, &suite_root)?;
    Ok(())
}

fn one_line(text: &str) -> String {
    text.replace('\n', "; ")
}
