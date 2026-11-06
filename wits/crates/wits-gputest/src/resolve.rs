//! Resolve one case into everything a run needs: the installed trees, the
//! composed environment, the argv inputs and the identity.
//!
//! `run` and `run -n` share it. The process environment has already been
//! scrubbed (`env::scrub`), so `Session::caller` is exactly the environment
//! `compose` diffs against and the runner inherits.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_actions::{compose, overlay_between, DevenvRequest, Env, Identity, Overlay};
use wits_core::git::Repository;
use wits_project::{identity_repo, resolve_paths, PathInput, Profile, Project, Workspace};

use crate::archive::DriverRecord;
use crate::config::Config;
use crate::content::{self, Content};
use crate::device::Device;
use crate::icd;
use crate::stamp::{self, Stamp};
use crate::suite::Suite;
use crate::util;

/// Which build a run tests when it is not the driver's declared one. These
/// are the flags `wits build` and `wits devenv` take, so a review checkout is
/// addressed the same way everywhere; the plugin re-declares them because the
/// `wits` binary's copy is not a library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct ProfileFlags {
    /// Target branch. Default: the driver's branch, else the main branch of
    /// the resolved focus's identity repo.
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    /// Use the selected detached HEAD as-is, without assigning it a branch
    /// identity or switching any checkout.
    #[arg(long, conflicts_with = "branch")]
    pub detach: bool,
    /// Build type; default: the driver's `build_type` (release).
    #[arg(short = 'B', long = "build-type")]
    pub build_type: Option<String>,
    /// Select a declared toolchain.
    #[arg(short = 'T', long)]
    pub toolchain: Option<String>,
    /// Build-system generator.
    #[arg(short = 'G', long)]
    pub generator: Option<String>,
    /// Apply a preset (repeatable; accepts org/preset).
    #[arg(short = 'p', long = "preset")]
    pub presets: Vec<String>,
    /// The repo to build or describe.
    #[arg(long)]
    pub focus: Option<String>,
    /// Build the base from this checkout verbatim.
    #[arg(long = "work-dir", value_name = "DIR")]
    pub work_dir: Option<PathBuf>,
    /// Register a template variable, exposed as `{{spec.KEY}}`.
    #[arg(long = "spec", value_name = "KEY=VALUE", value_parser = parse_spec)]
    pub specs: Vec<(String, String)>,
    /// Override the resolved install prefix.
    #[arg(long = "install-dir", value_name = "DIR")]
    pub install_dir: Option<PathBuf>,
    /// Override the resolved build directory.
    #[arg(long = "build-dir", value_name = "DIR")]
    pub build_dir: Option<PathBuf>,
}

fn parse_spec(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((key, _)) if key.trim().is_empty() => Err(format!("empty key in spec '{value}'")),
        Some((key, value)) => Ok((key.trim().to_owned(), value.to_owned())),
        None => Err(format!("expected KEY=VALUE, got '{value}'")),
    }
}

/// What every case of one `run` shares.
pub struct Session<'a> {
    pub config: &'a Config,
    pub ws: &'a Workspace,
    pub profile: &'a ProfileFlags,
    /// Where the command runs; a detached build takes its checkout here.
    pub here: Option<PathBuf>,
    /// The scrubbed process environment.
    pub caller: Env,
    pub device: Device,
}

impl Session<'_> {
    /// Whether profile flags chose another build than the case's own: a review
    /// checkout, another branch or build type. Such a run compares against the
    /// case's state but leaves it alone, so the declared build's next run still
    /// compares with the declared build.
    pub fn compare_only(&self) -> bool {
        *self.profile != ProfileFlags::default()
    }
}

/// A suite's installed trees, verified the way a run needs them.
pub struct Installed {
    pub suite_root: PathBuf,
    pub runner_root: PathBuf,
    pub suite: Stamp,
    pub runner: Stamp,
}

