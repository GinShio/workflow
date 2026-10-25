//! The `wits devenv` action — run a program against one build of a project.
//!
//! Running a program against a particular build — a branch's tree, a review
//! snapshot — needs that build's runtime environment: loader manifests, library
//! paths, tool paths. The registry declares it as `devenv`, beside the build-time
//! `environment` and never mixed with it; the build system contributes the base
//! where it keeps one of its own (`meson devenv`); this action composes the two
//! for the same build `wits build` makes from the same flags, through the
//! [`crate::Identity`] the two share.
//!
//! The base is read back as data rather than nested around the program. Meson
//! composes its developer environment in its own process, so `meson devenv` runs
//! the `wits` binary's hidden `__devenv-capture`, and the environment that process
//! inherited comes back as a file of exact bytes. Meson's own `--dump` is no
//! substitute: it is shell text whose prepends are literal `$VAR` placeholders.
//! With real values in hand, a declared `prepend` lands on a value Meson set, and
//! the CLI's `--dump` speaks a shell's or an editor's own syntax.
//!
//! It never switches a branch. An in-place `--branch` selects that branch's
//! build tree and leaves the checkout where it is: switching would have to hold
//! for the life of whatever runs, a shell included.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_build::backend_for;
use wits_core::process::Command;
use wits_project::{resolve_plan, BuildInput, PathInput, Project, Workspace};

use crate::{Env, Identity};

/// Set in every devenv to the project it belongs to (`org/name`), so a prompt
/// or a script can tell it is inside one.
const MARKER: &str = "WITS_DEVENV";

/// One devenv request: the resolved identity plus the binary that implements
/// the hidden `__devenv-capture` plumbing — the `wits` binary itself, for the
/// CLI; an orchestrator may pass its own path.
pub struct DevenvRequest {
    pub identity: Identity,
    pub capture_program: PathBuf,
}

/// The change a devenv makes to an environment: the variables to set — added or
/// changed — and the names to remove, relative to the environment [`compose`]
/// was given. [`run`] applies exactly these and touches nothing else, so a
/// variable the devenv does not change passes to the program byte-for-byte and
/// is never converted through a string.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub set: Vec<(OsString, OsString)>,
    pub unset: Vec<OsString>,
}

/// Compose the change a program run against this build sees: the backend's base
/// and the declared operations, diffed against the environment the program
/// would otherwise inherit (`caller`).
///
/// The result is an [`Overlay`] rather than the whole environment, because the
/// only questions asked of a devenv — run a program with it, or print what it
/// changes — are both about the change. `caller` must be the environment the
/// program will inherit; the CLI passes its own process environment.
pub fn compose(
    ws: &Workspace,
    project: &Project,
    request: &DevenvRequest,
    caller: &Env,
) -> Result<Overlay> {
    let identity = &request.identity;
    let profile = &identity.profile;
    // No toolchain injection and no extra args: those shape the build's own
    // commands, and nothing of them reaches a program run against its output.
    let plan = resolve_plan(
        ws,
        project,
        &BuildInput {
            paths: PathInput {
                profile,
                branch: identity.branch.as_deref(),
                work_dir: identity.work_dir.as_deref(),
                build_dir: identity.build_dir.as_deref(),
                install_dir: identity.install_dir.as_deref(),
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        },
    )?;
    let build_dir = plan.paths.build_dir.as_deref().with_context(|| {
        format!(
            "project '{}' declares no build_dir, so there is no build to run against",
            project.key()
        )
    })?;
    if !build_dir.is_dir() {
        bail!(
            "build directory {} does not exist; build it first with `wits build`",
            build_dir.display()
        );
    }

    let mut env = base(project, build_dir, &request.capture_program, caller)?;
    for entry in &plan.logical.devenv {
        entry.apply(&mut env);
    }
    env.insert(MARKER.into(), project.key().into());
    Ok(overlay_between(caller, &env))
}

/// The difference between the environment the program would inherit (`caller`)
/// and the composed one: what to set, what to remove. Both sides are ordered
/// maps, so the lists are stable.
fn overlay_between(caller: &Env, env: &Env) -> Overlay {
    let mut overlay = Overlay::default();
    for (name, value) in env {
        if caller.get(name) != Some(value) {
            overlay.set.push((name.clone(), value.clone()));
        }
    }
    for name in caller.keys() {
        if !env.contains_key(name) {
            overlay.unset.push(name.clone());
        }
    }
    overlay
}

/// The environment the declared operations land on: the build system's own
/// developer environment where it keeps one, else the caller's.
fn base(project: &Project, build_dir: &Path, capture_program: &Path, caller: &Env) -> Result<Env> {
    let Some(backend) = project.settings().build_system.map(backend_for) else {
        return Ok(caller.clone());
    };
    let Some(runner) = backend.devenv_runner(build_dir) else {
        return Ok(caller.clone());
    };
    if !backend.is_configured(build_dir) {
        bail!(
            "build directory {} is not configured; configure it first with `wits build`",
            build_dir.display()
        );
    }

    // A file rather than the runner's stdout: `meson devenv` writes its own
    // messages there, errors included.
    let file = tempfile::NamedTempFile::new().context("creating the devenv capture file")?;
    let (program, prefix) = runner
        .split_first()
        .context("the backend's devenv runner is empty")?;
    // The base must be known for the result to be described, so this read runs
    // under a dry-run too — the program it would run is what dry-run withholds.
    // Meson's run mode writes nothing outside the build directory, and inside it
    // only GDB auto-load files, for a project that installs GDB helpers.
    let result = Command::new(program)
        .args(prefix.iter().cloned())
        .args([
            capture_program.display().to_string(),
            "__devenv-capture".to_owned(),
            file.path().display().to_string(),
        ])
        .force_run()
        .exec()?;
    if !result.is_success() {
        let output = [result.stdout.trim(), result.stderr.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "`{}` failed (exit {}){}{output}",
            runner.join(" "),
            result.exit_code,
            if output.is_empty() { "" } else { ":\n" }
        );
    }
    read_capture(file.path())
}

/// `wits __devenv-capture FILE`: write the environment this process inherited
/// to `FILE`, each `NAME=VALUE` terminated by a NUL — the one separator no
/// variable can contain, so the bytes come back exactly.
pub fn capture(file: &Path) -> Result<()> {
    let mut out = Vec::new();
    for (name, value) in std::env::vars_os() {
        out.extend_from_slice(name.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    std::fs::write(file, out).with_context(|| format!("writing {}", file.display()))
}

fn read_capture(file: &Path) -> Result<Env> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    Ok(parse_capture(&bytes))
}

fn parse_capture(bytes: &[u8]) -> Env {
    bytes
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|&b| b == b'=')?;
            Some((
                OsString::from_vec(entry[..eq].to_vec()),
                OsString::from_vec(entry[eq + 1..].to_vec()),
            ))
        })
        .collect()
}

