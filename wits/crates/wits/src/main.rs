//! `wits` — a single binary that collects personal workflow tools behind one
//! command tree.
//!
//! The collection grows one subcommand at a time. Keeping everything in one
//! binary (rather than a pile of scripts) buys a shared library ([`wits_util`]),
//! consistent flags, and a single thing to build and put on `$PATH`. The
//! built-ins are `transcrypt`, `stack`, `review`, `worktree`, `project`, `build`,
//! `update`, and `dotfiles`; adding one is a module under `cmd/` and a match arm
//! below.
//!
//! There are two ways to invoke a built-in, the way `mount` accepts either
//! `mount -t xfs` or `mount.xfs`: the umbrella form `wits foo` and the direct
//! form `wits-foo` (a symlink to this binary whose name we read from `argv[0]`
//! and splice back in). The applet names come straight from the subcommand list,
//! so a new command earns its direct form for free.
//!
//! Anything that is *not* a built-in — `wits foo` where `foo` is unknown — is
//! dispatched git-style to a `wits-foo` executable on `$PATH`. That is the whole
//! plugin system: a plugin is any executable named `wits-<name>`, in any
//! language, optionally sharing this crate's [`wits_util`] floor. `wits help`
//! lists the built-ins and the plugins it finds on `$PATH`.

use std::ffi::OsString;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(not(unix))]
compile_error!("wits requires a Unix platform (uses exec() for plugin dispatch)");

use clap::{CommandFactory, Parser, Subcommand};

mod cmd;

#[derive(Debug, Parser)]
#[command(
    name = "wits",
    version,
    about = "Personal workflow tools, collected behind one command.",
    arg_required_else_help = true
)]
struct Cli {
    /// Show the individual git commands as they run.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Print mutating actions instead of performing them.
    ///
    /// Read-only queries still run, so control flow stays correct: a dry-run
    /// still asks git and the forge what the world looks like in order to decide
    /// what it *would* do, then prints the pushes, MR changes, and file writes
    /// rather than carrying them out.
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Transparent file encryption driven by git's clean/smudge filters.
    Transcrypt(cmd::transcrypt::TranscryptArgs),
    /// Manage a stack of branches as a set of merge requests.
    Stack(cmd::stack::StackArgs),
    /// Review merge requests locally: fetch, comment, and submit across forges.
    Review(cmd::review::ReviewArgs),
    /// Create, inspect, and reclaim git worktrees in any repository.
    Worktree(cmd::worktree::WorktreeArgs),
    /// Describe source projects and answer machine-readable path queries.
    Project(cmd::project::ProjectArgs),
    /// Configure and build a project.
    Build(cmd::build::BuildArgs),
    /// Refresh git for every repo of a project.
    Update(cmd::update::UpdateArgs),
    /// Compile a dotfiles manifest tree into Dotdrop's inputs.
    Dotfiles(cmd::dotfiles::DotfilesArgs),
    /// Print the built-in subcommand names, one per line — a runtime cross-check
    /// of the applet set.
    #[command(name = "__applets", hide = true)]
    Applets,

    /// Any other `wits <name>` runs a `wits-<name>` executable from `$PATH`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> anyhow::Result<()> {
    // Intercept the umbrella's top-level help so we can append discovered
    // plugins; clap would otherwise print and exit before `main` sees anything.
    // Applet and subcommand invocations are left entirely to clap.
    if invoked_as_umbrella() && wants_top_level_help() {
        return print_help_with_plugins();
    }

    let cli = parse_args();
    wits_util::log::init(cli.verbose, cli.dry_run);

    match &cli.command {
        Commands::Transcrypt(args) => cmd::transcrypt::run(args),
        Commands::Stack(args) => cmd::stack::run(args),
        Commands::Review(args) => cmd::review::run(args),
        Commands::Worktree(args) => cmd::worktree::run(args),
        Commands::Project(args) => cmd::project::run(args),
        Commands::Build(args) => cmd::build::run(args),
        Commands::Update(args) => cmd::update::run(args),
        Commands::Dotfiles(args) => cmd::dotfiles::run(args),
        Commands::Applets => {
            for name in builtin_names() {
                println!("{name}");
            }
            Ok(())
        }
        Commands::External(args) => dispatch_plugin(args),
    }
}

