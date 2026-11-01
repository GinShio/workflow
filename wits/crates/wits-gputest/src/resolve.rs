//! Resolve one case into everything a run needs: the composed environment, the
//! installed roots and stamps, the kind's argv and the identity.
//!
//! This is pipeline steps 1–6 of the design. `run` and `plan` share it; the
//! only difference is that `run` goes on to execute and record. The process
//! environment has already been scrubbed by the caller, so `caller` is exactly
//! the environment `compose` diffs against and the runner inherits.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_actions::{compose, overlay_between, DevenvRequest, Env, Identity, Overlay};
use wits_core::git::Repository;
use wits_project::{
    identity_repo, resolve_paths, resolve_plan, BuildInput, PathInput, Profile, Project, Workspace,
};

use crate::config::{Case, Config, Store, Suite};
use crate::content;
use crate::env;
use crate::icd;
use crate::kinds::RunContext;
use crate::stamp::{self, Stamp};
use crate::state::State;
use crate::store::{self, ArchiveRef};
use crate::time;
use crate::util;

/// The per-command flags the plugin honours. They are the ones `build` and
/// `devenv` already use; the plugin re-declares them because `IdentityArgs`
/// lives in the CLI binary, not in a library.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub branch: Option<String>,
    pub detach: bool,
    pub build_type: Option<String>,
    pub toolchain: Option<String>,
    pub generator: Option<String>,
    pub presets: Vec<String>,
    pub focus: Option<String>,
    pub work_dir: Option<PathBuf>,
    pub build_dir: Option<PathBuf>,
    pub install_dir: Option<PathBuf>,
    pub specs: BTreeMap<String, String>,
    /// Where the caller stands; a detached build takes its checkout here.
    pub here: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Components {
    pub driver_icd: String,
    pub driver_libs: String,
    pub driver_config: String,
    pub suite: String,
    pub runner: String,
    pub argv: String,
}

#[derive(Debug, Clone)]
pub struct DriverMeta {
    pub project: String,
    pub focus: String,
    pub branch: Option<String>,
    pub build_type: String,
    pub toolchain: Option<String>,
    pub generator: Option<String>,
    pub presets: Vec<String>,
    pub commits: BTreeMap<String, String>,
}

/// One case, resolved as far as it can be without choosing a run id and
/// starting the runner.
pub struct Prepared {
    pub case: String,
    pub suite: Suite,
    pub driver_meta: DriverMeta,
    pub suite_project: String,
    pub runner_project: String,
    pub suite_root: PathBuf,
    pub runner_root: PathBuf,
    pub suite_stamp: Stamp,
    pub runner_stamp: Stamp,
    pub env: Env,
    pub overlay: Overlay,
    pub components: Components,
    pub identity: String,
    pub jobs: u32,
}

impl Prepared {
    /// The `[test]`/`[runner]` provenance groups the archive records.
    pub fn test_record(&self) -> crate::archive::TestRecord {
        crate::archive::TestRecord {
            project: self.suite_project.clone(),
            branch: self.suite_stamp.project.branch.clone(),
            commits: self.suite_stamp.project.commits.clone(),
        }
    }

    pub fn runner_record(&self) -> crate::archive::RunnerRecord {
        crate::archive::RunnerRecord {
            project: self.runner_project.clone(),
            branch: self.runner_stamp.project.branch.clone(),
            commits: self.runner_stamp.project.commits.clone(),
        }
    }

    pub fn content_record(&self) -> crate::archive::ContentRecord {
        crate::archive::ContentRecord {
            driver_icd: self.components.driver_icd.clone(),
            driver_libs: self.components.driver_libs.clone(),
            driver_config: self.components.driver_config.clone(),
            suite: self.components.suite.clone(),
            runner: self.components.runner.clone(),
        }
    }
}

