//! The run pipeline: prepare, decide, choose a baseline, execute, classify,
//! archive, update state, report.
//!
//! `run` and `plan` share [`resolve::prepare`]. `plan` chooses only a
//! hypothetical run id so it can print the argv, and stops before creating the
//! run directory or starting the runner; `run` goes through the whole sequence.
//! A case's failure never stops the remaining cases: the service wants the
//! whole night's picture.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use wits_actions::Overlay;
use wits_project::Workspace;

use crate::archive::{self, DeviceRecord, DriverRecord, RunRecord};
use crate::config::Config;
use crate::device::{self, Device};
use crate::env;
use crate::kinds::RunContext;
use crate::report::{self, CaseReport, Counts, Verdict};
use crate::resolve::{self, Baseline, Flags};
use crate::state::{self, State};
use crate::store;

/// Run every named case, printing one report line each, and return the command
/// exit status: 0 pass/improved/skipped, 1 regression, 2 error.
pub fn run_cases(
    config: &Config,
    ws: &Workspace,
    cases: &[String],
    flags: &Flags,
    if_changed: bool,
) -> Result<u8> {
    // The CLI scrubs before Workspace::load, as the design's step 2 requires;
    // this repeated call is idempotent and keeps an in-process caller safe.
    env::scrub_process_env();
    let caller = env::caller();
    let device = device::probe()?;
    let mut status = 0u8;
    let mut cache = crate::content::HashCache::new();
    let mut session = Session {
        caller: &caller,
        device: &device,
        cache: &mut cache,
    };
    for case in cases {
        let report = run_one(config, ws, case, flags, if_changed, &mut session);
        report.print();
        status = status.max(match report.verdict {
            Verdict::Error => 2,
            Verdict::Regression => 1,
            _ => 0,
        });
    }
    Ok(status)
}

/// Print everything a run would do, without running or writing.
pub fn plan_cases(config: &Config, ws: &Workspace, cases: &[String], flags: &Flags) -> Result<()> {
    // As in `run_cases`: the CLI has already scrubbed before loading the
    // registry, and this keeps a direct caller of the pipeline safe.
    env::scrub_process_env();
    let caller = env::caller();
    let device = device::probe()?;
    let mut failed = 0usize;
    let mut cache = crate::content::HashCache::new();
    let mut session = Session {
        caller: &caller,
        device: &device,
        cache: &mut cache,
    };
    for case in cases {
        if let Err(err) = plan_one(config, ws, case, flags, &mut session) {
            eprintln!("Error: case '{case}': {err:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} case(s) could not be resolved");
    }
    Ok(())
}

/// The per-command pieces every case shares: the scrubbed caller, the probed
/// device and the file-hash cache.
struct Session<'a> {
    caller: &'a env::Env,
    device: &'a Device,
    cache: &'a mut crate::content::HashCache,
}

