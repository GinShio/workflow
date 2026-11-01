//! The runner shapes, and what each owns.
//!
//! A kind is one runner invocation shape, not one API: `deqp` is dEQP Vulkan
//! through `deqp-runner`, `piglit` is Piglit GL through `piglit-runner`. The
//! kind owns the binary name, the argv, the installed-layout check and the
//! driver guard. The core pipeline never branches on the kind.
//!
//! The evidence set and the exit codes are *not* the kind's: every runner in
//! the `deqp-runner` project writes `results.csv`, `failures.csv`,
//! `failures-all.csv` and `expectations.json`, and exits 0/1/2 the same way.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use wits_actions::Env;

use crate::config::{DeqpDecl, PiglitDecl, Suite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deqp,
    Piglit,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Deqp => "deqp",
            Kind::Piglit => "piglit",
        }
    }

    /// The binary the runner project installs for this shape. Kept on `Kind`
    /// because `install` groups runner projects by kind before any suite shape
    /// is involved.
    pub fn runner_bin(self) -> &'static str {
        match self {
            Kind::Deqp => "deqp-runner",
            Kind::Piglit => "piglit-runner",
        }
    }
}

/// The resolved per-kind declaration. The variant *is* the kind's data, so a
/// suite cannot claim one kind while carrying another's table: the invariant
/// the loader enforces is the type's shape here.
#[derive(Debug, Clone)]
pub enum SuiteShape {
    Deqp(DeqpDecl),
    Piglit(PiglitDecl),
}

impl SuiteShape {
    pub fn kind(&self) -> Kind {
        match self {
            SuiteShape::Deqp(_) => Kind::Deqp,
            SuiteShape::Piglit(_) => Kind::Piglit,
        }
    }

    pub fn runner_bin(&self) -> &'static str {
        self.kind().runner_bin()
    }

    /// The complete command line for one run. Every path is absolute; the args
    /// are a list, never a shell string, so nothing is word-split or quoted.
    pub fn argv(&self, ctx: &RunContext<'_>) -> Result<Vec<String>> {
        let runner = path_text(&ctx.runner_root.join(self.runner_bin()), "runner binary")?;
        let mut argv = vec![runner, "run".to_owned()];
        argv.push("--output".to_owned());
        argv.push(path_text(ctx.run_dir, "run directory")?);

        match self {
            SuiteShape::Deqp(_) => {
                for entry in &ctx.suite.case_list {
                    argv.push("--caselist".to_owned());
                    argv.push(path_text(
                        &ctx.suite_root.join(entry),
                        &format!("caselist {entry:?}"),
                    )?);
                }
            }
            SuiteShape::Piglit(piglit) => {
                argv.push("--piglit-folder".to_owned());
                argv.push(path_text(
                    &ctx.suite_root.join(&piglit.folder),
                    "piglit folder",
                )?);
                argv.push("--profile".to_owned());
                argv.push(ctx.suite.case_list[0].clone());
            }
        }

        if let Some(baseline) = ctx.baseline {
            argv.push("--baseline".to_owned());
            argv.push(path_text(baseline, "baseline")?);
        }
        argv.push("--jobs".to_owned());
        argv.push(ctx.jobs.to_string());
        argv.push("--timeout".to_owned());
        argv.push(ctx.suite.timeout.to_string());

        match self {
            SuiteShape::Deqp(deqp) => {
                if let Some(n) = deqp.tests_per_group {
                    argv.push("--tests-per-group".to_owned());
                    argv.push(n.to_string());
                }
                argv.push("--deqp".to_owned());
                argv.push(path_text(
                    &ctx.suite_root.join(&deqp.exe),
                    "dEQP executable",
                )?);
                argv.push("--".to_owned());
                argv.extend(deqp.args.iter().cloned());
                if let Some(waiver) = &deqp.waiver {
                    argv.push(format!(
                        "--deqp-waiver-file={}",
                        path_text(&ctx.suite_root.join(waiver), "waiver")?
                    ));
                }
            }
            SuiteShape::Piglit(_) => {}
        }

        Ok(argv)
    }

    /// The argv that participates in identity: the run directory replaced by a
    /// marker and the baseline omitted, so the first run and a rerun of the
    /// same build hash the same command line.
    pub fn identity_argv(&self, ctx: &RunContext<'_>) -> Result<Vec<String>> {
        let ctx = RunContext {
            run_dir: Path::new(crate::content::RUN_DIR_MARKER),
            baseline: None,
            ..*ctx
        };
        self.argv(&ctx)
    }

    /// Verify the installed layout the kind needs. The caller has already
    /// checked the suite root and the install stamp; this is the kind's own
    /// asset set.
    pub fn check_assets(&self, suite: &Suite, suite_root: &Path) -> Result<()> {
        match self {
            SuiteShape::Deqp(deqp) => check_deqp_assets(suite, deqp, suite_root),
            SuiteShape::Piglit(piglit) => check_piglit_assets(suite, piglit, suite_root),
        }
    }

    /// The API-specific guard. dEQP Vulkan must load exactly one selected ICD;
    /// Piglit GL needs no ICD and may select zink through its own environment.
    pub fn check_driver(&self, env: &Env) -> Result<()> {
        match self {
            SuiteShape::Deqp(_) => crate::icd::guard(env).map(|_| ()),
            SuiteShape::Piglit(_) => Ok(()),
        }
    }
}