/// Resolve the case's driver, suite, environment, assets and identity.
pub fn prepare(
    ws: &Workspace,
    config: &Config,
    case_name: &str,
    flags: &Flags,
    caller: &Env,
    cache: &mut content::HashCache,
) -> Result<Prepared> {
    let case: &Case = config.case(case_name)?;
    let driver = config.driver(&case.driver)?.clone();
    let suite = config.suite(&case.suite)?.clone();

    let driver_project = ws.project(&driver.project).with_context(|| {
        format!(
            "driver '{}': project '{}' is not in the registry",
            driver.name, driver.project
        )
    })?;
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

    let here = flags.here.clone().or_else(|| std::env::current_dir().ok());
    let profile = Profile {
        build_type: flags
            .build_type
            .clone()
            .or_else(|| Some(driver.build_type.clone())),
        toolchain: flags.toolchain.clone(),
        generator: flags.generator.clone(),
        presets: flags.presets.clone(),
        focus: flags.focus.clone(),
        specs: flags.specs.clone(),
    };

    if flags.detach && flags.branch.is_some() {
        bail!("--branch and --detach cannot be used together");
    }
    let branch = if flags.detach {
        None
    } else if let Some(branch) = flags.branch.clone().or_else(|| driver.branch.clone()) {
        Some(branch)
    } else {
        Some(focus_main_branch(driver_project, flags.focus.as_deref())?)
    };

    let identity = Identity {
        profile: profile.clone(),
        branch: branch.clone(),
        work_dir: flags.work_dir.clone(),
        build_dir: flags.build_dir.clone(),
        install_dir: flags.install_dir.clone(),
        here: here.clone(),
    };
    let capture_program =
        std::env::current_exe().context("locating the running wits-gputest binary")?;
    let composed = compose(
        ws,
        driver_project,
        &DevenvRequest {
            identity,
            capture_program,
        },
        caller,
    )
    .with_context(|| format!("composing the environment for case '{case_name}'"))?;

    let mut composed_env = caller.clone();
    env::apply_overlay(&mut composed_env, &composed);
    if let Some(select) = &driver.select {
        composed_env.insert(
            OsString::from("VK_LOADER_DRIVERS_SELECT"),
            OsString::from(select),
        );
    }
    for entry in &driver.env {
        env::apply_entry(&mut composed_env, entry);
    }
    for entry in &suite.env {
        env::apply_entry(&mut composed_env, entry);
    }
    for entry in &case.env {
        env::apply_entry(&mut composed_env, entry);
    }
    let overlay = overlay_between(caller, &composed_env);

    suite.shape.check_driver(&composed_env).with_context(|| {
        format!(
            "case '{case_name}': driver '{}' (project '{}'); if the driver's ICD is not \
             installed, run `wits build '{}' --install`",
            driver.name, driver.project, driver.project
        )
    })?;

    let suite_root = install_root(ws, suite_project)?;
    let runner_root = install_root(ws, runner_project)?;
    ensure_separate_roots(&suite_root, &runner_root)?;
    let suite_stamp = stamp::read(&suite_root, &suite_project.key())?;
    let runner_stamp = stamp::read(&runner_root, &runner_project.key())?;

    let runner_bin = runner_root.join(suite.shape.runner_bin());
    if !runner_bin.is_file() {
        bail!(
            "suite '{}': runner binary {} is missing; install with `wits gputest install {}`",
            suite.name,
            runner_bin.display(),
            suite.name
        );
    }
    if !util::is_executable(&runner_bin)? {
        bail!(
            "suite '{}': runner binary {} is not executable",
            suite.name,
            runner_bin.display()
        );
    }
    suite.shape.check_assets(&suite, &suite_root)?;

    let jobs = suite.jobs.number();
    if jobs == 0 {
        bail!("suite '{}': jobs resolved to zero", suite.name);
    }
    let run_ctx = RunContext {
        run_dir: Path::new(content::RUN_DIR_MARKER),
        baseline: None,
        suite_root: &suite_root,
        runner_root: &runner_root,
        suite: &suite,
        jobs,
    };
    let identity_argv = suite.shape.identity_argv(&run_ctx)?;

    let components = Components {
        driver_icd: content::hash_set(&icd::component(&composed_env, cache)?)?,
        driver_libs: hash_libs(&composed_env, cache)?,
        driver_config: hash_config(&composed_env, cache)?,
        suite: suite_stamp.hash.clone(),
        runner: runner_stamp.hash.clone(),
        argv: content::hash_argv(&identity_argv),
    };
    let identity = content::hash_identity(&[
        ("suite", components.suite.clone()),
        ("runner", components.runner.clone()),
        ("driver.icd", components.driver_icd.clone()),
        ("driver.libs", components.driver_libs.clone()),
        ("driver.config", components.driver_config.clone()),
        ("argv", components.argv.clone()),
        ("env", content::hash_overlay(&overlay)),
    ]);

    // `compose` resolves a plan internally; this second resolve is the archive's
    // metadata source (focus, toolchain, generator, presets, commits), so the
    // record describes what the build used rather than what the command said.
    let plan = resolve_plan(
        ws,
        driver_project,
        &BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: branch.as_deref(),
                work_dir: flags.work_dir.as_deref(),
                build_dir: flags.build_dir.as_deref(),
                install_dir: flags.install_dir.as_deref(),
                here: here.as_deref(),
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        },
    )?;
    let driver_meta = DriverMeta {
        project: driver_project.key(),
        focus: plan.paths.focus.clone(),
        branch: branch.clone(),
        build_type: plan.paths.build_type.clone(),
        toolchain: plan.paths.toolchain.as_ref().map(|t| t.name.clone()),
        generator: plan.paths.generator.clone(),
        presets: plan.presets.clone(),
        commits: commits(&plan.paths.work_dirs, branch.as_deref()),
    };

    Ok(Prepared {
        case: case.name.clone(),
        suite,
        driver_meta,
        suite_project: suite_project.key(),
        runner_project: runner_project.key(),
        suite_root,
        runner_root,
        suite_stamp,
        runner_stamp,
        env: composed_env,
        overlay,
        components,
        identity,
        jobs,
    })
}

