//! `wits devenv` — run a program against one build of a project.
//!
//! Running a program against a particular build — a branch's tree, a review
//! snapshot — needs that build's runtime environment: loader manifests, library
//! paths, tool paths. The registry declares it as `devenv`, beside the build-time
//! `environment` and never mixed with it; the build system contributes the base
//! where it keeps one of its own (`meson devenv`); this command composes the two
//! for the same build `wits build` makes from the same flags, through the
//! [`IdentityArgs`] the two share.
//!
//! The base is read back as data rather than nested around the program. Meson
//! composes its developer environment in its own process, so `meson devenv` runs
//! this binary's hidden `__devenv-capture`, and the environment that process
//! inherited comes back as a file of exact bytes. Meson's own `--dump` is no
//! substitute: it is shell text whose prepends are literal `$VAR` placeholders.
//! With real values in hand, a declared `prepend` lands on a value Meson set, and
//! `--dump` speaks a shell's or an editor's own syntax.
//!
//! It never switches a branch. An in-place `--branch` selects that branch's
//! build tree and leaves the checkout where it is: switching would have to hold
//! for the life of whatever runs, a shell included.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};

use wits_util::build_system::backend_for;
use wits_util::process::Command;
use wits_util::project::resolve::{self, PlanInput};
use wits_util::project::resolve_target;
use wits_util::project::workspace::{ProjectData, Workspace};

use crate::cmd::build::{target_branch, IdentityArgs};

/// Set in every devenv to the project it belongs to (`org/name`), so a prompt
/// or a script can tell it is inside one.
const MARKER: &str = "WITS_DEVENV";

#[derive(Debug, Args)]
pub struct DevenvArgs {
    /// Project name or path (default: the project owning the current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    #[command(flatten)]
    pub identity: IdentityArgs,
    /// Print what the environment changes instead of running anything. Bare,
    /// or `=sh` / `=fish` / `=json` to choose the syntax.
    #[arg(
        long,
        value_name = "FORMAT",
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "sh",
        conflicts_with = "command"
    )]
    pub dump: Option<DumpFormat>,
    /// The program to run, after `--` (default: `$SHELL`).
    #[arg(last = true, value_name = "CMD")]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DumpFormat {
    /// POSIX `export`/`unset` lines, for `eval` in sh, bash, or zsh.
    Sh,
    /// `set -gx`/`set -e` lines, for `eval` in fish.
    Fish,
    /// `{"set": {…}, "unset": […]}`, for an editor.
    Json,
}

type Env = BTreeMap<OsString, OsString>;

pub fn run(args: &DevenvArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let project = resolve_target(&ws, args.target.as_deref())?;
    let caller: Env = std::env::vars_os().collect();
    let env = compose(&ws, project, &args.identity, &caller)?;
    let delta = Delta::between(&caller, &env)?;
    match args.dump {
        Some(format) => {
            print!("{}", delta.render(format)?);
            Ok(())
        }
        None => run_in(&delta, &args.command),
    }
}

