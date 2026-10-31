//! `wits gputest prune` — remove expired archives, stale `.part` files and
//! empty directories. State is deliberately left alone.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::store;

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let dry_run = wits_core::log::is_dry_run();
    let report = store::prune(&config.store, dry_run)?;
    for path in &report.removed {
        if dry_run {
            println!("[DRY-RUN] remove {}", path.display());
        } else {
            println!("removed {}", path.display());
        }
    }
    if !report.failed.is_empty() {
        for failure in &report.failed {
            eprintln!("error: {failure}");
        }
        bail!("{} prune operation(s) failed", report.failed.len());
    }
    Ok(())
}