/// The repo that carries the focus's branch identity: the focus itself when it
/// has its own git, else its anchor. A subtree has no `main_branch` of its own,
/// so the branch comes from the nearest own-git repo.
fn identity_main_branch(
    project: &Project,
    focus: Option<&str>,
) -> Result<(String, Option<String>)> {
    let flag = focus.is_some();
    let focus = project.focus_name(focus);
    // `resolve_paths` validates the focus too, but the branch is resolved first;
    // name the bad focus here rather than blame its missing `main_branch`.
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

/// The default branch a driver run resolves when neither the driver table nor
/// the command line names one: the identity repo's `main_branch` for the
/// *resolved* focus (`--focus`, else the project's). The resolved focus is
/// what `resolve_paths`/`compose` build, so the branch and the build cannot
/// disagree.
pub fn focus_main_branch(project: &Project, focus: Option<&str>) -> Result<String> {
    let (name, main) = identity_main_branch(project, focus)?;
    main.with_context(|| {
        format!(
            "project '{}' has no main_branch on repo '{name}'; \
             pass --branch or declare the branch",
            project.key()
        )
    })
}

/// The suite and runner roots must be disjoint. Each install stamp hashes its
/// whole `install_dir` subtree; a root that contains the other makes one
/// project's stamp change when the other installs, which turns ordinary suite
/// maintenance into an identity change. The design states this as a must, so a
/// nested layout is refused rather than silently accepted.
pub fn ensure_separate_roots(suite_root: &Path, runner_root: &Path) -> Result<()> {
    // Compare both as written and, where the paths exist, after resolving
    // symlinks: two spellings of one root are still one root.
    let suite_real = wits_core::path::real(suite_root);
    let runner_real = wits_core::path::real(runner_root);
    if suite_root == runner_root
        || suite_real == runner_real
        || suite_real.starts_with(&runner_real)
        || runner_real.starts_with(&suite_real)
    {
        bail!(
            "suite root {} and runner root {} overlap; each install stamp hashes its whole \
             install_dir, so the roots must be disjoint. Give the projects separate roots, \
             for example $XDG_RUNTIME_DIR/wits/gputest/{{deqp,piglit,runner}}",
            suite_root.display(),
            runner_root.display()
        );
    }
    Ok(())
}

/// The project's identity-repo `main_branch`: what `install` builds and what
/// the suite/runner roots resolve against. The identity repo is the nearest
/// own-git repo from the focus, so a subtree focus still resolves from the
/// checkout that carries the branch.
pub fn project_main_branch(project: &Project) -> Result<String> {
    let (name, main) = identity_main_branch(project, None)?;
    main.with_context(|| {
        format!(
            "project '{}' has no main_branch on repo '{name}'; \
             gputest installs the main branch, so it must be declared",
            project.key()
        )
    })
}

/// The `install_dir` of a suite or runner project, resolved with `release` on
/// the project's main branch. Assets are installed, so there is no per-suite
/// profile here.
pub fn install_root(ws: &Workspace, project: &Project) -> Result<PathBuf> {
    let main = project_main_branch(project)?;
    let profile = Profile {
        build_type: Some("release".to_owned()),
        ..Profile::default()
    };
    let paths = resolve_paths(
        ws,
        project,
        &PathInput {
            profile: &profile,
            branch: Some(main.as_str()),
            work_dir: None,
            build_dir: None,
            install_dir: None,
            here: None,
        },
    )
    .with_context(|| format!("resolving the install root of project '{}'", project.key()))?;
    let root = paths.install_dir.with_context(|| {
        format!(
            "project '{}' declares no install_dir; a suite root must be installed",
            project.key()
        )
    })?;
    if !root.is_absolute() {
        bail!(
            "project '{}': install_dir {} is not absolute; suite paths and the runner argv must be absolute",
            project.key(),
            root.display()
        );
    }
    Ok(root)
}

/// The baseline a run would compare against, and the previous archive id the
/// state will remember.
#[derive(Debug, Clone)]
pub struct Baseline {
    pub archive: Option<ArchiveRef>,
    pub note: Option<String>,
}

/// Choose the baseline for this identity from the state.
///
/// The same identity uses the stored `baseline`, never the stored `run`: a
/// rerun must return the same verdict, not compare the build with itself. A
/// different identity uses the previous run. A missing archive is treated as
/// no baseline and said so.
pub fn choose_baseline(
    store: &Store,
    case: &str,
    device: &str,
    identity: &str,
    state: Option<&State>,
) -> Baseline {
    let candidate = match state {
        Some(state) if state.hash == identity => state.baseline.clone(),
        Some(state) => Some(state.run.clone()),
        None => None,
    };
    let Some(id) = candidate else {
        return Baseline {
            archive: None,
            note: None,
        };
    };
    let path = store::archive_path(store, case, device, &id);
    if path.is_file() {
        Baseline {
            archive: Some(ArchiveRef {
                device: device.to_owned(),
                id,
                path,
            }),
            note: None,
        }
    } else {
        Baseline {
            archive: None,
            note: Some(format!(
                "baseline archive {id} is gone; running without a baseline"
            )),
        }
    }
}

/// The run directory root: `$XDG_RUNTIME_DIR/gputest`, else the service
/// runner's `/run/user/<uid>/gputest`.
pub fn runtime_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir).join("gputest"));
        }
    }
    Ok(PathBuf::from(format!("/run/user/{}/gputest", uid()?)))
}