fn plan_one(
    config: &Config,
    ws: &Workspace,
    case: &str,
    flags: &Flags,
    session: &mut Session<'_>,
) -> Result<()> {
    let Session {
        caller,
        device,
        cache,
    } = session;
    let prepared = resolve::prepare(ws, config, case, flags, caller, cache)?;
    let state = state::read(case, &device.key())?;
    let id = resolve::next_run_id(&config.store, case, &device.key(), state.as_ref())?;
    let run_dir = resolve::run_dir(case, &id)?;
    let baseline = resolve::choose_baseline(
        &config.store,
        case,
        &device.key(),
        &prepared.identity,
        state.as_ref(),
    );
    let baseline_path = baseline
        .archive
        .as_ref()
        .map(|_| run_dir.join("baseline.csv"));
    let ctx = RunContext {
        run_dir: &run_dir,
        baseline: baseline_path.as_deref(),
        suite_root: &prepared.suite_root,
        runner_root: &prepared.runner_root,
        suite: &prepared.suite,
        jobs: prepared.jobs,
    };
    let argv = prepared.suite.shape.argv(&ctx)?;

    println!("case {}", prepared.case);
    println!(
        "  driver    {} (focus {}, branch {}, build_type {})",
        prepared.driver_meta.project,
        prepared.driver_meta.focus,
        prepared.driver_meta.branch.as_deref().unwrap_or("detached"),
        prepared.driver_meta.build_type
    );
    if let Some(toolchain) = &prepared.driver_meta.toolchain {
        println!("  toolchain {toolchain}");
    }
    if let Some(generator) = &prepared.driver_meta.generator {
        println!("  generator {generator}");
    }
    if !prepared.driver_meta.presets.is_empty() {
        println!("  presets   {}", prepared.driver_meta.presets.join(", "));
    }
    println!(
        "  suite     {} ({}) project {}",
        prepared.suite.name,
        prepared.suite.kind().as_str(),
        prepared.suite_project
    );
    println!("  suite root  {}", prepared.suite_root.display());
    println!("  runner root {}", prepared.runner_root.display());
    println!("  run dir     {}", run_dir.display());
    println!("  environment:");
    for (name, value) in &prepared.overlay.set {
        println!(
            "    set {}={}",
            name.to_string_lossy(),
            value.to_string_lossy()
        );
    }
    for name in &prepared.overlay.unset {
        println!("    unset {}", name.to_string_lossy());
    }
    println!("  argv:");
    for arg in &argv {
        println!("    {arg}");
    }
    println!("  identity {}", prepared.identity);
    println!("    suite         {}", prepared.components.suite);
    println!("    runner        {}", prepared.components.runner);
    println!("    driver.icd    {}", prepared.components.driver_icd);
    println!("    driver.libs   {}", prepared.components.driver_libs);
    println!("    driver.config {}", prepared.components.driver_config);
    println!("    argv          {}", prepared.components.argv);
    println!(
        "    env           {}",
        crate::content::hash_overlay(&prepared.overlay)
    );
    match &baseline.archive {
        Some(archive) => println!("  baseline {} ({})", archive.id, archive.path.display()),
        None => println!("  baseline none"),
    }
    if let Some(note) = &baseline.note {
        println!("  note     {note}");
    }
    match state.as_ref() {
        Some(state) if state.hash == prepared.identity => {
            println!("  state {} (same; --if-changed would skip)", state.hash)
        }
        Some(state) => println!("  state {} (different)", state.hash),
        None => println!("  state none"),
    }
    Ok(())
}

