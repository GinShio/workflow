//! The cmake backend.

use std::path::Path;

use crate::template::Value;

use crate::project::model::{LogicalConfig, Toolchain};
use crate::project::resolve::ToolchainInjector;

use super::{apply_passthrough, cmake_definition, Backend, BuildMode, EmitContext, Step};

pub struct Cmake;

impl ToolchainInjector for Cmake {
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig) {
        // cmake is configured entirely through `-D`; it does not need (and can be
        // confused by) `CC`/`CXX`/`AR`/… in the environment, so no universal env
        // is set here — only native definitions and the verbatim pass-through.
        let def = |cfg: &mut LogicalConfig, key: &str, val: &Option<String>| {
            if let Some(v) = val {
                cfg.set_definition(key, Value::Str(v.clone()));
            }
        };
        def(cfg, "CMAKE_C_COMPILER", &tc.cc);
        def(cfg, "CMAKE_CXX_COMPILER", &tc.cxx);
        def(cfg, "CMAKE_AR", &tc.ar);
        def(cfg, "CMAKE_RANLIB", &tc.ranlib);
        def(cfg, "CMAKE_NM", &tc.nm);
        def(cfg, "CMAKE_STRIP", &tc.strip);

        if let Some(launcher) = &tc.launcher {
            cfg.set_definition("CMAKE_C_COMPILER_LAUNCHER", Value::Str(launcher.clone()));
            cfg.set_definition("CMAKE_CXX_COMPILER_LAUNCHER", Value::Str(launcher.clone()));
        }
        if !tc.c_flags.is_empty() {
            cfg.set_definition("CMAKE_C_FLAGS", Value::Str(tc.c_flags.join(" ")));
        }
        if !tc.cxx_flags.is_empty() {
            cfg.set_definition("CMAKE_CXX_FLAGS", Value::Str(tc.cxx_flags.join(" ")));
        }
        // A linker choice reaches cmake as a link flag; `-fuse-ld` is what both
        // gcc and clang understand, and it composes with any declared link_flags.
        let mut link_flags = tc.link_flags.clone();
        if let Some(linker) = &tc.linker {
            link_flags.push(format!("-fuse-ld={linker}"));
        }
        if !link_flags.is_empty() {
            let joined = link_flags.join(" ");
            for key in ["CMAKE_EXE_LINKER_FLAGS", "CMAKE_SHARED_LINKER_FLAGS"] {
                cfg.set_definition(key, Value::Str(joined.clone()));
            }
        }

        // A compile-commands database is nearly always wanted (editors, clangd);
        // it is a default, so a preset/CLI override still wins.
        if !cfg.has_definition("CMAKE_EXPORT_COMPILE_COMMANDS") {
            cfg.set_definition("CMAKE_EXPORT_COMPILE_COMMANDS", Value::Bool(true));
        }

        apply_passthrough(tc, cfg);
    }
}

impl Backend for Cmake {
    fn name(&self) -> &str {
        "cmake"
    }

    fn is_configured(&self, build_dir: &Path) -> bool {
        build_dir.join("CMakeCache.txt").exists()
    }

    fn steps(&self, ctx: &EmitContext<'_>) -> anyhow::Result<Vec<Step>> {
        let build = ctx.build_dir.display().to_string();
        let source = ctx.source_dir.display().to_string();
        let configured = self.is_configured(ctx.build_dir);
        let multi_config = is_multi_config(ctx.generator);

        if ctx.mode == BuildMode::Uninstall {
            let manifest = ctx.build_dir.join("install_manifest.txt");
            return Ok(vec![Step::new(
                "Uninstall (from install_manifest.txt)",
                "sh",
                vec![
                    "-c".into(),
                    format!(
                        "xargs rm -f -v < {}",
                        shell_quote(&manifest.display().to_string())
                    ),
                ],
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
        if ctx.mode == BuildMode::Reconfig && ctx.build_dir.exists() {
            steps.push(Step::new(
                "Remove build directory",
                "rm",
                vec!["-rf".into(), build.clone()],
                ctx.source_dir,
            ));
        }

        if should_configure {
            let mut args = vec!["-S".into(), source.clone(), "-B".into(), build.clone()];
            if let Some(gen) = ctx.generator {
                args.push("-G".into());
                args.push(gen.to_string());
            }
            // A multi-config generator picks the type at build time (--config),
            // so it must not be pinned at configure.
            if !multi_config {
                args.push("-D".into());
                args.push(format!(
                    "CMAKE_BUILD_TYPE:STRING={}",
                    build_type_cmake(ctx.build_type)
                ));
            }
            for (key, value) in &ctx.logical.definitions {
                args.push("-D".into());
                args.push(cmake_definition(key, value));
            }
            if let Some(install) = ctx.install_dir {
                args.push("-D".into());
                args.push(format!("CMAKE_INSTALL_PREFIX:PATH={}", install.display()));
            }
            args.extend(ctx.logical.extra_config_args.iter().cloned());
            steps.push(Step::new("Configure", "cmake", args, ctx.source_dir));
        }

        if should_build {
            let mut args = vec!["--build".into(), build.clone()];
            if multi_config {
                args.push("--config".into());
                args.push(build_type_cmake(ctx.build_type).to_string());
            }
            if let Some(target) = ctx.target {
                args.push("--target".into());
                args.push(target.to_string());
            }
            args.extend(ctx.logical.extra_build_args.iter().cloned());
            steps.push(Step::new("Build", "cmake", args, ctx.source_dir));
        }

        if ctx.install {
            let mut args = vec!["--install".into(), build.clone()];
            if multi_config {
                args.push("--config".into());
                args.push(build_type_cmake(ctx.build_type).to_string());
            }
            args.extend(ctx.logical.extra_install_args.iter().cloned());
            steps.push(Step::new("Install", "cmake", args, ctx.source_dir));
        }

        Ok(steps)
    }
}

/// Multi-config generators (Ninja Multi-Config, Visual Studio, Xcode) select the
/// build type per build with `--config`, not once at configure.
fn is_multi_config(generator: Option<&str>) -> bool {
    generator.is_some_and(|g| {
        let g = g.to_lowercase();
        g.contains("multi-config") || g.contains("visual studio") || g.contains("xcode")
    })
}

/// Map a (lowercase, meson-aligned) build type to cmake's spelling.
fn build_type_cmake(bt: &str) -> &str {
    match bt {
        "debug" => "Debug",
        "release" => "Release",
        "debugoptimized" => "RelWithDebInfo",
        "minsize" | "minsizerel" => "MinSizeRel",
        other => other,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_toolchain_to_definitions_without_polluting_env() {
        let tc = Toolchain {
            name: "clang".into(),
            cc: Some("clang".into()),
            cxx: Some("clang++".into()),
            ..Default::default()
        };
        let mut cfg = LogicalConfig::default();
        Cmake.apply_toolchain(&tc, &mut cfg);

        // cmake derives the compiler definition from the toolchain's cc…
        assert!(cfg.definitions.iter().any(|(k, _)| k == "CMAKE_C_COMPILER"));
        // …and deliberately does *not* set CC/CXX/… in the environment.
        assert_eq!(cfg.env_entry("CC"), None);
    }
}
