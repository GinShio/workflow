//! `wits gputest history` — the archives for one case, newest first.
//!
//! Only each archive's first member is read; the CSVs behind it stay compressed
//! and untouched.

use anyhow::Result;

use crate::archive;
use crate::cmd::check_case_component;
use crate::cmd::HistoryArgs;
use crate::config::Config;
use crate::store;
use crate::time;

pub fn run(args: &HistoryArgs) -> Result<()> {
    check_case_component(&args.case)?;
    let config = Config::load()?;
    let archives = store::archives(&config.store, &args.case)?;
    let limit = args.limit.unwrap_or(usize::MAX);
    for archive_ref in archives.into_iter().take(limit) {
        match archive::read_run(&archive_ref.path) {
            Ok(record) => {
                let commit = record
                    .driver_commit()
                    .map(|commit| commit.chars().take(12).collect::<String>())
                    .unwrap_or_else(|| "-".to_owned());
                println!(
                    "{:<22} {:<21} {:<5} {}",
                    archive_ref.id,
                    time::display_run_id(&archive_ref.id),
                    record.exit,
                    commit
                );
            }
            Err(err) => {
                eprintln!("{}: {err:#}", archive_ref.path.display());
            }
        }
    }
    Ok(())
}
