//! `project __names` — the name lists shell completion needs.

use std::collections::BTreeSet;

use anyhow::Result;
use clap::{Args, ValueEnum};

use wits_project::{resolve_target, Project, Workspace};

#[derive(Debug, Args)]
pub struct NamesArgs {
    /// Which names to print.
    #[arg(value_enum)]
    pub kind: NamesKind,
    /// Scope the answer to one project, by name (`org/name` disambiguates) or
    /// path. Presets and repos become that project's own; without it, the
    /// union over every project — the over-approximation completion offers
    /// when no target is on the line yet.
    pub target: Option<String>,
}

/// The name lists [`NamesArgs`] can print. Each serves one flag family —
/// `--toolchain` takes a registry toolchain, `--preset` a preset the target
/// resolves, `--focus` and `repos.<name>.*` a repo of the target. With a
/// target, every line is a spelling that flag accepts for it; without one,
/// the union is the over-approximation completion falls back to while the
/// target is still being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NamesKind {
    /// Toolchains declared in the registry.
    Toolchains,
    /// Presets: bare names the target merges at its default focus, plus its
    /// org's `org/name` forms. The list is for that focus — a `--focus`
    /// override is a different merge: it adds the override repo's own names
    /// and drops the default focus's.
    Presets,
    /// Repo names of the target project.
    Repos,
}

/// `__names`: one candidate per line for shell completion, spelled the way
/// the matching flag accepts it. The consumer is fish completion, which reads
/// this instead of holding its own parser of the registry — so the contract
/// is the output shape alone: sorted, deduplicated, one line per name,
/// nothing else on stdout. The lists are the candidates completion should
/// offer, not an exhaustive enumeration of every accepted spelling — a
/// `--preset` may also reach another org's presets as `org/name`.
pub(super) fn run(ws: &Workspace, args: &NamesArgs) -> Result<()> {
    match args.kind {
        NamesKind::Toolchains => {
            for name in ws.toolchain_names() {
                println!("{name}");
            }
        }
        NamesKind::Presets => {
            for name in preset_names(ws, args.target.as_deref())? {
                println!("{name}");
            }
        }
        NamesKind::Repos => {
            for name in repo_names(ws, args.target.as_deref())? {
                println!("{name}");
            }
        }
    }
    Ok(())
}

/// The preset candidates to print for `--preset`: for a target, that
/// project's candidates at its default focus — the bare mergeable names plus
/// its org's qualified `org/name` forms; without one, the union over every
/// project. The same name may be declared at several layers and deduplicates
/// to one line.
fn preset_names(ws: &Workspace, target: Option<&str>) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for project in targets(ws, target)? {
        let focus = project.focus_name(None);
        names.extend(ws.preset_names(project, focus));
    }
    Ok(names.into_iter().collect())
}

/// The repo names a project's flags can address: `--focus` and the
/// `repos.<name>.*` paths, for the target or across the registry.
fn repo_names(ws: &Workspace, target: Option<&str>) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for project in targets(ws, target)? {
        names.extend(ws.repo_names(project));
    }
    Ok(names.into_iter().collect())
}

/// The projects a name query answers for: one resolved target, or every
/// project in the registry. A target goes through [`resolve_target`], the one
/// name/path funnel the other verbs share; absent, a union needs no cwd
/// context and answers for the whole registry.
fn targets<'a>(ws: &'a Workspace, target: Option<&str>) -> Result<Vec<&'a Project>> {
    match target {
        None => Ok(ws.projects().collect()),
        Some(target) => resolve_target(ws, Some(target)).map(|p| vec![p]),
    }
}
