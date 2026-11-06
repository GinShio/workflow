//! `wits gputest list` — one line per case: its newest archive on this device,
//! and whether the case can run now. It exits 1 when any case cannot, so it is
//! also the check before a long run. It reads the registry, the stamps, the
//! installed layouts and `run.toml`s, and never composes a driver.

use anyhow::{Context, Result};

use wits_project::Workspace;

use crate::archive;
use crate::config::{Case, Config};
use crate::{device, resolve, store};

pub fn run() -> Result<u8> {
    let config = Config::load()?;
    let ws = Workspace::load()?;
    let device = device::probe()?.key();
    let mut blocked = false;
    for case in config.cases.values() {
        let mut line = format!("{:<20} {}", case.name, newest(&config, &case.name, &device));
        if let Err(err) = ready(&config, &ws, case) {
            blocked = true;
            line.push_str(&format!(
                "  [not ready: {}]",
                format!("{err:#}").replace('\n', "; ")
            ));
        }
        println!("{line}");
    }
    Ok(u8::from(blocked))
}

/// The case's newest archive on this device, as `<id>  <summary>`.
fn newest(config: &Config, case: &str, device: &str) -> String {
    let found = match store::archives(&config.store, case) {
        Ok(archives) => archives
            .into_iter()
            .find(|archive| archive.device == device),
        Err(err) => return format!("{:<16}  {err:#}", "-"),
    };
    let Some(found) = found else {
        return format!("{:<16}  never run", "-");
    };
    match archive::read_run(&found.path) {
        Ok(record) => format!("{:<16}  {}", found.id, record.summary()),
        Err(err) => format!("{:<16}  unreadable: {err:#}", found.id),
    }
}

/// Whether the case can run: its driver's project and its layer's are in the
/// registry, and its suite is installed.
fn ready(config: &Config, ws: &Workspace, case: &Case) -> Result<()> {
    let driver = config.driver(&case.driver)?;
    ws.project(&driver.project).with_context(|| {
        format!(
            "driver '{}': project '{}' is not in the registry",
            driver.name, driver.project
        )
    })?;
    if let Some(name) = &case.layer {
        let layer = config.layer(name)?;
        ws.project(&layer.project).with_context(|| {
            format!(
                "layer '{}': project '{}' is not in the registry",
                layer.name, layer.project
            )
        })?;
    }
    resolve::installed(ws, config.suite(&case.suite)?)?;
    Ok(())
}