fn run_one(
    config: &Config,
    ws: &Workspace,
    case: &str,
    flags: &Flags,
    if_changed: bool,
    session: &mut Session<'_>,
) -> CaseReport {
    let Session {
        caller,
        device,
        cache,
    } = session;
    let prepared = match resolve::prepare(ws, config, case, flags, caller, cache) {
        Ok(prepared) => prepared,
        Err(err) => return error_report(case, None, format!("{err:#}")),
    };
    let state = match state::read(case, &device.key()) {
        Ok(state) => state,
        Err(err) => return error_report(case, None, format!("{err:#}")),
    };
    if if_changed
        && state
            .as_ref()
            .is_some_and(|state| state.hash == prepared.identity)
    {
        return CaseReport {
            case: prepared.case,
            id: None,
            verdict: Verdict::Skipped,
            counts: Counts::default(),
            detail: Some("identity unchanged".to_owned()),
        };
    }

    let id = match resolve::next_run_id(&config.store, case, &device.key(), state.as_ref()) {
        Ok(id) => id,
        Err(err) => return error_report(case, None, format!("{err:#}")),
    };
    let run_dir = match resolve::run_dir(case, &id) {
        Ok(dir) => dir,
        Err(err) => return error_report(case, Some(id), format!("{err:#}")),
    };
    if let Err(err) = std::fs::create_dir_all(&run_dir) {
        return error_report(
            case,
            Some(id),
            format!("creating the run directory {}: {err}", run_dir.display()),
        );
    }
    // Fail before the test can run for hours and then discover it cannot keep
    // its evidence.
    let archive_dir = store::archive_dir(&config.store, case, &device.key());
    if let Err(err) = std::fs::create_dir_all(&archive_dir) {
        return error_report(
            case,
            Some(id),
            format!(
                "creating the archive directory {}: {err}",
                archive_dir.display()
            ),
        );
    }

    let baseline = resolve::choose_baseline(
        &config.store,
        case,
        &device.key(),
        &prepared.identity,
        state.as_ref(),
    );
    if let Some(note) = &baseline.note {
        log::warn!("{note}");
    }
    let (baseline_path, baseline_used) = match extract_baseline(&baseline, &run_dir) {
        Ok((path, used, note)) => {
            if let Some(note) = note {
                log::warn!("{note}");
            }
            (path, used)
        }
        Err(err) => return error_report(case, Some(id), format!("{err:#}")),
    };

    let (env_set, env_unset) = match overlay_strings(&prepared.overlay) {
        Ok(values) => values,
        Err(err) => return error_report(case, Some(id), format!("{err:#}")),
    };
    let ctx = RunContext {
        run_dir: &run_dir,
        baseline: baseline_path.as_deref(),
        suite_root: &prepared.suite_root,
        runner_root: &prepared.runner_root,
        suite: &prepared.suite,
        jobs: prepared.jobs,
    };
    let argv = match prepared.suite.shape.argv(&ctx) {
        Ok(argv) => argv,
        Err(err) => return error_report(case, Some(id), format!("{err:#}")),
    };

    let started = Instant::now();
    let mut command =
        std::process::Command::new(argv.first().expect("the kind's argv always names a runner"));
    command
        .args(&argv[1..])
        .current_dir(&run_dir)
        .env_clear()
        .envs(&prepared.env);
    let status = command.status();
    let duration = started.elapsed().as_secs();
    let status = match status {
        Ok(status) => status,
        Err(err) => {
            return error_report(
                case,
                Some(id),
                format!("spawning runner {}: {err}", argv[0]),
            )
        }
    };
    let signal = status.signal();
    let exit = status.code().unwrap_or_else(|| -(signal.unwrap_or(1)));
    let completed = matches!(exit, 0..=2) && archive::required_csvs_present(&run_dir);

    let record = RunRecord {
        started: id.clone(),
        duration,
        exit,
        env: env_set,
        unset: env_unset,
        argv,
        device: DeviceRecord {
            pci: device.pci.clone(),
            id: device.id.clone(),
            drm: device.drm.clone(),
            kernel: device.kernel.clone(),
        },
        driver: DriverRecord {
            project: prepared.driver_meta.project.clone(),
            focus: prepared.driver_meta.focus.clone(),
            branch: prepared.driver_meta.branch.clone(),
            build_type: prepared.driver_meta.build_type.clone(),
            toolchain: prepared.driver_meta.toolchain.clone(),
            generator: prepared.driver_meta.generator.clone(),
            presets: prepared.driver_meta.presets.clone(),
            commits: prepared.driver_meta.commits.clone(),
        },
        test: prepared.test_record(),
        runner: prepared.runner_record(),
        content: prepared.content_record(),
    };

    // From here on the runner has been spawned: the evidence is archived even
    // for a signal or an unexpected exit. The state only moves on a completed
    // run.
    if let Err(err) = archive::write_run_toml(&run_dir, &record) {
        return error_report(case, Some(id), format!("{err:#}"));
    }
    let archive_path = run_dir.join(format!("{id}.tar.zst"));
    if let Err(err) = archive::write(&archive_path, &record, &run_dir) {
        return error_report(case, Some(id), format!("{err:#}"));
    }
    if let Err(err) = store::install_archive(&config.store, case, &device.key(), &id, &archive_path)
    {
        return error_report(case, Some(id), format!("{err:#}"));
    }

    let counts = report::counts(&run_dir);
    let (verdict, detail) = classify(exit, signal, completed, &run_dir);
    if completed {
        let baseline_id = baseline_used
            .then(|| baseline.archive.as_ref().map(|archive| archive.id.clone()))
            .flatten();
        let state = State {
            hash: prepared.identity.clone(),
            run: id.clone(),
            baseline: baseline_id,
        };
        if let Err(err) = state::write(case, &device.key(), &state) {
            return error_report(case, Some(id), format!("{err:#}"));
        }
    }
    CaseReport {
        case: prepared.case,
        id: Some(id),
        verdict,
        counts,
        detail,
    }
}

