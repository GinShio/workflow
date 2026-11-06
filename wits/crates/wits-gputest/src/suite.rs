//! A suite — one runner invocation — and what running it means for each runner
//! shape: the binary, the argv, the installed layout the suite needs, and the
//! driver it accepts. The pipeline never branches on the kind itself.
//!
//! Both shapes are binaries from the deqp-runner project and leave the same
//! evidence behind; `report.rs` reads it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::{self, Deserializer};
use serde::Deserialize;

use wits_actions::Env;
use wits_project::DevenvEntry;

/// The marker the run directory is replaced with in the identity argv.
const RUN_DIR_MARKER: &str = "{run}";

#[derive(Debug, Clone)]
pub struct Suite {
    pub name: String,
    /// The project whose `install_dir` is the suite root.
    pub project: String,
    /// The project whose `install_dir` holds the runner binaries.
    pub runner: String,
    pub case_list: Vec<String>,
    pub jobs: Jobs,
    /// Seconds one test may run before the runner records a `Timeout`.
    pub timeout: u64,
    pub env: Vec<DevenvEntry>,
    pub kind: Kind,
}

/// The runner shape, carrying the declaration only that shape has. The config
/// names it by its table: `[suite.<name>.deqp]` or `[suite.<name>.piglit]`.
#[derive(Debug, Clone)]
pub enum Kind {
    Deqp(DeqpDecl),
    Piglit(PiglitDecl),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeqpDecl {
    pub exe: String,
    pub waiver: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub tests_per_group: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiglitDecl {
    /// The directory holding `tests/`, `bin/` and `lib/`.
    pub folder: String,
}

/// Parallel test processes. `"cores"` means physical cores, the right number
/// for CPU-bound shader compilation; a number is used verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Jobs {
    #[default]
    Cores,
    Count(u32),
}

impl Jobs {
    pub fn number(self) -> u32 {
        match self {
            Jobs::Cores => crate::cores::physical_cores(),
            Jobs::Count(n) => n,
        }
    }
}

impl<'de> Deserialize<'de> for Jobs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Count(u32),
        }
        match Raw::deserialize(d)? {
            Raw::Word(word) if word == "cores" => Ok(Jobs::Cores),
            Raw::Word(word) => Err(de::Error::custom(format!(
                "jobs must be \"cores\" or a positive integer (got {word:?})"
            ))),
            Raw::Count(n) => Ok(Jobs::Count(n)),
        }
    }
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Deqp(_) => "deqp",
            Kind::Piglit(_) => "piglit",
        }
    }

    /// The binary the runner project installs for this shape.
    pub fn runner_bin(&self) -> &'static str {
        match self {
            Kind::Deqp(_) => "deqp-runner",
            Kind::Piglit(_) => "piglit-runner",
        }
    }
}

/// The inputs to one runner command line. Every path is absolute.
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    pub run_dir: &'a Path,
    pub baseline: Option<&'a Path>,
    pub suite_root: &'a Path,
    pub runner_root: &'a Path,
    pub jobs: u32,
}

impl Suite {
    /// The complete command line for one run. The args are a list, never a
    /// shell string, so nothing is word-split or quoted.
    pub fn argv(&self, ctx: &RunContext<'_>) -> Result<Vec<String>> {
        let mut argv = vec![
            path_text(&ctx.runner_root.join(self.kind.runner_bin()))?,
            "run".to_owned(),
            "--output".to_owned(),
            path_text(ctx.run_dir)?,
        ];
        match &self.kind {
            Kind::Deqp(_) => {
                for entry in &self.case_list {
                    argv.push("--caselist".to_owned());
                    argv.push(path_text(&ctx.suite_root.join(entry))?);
                }
            }
            Kind::Piglit(piglit) => {
                argv.push("--piglit-folder".to_owned());
                argv.push(path_text(&ctx.suite_root.join(&piglit.folder))?);
                argv.push("--profile".to_owned());
                argv.push(self.case_list[0].clone());
            }
        }
        if let Some(baseline) = ctx.baseline {
            argv.push("--baseline".to_owned());
            argv.push(path_text(baseline)?);
        }
        argv.push("--jobs".to_owned());
        argv.push(ctx.jobs.to_string());
        argv.push("--timeout".to_owned());
        argv.push(self.timeout.to_string());
        if let Kind::Deqp(deqp) = &self.kind {
            if let Some(n) = deqp.tests_per_group {
                argv.push("--tests-per-group".to_owned());
                argv.push(n.to_string());
            }
            argv.push("--deqp".to_owned());
            argv.push(path_text(&ctx.suite_root.join(&deqp.exe))?);
            argv.push("--".to_owned());
            argv.extend(deqp.args.iter().cloned());
            if let Some(waiver) = &deqp.waiver {
                argv.push(format!(
                    "--deqp-waiver-file={}",
                    path_text(&ctx.suite_root.join(waiver))?
                ));
            }
        }
        Ok(argv)
    }