/// The run directory for one case and id.
pub fn run_dir(case: &str, id: &str) -> Result<PathBuf> {
    Ok(runtime_root()?.join(case).join(id))
}

/// A run id whose directory and archive do not already exist, with a short
/// suffix when a second run could start in the same second.
pub fn next_run_id(
    store: &Store,
    case: &str,
    device: &str,
    state: Option<&State>,
) -> Result<String> {
    let runtime = runtime_root()?;
    let state_run = state.map(|state| state.run.as_str());
    let base = time::now_run_id();
    let taken = |id: &str| {
        store::archive_path(store, case, device, id).exists()
            || runtime.join(case).join(id).exists()
            || state_run == Some(id)
    };
    if !taken(&base) {
        return Ok(base);
    }
    for n in 2..=9999 {
        let id = time::suffixed(&base, n);
        if !taken(&id) {
            return Ok(id);
        }
    }
    bail!("could not find an unused run id for case '{case}' at {base}")
}

fn uid() -> Result<u32> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(value) = rest.split_whitespace().next() {
                    if let Ok(uid) = value.parse() {
                        return Ok(uid);
                    }
                }
            }
        }
    }
    if let Ok(value) = std::env::var("UID") {
        if let Ok(uid) = value.parse() {
            return Ok(uid);
        }
    }
    bail!("cannot determine the process uid for the /run/user fallback; set XDG_RUNTIME_DIR")
}