/// The inputs to one kind's argv. Paths are already absolute; the config
/// spells suite-relative paths, and the caller joins them to the suite root.
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    pub run_dir: &'a Path,
    pub baseline: Option<&'a Path>,
    pub suite_root: &'a Path,
    pub runner_root: &'a Path,
    pub suite: &'a Suite,
    pub jobs: u32,
}

fn check_deqp_assets(suite: &Suite, deqp: &DeqpDecl, suite_root: &Path) -> Result<()> {
    let exe = suite_root.join(&deqp.exe);
    if !exe.is_file() {
        bail!(
            "suite '{}': dEQP executable {} is missing; install with `wits gputest install {}`",
            suite.name,
            exe.display(),
            suite.name
        );
    }
    if !crate::util::is_executable(&exe)? {
        bail!(
            "suite '{}': dEQP executable {} is not executable",
            suite.name,
            exe.display()
        );
    }
    for entry in &suite.case_list {
        let path = suite_root.join(entry);
        let meta = std::fs::metadata(&path).with_context(|| {
            format!(
                "suite '{}': caselist {} is missing; install with `wits gputest install {}`",
                suite.name,
                path.display(),
                suite.name
            )
        })?;
        if !meta.is_file() {
            bail!(
                "suite '{}': caselist {} is not a regular file",
                suite.name,
                path.display()
            );
        }
        if meta.len() == 0 {
            bail!(
                "suite '{}': caselist {} is empty; a run would test nothing",
                suite.name,
                path.display()
            );
        }
    }
    if let Some(waiver) = &deqp.waiver {
        let path = suite_root.join(waiver);
        let meta = std::fs::metadata(&path).with_context(|| {
            format!(
                "suite '{}': waiver {} is missing; install with `wits gputest install {}`",
                suite.name,
                path.display(),
                suite.name
            )
        })?;
        if !meta.is_file() || meta.len() == 0 {
            bail!(
                "suite '{}': waiver {} is not a non-empty file",
                suite.name,
                path.display()
            );
        }
    }
    let data = suite_root.join("vulkan");
    if !data.is_dir() {
        bail!(
            "suite '{}': dEQP data directory {} is missing; install with `wits gputest install {}`",
            suite.name,
            data.display(),
            suite.name
        );
    }
    Ok(())
}

