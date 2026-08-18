//! `wits dotfiles` — compile a manifest tree into Dotdrop's inputs.
//!
//! The dotfiles repository describes itself in TOML: what each module deploys,
//! which machines exist, and which content layers and execution contexts each
//! machine wants. Dotdrop cannot express any of that — it has no conditional
//! selection, no layered sources, and no notion of privilege — so something has
//! to compile the description down to the flat catalogue Dotdrop does
//! understand. That is this command, and nothing more: it renders no file
//! bodies, installs no packages, and holds no opinion about encryption.
//!
//! Two verbs, because there are exactly two questions worth asking of a
//! description like this — *is it coherent?* (`check`) and *what does it mean?*
//! (`generate`). Deployment stays where it already is: `dotdrop install -c` on
//! one of the generated entrypoints.
//!
//! The pipeline is [`layout`] (where things are) -> [`tree`] (find and read) ->
//! [`resolve`] (decide) -> [`emit`] (write). Two seams matter. `resolve`
//! produces a whole [`Plan`](resolve::Plan) rather than writing as it goes, so
//! every decision is made before any file is touched and a different backend
//! would replace only the last stage. And every path — input and output alike —
//! comes from [`layout`], so what this command knows about a repository is its
//! *shape*, never its filenames.

mod emit;
mod layout;
mod model;
mod resolve;
mod tree;

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use tree::Repo;

/// `wits dotfiles` — generate or validate a repository's deployment inputs.
#[derive(Debug, Args)]
pub struct DotfilesArgs {
    #[command(subcommand)]
    pub command: DotfilesSub,
    /// The layout declaration to read. Default: `$WITS_DOTFILES_CONFIG`, then
    /// `wits.dotfiles.config`, then the nearest ancestor holding a
    /// `dotfiles.toml`.
    #[arg(long, value_name = "FILE", global = true)]
    pub config: Option<PathBuf>,
    /// A repository root to read the default-named declaration from — the same
    /// as `--config DIR/dotfiles.toml`.
    #[arg(long, value_name = "DIR", global = true, conflicts_with = "config")]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum DotfilesSub {
    /// Write the Dotdrop bundle and per-host entrypoints.
    Generate,
    /// Validate the manifests without writing anything.
    Check,
}

pub fn run(args: &DotfilesArgs) -> Result<()> {
    let repo = Repo::open(args.config.as_deref(), args.root.as_deref())?;
    match args.command {
        DotfilesSub::Generate => generate(&repo),
        DotfilesSub::Check => check(&repo),
    }
}

/// Validation is a read of the manifests plus one full resolve: a plan that
/// cannot be built is the most complete statement of "this does not work", and
/// building one has no side effects.
fn check(repo: &Repo) -> Result<()> {
    let (problems, mut notes) = resolve::inspect(repo)?;
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("{problem}");
        }
        bail!("{} manifest problem(s)", problems.len());
    }

    // Stale output needs the plan (it is defined as "what generate would not
    // write"), so it can only be reported once resolution has succeeded.
    let plan = resolve::plan(repo)?;
    notes.extend(plan.notes.iter().cloned());
    notes.extend(resolve::stale(repo, &plan)?);
    notes.sort();
    notes.dedup();

    report(&plan, &notes);
    Ok(())
}

fn generate(repo: &Repo) -> Result<()> {
    let plan = resolve::plan(repo)?;
    let written = emit::generate(repo, &plan)?;

    let mut notes = plan.notes.clone();
    notes.extend(resolve::stale(repo, &plan)?);
    notes.sort();
    notes.dedup();
    for note in &notes {
        eprintln!("{note}");
    }

    for path in &written.written {
        println!("{}", path.display());
    }
    println!(
        "{} written, {} unchanged",
        written.written.len(),
        written.unchanged
    );
    Ok(())
}

/// What `check` prints when the manifests hold together: the shape of the thing,
/// so the number that is wrong is visible without diffing generated output.
fn report(plan: &resolve::Plan, notes: &[String]) {
    for note in notes {
        eprintln!("{note}");
    }
    for entry in &plan.entrypoints {
        println!(
            "{:<8} {:<32} {:>3} unit(s)  {}",
            entry.plane,
            entry.host,
            entry.dotfiles.len(),
            entry.path.display()
        );
    }
    if !plan.overlay_variables.is_empty() {
        let overlays: Vec<&str> = plan.overlay_variables.keys().map(String::as_str).collect();
        println!("overlay aggregates: {}", overlays.join(", "));
    }
    if notes.is_empty() {
        println!("ok");
    } else {
        println!("ok ({} note(s))", notes.len());
    }
}