pub(crate) fn commits(
    work_dirs: &BTreeMap<String, PathBuf>,
    branch: Option<&str>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, path) in work_dirs {
        let repo = Repository::new(path);
        let commit = branch
            .and_then(|branch| repo.rev_parse(&format!("refs/heads/{branch}")))
            .or_else(|| repo.rev_parse("HEAD"));
        if let Some(commit) = commit {
            out.insert(name.clone(), commit);
        }
    }
    out
}

/// The `driver.libs` component: every `*.so*` directly under the loader paths
/// the composed environment carries. A symlink is followed to its target's
/// content and labelled by the path the loader sees.
fn hash_libs(env: &Env, cache: &mut content::HashCache) -> Result<String> {
    let mut entries = Vec::new();
    for key in [
        "LD_LIBRARY_PATH",
        "LIBGL_DRIVERS_PATH",
        "GBM_BACKENDS_PATH",
        "LIBVA_DRIVERS_PATH",
    ] {
        for dir in util::env_paths(env, key) {
            let read = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("reading loader path {}", dir.display()))
                }
            };
            for entry in read {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.contains(".so") {
                    continue;
                }
                let path = dir.join(&name);
                let meta = std::fs::metadata(&path)
                    .with_context(|| format!("reading loader entry {}", path.display()))?;
                if !meta.is_file() {
                    continue;
                }
                entries.push((
                    format!("{}", path.to_string_lossy()),
                    cache.hash_file(&path)?,
                ));
            }
        }
    }
    content::hash_set(&entries)
}

/// The `driver.config` component: the EGL vendor files and every `*.conf`
/// under the drirc config directory.
fn hash_config(env: &Env, cache: &mut content::HashCache) -> Result<String> {
    let mut entries = Vec::new();
    for file in util::env_paths(env, "__EGL_VENDOR_LIBRARY_FILENAMES") {
        let meta = std::fs::metadata(&file)
            .with_context(|| format!("reading EGL vendor file {}", file.display()))?;
        if !meta.is_file() {
            bail!(
                "__EGL_VENDOR_LIBRARY_FILENAMES names {}, which is not a file",
                file.display()
            );
        }
        entries.push((
            format!("{}", file.to_string_lossy()),
            cache.hash_file(&file)?,
        ));
    }
    for dir in util::env_paths(env, "DRIRC_CONFIGDIR") {
        walk_conf(&dir, &mut entries, cache)?;
    }
    content::hash_set(&entries)
}

