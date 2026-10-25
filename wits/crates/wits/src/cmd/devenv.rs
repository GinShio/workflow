//! `wits devenv` — the CLI shell over [`wits_actions::compose`] and
//! [`wits_actions::run`].
//!
//! Composing the environment (the backend's base, the declared operations, the
//! marker) and replacing the process with the program are the action's. What
//! stays here is what only a command line needs: parsing, the `--dump` formats,
//! and the text form of the action's overlay, which is what those formats print.

use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use wits_actions::{compose, DevenvRequest, Env, Overlay};
use wits_project::{resolve_target, Workspace};

use crate::cmd::identity::IdentityArgs;

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

pub fn run(args: &DevenvArgs) -> Result<()> {
    let ws = Workspace::load()?;
    let project = resolve_target(&ws, args.target.as_deref())?;
    let caller: Env = std::env::vars_os().collect();
    let request = DevenvRequest {
        identity: args.identity.to_identity(&ws, project)?,
        capture_program: std::env::current_exe().context("locating the running wits binary")?,
    };
    let overlay = compose(&ws, project, &request, &caller)?;
    match args.dump {
        Some(format) => {
            print!("{}", Delta::from_overlay(&overlay)?.render(format)?);
            Ok(())
        }
        None => wits_actions::run(&overlay, &args.command),
    }
}

/// The overlay spelled as text, which is what `--dump` prints. The conversion
/// is where a non-UTF-8 entry stops: a dump is text, while `run` is the path
/// that carries the bytes.
#[derive(Debug, Default, PartialEq)]
struct Delta {
    set: Vec<(String, String)>,
    unset: Vec<String>,
}

impl Delta {
    fn from_overlay(overlay: &Overlay) -> Result<Delta> {
        let name = |s: &OsString| {
            s.to_str().map(str::to_owned).with_context(|| {
                format!("devenv variable {} is not valid UTF-8", s.to_string_lossy())
            })
        };
        let mut delta = Delta::default();
        for (key, value) in &overlay.set {
            let key = name(key)?;
            let value = value.to_str().map(str::to_owned).with_context(|| {
                format!("devenv variable {key} has a value that is not valid UTF-8")
            })?;
            delta.set.push((key, value));
        }
        for key in &overlay.unset {
            delta.unset.push(name(key)?);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn the_text_view_names_only_what_the_overlay_changed() {
        let overlay = Overlay {
            set: vec![(os("ADD"), os("a")), (os("CHANGE"), os("new"))],
            unset: vec![os("DROP")],
        };
        assert_eq!(
            Delta::from_overlay(&overlay).unwrap(),
            Delta {
                set: vec![("ADD".into(), "a".into()), ("CHANGE".into(), "new".into())],
                unset: vec!["DROP".into()],
            }
        );
    }

    /// A dump is text, so a non-UTF-8 entry is refused there — `run` is the
    /// path that carries the bytes.
    #[test]
    fn the_text_view_refuses_a_non_utf8_entry() {
        use std::os::unix::ffi::OsStringExt;
        let overlay = Overlay {
            set: vec![(os("K"), OsString::from_vec(b"v\xff".to_vec()))],
            unset: vec![],
        };
        assert!(Delta::from_overlay(&overlay).is_err());
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