    /// The argv that identity hashes: the run directory replaced by a marker
    /// and no baseline, so a first run and a rerun of one build hash the same
    /// command line.
    pub fn identity_argv(
        &self,
        suite_root: &Path,
        runner_root: &Path,
        jobs: u32,
    ) -> Result<Vec<String>> {
        self.argv(&RunContext {
            run_dir: Path::new(RUN_DIR_MARKER),
            baseline: None,
            suite_root,
            runner_root,
            jobs,
        })
    }

    /// Verify the installed layout this shape needs under the suite root.
    pub fn check_assets(&self, suite_root: &Path) -> Result<()> {
        match &self.kind {
            Kind::Deqp(deqp) => self.check_deqp_assets(deqp, suite_root),
            Kind::Piglit(piglit) => self.check_piglit_assets(piglit, suite_root),
        }
    }

    /// The API-specific driver guard. dEQP Vulkan must load exactly one
    /// selected ICD; Piglit GL needs no ICD and may select zink through its
    /// own environment.
    pub fn check_driver(&self, env: &Env) -> Result<()> {
        match self.kind {
            Kind::Deqp(_) => crate::icd::guard(env),
            Kind::Piglit(_) => Ok(()),
        }
    }

    fn check_deqp_assets(&self, deqp: &DeqpDecl, root: &Path) -> Result<()> {
        let exe = root.join(&deqp.exe);
        if !exe.is_file() {
            bail!("dEQP executable {} is missing", exe.display());
        }
        if !crate::util::is_executable(&exe)? {
            bail!("dEQP executable {} is not executable", exe.display());
        }
        for entry in &self.case_list {
            non_empty_file(&root.join(entry), "caselist")?;
        }
        if let Some(waiver) = &deqp.waiver {
            non_empty_file(&root.join(waiver), "waiver")?;
        }
        let data = root.join("vulkan");
        if !data.is_dir() {
            bail!("dEQP data directory {} is missing", data.display());
        }
        Ok(())
    }

    fn check_piglit_assets(&self, piglit: &PiglitDecl, root: &Path) -> Result<()> {
        let folder = root.join(&piglit.folder);
        let tests = folder.join("tests");
        if !tests.is_dir() {
            bail!("Piglit profile directory {} is missing", tests.display());
        }
        let profile = &self.case_list[0];
        if !profile_candidates(profile)
            .iter()
            .any(|candidate| tests.join(candidate).is_file())
        {
            bail!(
                "Piglit profile '{profile}' does not exist under {} in any spelling piglit-runner reads",
                tests.display()
            );
        }
        for (dir, holds) in [("bin", "test executables"), ("lib", "shared libraries")] {
            let path = folder.join(dir);
            let mut entries = std::fs::read_dir(&path).with_context(|| {
                format!("Piglit {holds} directory {} is missing", path.display())
            })?;
            if entries.next().is_none() {
                bail!("Piglit {holds} directory {} is empty", path.display());
            }
        }
        Ok(())
    }
}

/// A run would silently test fewer cases with an empty caselist or waiver.
fn non_empty_file(path: &Path, what: &str) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("{what} {} is missing", path.display()))?;
    if !meta.is_file() || meta.len() == 0 {
        bail!("{what} {} is not a non-empty file", path.display());
    }
    Ok(())
}