/// The suite's installed trees: both roots, both stamps, the runner binary and
/// the kind's assets. `run` and `list` ask the same question here.
pub fn installed(ws: &Workspace, suite: &Suite) -> Result<Installed> {
    let suite_project = ws.project(&suite.project).with_context(|| {
        format!(
            "suite '{}': project '{}' is not in the registry",
            suite.name, suite.project
        )
    })?;
    let runner_project = ws.project(&suite.runner).with_context(|| {
        format!(
            "suite '{}': runner project '{}' is not in the registry",
            suite.name, suite.runner
        )
    })?;
    let suite_root = install_paths(ws, suite_project)?.root;
    let runner_root = install_paths(ws, runner_project)?.root;
    let verify = || -> Result<(Stamp, Stamp)> {
        let suite_stamp = stamp::read(&suite_root, &suite_project.key())?;
        let runner_stamp = stamp::read(&runner_root, &runner_project.key())?;
        check_runner(&runner_root, suite)?;
        suite.check_assets(&suite_root)?;
        Ok((suite_stamp, runner_stamp))
    };
    let (suite_stamp, runner_stamp) = verify().with_context(|| {
        format!(
            "suite '{}' is not installed; run `wits gputest install {}`",
            suite.name, suite.name
        )
    })?;
    Ok(Installed {
        suite_root,
        runner_root,
        suite: suite_stamp,
        runner: runner_stamp,
    })
}

/// The runner binary the suite's shape needs, present and executable under
/// `root`.
pub fn check_runner(root: &Path, suite: &Suite) -> Result<()> {
    let binary = root.join(suite.kind.runner_bin());
    if !binary.is_file() {
        bail!("runner binary {} is missing", binary.display());
    }
    if !util::is_executable(&binary)? {
        bail!("runner binary {} is not executable", binary.display());
    }
    Ok(())
}

/// A suite or runner project's installed tree: the build `install` makes,
/// `release` on the project's main branch, and the checkouts it reads.
pub struct InstallPaths {
    pub branch: String,
    pub root: PathBuf,
    pub work_dirs: BTreeMap<String, PathBuf>,
}

pub fn install_paths(ws: &Workspace, project: &Project) -> Result<InstallPaths> {
    let (repo, main) = identity_main_branch(project, None)?;
    let branch = main.with_context(|| {
        format!(
            "project '{}' has no main_branch on repo '{repo}'; gputest installs the main \
             branch, so it must be declared",
            project.key()
        )
    })?;
    let profile = Profile {
        build_type: Some("release".to_owned()),
        ..Profile::default()
    };
    let paths = resolve_paths(
        ws,
        project,
        &PathInput {
            profile: &profile,
            branch: Some(&branch),
            work_dir: None,
            build_dir: None,
            install_dir: None,
            here: None,
        },
    )
    .with_context(|| format!("resolving the install root of project '{}'", project.key()))?;
    let root = paths.install_dir.with_context(|| {
        format!(
            "project '{}' declares no install_dir; a suite or runner tree must be installed",
            project.key()
        )
    })?;
    if !root.is_absolute() {
        bail!(
            "project '{}': install_dir {} is not absolute; the runner's argv names absolute paths",
            project.key(),
            root.display()
        );
    }
    Ok(InstallPaths {
        branch,
        root,
        work_dirs: paths.work_dirs,
    })
}

/// The repo that carries the focus's branch identity — the focus itself when
/// it has its own git, else its anchor — and that repo's `main_branch`. A
/// subtree has no `main_branch` of its own.
fn identity_main_branch(
    project: &Project,
    focus: Option<&str>,
) -> Result<(String, Option<String>)> {
    let flag = focus.is_some();
    let focus = project.focus_name(focus);
    // `resolve_paths` validates the focus too, but the branch is resolved
    // first; name the bad focus here rather than blame its missing branch.
    if project.repo(focus).is_none() {
        bail!(
            "project '{}' has no repo '{focus}'{}",
            project.key(),
            if flag { " (--focus)" } else { "" }
        );
    }
    let name = identity_repo(project, focus).unwrap_or_else(|| focus.to_owned());
    let main = project
        .repo(&name)
        .and_then(|repo| repo.decl().main_branch.clone());
    Ok((name, main))
}