fn check_piglit_assets(suite: &Suite, piglit: &PiglitDecl, suite_root: &Path) -> Result<()> {
    let folder = suite_root.join(&piglit.folder);
    if !folder.is_dir() {
        bail!(
            "suite '{}': Piglit folder {} is missing; install with `wits gputest install {}`",
            suite.name,
            folder.display(),
            suite.name
        );
    }

    let tests = folder.join("tests");
    if !tests.is_dir() {
        bail!(
            "suite '{}': Piglit profile directory {} is missing; install with `wits gputest install {}`",
            suite.name,
            tests.display(),
            suite.name
        );
    }
    let profile = &suite.case_list[0];
    let found = profile_candidates(profile)
        .iter()
        .map(|candidate| tests.join(candidate))
        .find(|path| path.is_file());
    if found.is_none() {
        bail!(
            "suite '{}': Piglit profile '{}' does not exist under {} (or any of its fallback spellings); install with `wits gputest install {}`",
            suite.name,
            profile,
            tests.display(),
            suite.name
        );
    }

    for (key, dir) in [("bin", "test executables"), ("lib", "shared libraries")] {
        let path = folder.join(key);
        if !path.is_dir() {
            bail!(
                "suite '{}': Piglit {} directory {} is missing; install with `wits gputest install {}`",
                suite.name,
                dir,
                path.display(),
                suite.name
            );
        }
        let mut entries =
            std::fs::read_dir(&path).with_context(|| format!("reading {}", path.display()))?;
        if entries.next().is_none() {
            bail!(
                "suite '{}': Piglit {} directory {} is empty; install with `wits gputest install {}`",
                suite.name,
                dir,
                path.display(),
                suite.name
            );
        }
    }
    Ok(())
}

/// The profile spellings `piglit-runner`'s `read_profile_file` tries, in its
/// order. The runner has no process-isolation flag in this config, so it uses
/// the no-isolation forms first.
fn profile_candidates(profile: &str) -> Vec<PathBuf> {
    let base = Path::new(profile);
    [
        "no_isolation.meta.xml",
        "meta.xml",
        "no_isolation.xml.gz",
        "no_isolation.xml",
        "xml.gz",
        "xml",
    ]
    .into_iter()
    .map(|ext| base.with_extension(ext))
    .collect()
}

