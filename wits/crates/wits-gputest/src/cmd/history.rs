//! `wits gputest history` — the archives for one case, newest first.
//!
//! Only each archive's `run.toml` is read; the CSVs behind it stay compressed.

use anyhow::Result;

use crate::archive;
use crate::cmd::HistoryArgs;
use crate::config::Config;
use crate::store;

pub fn run(args: &HistoryArgs) -> Result<()> {
    let config = Config::load()?;
    let archives = store::archives(&config.store, &args.case)?;
    if archives.is_empty() {
        eprintln!(
            "no archives for case '{}' under {}",
            args.case,
            config.store.root.display()
        );
    }
    for found in archives.into_iter().take(args.limit.unwrap_or(usize::MAX)) {
        match archive::read_run(&found.path) {
            Ok(record) => {
                let commit = record
                    .driver_commit()
                    .map(|commit| commit.chars().take(12).collect::<String>())
                    .unwrap_or_else(|| "-".to_owned());
                println!(
                    "{}  {}  {:<12}  {}",
                    found.id,
                    found.device,
                    commit,
                    record.summary()
                );
            }
            Err(err) => eprintln!("{}: {err:#}", found.path.display()),
        }
    }
    Ok(())
}
