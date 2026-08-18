//! Running an external command, with dry-run baked in.
//!
//! Workflow tools spend most of their time shelling out, and the thing that
//! makes that annoying to get right is dry-run: you want `-n` to suppress
//! anything that *changes* the world, yet the read-only queries that decide
//! what to do next must still run, or every dry-run degenerates into a no-op
//! that tells you nothing. That tension is the whole reason this wrapper
//! exists — [`force_run`](Command::force_run) is how a caller marks a query as
//! safe to execute regardless.

use std::path::PathBuf;
use std::process::Stdio;
use thiserror::Error;

use crate::log as wits_log;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to spawn '{program}': {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("command output is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    /// Captured stderr. Git puts its actual error text here, so keeping it lets
    /// callers report *why* a push or fetch failed instead of a bare code.
    pub stderr: String,
}

impl CommandResult {
    #[inline]
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }

    /// stdout without its trailing newline — the common case for single-line
    /// git output like a hash or a config value.
    #[inline]
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim_end_matches(['\r', '\n'])
    }
}

pub struct Command {
    program: String,
    argv: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    env_remove: Vec<String>,
    force_run: bool,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        let program = program.into();
        Self {
            argv: vec![program.clone()],
            program,
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            force_run: false,
        }
    }

    pub fn args<I, S>(&mut self, iter: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv.extend(iter.into_iter().map(Into::into));
        self
    }

    pub fn current_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.cwd = Some(path.into());
        self
    }

    /// Set an environment variable for the child. Some programs only expose a
    /// behaviour through the environment, with no equivalent flag to pass on the
    /// command line, so configuring them at all requires this.
    pub fn env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Remove an environment variable from the child, so an inherited value can
    /// never leak in. The tool pins location by `current_dir`/flags, so a
    /// context-pinning var the caller's shell exported (e.g. `GIT_DIR`) must be
    /// scrubbed or it would silently override that.
    pub fn env_remove(&mut self, key: impl Into<String>) -> &mut Self {
        self.env_remove.push(key.into());
        self
    }

    /// Mark this command as a read-only query that must run even under dry-run.
    pub fn force_run(&mut self) -> &mut Self {
        self.force_run = true;
        self
    }

    fn format_cmd(&self) -> String {
        let quote = |s: &str| {
            if s.contains(' ') {
                format!("\"{s}\"")
            } else {
                s.to_owned()
            }
        };
        // Env assignments lead, shell-style (`KEY=val program args …`), so a
        // verbose or dry-run line is a command you could paste — and so the
        // environment a child actually sees is visible when debugging (some
        // behaviour is only reachable through the environment, with no flag).
        let mut parts: Vec<String> = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={}", quote(value)))
            .collect();
        parts.extend(self.argv.iter().map(|a| quote(a)));
        let rendered = parts.join(" ");
        match &self.cwd {
            Some(cwd) => format!("{rendered} (cwd={})", cwd.display()),
            None => rendered,
        }
    }

    fn build_std_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.argv[1..]);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        for key in &self.env_remove {
            cmd.env_remove(key);
        }
        cmd
    }

    /// Run and capture stdout. Under dry-run an unforced command is printed
    /// rather than executed, and a synthetic success is returned so callers can
    /// proceed as if nothing failed.
    pub fn exec(&self) -> Result<CommandResult, ProcessError> {
        if wits_log::is_dry_run() && !self.force_run {
            wits_log::dry_run(&self.format_cmd());
            return Ok(CommandResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        if wits_log::is_verbose() {
            log::debug!("running: {}", self.format_cmd());
        }

        let output = self
            .build_std_command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| ProcessError::Spawn {
                program: self.program.clone(),
                source,
            })?;

        Ok(CommandResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run while inheriting the parent's stdio, returning only the exit code.
    ///
    /// Some commands *are* an interaction: they hand control to an editor or
    /// pager and must own the terminal to function. Capturing their stdio (as
    /// [`exec`](Command::exec) does) would starve that interaction of the
    /// terminal it needs, so the two run paths stay separate rather than being
    /// merged behind a flag. Dry-run is still honoured: a mutating interactive
    /// command is described, not performed.
    pub fn status(&self) -> Result<i32, ProcessError> {
        if wits_log::is_dry_run() && !self.force_run {
            wits_log::dry_run(&self.format_cmd());
            return Ok(0);
        }
        if wits_log::is_verbose() {
            log::debug!("running: {}", self.format_cmd());
        }

        let status = self
            .build_std_command()
            .status()
            .map_err(|source| ProcessError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        Ok(status.code().unwrap_or(-1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_cmd_leads_with_env_and_quotes_spaces() {
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "a message"])
            .env("GIT_AUTHOR_NAME", "A B");
        assert_eq!(
            cmd.format_cmd(),
            "GIT_AUTHOR_NAME=\"A B\" git commit -m \"a message\""
        );
    }

    #[test]
    fn captures_stdout() {
        let _guard = crate::log::test_flag_guard();
        // force_run so a parallel test toggling the global dry-run flag can't
        // turn this into a skipped, synthetic-success no-op.
        let result = Command::new("echo")
            .args(["hello"])
            .force_run()
            .exec()
            .unwrap();
        assert!(result.is_success());
        assert_eq!(result.stdout_trimmed(), "hello");
    }

    #[test]
    fn dry_run_skips_unforced_commands() {
        let _guard = crate::log::test_flag_guard();
        crate::log::init(false, true);
        // `false` would exit non-zero, but dry-run never spawns it.
        let result = Command::new("false").exec().unwrap();
        assert!(result.is_success());
        crate::log::init(false, false);
    }

    #[test]
    fn force_run_executes_during_dry_run() {
        let _guard = crate::log::test_flag_guard();
        crate::log::init(false, true);
        let result = Command::new("echo")
            .args(["forced"])
            .force_run()
            .exec()
            .unwrap();
        assert_eq!(result.stdout_trimmed(), "forced");
        crate::log::init(false, false);
    }
}