fn path_text(path: &Path, what: &str) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{what} {} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeqpDecl, Jobs, PiglitDecl};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn suite(kind: Kind) -> Suite {
        let shape = match kind {
            Kind::Deqp => SuiteShape::Deqp(DeqpDecl {
                exe: "deqp-vk".to_owned(),
                waiver: Some("waivers/vk.xml".to_owned()),
                args: vec!["--deqp-log-images=disable".to_owned()],
                tests_per_group: Some(4096),
            }),
            Kind::Piglit => SuiteShape::Piglit(PiglitDecl {
                folder: "lib64/piglit".to_owned(),
            }),
        };
        Suite {
            name: "s".to_owned(),
            shape,
            project: "test".to_owned(),
            runner: "runner".to_owned(),
            case_list: match kind {
                Kind::Deqp => vec!["mustpass/vk.txt".to_owned()],
                Kind::Piglit => vec!["quick".to_owned()],
            },
            jobs: Jobs::Count(4),
            timeout: 300,
            env: Vec::new(),
        }
    }

    fn context<'a>(
        suite: &'a Suite,
        run_dir: &'a Path,
        baseline: Option<&'a Path>,
        suite_root: &'a Path,
        runner_root: &'a Path,
    ) -> RunContext<'a> {
        RunContext {
            run_dir,
            baseline,
            suite_root,
            runner_root,
            suite,
            jobs: 8,
        }
    }

    #[test]
    fn deqp_argv_is_absolute_and_ordered() {
        let suite = suite(Kind::Deqp);
        let root = Path::new("/suite");
        let runner = Path::new("/runner");
        let run = Path::new("/run/1");
        let baseline = Path::new("/run/1/baseline.csv");
        let argv = suite
            .shape
            .argv(&context(&suite, run, Some(baseline), root, runner))
            .unwrap();
        assert_eq!(
            argv,
            [
                "/runner/deqp-runner",
                "run",
                "--output",
                "/run/1",
                "--caselist",
                "/suite/mustpass/vk.txt",
                "--baseline",
                "/run/1/baseline.csv",
                "--jobs",
                "8",
                "--timeout",
                "300",
                "--tests-per-group",
                "4096",
                "--deqp",
                "/suite/deqp-vk",
                "--",
                "--deqp-log-images=disable",
                "--deqp-waiver-file=/suite/waivers/vk.xml",
            ]
        );
    }

    #[test]
    fn the_identity_argv_omits_the_baseline_and_marks_the_run_dir() {
        let suite = suite(Kind::Deqp);
        let root = Path::new("/suite");
        let runner = Path::new("/runner");
        let ctx = context(
            &suite,
            Path::new("/run/1"),
            Some(Path::new("/b")),
            root,
            runner,
        );
        let first = suite.shape.identity_argv(&ctx).unwrap();
        assert!(!first.iter().any(|a| a == "--baseline"));
        assert!(first.iter().any(|a| a == "{run}"));
        let ctx2 = context(&suite, Path::new("/other"), None, root, runner);
        assert_eq!(first, suite.shape.identity_argv(&ctx2).unwrap());
    }

    #[test]
    fn piglit_argv_uses_the_folder_and_one_profile() {
        let suite = suite(Kind::Piglit);
        let root = Path::new("/suite");
        let runner = Path::new("/runner");
        let argv = suite
            .shape
            .argv(&context(&suite, Path::new("/run"), None, root, runner))
            .unwrap();
        assert_eq!(
            argv,
            [
                "/runner/piglit-runner",
                "run",
                "--output",
                "/run",
                "--piglit-folder",
                "/suite/lib64/piglit",
                "--profile",
                "quick",
                "--jobs",
                "8",
                "--timeout",
                "300",
            ]
        );
    }

    #[test]
    fn check_assets_accepts_the_deqp_layout_and_names_the_install_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = suite(Kind::Deqp);
        fs::create_dir_all(root.join("vulkan")).unwrap();
        fs::create_dir_all(root.join("waivers")).unwrap();
        fs::create_dir_all(root.join("mustpass")).unwrap();
        fs::write(root.join("mustpass/vk.txt"), "dEQP-VK.info\n").unwrap();
        fs::write(root.join("waivers/vk.xml"), "<waivers/>").unwrap();
        fs::write(root.join("deqp-vk"), "").unwrap();
        fs::set_permissions(root.join("deqp-vk"), fs::Permissions::from_mode(0o755)).unwrap();
        suite.shape.check_assets(&suite, root).unwrap();

        fs::remove_file(root.join("deqp-vk")).unwrap();
        let err = suite.shape.check_assets(&suite, root).unwrap_err();
        assert!(format!("{err:#}").contains("wits gputest install s"));
    }

    #[test]
    fn check_assets_refuses_an_empty_caselist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = suite(Kind::Deqp);
        fs::create_dir_all(root.join("vulkan")).unwrap();
        fs::create_dir_all(root.join("waivers")).unwrap();
        fs::create_dir_all(root.join("mustpass")).unwrap();
        fs::write(root.join("mustpass/vk.txt"), "").unwrap();
        fs::write(root.join("waivers/vk.xml"), "<waivers/>").unwrap();
        fs::write(root.join("deqp-vk"), "").unwrap();
        fs::set_permissions(root.join("deqp-vk"), fs::Permissions::from_mode(0o755)).unwrap();
        let err = suite.shape.check_assets(&suite, root).unwrap_err();
        assert!(format!("{err:#}").contains("empty"), "{err:#}");
    }

    #[test]
    fn check_assets_accepts_the_cmake_piglit_layout() {
        let dir = tempfile::tempdir().unwrap();
        let suite = suite(Kind::Piglit);
        let folder = dir.path().join("lib64/piglit");
        fs::create_dir_all(folder.join("tests")).unwrap();
        fs::create_dir_all(folder.join("bin")).unwrap();
        fs::create_dir_all(folder.join("lib")).unwrap();
        fs::write(folder.join("tests/quick.meta.xml"), "<profile/>").unwrap();
        fs::write(folder.join("bin/shader_runner"), "").unwrap();
        fs::write(folder.join("lib/libpiglit.so"), "").unwrap();
        suite.shape.check_assets(&suite, dir.path()).unwrap();

        fs::remove_file(folder.join("tests/quick.meta.xml")).unwrap();
        let err = suite.shape.check_assets(&suite, dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("profile 'quick'"), "{err:#}");
    }
}