fn extract_baseline(
    baseline: &Baseline,
    run_dir: &Path,
) -> Result<(Option<std::path::PathBuf>, bool, Option<String>)> {
    let Some(archive) = &baseline.archive else {
        return Ok((None, false, None));
    };
    let destination = run_dir.join("baseline.csv");
    match archive::extract_baseline(&archive.path, run_dir)? {
        true => Ok((Some(destination), true, None)),
        false => Ok((
            None,
            false,
            Some(format!(
                "baseline archive {} has no failures-all.csv; running without a baseline",
                archive.path.display()
            )),
        )),
    }
}

fn classify(
    exit: i32,
    signal: Option<i32>,
    completed: bool,
    run_dir: &Path,
) -> (Verdict, Option<String>) {
    if let Some(signal) = signal {
        return (
            Verdict::Error,
            Some(format!("runner exited with signal {signal}")),
        );
    }
    if !completed {
        if matches!(exit, 0..=2) {
            let missing = archive::REQUIRED_CSVS
                .iter()
                .filter(|name| !run_dir.join(name).is_file())
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            return (
                Verdict::Error,
                Some(format!(
                    "runner exited {exit} without the expected CSVs ({missing})"
                )),
            );
        }
        return (
            Verdict::Error,
            Some(format!("runner exited with code {exit}")),
        );
    }
    match exit {
        0 => (Verdict::Pass, None),
        1 => (Verdict::Regression, None),
        2 => (Verdict::Improved, None),
        _ => (
            Verdict::Error,
            Some(format!("runner exited with code {exit}")),
        ),
    }
}

fn overlay_strings(overlay: &Overlay) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut set = BTreeMap::new();
    for (name, value) in &overlay.set {
        let name = env_name(name)?;
        let value = value
            .to_str()
            .with_context(|| format!("environment entry {name} has a value that is not UTF-8"))?;
        set.insert(name.to_owned(), value.to_owned());
    }
    let mut unset = Vec::with_capacity(overlay.unset.len());
    for name in &overlay.unset {
        unset.push(env_name(name)?.to_owned());
    }
    unset.sort();
    Ok((set, unset))
}

fn env_name(name: &OsString) -> Result<&str> {
    name.to_str()
        .with_context(|| format!("environment entry {} is not UTF-8", name.to_string_lossy()))
}

