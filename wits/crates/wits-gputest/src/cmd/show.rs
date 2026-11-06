//! `wits gputest show` — one archive's `run.toml` as written, and optionally
//! the runner's `failures.csv`.

use std::io::Write;

use anyhow::Result;

use crate::archive;
use crate::cmd::ShowArgs;
use crate::config::Config;
use crate::store;

pub fn run(args: &ShowArgs) -> Result<()> {
    let config = Config::load()?;
    let found = store::find(&config.store, &args.case, &args.id)?;
    let run_toml = archive::read_first(&found.path)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&run_toml)?;
    if args.failures && !archive::copy_member(&found.path, "failures.csv", &mut stdout)? {
        eprintln!("{} has no failures.csv", found.path.display());
    }
    stdout.flush()?;
    Ok(())
}