/// The branch a driver run resolves when neither the driver table nor the
/// command line names one: the identity repo's `main_branch` for the
/// *resolved* focus, which is what `compose` builds, so the branch and the
/// build cannot disagree.
fn focus_main_branch(project: &Project, focus: Option<&str>) -> Result<String> {
    let (name, main) = identity_main_branch(project, focus)?;
    main.with_context(|| {
        format!(
            "project '{}' has no main_branch on repo '{name}'; pass --branch or declare the \
             driver's branch",
            project.key()
        )
    })
}

/// One case, resolved as far as it can be without a run id.
pub struct Prepared {
    pub suite: Suite,
    pub installed: Installed,
    pub driver: DriverRecord,
    /// The caller plus the overlay: what the runner is spawned with.
    pub env: Env,
    /// What `env` changes relative to the caller.
    pub overlay: Overlay,
    pub content: Content,
    pub jobs: u32,
}

/// Resolve the case's suite, driver, environment and identity.
pub fn prepare(s: &Session<'_>, case_name: &str) -> Result<Prepared> {
    let case = s.config.case(case_name)?;
    let driver = s.config.driver(&case.driver)?;
    let suite = s.config.suite(&case.suite)?.clone();

    // The installed trees first: checking them reads a few files, while
    // composing the driver runs Meson.
    let installed = installed(s.ws, &suite)?;

    let project = s.ws.project(&driver.project).with_context(|| {
        format!(
            "driver '{}': project '{}' is not in the registry",
            driver.name, driver.project
        )
    })?;
    let flags = s.profile;
    let profile = Profile {
        build_type: Some(
            flags
                .build_type
                .clone()
                .unwrap_or_else(|| driver.build_type.clone()),
        ),
        toolchain: flags.toolchain.clone(),
        generator: flags.generator.clone(),
        presets: flags.presets.clone(),
        focus: flags.focus.clone(),
        specs: flags.specs.iter().cloned().collect(),
    };
    let branch = if flags.detach {
        None
    } else {
        match flags.branch.clone().or_else(|| driver.branch.clone()) {
            Some(branch) => Some(branch),
            None => Some(focus_main_branch(project, flags.focus.as_deref())?),
        }
    };
    let composed = compose(
        s.ws,
        project,
        &DevenvRequest {
            identity: Identity {
                profile: profile.clone(),
                branch: branch.clone(),
                work_dir: flags.work_dir.clone(),
                build_dir: flags.build_dir.clone(),
                install_dir: flags.install_dir.clone(),
                here: s.here.clone(),
            },
            capture_program: std::env::current_exe()
                .context("locating the running wits-gputest binary")?,
        },
        &s.caller,
    )
    .with_context(|| format!("composing the environment for case '{case_name}'"))?;

    let mut env = s.caller.clone();
    for (name, value) in composed.set {
        env.insert(name, value);
    }
    for name in &composed.unset {
        env.remove(name);
    }
    if let Some(select) = &driver.select {
        env.insert("VK_LOADER_DRIVERS_SELECT".into(), select.into());
    }
    for entry in driver.env.iter().chain(&suite.env).chain(&case.env) {
        entry.apply(&mut env);
    }
    let overlay = overlay_between(&s.caller, &env);
    suite.check_driver(&env).with_context(|| {
        format!(
            "case '{case_name}': driver '{}' (project '{}')",
            driver.name, driver.project
        )
    })?;

    let jobs = suite.jobs.number();
    let identity_argv = suite.identity_argv(&installed.suite_root, &installed.runner_root, jobs)?;
    let mut content = Content {
        identity: String::new(),
        suite: installed.suite.hash.clone(),
        runner: installed.runner.hash.clone(),
        driver_icd: icd::component(&env)?,
        driver_libs: hash_libs(&env)?,
        driver_config: hash_config(&env)?,
        argv: content::hash_argv(&identity_argv),
        env: content::hash_overlay(&overlay, &s.caller),
    };
    content.identity = content.digest();

    // `compose` resolved these paths from the same inputs; the record names
    // the build tree the environment came from.
    let paths = resolve_paths(
        s.ws,
        project,
        &PathInput {
            profile: &profile,
            branch: branch.as_deref(),
            work_dir: flags.work_dir.as_deref(),
            build_dir: flags.build_dir.as_deref(),
            install_dir: flags.install_dir.as_deref(),
            here: s.here.as_deref(),
        },
    )?;
    let driver = DriverRecord {
        project: project.key(),
        build_dir: paths
            .build_dir
            .with_context(|| format!("project '{}' declares no build_dir", project.key()))?,
        commits: commits(&paths.work_dirs, branch.as_deref()),
        focus: paths.focus,
        branch,
        build_type: paths.build_type,
    };

    Ok(Prepared {
        suite,
        installed,
        driver,
        env,
        overlay,
        content,
        jobs,
    })
}