fn error_report(case: &str, id: Option<String>, detail: String) -> CaseReport {
    CaseReport {
        case: case.to_owned(),
        id,
        verdict: Verdict::Error,
        counts: Counts::default(),
        detail: Some(detail.replace('\n', "; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::device::Device;
    use crate::stamp::{self, Stamp, StampProject};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use wits_project::Workspace;

    fn write(path: &std::path::Path, text: &str, mode: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn project_file(root: &std::path::Path, name: &str, body: &str) {
        write(&root.join(format!("{name}.toml")), body, 0o644);
    }

    fn fake_workspace(root: &std::path::Path) {
        let driver = root.join("driver-src");
        let suite = root.join("suite");
        let runner = root.join("runner");
        fs::create_dir_all(&driver).unwrap();
        fs::create_dir_all(&suite).unwrap();
        fs::create_dir_all(&runner).unwrap();

        let projects = root.join("projects");
        fs::create_dir_all(&projects).unwrap();
        project_file(
            &projects,
            "driver",
            &format!(
                "[project]\n[repos.main]\npath = \"{}\"\nbuild_dir = \"{}\"\nmain_branch = \"main\"\n",
                driver.display(),
                root.join("build").display()
            ),
        );
        project_file(
            &projects,
            "suite",
            &format!(
                "[project]\n[repos.main]\npath = \"{}\"\ninstall_dir = \"{}\"\nmain_branch = \"main\"\n",
                suite.display(),
                suite.display()
            ),
        );
        project_file(
            &projects,
            "runner",
            &format!(
                "[project]\n[repos.main]\npath = \"{}\"\ninstall_dir = \"{}\"\nmain_branch = \"main\"\n",
                runner.display(),
                runner.display()
            ),
        );
        fs::create_dir_all(root.join("build")).unwrap();

        // The CMake Piglit layout the kind checks.
        let piglit = suite.join("lib64/piglit");
        fs::create_dir_all(piglit.join("tests")).unwrap();
        fs::create_dir_all(piglit.join("bin")).unwrap();
        fs::create_dir_all(piglit.join("lib")).unwrap();
        fs::write(piglit.join("tests/quick.meta.xml"), "<profile/>").unwrap();
        fs::write(piglit.join("bin/shader_runner"), "").unwrap();
        fs::write(piglit.join("lib/libpiglit.so"), "").unwrap();

        let fake = r#"#!/bin/sh
out=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) out=$2; shift 2 ;;
        *) shift ;;
    esac
done
printf 't,Pass,0.1\n' > "$out/results.csv"
: > "$out/failures.csv"
: > "$out/failures-all.csv"
printf '[]\n' > "$out/expectations.json"
exit 0
"#;
        write(&runner.join("piglit-runner"), fake, 0o755);

        let suite_stamp = Stamp {
            hash: "sha256:suite-v1".to_owned(),
            installed: "20261026T213012Z".to_owned(),
            project: StampProject {
                key: "suite".to_owned(),
                branch: Some("main".to_owned()),
                commits: BTreeMap::new(),
            },
        };
        stamp::write(&suite, &suite_stamp).unwrap();
        let runner_stamp = Stamp {
            hash: "sha256:runner-v1".to_owned(),
            installed: "20261026T213012Z".to_owned(),
            project: StampProject {
                key: "runner".to_owned(),
                branch: Some("main".to_owned()),
                commits: BTreeMap::new(),
            },
        };
        stamp::write(&runner, &runner_stamp).unwrap();
    }

    fn fake_config(root: &std::path::Path) -> Config {
        let config_root = root.join("config");
        fs::create_dir_all(&config_root).unwrap();
        let text = format!(
            r#"
[store]
root = "{}"

[driver.d]
project = "driver"

[suite.p]
kind = "piglit"
project = "suite"
runner = "runner"
case_list = ["quick"]

[suite.p.piglit]
folder = "lib64/piglit"

[case.c]
driver = "d"
suite = "p"
"#,
            root.join("store").display()
        );
        fs::write(config_root.join("gputest.toml"), text).unwrap();
        Config::load_from(&config_root).unwrap()
    }

    fn flags(root: &std::path::Path) -> Flags {
        Flags {
            here: Some(root.to_path_buf()),
            ..Flags::default()
        }
    }

    #[test]
    fn classify_maps_the_runner_exit_to_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();

        // 0/1/2 without the required CSVs did not complete.
        assert_eq!(classify(0, None, false, run).0, Verdict::Error);
        assert!(classify(1, None, false, run)
            .1
            .unwrap()
            .contains("without the expected CSVs"));
        // A code outside 0/1/2 is an error even with the CSVs present.
        assert_eq!(classify(4, None, false, run).0, Verdict::Error);
        assert!(classify(4, None, false, run).1.unwrap().contains("code 4"));
        // A signal is an error, never a regression.
        assert!(classify(0, Some(9), false, run)
            .1
            .unwrap()
            .contains("signal 9"));

        for name in archive::REQUIRED_CSVS {
            fs::write(run.join(name), "").unwrap();
        }
        assert_eq!(classify(0, None, true, run).0, Verdict::Pass);
        assert_eq!(classify(1, None, true, run).0, Verdict::Regression);
        assert_eq!(classify(2, None, true, run).0, Verdict::Improved);
        assert_eq!(classify(3, None, true, run).0, Verdict::Error);
    }

    #[test]
    fn a_fake_piglit_case_runs_archives_states_skips_and_rebaselines() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let _runtime = crate::testutil::EnvGuard::set("XDG_RUNTIME_DIR", root.join("run"));
        let _state_env = crate::testutil::EnvGuard::set("XDG_STATE_HOME", root.join("state"));
        fs::create_dir_all(root.join("run")).unwrap();

        fake_workspace(root);
        let config = fake_config(root);
        let ws = Workspace::load_from(&root.join("projects")).unwrap();
        let device = Device {
            pci: "0000:01:00.0".to_owned(),
            id: "1002:150e".to_owned(),
            drm: "amdgpu".to_owned(),
            kernel: "test".to_owned(),
        };
        let caller: env::Env = std::env::vars_os().collect();
        let flags = flags(root);
        let mut cache = crate::content::HashCache::new();
        let mut session = Session {
            caller: &caller,
            device: &device,
            cache: &mut cache,
        };

        let first = run_one(&config, &ws, "c", &flags, false, &mut session);
        assert_eq!(first.verdict, Verdict::Pass, "{first:?}");
        let first_id = first.id.clone().unwrap();
        assert_eq!(first.counts, Counts::default());

        let state = state::read("c", "1002-150e").unwrap().unwrap();
        assert_eq!(state.run, first_id);
        assert_eq!(state.baseline, None);
        let archive = store::find(&config.store, "c", &first_id).unwrap();
        let record = archive::read_run(&archive.path).unwrap();
        assert_eq!(record.test.project, "suite");
        assert_eq!(record.runner.project, "runner");
        assert_eq!(record.driver.project, "driver");
        assert_eq!(record.env.get("WITS_DEVENV"), Some(&"driver".to_owned()));
        assert_eq!(record.content.suite, "sha256:suite-v1");
        assert!(archive::required_csvs_present(
            resolve::run_dir("c", &first_id).unwrap().as_path()
        ));

        // The same identity with --if-changed records nothing.
        let skipped = run_one(&config, &ws, "c", &flags, true, &mut session);
        assert_eq!(skipped.verdict, Verdict::Skipped);
        assert!(skipped.id.is_none());
        assert_eq!(
            state::read("c", "1002-150e").unwrap().unwrap().run,
            first_id
        );
        assert_eq!(store::archives(&config.store, "c").unwrap().len(), 1);

        // A changed suite stamp is a changed identity: the previous archive is
        // the baseline, and the state remembers both ids separately.
        let runner = root.join("runner");
        stamp::write(
            &runner,
            &Stamp {
                hash: "sha256:runner-v2".to_owned(),
                installed: "20261027T213012Z".to_owned(),
                project: StampProject {
                    key: "runner".to_owned(),
                    branch: Some("main".to_owned()),
                    commits: BTreeMap::new(),
                },
            },
        )
        .unwrap();
        let second = run_one(&config, &ws, "c", &flags, false, &mut session);
        assert_eq!(second.verdict, Verdict::Pass, "{second:?}");
        let second_id = second.id.unwrap();
        assert_ne!(second_id, first_id);
        let state = state::read("c", "1002-150e").unwrap().unwrap();
        assert_eq!(state.run, second_id);
        assert_eq!(state.baseline.as_deref(), Some(first_id.as_str()));
        assert_eq!(store::archives(&config.store, "c").unwrap().len(), 2);
    }
}