/// The profile spellings piglit-runner's `read_profile_file` tries, in its
/// order. The runner has no process-isolation flag here, so it reads the
/// no-isolation forms first.
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

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn suite(deqp: bool) -> Suite {
        Suite {
            name: "s".to_owned(),
            project: "test".to_owned(),
            runner: "runner".to_owned(),
            case_list: vec![if deqp { "mustpass/vk.txt" } else { "quick" }.to_owned()],
            jobs: Jobs::Count(4),
            timeout: 300,
            env: Vec::new(),
            kind: if deqp {
                Kind::Deqp(DeqpDecl {
                    exe: "deqp-vk".to_owned(),
                    waiver: Some("waivers/vk.xml".to_owned()),
                    args: vec!["--deqp-log-images=disable".to_owned()],
                    tests_per_group: Some(4096),
                })
            } else {
                Kind::Piglit(PiglitDecl {
                    folder: "lib64/piglit".to_owned(),
                })
            },
        }
    }

    fn context<'a>(run_dir: &'a Path, baseline: Option<&'a Path>) -> RunContext<'a> {
        RunContext {
            run_dir,
            baseline,
            suite_root: Path::new("/suite"),
            runner_root: Path::new("/runner"),
            jobs: 8,
        }
    }

    #[test]
    fn deqp_argv_is_absolute_and_ordered() {
        let argv = suite(true)
            .argv(&context(
                Path::new("/run/1"),
                Some(Path::new("/run/1/baseline.csv")),
            ))
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
        let suite = suite(true);
        let identity = suite
            .identity_argv(Path::new("/suite"), Path::new("/runner"), 8)
            .unwrap();
        assert!(!identity.iter().any(|a| a == "--baseline"));
        assert!(identity.iter().any(|a| a == RUN_DIR_MARKER));
        let with_baseline = suite
            .argv(&context(Path::new("/run/2"), Some(Path::new("/b"))))
            .unwrap();
        assert_ne!(identity, with_baseline);
    }

    #[test]
    fn piglit_argv_uses_the_folder_and_one_profile() {
        let argv = suite(false)
            .argv(&context(Path::new("/run"), None))
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
    fn check_assets_accepts_the_deqp_layout_and_refuses_an_empty_caselist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = suite(true);
        fs::create_dir_all(root.join("vulkan")).unwrap();
        fs::create_dir_all(root.join("waivers")).unwrap();
        fs::create_dir_all(root.join("mustpass")).unwrap();
        fs::write(root.join("mustpass/vk.txt"), "dEQP-VK.info\n").unwrap();
        fs::write(root.join("waivers/vk.xml"), "<waivers/>").unwrap();
        fs::write(root.join("deqp-vk"), "").unwrap();
        fs::set_permissions(root.join("deqp-vk"), fs::Permissions::from_mode(0o755)).unwrap();
        suite.check_assets(root).unwrap();

        fs::write(root.join("mustpass/vk.txt"), "").unwrap();
        let err = suite.check_assets(root).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a non-empty file"),
            "{err:#}"
        );

        fs::remove_file(root.join("deqp-vk")).unwrap();
        let err = suite.check_assets(root).unwrap_err();
        assert!(format!("{err:#}").contains("dEQP executable"), "{err:#}");
    }

    #[test]
    fn check_assets_accepts_the_cmake_piglit_layout() {
        let dir = tempfile::tempdir().unwrap();
        let suite = suite(false);
        let folder = dir.path().join("lib64/piglit");
        fs::create_dir_all(folder.join("tests")).unwrap();
        fs::create_dir_all(folder.join("bin")).unwrap();
        fs::create_dir_all(folder.join("lib")).unwrap();
        fs::write(folder.join("tests/quick.meta.xml"), "<profile/>").unwrap();
        fs::write(folder.join("bin/shader_runner"), "").unwrap();
        fs::write(folder.join("lib/libpiglit.so"), "").unwrap();
        suite.check_assets(dir.path()).unwrap();

        fs::remove_file(folder.join("tests/quick.meta.xml")).unwrap();
        let err = suite.check_assets(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("profile 'quick'"), "{err:#}");
    }
}
