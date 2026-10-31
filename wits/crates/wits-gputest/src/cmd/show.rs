//! `wits gputest show` — one archive, as it is.

use std::io::Write;

use anyhow::{bail, Result};

use crate::archive;
use crate::cmd::ShowArgs;
use crate::cmd::{check_case_component, check_run_id};
use crate::config::Config;
use crate::store;

pub fn run(args: &ShowArgs) -> Result<()> {
    check_case_component(&args.case)?;
    check_run_id(&args.id)?;
    let config = Config::load()?;
    let archive_ref = store::find(&config.store, &args.case, &args.id)?;

    let (name, bytes) = archive::read_first(&archive_ref.path)?;
    if name != archive::RUN_TOML {
        bail!(
            "archive {} begins with {name:?}, not {}",
            archive_ref.path.display(),
            archive::RUN_TOML
        );
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    if !bytes.ends_with(b"\n") {
        stdout.write_all(b"\n")?;
    }
    if args.failures && !archive::copy_member(&archive_ref.path, "failures.csv", &mut stdout)? {
        eprintln!("{} has no failures.csv", archive_ref.path.display());
    }
    stdout.flush()?;
    Ok(())
}
