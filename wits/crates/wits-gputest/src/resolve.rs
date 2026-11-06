//! Resolve one case into everything a run needs: the installed trees, the
//! composed environment, the argv inputs and the identity.
//!
//! `run` and `run -n` share it. The process environment has already been
//! scrubbed (`env::scrub`), so `Session::caller` is exactly the environment
//! `compose` diffs against and the runner inherits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;

use wits_actions::{compose, overlay_between, DevenvRequest, Env, Identity, Overlay};
use wits_core::git::Repository;
use wits_project::{identity_repo, resolve_paths, PathInput, Profile, Project, Workspace};

use crate::archive::{BuildRecord, LayerRecord};
use crate::config::{Config, Files, Layer};
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
    pub driver: BuildRecord,
    pub layer: Option<LayerRecord>,
    /// The caller plus the overlay: what the runner is spawned with.
    pub env: Env,
    /// What `env` changes relative to the caller.
    pub overlay: Overlay,
    pub content: Content,
    pub jobs: u32,
}

/// Resolve the case's suite, driver, layer, environment and identity.
pub fn prepare(s: &Session<'_>, case_name: &str) -> Result<Prepared> {
    let case = s.config.case(case_name)?;
    let driver = s.config.driver(&case.driver)?;
    let layer = case
        .layer
        .as_deref()
        .map(|name| s.config.layer(name))
        .transpose()?;
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
    let capture_program =
        std::env::current_exe().context("locating the running wits-gputest binary")?;

    // A layer from another project brings its own build, composed first so
    // the driver's lands on top: where both set a variable the driver's value
    // wins, and the ICD stays the driver's.
    let mut env = s.caller.clone();
    let layer_record = layer
        .map(|layer| compose_layer(s, layer, project, &capture_program, &mut env))
        .transpose()?;
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
            capture_program,
        },
        &env,
    )
    .with_context(|| format!("composing the environment for case '{case_name}'"))?;
    apply(&mut env, composed);
    if let Some(select) = &driver.select {
        env.insert("VK_LOADER_DRIVERS_SELECT".into(), select.into());
    }
    let layer_env = layer.map_or(&[][..], |layer| &layer.env);
    for entry in driver
        .env
        .iter()
        .chain(layer_env)
        .chain(&suite.env)
        .chain(&case.env)
    {
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
    let no_layer = Files::default();
    let layer_files = layer.map_or(&no_layer, |layer| &layer.files);
    let mut content = Content {
        identity: String::new(),
        suite: installed.suite.hash.clone(),
        runner: installed.runner.hash.clone(),
        driver_icd: icd::component(&env)?,
        driver_libs: hash_libs(&driver.files.libs, &driver.files.linked, &env)?,
        driver_config: hash_config(&driver.files.config)?,
        layer_libs: hash_libs(&layer_files.libs, &layer_files.linked, &env)?,
        layer_config: hash_config(&layer_files.config)?,
        argv: content::hash_argv(&identity_argv),
        env: content::hash_overlay(&overlay, &s.caller),
    };
    content.identity = content.digest();

    // `compose` resolved these paths from the same inputs; the record names
    // the build tree the environment came from.
    let driver = build_record(
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

    Ok(Prepared {
        suite,
        installed,
        driver,
        layer: layer_record,
        env,
        overlay,
        content,
        jobs,
    })
}

/// The case's layer as the archive records it, its build composed into `env`
/// when it brings one. A layer from the driver's project runs from the
/// driver's build, whichever build the run selects; one from another project
/// brings that project's release build on its main branch, the build its
/// nightly makes, whatever the profile flags say.
fn compose_layer(
    s: &Session<'_>,
    layer: &Layer,
    driver_project: &Project,
    capture_program: &Path,
    env: &mut Env,
) -> Result<LayerRecord> {
    let project = s.ws.project(&layer.project).with_context(|| {
        format!(
            "layer '{}': project '{}' is not in the registry",
            layer.name, layer.project
        )
    })?;
    let mut record = LayerRecord {
        name: layer.name.clone(),
        project: project.key(),
        build: None,
    };
    if project.key() == driver_project.key() {
        return Ok(record);
    }
    let (repo, main) = identity_main_branch(project, None)?;
    let branch = main.with_context(|| {
        format!(
            "layer '{}': project '{}' has no main_branch on repo '{repo}', and a layer \
             from another project than its driver's runs from its main branch",
            layer.name,
            project.key()
        )
    })?;
    let profile = Profile {
        build_type: Some("release".to_owned()),
        ..Profile::default()
    };
    let composed = compose(
        s.ws,
        project,
        &DevenvRequest {
            identity: Identity {
                profile: profile.clone(),
                branch: Some(branch.clone()),
                work_dir: None,
                build_dir: None,
                install_dir: None,
                here: None,
            },
            capture_program: capture_program.to_path_buf(),
        },
        env,
    )
    .with_context(|| format!("composing the environment of layer '{}'", layer.name))?;
    apply(env, composed);
    record.build = Some(build_record(
        s.ws,
        project,
        &PathInput {
            profile: &profile,
            branch: Some(&branch),
            work_dir: None,
            build_dir: None,
            install_dir: None,
            here: None,
        },
    )?);
    Ok(record)
}

/// Apply an overlay to the environment it was composed against.
fn apply(env: &mut Env, overlay: Overlay) {
    for (name, value) in overlay.set {
        env.insert(name, value);
    }
    for name in &overlay.unset {
        env.remove(name);
    }
}

/// The record of the build of `project` that `input` names.
fn build_record(ws: &Workspace, project: &Project, input: &PathInput) -> Result<BuildRecord> {
    let paths = resolve_paths(ws, project, input)?;
    Ok(BuildRecord {
        project: project.key(),
        build_dir: paths
            .build_dir
            .with_context(|| format!("project '{}' declares no build_dir", project.key()))?,
        commits: commits(&paths.work_dirs, input.branch),
        focus: paths.focus,
        branch: input.branch.map(str::to_owned),
        build_type: paths.build_type,
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

/// The `driver.libs` component: the files `paths` lists, and the libraries the
/// ICD links that `linked` names. A name naming nothing the ICD links, or a
/// library the dynamic loader does not find, is an error like a missing path.
fn hash_libs(paths: &[PathBuf], linked: &[String], env: &Env) -> Result<String> {
    let mut entries = declared_entries("libs", paths)?;
    if !linked.is_empty() {
        let loaded = icd::linked(env)?;
        for name in linked {
            let mut named = loaded.iter().filter(|(soname, _)| names(name, soname));
            let Some(first) = named.next() else {
                bail!("the driver's libs entry '{name}' names no library its ICD links");
            };
            for (soname, path) in std::iter::once(first).chain(named) {
                let path = path.as_ref().with_context(|| {
                    format!(
                        "the driver's libs entry '{name}' names {soname}, which the dynamic \
                         loader does not find"
                    )
                })?;
                entries.push((
                    path.to_string_lossy().into_owned(),
                    content::hash_file(path)?,
                ));
            }
        }
    }
    content::hash_set(&entries)
}

/// The `driver.config` component: the files `paths` lists.
fn hash_config(paths: &[PathBuf]) -> Result<String> {
    content::hash_set(&declared_entries("config", paths)?)
}

/// Whether a bare `libs` name names a linked library: it is that library's
/// whole name or the part before its version, so `libLLVM.so` names
/// `libLLVM.so.23.1` while `libz.so` does not name `libzstd.so.1`.
fn names(name: &str, linked: &str) -> bool {
    linked
        .strip_prefix(name)
        .is_some_and(|version| version.is_empty() || version.starts_with('.'))
}

/// The content of what the driver's `key` lists by path, and of nothing else.
/// A listed file stands for itself and a listed directory for every file
/// directly inside it, scanned, not walked. Each file is labelled by the path
/// the driver opens and hashed by the content that path resolves to. A file in
/// a listed directory that resolves to nothing cannot be opened and is
/// skipped; a listed path that is missing is an error instead, so a mistyped
/// one cannot pin the identity while the real file changes.
fn declared_entries(key: &str, declared: &[PathBuf]) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    for path in declared {
        if path.is_dir() {
            let reading = || format!("reading the driver's {key} directory {}", path.display());
            for entry in std::fs::read_dir(path).with_context(reading)? {
                let file = entry.with_context(reading)?.path();
                if is_loadable_file(&file)? {
                    entries.push((
                        file.to_string_lossy().into_owned(),
                        content::hash_file(&file)?,
                    ));
                }
            }
        } else if path.is_file() {
            entries.push((
                path.to_string_lossy().into_owned(),
                content::hash_file(path)?,
            ));
        } else {
            bail!(
                "the driver's {key} entry {} is missing or neither a file nor a directory",
                path.display()
            );
        }
    }
    Ok(entries)
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

    #[test]
    fn a_declared_file_moves_the_component_and_fails_once_gone() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.cfg");
        fs::write(&settings, "a").unwrap();
        let declared = [settings.clone()];
        let first = hash_config(&declared).unwrap();
        assert_ne!(first, hash_config(&[]).unwrap());

        fs::write(&settings, "b").unwrap();
        assert_ne!(first, hash_config(&declared).unwrap());

        fs::remove_file(&settings).unwrap();
        let err = format!("{:#}", hash_config(&declared).unwrap_err());
        assert!(
            err.contains("config entry") && err.contains("is missing"),
            "{err}"
        );
    }

    #[test]
    fn a_declared_directory_stands_for_every_file_directly_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        fs::create_dir_all(lib.join("sub")).unwrap();
        fs::write(lib.join("libradeon.so.1"), "x").unwrap();
        fs::write(lib.join("sub/libnested.so"), "nested").unwrap();
        let declared = [lib.clone()];
        let libs = |declared: &[PathBuf]| hash_libs(declared, &[], &Env::new()).unwrap();
        let first = libs(&declared);

        // A subdirectory is not walked, and a dangling symlink is no file.
        fs::write(lib.join("sub/libnested.so"), "edited").unwrap();
        std::os::unix::fs::symlink("gone.so.1", lib.join("libgone.so")).unwrap();
        assert_eq!(first, libs(&declared));

        fs::write(lib.join("libradeon.so.1"), "y").unwrap();
        let edited = libs(&declared);
        assert_ne!(first, edited);

        // Whatever the file's name: the declaration chose the directory.
        fs::write(lib.join("notes.txt"), "z").unwrap();
        assert_ne!(edited, libs(&declared));
    }

    #[test]
    fn a_bare_name_names_a_linked_library_with_or_without_its_version() {
        assert!(names("libLLVM.so", "libLLVM.so.23.1"));
        assert!(names("libLLVM.so", "libLLVM.so"));
        assert!(!names("libz.so", "libzstd.so.1"));
        assert!(!names("libLLVM.so.2", "libLLVM.so.23.1"));
    }

    #[test]
    fn a_bare_name_hashes_the_library_the_icd_links() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("self.json");
        let exe = std::env::current_exe().unwrap();
        fs::write(
            &manifest,
            format!(r#"{{"file_format_version":"1.0.0","ICD":{{"library_path":{exe:?}}}}}"#),
        )
        .unwrap();
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), manifest.into_os_string());
        env.insert("PATH".into(), std::env::var_os("PATH").unwrap());

        let none = hash_libs(&[], &[], &env).unwrap();
        assert_ne!(none, hash_libs(&[], &["libc.so".into()], &env).unwrap());

        let err = format!(
            "{:#}",
            hash_libs(&[], &["libnowhere.so".into()], &env).unwrap_err()
        );
        assert!(err.contains("names no library its ICD links"), "{err}");
    }
}
