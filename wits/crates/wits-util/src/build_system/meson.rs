//! The meson backend. The canonical toolchain vocabulary is aligned with meson's
//! native file, so translation here is close to the identity: mostly environment.

use std::path::Path;

use crate::project::model::{LogicalConfig, Toolchain};
use crate::project::resolve::ToolchainInjector;

use super::{
    apply_passthrough, meson_definition, set_universal_env, Backend, BuildMode, EmitContext, Step,
};

pub struct Meson;

impl ToolchainInjector for Meson {
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig) {
        set_universal_env(tc, cfg);

        // A launcher wraps the compiler by prefixing it (`ccache clang`).
        if let Some(launcher) = &tc.launcher {
            if let Some(cc) = &tc.cc {
                cfg.set_env("CC", format!("{launcher} {cc}"));
            }
            if let Some(cxx) = &tc.cxx {
                cfg.set_env("CXX", format!("{launcher} {cxx}"));
            }
        }
        if let Some(linker) = &tc.linker {
            cfg.set_env("CC_LD", linker.clone());
            cfg.set_env("CXX_LD", linker.clone());
        }

        apply_passthrough(tc, cfg);
    }
}

impl Backend for Meson {
    fn name(&self) -> &str {
        "meson"
    }

    fn is_configured(&self, build_dir: &Path) -> bool {
        build_dir
            .join("meson-private")
            .join("coredata.dat")
            .exists()
    }

    fn steps(&self, ctx: &EmitContext<'_>) -> anyhow::Result<Vec<Step>> {
        let build = ctx.build_dir.display().to_string();
        let source = ctx.source_dir.display().to_string();
        let configured = self.is_configured(ctx.build_dir);

        if ctx.mode == BuildMode::Uninstall {
            return Ok(vec![Step::new(
                "Uninstall (ninja)",
                "ninja",
                vec!["-C".into(), build, "uninstall".into()],
                ctx.source_dir,
            )]);
        }

        let mut steps = Vec::new();
        let should_configure = matches!(ctx.mode, BuildMode::ConfigOnly | BuildMode::Reconfig)
            || (ctx.mode == BuildMode::Auto && !configured);
        let should_build = matches!(ctx.mode, BuildMode::Auto | BuildMode::BuildOnly);

        if ctx.mode == BuildMode::BuildOnly && !configured {
            anyhow::bail!("build directory is not configured; configure first or use auto mode");
        }

        if should_configure {
            let mut args = vec!["setup".into(), build.clone(), source.clone()];
            if ctx.mode == BuildMode::Reconfig && ctx.build_dir.exists() {
                args.push("--wipe".into());
            }
            args.push(format!("--buildtype={}", ctx.build_type));
            if let Some(install) = ctx.install_dir {
                args.push(format!("--prefix={}", install.display()));
            }
            for (key, value) in &ctx.logical.definitions {
                args.push("-D".into());
                args.push(meson_definition(key, value));
            }
            args.extend(ctx.logical.extra_config_args.iter().cloned());
            steps.push(Step::new("Configure", "meson", args, ctx.source_dir));
        }

        if should_build {
            let mut args = vec!["compile".into(), "-C".into(), build.clone()];
            if let Some(target) = ctx.target {
                args.push(target.to_string());
            }
            args.extend(ctx.logical.extra_build_args.iter().cloned());
            steps.push(Step::new("Build", "meson", args, ctx.source_dir));
        }

        if ctx.install {
            let mut args = vec!["install".into(), "-C".into(), build];
            args.extend(ctx.logical.extra_install_args.iter().cloned());
            steps.push(Step::new("Install", "meson", args, ctx.source_dir));
        }

        Ok(steps)
    }
}