/// Each checkout's commit on `branch`, else at `HEAD`.
pub fn commits(
    work_dirs: &BTreeMap<String, PathBuf>,
    branch: Option<&str>,
) -> BTreeMap<String, String> {
    work_dirs
        .iter()
        .filter_map(|(name, path)| {
            let repo = Repository::new(path);
            let commit = branch
                .and_then(|branch| repo.rev_parse(&format!("refs/heads/{branch}")))
                .or_else(|| repo.rev_parse("HEAD"))?;
            Some((name.clone(), commit))
        })
        .collect()
}

/// The `driver.libs` component: every shared object directly under the loader
/// paths the composed environment carries, labelled by the path the loader
/// sees and hashed by the content it resolves to. An entry that resolves to
/// nothing cannot load, and is skipped.
fn hash_libs(env: &Env) -> Result<String> {
    let mut entries = Vec::new();
    for key in [
        "LD_LIBRARY_PATH",
        "LIBGL_DRIVERS_PATH",
        "GBM_BACKENDS_PATH",
        "LIBVA_DRIVERS_PATH",
    ] {
        for dir in util::env_paths(env, key) {
            let read = match std::fs::read_dir(&dir) {
                Ok(read) => read,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("reading loader path {}", dir.display()))
                }
            };
            for entry in read {
                let path = entry
                    .with_context(|| format!("reading loader path {}", dir.display()))?
                    .path();
                let shared_object = path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.ends_with(".so") || name.contains(".so."));
                if shared_object && is_loadable_file(&path)? {
                    entries.push((
                        path.to_string_lossy().into_owned(),
                        content::hash_file(&path)?,
                    ));
                }
            }
        }
    }
    content::hash_set(&entries)
}