/// Run an unknown subcommand as an external `wits-<name>` executable, git-style.
/// On success `exec` replaces this process, so the plugin owns the terminal and
/// its exit status is ours; it only returns here on failure.
fn dispatch_plugin(args: &[OsString]) -> anyhow::Result<()> {
    let (name, rest) = args
        .split_first()
        .expect("external_subcommand always carries the name");
    let bin = format!("wits-{}", name.to_string_lossy());
    let err = std::process::Command::new(&bin).args(rest).exec();
    match err.kind() {
        std::io::ErrorKind::NotFound => anyhow::bail!(
            "unknown subcommand '{}': no built-in, and no '{bin}' on PATH",
            name.to_string_lossy()
        ),
        _ => Err(anyhow::Error::new(err).context(format!("failed to run plugin '{bin}'"))),
    }
}

/// Parse the command line, honouring the `wits-<sub>` applet form. When the
/// program was run under such a name, the subcommand is taken from `argv[0]`;
/// otherwise this is the plain `wits <command>` path.
fn parse_args() -> Cli {
    let mut argv = std::env::args_os();
    let prog = argv.next().unwrap_or_default();

    match applet_from_prog(&prog.to_string_lossy()) {
        Some(applet) => {
            let spliced = [OsString::from("wits"), OsString::from(applet)]
                .into_iter()
                .chain(argv);
            Cli::parse_from(spliced)
        }
        None => Cli::parse(),
    }
}

/// Resolve `argv[0]` to a built-in subcommand for the direct form. Only the
/// `wits-<sub>` (dash) spelling is recognised; a leading path is stripped, and
/// `wits` itself — or any name that is not a built-in — falls through to normal
/// parsing (so a `wits-foo` *plugin* binary is never shadowed here).
fn applet_from_prog(prog: &str) -> Option<String> {
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    let stem = base.strip_prefix("wits-")?;
    builtin_names().into_iter().find(|name| name == stem)
}

/// The built-in subcommand names: the real commands, excluding the hidden
/// `__applets`, the external catch-all, and clap's auto `help`.
fn builtin_names() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .filter(|name| name != "help")
        .collect()
}

/// Whether this process was launched as the bare `wits` umbrella (not a
/// `wits-<sub>` applet or plugin), so top-level help belongs to us.
fn invoked_as_umbrella() -> bool {
    std::env::args_os()
        .next()
        .as_deref()
        .and_then(|p| Path::new(p).file_name().map(|f| f == "wits"))
        .unwrap_or(false)
}

/// A bare top-level help request: `wits`, `wits -h`, `wits --help`, or
/// `wits help` with nothing after it. Subcommand help (`wits stack --help`) is
/// left to clap.
fn wants_top_level_help() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => true,
        [one] => one == "-h" || one == "--help" || one == "help",
        _ => false,
    }
}

/// Print the standard clap help, then a section listing the `wits-*` plugins
/// discovered on `$PATH`.
fn print_help_with_plugins() -> anyhow::Result<()> {
    Cli::command().print_help()?;
    println!();
    let plugins = discover_plugins();
    if !plugins.is_empty() {
        println!("\nPlugins (wits-* found on PATH):");
        for name in plugins {
            println!("  {name}");
        }
    }
    Ok(())
}

/// Every `wits-<name>` executable on `$PATH` that is not one of the built-in
/// applet symlinks, deduplicated and sorted. This is how the plugin system makes
/// itself visible without a registry.
fn discover_plugins() -> Vec<String> {
    let builtins: std::collections::HashSet<String> = builtin_names().into_iter().collect();
    let mut found = std::collections::BTreeSet::new();

    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(sub) = name.strip_prefix("wits-") else {
                continue;
            };
            // Skip the built-in applet symlinks; they are `wits` itself.
            if sub.is_empty() || builtins.contains(sub) {
                continue;
            }
            if is_executable(&entry.path()) {
                found.insert(sub.to_owned());
            }
        }
    }
    found.into_iter().collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umbrella_name_is_not_an_applet() {
        assert_eq!(applet_from_prog("wits"), None);
        assert_eq!(applet_from_prog("/usr/local/bin/wits"), None);
    }

    #[test]
    fn the_dash_form_resolves_to_a_builtin() {
        assert_eq!(
            applet_from_prog("wits-transcrypt").as_deref(),
            Some("transcrypt")
        );
        assert_eq!(
            applet_from_prog("/home/me/.local/bin/wits-transcrypt").as_deref(),
            Some("transcrypt")
        );
    }

    #[test]
    fn dropped_forms_do_not_resolve() {
        // Dot form and bare names are no longer applets.
        assert_eq!(applet_from_prog("wits.transcrypt"), None);
        assert_eq!(applet_from_prog("transcrypt"), None);
    }

    #[test]
    fn unknown_dash_names_fall_through() {
        // A `wits-gputest` plugin binary must not be mistaken for a built-in.
        assert_eq!(applet_from_prog("wits-gputest"), None);
        assert_eq!(applet_from_prog("wits-bogus"), None);
    }
}