/// Replace this process with `command` — or `$SHELL` when none is given — in
/// the ambient environment plus `overlay`. Runs in the caller's working
/// directory, so a relative path in the command means what was typed.
///
/// Only the overlay's names are touched: everything else is inherited exactly
/// as it is, so a variable the devenv does not change never passes through a
/// conversion, UTF-8 or otherwise. A named one is set or removed with its exact
/// bytes. `overlay` is relative to the environment [`compose`] was given, which
/// must therefore be the one this process inherits — the CLI passes its own
/// environment to both.
pub fn run(overlay: &Overlay, command: &[String]) -> Result<()> {
    let (program, args) = match command.split_first() {
        Some((program, args)) => (program.clone(), args.to_vec()),
        None => (
            std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.is_empty())
                .unwrap_or_else(|| "/bin/sh".to_owned()),
            Vec::new(),
        ),
    };
    let mut cmd = Command::new(&program);
    cmd.args(args);
    for (name, value) in &overlay.set {
        cmd.env_os(name, value);
    }
    for name in &overlay.unset {
        cmd.env_remove_os(name);
    }
    cmd.exec_replace()
        .with_context(|| format!("running '{program}' in the devenv"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn a_capture_round_trips_values_holding_newlines_and_equals_signs() {
        let bytes = b"A=1\0B=x=y\0C=line\nnext\0\0NOEQ\0";
        assert_eq!(
            parse_capture(bytes),
            env(&[("A", "1"), ("B", "x=y"), ("C", "line\nnext")])
        );
    }

    /// The overlay is the change and nothing else: unchanged entries are absent,
    /// removals are the caller's names the composed environment dropped, and a
    /// non-UTF-8 value rides along as bytes, ready for `run`.
    #[test]
    fn the_overlay_names_only_what_changed_or_left() {
        let caller = env(&[("KEEP", "1"), ("CHANGE", "old"), ("DROP", "x")]);
        let composed = env(&[("KEEP", "1"), ("CHANGE", "new"), ("ADD", "a")]);
        assert_eq!(
            overlay_between(&caller, &composed),
            Overlay {
                set: vec![
                    (OsString::from("ADD"), OsString::from("a")),
                    (OsString::from("CHANGE"), OsString::from("new")),
                ],
                unset: vec![OsString::from("DROP")],
            }
        );

        use std::os::unix::ffi::OsStringExt;
        let odd = OsString::from_vec(b"v\xff".to_vec());
        let mut caller = Env::new();
        caller.insert(OsString::from("X"), odd.clone());
        caller.insert(OsString::from("SAME"), OsString::from("s"));
        let mut joined = odd.clone();
        joined.push(":more");
        let mut composed = caller.clone();
        composed.insert(OsString::from("X"), joined.clone());
        assert_eq!(
            overlay_between(&caller, &composed).set,
            vec![(OsString::from("X"), joined)]
        );
        assert!(overlay_between(&caller, &caller).set.is_empty());
    }
}