/// The `driver.config` component: the EGL vendor files, and the drirc files
/// Mesa reads (`src/util/xmlconfig.c`, `driParseConfigFiles`): every `*.conf`
/// directly inside each `DRIRC_CONFIGDIR` entry — scanned, not walked — then
/// `$HOME/.drirc`. A driver whose environment sets no `DRIRC_CONFIGDIR` reads
/// its compiled-in data directory instead, which this does not cover.
fn hash_config(env: &Env) -> Result<String> {
    let mut entries = Vec::new();
    for file in util::env_paths(env, "__EGL_VENDOR_LIBRARY_FILENAMES") {
        if !file.is_file() {
            bail!(
                "__EGL_VENDOR_LIBRARY_FILENAMES names {}, which is not a file",
                file.display()
            );
        }
        entries.push((
            file.to_string_lossy().into_owned(),
            content::hash_file(&file)?,
        ));
    }
    for dir in util::env_paths(env, "DRIRC_CONFIGDIR") {
        let read = match std::fs::read_dir(&dir) {
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading drirc directory {}", dir.display()))
            }
        };
        for entry in read {
            let path = entry
                .with_context(|| format!("reading drirc directory {}", dir.display()))?
                .path();
            if path.extension().is_some_and(|ext| ext == "conf") && is_loadable_file(&path)? {
                entries.push((
                    path.to_string_lossy().into_owned(),
                    content::hash_file(&path)?,
                ));
            }
        }
    }
    if let Some(home) = env.get(OsStr::new("HOME")) {
        let drirc = Path::new(home).join(".drirc");
        if is_loadable_file(&drirc)? {
            entries.push((
                drirc.to_string_lossy().into_owned(),
                content::hash_file(&drirc)?,
            ));
        }
    }
    content::hash_set(&entries)
}

/// Whether `path` resolves to a regular file. A dangling symlink resolves to
/// nothing, like a missing path.
fn is_loadable_file(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_focus_override_takes_its_identity_repo_main_branch() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        fs::create_dir_all(&main).unwrap();
        fs::write(
            dir.path().join("p.toml"),
            format!(
                "[project]\n[repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\n\
                 [repos.engine]\npath = \"third_party/engine\"\nmain_branch = \"stable\"\n\
                 anchor = \"main\"\n",
                main.display()
            ),
        )
        .unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        let project = ws.project("p").unwrap();
        assert_eq!(focus_main_branch(project, None).unwrap(), "main");
        assert_eq!(
            focus_main_branch(project, Some("engine")).unwrap(),
            "stable"
        );
        assert!(focus_main_branch(project, Some("nope")).is_err());
    }

    fn env_with(key: &str, value: &Path) -> Env {
        let mut env = Env::new();
        env.insert(key.into(), value.as_os_str().to_owned());
        env
    }

    #[test]
    fn library_hashing_reads_shared_objects_and_skips_what_cannot_load() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("libradeon.so.1"), "x").unwrap();
        fs::write(dir.path().join("notes.sorted"), "not a library").unwrap();
        let env = env_with("LD_LIBRARY_PATH", dir.path());
        let first = hash_libs(&env).unwrap();
        assert_eq!(first, hash_libs(&env).unwrap());

        std::os::unix::fs::symlink("gone.so.1", dir.path().join("libgone.so")).unwrap();
        fs::write(dir.path().join("notes.sorted"), "edited").unwrap();
        assert_eq!(first, hash_libs(&env).unwrap());

        fs::write(dir.path().join("libradeon.so.1"), "y").unwrap();
        assert_ne!(first, hash_libs(&env).unwrap());
    }

    #[test]
    fn config_hashing_reads_drirc_files_the_way_mesa_does() {
        let dir = tempfile::tempdir().unwrap();
        let drirc = dir.path().join("drirc");
        fs::create_dir_all(drirc.join("tests")).unwrap();
        fs::write(drirc.join("00-defaults.conf"), "a").unwrap();
        fs::write(drirc.join("tests/00-test.conf"), "fixture").unwrap();
        fs::write(drirc.join("README"), "x").unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let mut env = env_with("DRIRC_CONFIGDIR", &drirc);
        env.insert("HOME".into(), home.as_os_str().to_owned());
        let first = hash_config(&env).unwrap();

        // Mesa scans the directory without descending into it.
        fs::write(drirc.join("tests/00-test.conf"), "edited fixture").unwrap();
        assert_eq!(first, hash_config(&env).unwrap());

        fs::write(drirc.join("00-defaults.conf"), "b").unwrap();
        let edited = hash_config(&env).unwrap();
        assert_ne!(first, edited);

        fs::write(home.join(".drirc"), "<driconf/>").unwrap();
        assert_ne!(edited, hash_config(&env).unwrap());
    }
}