fn walk_conf(
    dir: &Path,
    entries: &mut Vec<(String, String)>,
    cache: &mut content::HashCache,
) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("reading drirc directory {}", dir.display()))
        }
    };
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("reading drirc entry {}", path.display()))?;
        if meta.is_dir() {
            // A symlinked directory is skipped so a loop cannot recurse
            // forever; the config tree is a regular directory in practice.
            if entry.file_type()?.is_dir() {
                walk_conf(&path, entries, cache)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("conf") {
            entries.push((
                format!("{}", path.to_string_lossy()),
                cache.hash_file(&path)?,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Store;
    use crate::state::State;
    use std::fs;

    fn store(root: &Path) -> Store {
        Store {
            root: root.to_path_buf(),
            archive_days: 360,
        }
    }

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

        // No override: the project's focus (and therefore `main`).
        assert_eq!(focus_main_branch(project, None).unwrap(), "main");
        // The focus's own identity repo has a different `main_branch`.
        assert_eq!(
            focus_main_branch(project, Some("engine")).unwrap(),
            "stable"
        );
        // `install` never takes a focus override.
        assert_eq!(project_main_branch(project).unwrap(), "main");
    }

    #[test]
    fn the_same_identity_selects_the_stored_baseline_not_the_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let device = "1002-150e";
        for id in ["first", "second"] {
            let path = store::archive_path(&store, "c", device, id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        let state = State {
            hash: "sha256:same".to_owned(),
            run: "second".to_owned(),
            baseline: Some("first".to_owned()),
        };
        let baseline = choose_baseline(&store, "c", device, "sha256:same", Some(&state));
        assert_eq!(baseline.archive.unwrap().id, "first");
        assert!(baseline.note.is_none());
    }

    #[test]
    fn a_changed_identity_uses_the_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let device = "d";
        let path = store::archive_path(&store, "c", device, "old");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
        let state = State {
            hash: "sha256:old".to_owned(),
            run: "old".to_owned(),
            baseline: None,
        };
        let baseline = choose_baseline(&store, "c", device, "sha256:new", Some(&state));
        assert_eq!(baseline.archive.unwrap().id, "old");
    }

    #[test]
    fn a_gone_baseline_is_no_baseline_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let state = State {
            hash: "sha256:old".to_owned(),
            run: "gone".to_owned(),
            baseline: None,
        };
        let baseline = choose_baseline(&store, "c", "d", "sha256:new", Some(&state));
        assert!(baseline.archive.is_none());
        assert!(baseline.note.unwrap().contains("gone"));
    }

    #[test]
    fn overlapping_suite_and_runner_roots_are_refused() {
        let siblings = (
            Path::new("/run/user/1000/wits/gputest/deqp"),
            Path::new("/run/user/1000/wits/gputest/runner"),
        );
        ensure_separate_roots(siblings.0, siblings.1).unwrap();

        for (suite, runner) in [
            ("/a/deqp", "/a/deqp"),
            ("/a/deqp", "/a"),
            ("/a", "/a/runner"),
        ] {
            let err = ensure_separate_roots(Path::new(suite), Path::new(runner)).unwrap_err();
            assert!(
                format!("{err:#}").contains("overlap"),
                "{suite} {runner}: {err:#}"
            );
        }

        // A symlink alias is the same root even when the spellings differ.
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        std::fs::create_dir_all(actual.join("deqp")).unwrap();
        std::fs::create_dir_all(actual.join("runner")).unwrap();
        std::os::unix::fs::symlink(&actual, dir.path().join("alias")).unwrap();
        let err = ensure_separate_roots(&actual.join("deqp"), &dir.path().join("alias/deqp"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("overlap"), "{err:#}");
    }

    #[test]
    fn a_non_ascii_path_is_recorded_without_moving_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so.1");
        fs::write(&path, "x").unwrap();
        let mut env = Env::new();
        env.insert("LD_LIBRARY_PATH".into(), dir.path().as_os_str().to_owned());
        let mut cache = content::HashCache::new();
        let first = hash_libs(&env, &mut cache).unwrap();
        let second = hash_libs(&env, &mut cache).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn config_hashing_reads_the_drirc_tree() {
        let dir = tempfile::tempdir().unwrap();
        let drirc = dir.path().join("drirc");
        fs::create_dir_all(drirc.join("nested")).unwrap();
        fs::write(drirc.join("a.conf"), "a").unwrap();
        fs::write(drirc.join("nested/b.conf"), "b").unwrap();
        fs::write(drirc.join("ignored.txt"), "x").unwrap();
        let mut env = Env::new();
        env.insert("DRIRC_CONFIGDIR".into(), drirc.into_os_string());
        let mut cache = content::HashCache::new();
        let first = hash_config(&env, &mut cache).unwrap();
        fs::write(dir.path().join("drirc/nested/b.conf"), "changed").unwrap();
        let mut cache = content::HashCache::new();
        let second = hash_config(&env, &mut cache).unwrap();
        assert_ne!(first, second);
    }
}
