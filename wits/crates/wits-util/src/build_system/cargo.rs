//! The cargo backend. cargo drives rustc, so most of the canonical vocabulary
//! maps to environment (build scripts and the `cc` crate read `CC`/`CXX`), and
//! there is no separate configure step. cargo has no `-D`-style definition
//! mechanism, so project/preset `definitions` have no landing spot here and are
//! warned about rather than silently dropped or faked as `--config`.

use std::path::Path;

use crate::project::model::{LogicalConfig, Toolchain};
use crate::project::resolve::ToolchainInjector;

use super::{apply_passthrough, set_universal_env, Backend, BuildMode, EmitContext, Step};

pub struct Cargo;

impl ToolchainInjector for Cargo {
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig) {
        set_universal_env(tc, cfg);
        if let Some(launcher) = &tc.launcher {
            cfg.set_env("RUSTC_WRAPPER", launcher.clone());
        }
        if !tc.link_flags.is_empty() {
            cfg.set_env("RUSTFLAGS", tc.link_flags.join(" "));
        }
        apply_passthrough(tc, cfg);
    }
}

impl Backend for Cargo {
    fn name(&self) -> &str {
        "cargo"
    }

    fn is_configured(&self, _build_dir: &Path) -> bool {
        false // cargo has no separate configure step
    }

    fn steps(&self, ctx: &EmitContext<'_>) -> anyhow::Result<Vec<Step>> {
        if ctx.install || ctx.mode == BuildMode::Uninstall {
            anyhow::bail!("cargo projects do not support install/uninstall");
        }
        if ctx.target.is_some() {
            anyhow::bail!(
                "cargo does not take --target here; pass cargo flags via --extra-build-args"
            );
        }

        let build = ctx.build_dir.display().to_string();
        let mut steps = Vec::new();

        if ctx.mode == BuildMode::Reconfig {
            steps.push(Step::new(
                "Clean",
                "cargo",
                vec!["clean".into(), "--target-dir".into(), build.clone()],
                ctx.source_dir,
            ));
        }
        if ctx.mode == BuildMode::ConfigOnly {
            let mut args = vec!["fetch".into()];
            args.extend(ctx.logical.extra_config_args.iter().cloned());
            steps.push(Step::new(
                "Fetch dependencies",
                "cargo",
                args,
                ctx.source_dir,
            ));
            return Ok(steps);
        }

        let mut args = vec!["build".into(), "--target-dir".into(), build];
        match ctx.build_type {
            "release" => args.push("--release".into()),
            "debug" => {}
            other => {
                args.push("--profile".into());
                args.push(other.to_string());
            }
        }
        // cargo has no analogue of cmake/meson `-D` definitions: `--config` is a
        // *cargo-config* override on a fixed schema (`build.*`, `profile.*`, …),
        // not a general key=value channel, so a project/preset definition can't
        // be mapped onto it. Rather than drop them silently (looks fine, does
        // nothing) or fake a `--config` (looks like it works, doesn't), say so —
        // the real escape hatch for a cargo build is extra args / Cargo.toml.
        if !ctx.logical.definitions.is_empty() {
            let names: Vec<&str> = ctx
                .logical
                .definitions
                .iter()
                .map(|(k, _)| k.as_str())
                .collect();
            log::warn!(
                "cargo has no build-definition mechanism; ignoring {} definition(s) ({}). \
                 Use --extra-build-args (e.g. --features …) or [profile]/[features] in Cargo.toml.",
                names.len(),
                names.join(", ")
            );
        }
        args.extend(ctx.logical.extra_build_args.iter().cloned());
        steps.push(Step::new("Build", "cargo", args, ctx.source_dir));
        Ok(steps)
    }
}