/// The environment a program run against this build sees: the backend's base,
/// the declared operations on top, and the marker.
fn compose(
    ws: &Workspace,
    project: &ProjectData,
    identity: &IdentityArgs,
    caller: &Env,
) -> Result<Env> {
    let profile = identity.profile.to_profile();
    let branch = target_branch(ws, project, &profile, identity.detach)?;
    // No toolchain injection and no extra args: those shape the build's own
    // commands, and nothing of them reaches a program run against its output.
    let plan = resolve::plan(
        ws,
        project,
        &PlanInput {
            profile: &profile,
            branch: branch.as_deref(),
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
            build_dir_override: identity.build_dir.as_deref(),
            install_dir_override: identity.install_dir.as_deref(),
        },
    )?;
    let build_dir = plan.build_dir.as_deref().with_context(|| {
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

    let mut env = base(project, build_dir, caller)?;
    for entry in &plan.logical.devenv {
        entry.apply(&mut env);
    }
    env.insert(MARKER.into(), project.key().into());
    Ok(env)
}

/// The environment the declared operations land on: the build system's own
/// developer environment where it keeps one, else the caller's.
fn base(project: &ProjectData, build_dir: &Path, caller: &Env) -> Result<Env> {
    let Some(backend) = project.project.build_system.map(backend_for) else {
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
    let exe = std::env::current_exe().context("locating the running wits binary")?;
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
            exe.display().to_string(),
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

/// What a devenv changes relative to the caller's environment. Only these are
/// handed on: everything untouched is inherited as it already is.
#[derive(Debug, Default, PartialEq)]
struct Delta {
    set: Vec<(String, String)>,
    unset: Vec<String>,
}

impl Delta {
    /// The difference between two environments. A variable the devenv changes
    /// must be valid UTF-8 to be spelled on a command line or in a dump; one it
    /// leaves alone is never inspected.
    fn between(caller: &Env, env: &Env) -> Result<Delta> {
        let text = |s: &OsString| {
            s.to_str().map(str::to_owned).with_context(|| {
                format!("devenv variable {} is not valid UTF-8", s.to_string_lossy())
            })
        };
        let mut delta = Delta::default();
        for (name, value) in env {
            if caller.get(name) != Some(value) {
                delta.set.push((text(name)?, text(value)?));
            }
        }
        for name in caller.keys().filter(|name| !env.contains_key(*name)) {
            delta.unset.push(text(name)?);
        }
        Ok(delta)
    }

    fn render(&self, format: DumpFormat) -> Result<String> {
        let mut out = String::new();
        match format {
            DumpFormat::Sh => {
                for (name, value) in &self.set {
                    out.push_str(&format!("export {name}={}\n", sh_quote(value)));
                }
                for name in &self.unset {
                    out.push_str(&format!("unset {name}\n"));
                }
            }
            DumpFormat::Fish => {
                // fish splits a `*PATH` value on its colons by itself, and keeps
                // any other value whole, so one quoted string is right for both.
                for (name, value) in &self.set {
                    out.push_str(&format!("set -gx {name} {}\n", fish_quote(value)));
                }
                for name in &self.unset {
                    out.push_str(&format!("set -e {name}\n"));
                }
            }
            DumpFormat::Json => {
                let set: BTreeMap<&str, &str> = self
                    .set
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();
                let view = serde_json::json!({ "set": set, "unset": self.unset });
                out.push_str(&serde_json::to_string_pretty(&view)?);
                out.push('\n');
            }
        }
        Ok(out)
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

/// Replace this process with `command` — or `$SHELL` when none is given — in
/// the devenv. Runs in the caller's working directory, so a relative path in
/// the command means what was typed.
fn run_in(delta: &Delta, command: &[String]) -> Result<()> {
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
    for (name, value) in &delta.set {
        cmd.env(name, value);
    }
    for name in &delta.unset {
        cmd.env_remove(name);
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

    #[test]
    fn the_delta_names_only_what_the_devenv_changed() {
        let caller = env(&[("KEEP", "1"), ("CHANGE", "old"), ("DROP", "x")]);
        let devenv = env(&[("KEEP", "1"), ("CHANGE", "new"), ("ADD", "a")]);
        assert_eq!(
            Delta::between(&caller, &devenv).unwrap(),
            Delta {
                set: vec![("ADD".into(), "a".into()), ("CHANGE".into(), "new".into())],
                unset: vec!["DROP".into()],
            }
        );
    }

    #[test]
    fn each_dump_format_quotes_for_its_own_reader() {
        let delta = Delta {
            set: vec![("V".into(), r"it's a\b".into())],
            unset: vec!["GONE".into()],
        };
        assert_eq!(
            delta.render(DumpFormat::Sh).unwrap(),
            "export V='it'\\''s a\\b'\nunset GONE\n"
        );
        assert_eq!(
            delta.render(DumpFormat::Fish).unwrap(),
            "set -gx V 'it\\'s a\\\\b'\nset -e GONE\n"
        );
        let json: serde_json::Value =
            serde_json::from_str(&delta.render(DumpFormat::Json).unwrap()).unwrap();
        assert_eq!(json["set"]["V"], r"it's a\b");
        assert_eq!(json["unset"][0], "GONE");
    }
}
