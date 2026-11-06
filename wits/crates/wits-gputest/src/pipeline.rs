//! The run pipeline: prepare, decide, choose a baseline, execute, evaluate,
//! archive, update state, report — and under `-n`, the plan instead.
//!
//! A case's failure never stops the remaining cases: the service wants the
//! whole night's picture.

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use wits_actions::Overlay;

use crate::archive::{self, RunRecord};
use crate::config::Store;
use crate::report::{self, CaseReport};
use crate::resolve::{self, Session};
use crate::state::{self, State};
use crate::store;
use crate::suite::RunContext;
use crate::time;

/// The baseline's file name inside the run directory.
const BASELINE: &str = "baseline.csv";

/// Run every named case, printing one report line each, and return the command
/// exit status: 0 pass/improved/skipped, 1 regression, 2 error.
pub fn run_cases(s: &Session<'_>, cases: &[String], if_changed: bool) -> u8 {
    let mut status = 0;
    for case in cases {
        let mut claimed = None;
        let report = run_case(s, case, if_changed, &mut claimed)
            .unwrap_or_else(|err| CaseReport::error(case, claimed, &err));
        println!("{report}");
        status = status.max(report.verdict.exit_code());
    }
    status
}

/// Print everything a run of each case would do, without running or writing.
pub fn plan_cases(s: &Session<'_>, cases: &[String]) -> Result<()> {
    let mut failed = 0;
    for case in cases {
        if let Err(err) = print_plan(s, case) {
            eprintln!("Error: case '{case}': {err:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} case(s) could not be planned");
    }
    Ok(())
}

/// One case's run. `claimed` receives the run id as soon as there is one, so a
/// later error is still reported against it.
fn run_case(
    s: &Session<'_>,
    case: &str,
    if_changed: bool,
    claimed: &mut Option<String>,
) -> Result<CaseReport> {
    let prepared = resolve::prepare(s, case)?;
    let device = s.device.key();
    let state = state::read(case, &device)?;
    let identity = prepared.content.identity.clone();
    if if_changed && state.as_ref().is_some_and(|state| state.hash == identity) {
        return Ok(CaseReport::skipped(case, "identity unchanged"));
    }
    let store = &s.config.store;
    let (env, unset) = overlay_record(&prepared.overlay)?;

    // Fail before the test runs for hours and then cannot keep its evidence.
    store::ensure_archive_dir(store, case, &device)?;
    let mut run = RunDir::claim(store, case, &device)?;
    *claimed = Some(run.id.clone());

    let baseline = match choose_baseline(store, case, &device, &identity, state.as_ref()) {
        Some((id, archive)) => {
            let path = run.dir.join(BASELINE);
            if archive::extract(&archive, report::FAILING, &path)? {
                Some((id, path))
            } else {
                log::warn!(
                    "baseline archive {} has no {}; running without a baseline",
                    archive.display(),
                    report::FAILING
                );
                None
            }
        }
        None => None,
    };
    let baseline_path = baseline.as_ref().map(|(_, path)| path.as_path());
    let argv = prepared.suite.argv(&RunContext {
        run_dir: &run.dir,
        baseline: baseline_path,
        suite_root: &prepared.installed.suite_root,
        runner_root: &prepared.installed.runner_root,
        jobs: prepared.jobs,
    })?;

    let started = Instant::now();
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&run.dir)
        .env_clear()
        .envs(&prepared.env)
        .status()
        .with_context(|| format!("spawning runner {}", argv[0]))?;
    let duration = started.elapsed().as_secs();
    let exit = status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .expect("a waited-for child has exited or been killed by a signal");
    let outcome = report::evaluate(exit, &run.dir, baseline_path);

    let record = RunRecord {
        started: run.id.clone(),
        duration,
        exit,
        verdict: outcome
            .as_ref()
            .map_or(report::Verdict::Error, |counts| counts.verdict()),
        detail: outcome.as_ref().err().map(|err| format!("{err:#}")),
        baseline: baseline.map(|(id, _)| id),
        compare_only: s.compare_only(),
        argv,
        unset,
        counts: outcome.as_ref().ok().copied(),
        env,
        device: s.device.clone(),
        driver: prepared.driver,
        layer: prepared.layer,
        suite: prepared.installed.suite.source,
        runner: prepared.installed.runner.source,
        content: prepared.content,
    };
    if let Err(err) = store::save(store, case, &device, &record, &run.dir) {
        run.keep = true;
        return Err(err.context(format!(
            "archiving the run; its evidence stays in {}",
            run.dir.display()
        )));
    }

    let counts = match outcome {
        Ok(counts) => counts,
        Err(err) => return Ok(CaseReport::error(case, Some(run.id.clone()), &err)),
    };
    if !record.compare_only {
        state::write(
            case,
            &device,
            &State {
                hash: identity,
                run: run.id.clone(),
                baseline: record.baseline.clone(),
            },
        )?;
    }
    Ok(CaseReport {
        case: case.to_owned(),
        id: Some(run.id.clone()),
        verdict: counts.verdict(),
        counts: Some(counts),
        detail: None,
        compare_only: record.compare_only,
    })
}

/// The archive a run of `identity` compares against. The same identity reuses
/// the stored baseline, never the stored run, so a rerun returns the same
/// verdict rather than comparing the build with itself; a different identity
/// compares with the last run. A pruned archive is no baseline, and said so.
fn choose_baseline(
    store: &Store,
    case: &str,
    device: &str,
    identity: &str,
    state: Option<&State>,
) -> Option<(String, PathBuf)> {
    let id = match state? {
        state if state.hash == identity => state.baseline.clone()?,
        state => state.run.clone(),
    };
    let path = store::archive_path(store, case, device, &id);
    if path.is_file() {
        Some((id, path))
    } else {
        log::warn!("baseline archive {id} is gone; running without a baseline");
        None
    }
}

/// A claimed run directory, `$XDG_RUNTIME_DIR/gputest/<case>/<id>`. It is
/// removed when the run is done with it, because the archive carries
/// everything it held; `keep` leaves it for a person when the archive could
/// not be written.
struct RunDir {
    id: String,
    dir: PathBuf,
    keep: bool,
}

impl RunDir {
    /// Claim the current second as the run id. An id is a UTC second, so a
    /// second run of one case within it waits for the next one.
    fn claim(store: &Store, case: &str, device: &str) -> Result<Self> {
        let parent = runtime_root()?.join(case);
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        for _ in 0..3 {
            let id = time::now_run_id();
            let dir = parent.join(&id);
            if !store::archive_path(store, case, device, &id).exists() {
                match std::fs::create_dir(&dir) {
                    Ok(()) => {
                        return Ok(Self {
                            id,
                            dir,
                            keep: false,
                        })
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!("creating the run directory {}", dir.display())
                        })
                    }
                }
            }
            let into_second = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.subsec_nanos());
            std::thread::sleep(Duration::from_nanos(u64::from(1_000_000_000 - into_second)));
        }
        bail!("could not claim a run id for case '{case}': every recent second is taken")
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Err(err) = std::fs::remove_dir_all(&self.dir) {
            log::warn!("removing the run directory {}: {err}", self.dir.display());
        }
    }
}

/// Run directories are session scratch, so they live in the session's
/// runtime directory.
fn runtime_root() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .context("XDG_RUNTIME_DIR is not set; gputest keeps its run directories under it")?;
    Ok(PathBuf::from(dir).join("gputest"))
}

/// The overlay as `run.toml` records it. TOML holds text, so a name or value
/// that is not UTF-8 stops the case before the runner starts.
fn overlay_record(overlay: &Overlay) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let text = |value: &std::ffi::OsStr| {
        value
            .to_str()
            .map(str::to_owned)
            .with_context(|| format!("environment entry {} is not UTF-8", value.to_string_lossy()))
    };
    let set = overlay
        .set
        .iter()
        .map(|(name, value)| Ok((text(name)?, text(value)?)))
        .collect::<Result<_>>()?;
    let mut unset = overlay
        .unset
        .iter()
        .map(|name| text(name))
        .collect::<Result<Vec<_>>>()?;
    unset.sort();
    Ok((set, unset))
}

fn print_plan(s: &Session<'_>, case: &str) -> Result<()> {
    let prepared = resolve::prepare(s, case)?;
    let device = s.device.key();
    let state = state::read(case, &device)?;
    let identity = &prepared.content.identity;
    let run_dir = runtime_root()?.join(case).join(time::now_run_id());
    let baseline = choose_baseline(&s.config.store, case, &device, identity, state.as_ref());
    let baseline_path = baseline.as_ref().map(|_| run_dir.join(BASELINE));
    let installed = &prepared.installed;
    let argv = prepared.suite.argv(&RunContext {
        run_dir: &run_dir,
        baseline: baseline_path.as_deref(),
        suite_root: &installed.suite_root,
        runner_root: &installed.runner_root,
        jobs: prepared.jobs,
    })?;
    let driver = &prepared.driver;
    let content = &prepared.content;

    println!("case {case}");
    println!(
        "  driver      {} (focus {}, branch {}, build_type {})",
        driver.project,
        driver.focus,
        driver.branch.as_deref().unwrap_or("detached"),
        driver.build_type
    );
    println!("  build dir   {}", driver.build_dir.display());
    if let Some(layer) = &prepared.layer {
        match &layer.build {
            Some(build) => println!(
                "  layer       {} from {} (branch {}, build_type {}, build dir {})",
                layer.name,
                layer.project,
                build.branch.as_deref().unwrap_or("detached"),
                build.build_type,
                build.build_dir.display()
            ),
            None => println!(
                "  layer       {} from {}, in the driver's build",
                layer.name, layer.project
            ),
        }
    }
    println!(
        "  suite       {} ({}) from {}",
        prepared.suite.name,
        prepared.suite.kind.name(),
        installed.suite.source.project
    );
    println!("  suite root  {}", installed.suite_root.display());
    println!("  runner root {}", installed.runner_root.display());
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
    println!("  identity {identity}");
    for (label, hash) in [
        ("suite", &content.suite),
        ("runner", &content.runner),
        ("driver.icd", &content.driver_icd),
        ("driver.libs", &content.driver_libs),
        ("driver.config", &content.driver_config),
        ("layer.libs", &content.layer_libs),
        ("layer.config", &content.layer_config),
        ("argv", &content.argv),
        ("env", &content.env),
    ] {
        println!("    {label:<13} {hash}");
    }
    match &baseline {
        Some((id, path)) => println!("  baseline {id} ({})", path.display()),
        None => println!("  baseline none"),
    }
    match &state {
        Some(state) if &state.hash == identity => {
            println!(
                "  state    {} (same identity; --if-changed would skip)",
                state.run
            )
        }
        Some(state) => println!("  state    {} (another identity)", state.run),
        None => println!("  state    none"),
    }
    if s.compare_only() {
        println!("  profile flags given: the run compares but leaves the state alone");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::device::Device;
    use crate::report::{Counts, Verdict};
    use crate::resolve::ProfileFlags;
    use crate::stamp::{self, Source, Stamp};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use wits_project::Workspace;

    /// A runner that applies `--baseline` to the raw statuses in `raw.csv` the
    /// way deqp-runner 0.24 does, writes `results.csv`, and exits like it.
    const FAKE_RUNNER: &str = r#"#!/bin/sh
out= baseline=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) out=$2; shift 2 ;;
        --baseline) baseline=$2; shift 2 ;;
        *) shift ;;
    esac
done
raw="$(dirname "$0")/raw.csv"
awk -F, -v base="$baseline" '
BEGIN { if (base != "") while ((getline line < base) > 0) { split(line, f, ","); b[f[1]] = f[2] } }
{
    t = $1; s = $2; e = b[t]
    if (e != "") {
        if (s == "Fail" && (e == "Fail" || e == "ExpectedFail")) s = "ExpectedFail"
        else if (s == "Fail" && (e == "Crash" || e == "Timeout")) s = "UnexpectedImprovement(Fail)"
        else if (s == "Pass" && (e == "Fail" || e == "Crash" || e == "Missing" || e == "Timeout")) s = "UnexpectedImprovement(Pass)"
        else if (s == "Crash" && (e == "Crash" || e == "ExpectedFail")) s = "ExpectedFail"
        else if (s == "Timeout" && e == "Timeout") s = "ExpectedFail"
    }
    print t "," s ",0.1"
}' "$raw" > "$out/results.csv"
grep -Ev ',(Pass|Skip|Warn|Flake|KnownFlake|ExpectedFail),' "$out/results.csv" | cut -d, -f1,2 > "$out/failures.csv"
printf '[]\n' > "$out/expectations.json"
if grep -Eq ',(Fail|Crash|Timeout|Missing),' "$out/results.csv"; then exit 1; fi
if grep -q ',UnexpectedImprovement' "$out/results.csv"; then exit 2; fi
exit 0
"#;

    fn write(path: &Path, text: &str, mode: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn stamp_tree(root: &Path, project: &str, version: &str) {
        stamp::write(
            root,
            &Stamp {
                hash: format!("sha256:{project}-{version}"),
                installed: "20261026T213012Z".to_owned(),
                source: Source {
                    project: project.to_owned(),
                    branch: "main".to_owned(),
                    commits: BTreeMap::new(),
                },
            },
        )
        .unwrap();
    }

    /// A registry of a driver, a suite and a runner project, an installed
    /// Piglit layout, the fake runner, and a config declaring case `c`.
    fn fixture(root: &Path) -> (Workspace, Config) {
        let projects = root.join("projects");
        let project = |name: &str, extra: &str| {
            write(
                &projects.join(format!("{name}.toml")),
                &format!(
                    "[project]\n[repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n{extra}",
                    root.join(format!("{name}-src")).display()
                ),
                0o644,
            );
            fs::create_dir_all(root.join(format!("{name}-src"))).unwrap();
        };
        project(
            "driver",
            &format!(
                "build_dir = \"{}\"\n\n[project.devenv]\nSHARED = \"driver\"\n",
                root.join("build").display()
            ),
        );
        project(
            "layer",
            &format!(
                "build_dir = \"{}\"\n\n[project.devenv]\nSHARED = \"layer\"\nLAYER_ONLY = \"1\"\n",
                root.join("layer-build").display()
            ),
        );
        project(
            "suite",
            &format!("install_dir = \"{}\"\n", root.join("suite").display()),
        );
        project(
            "runner",
            &format!("install_dir = \"{}\"\n", root.join("runner").display()),
        );
        fs::create_dir_all(root.join("build")).unwrap();
        fs::create_dir_all(root.join("layer-build")).unwrap();
        write(&root.join("layer-libs/libgl.so"), "gl", 0o644);
        fs::create_dir_all(root.join("store")).unwrap();

        let piglit = root.join("suite/lib64/piglit");
        write(&piglit.join("tests/quick.meta.xml"), "<profile/>", 0o644);
        write(&piglit.join("bin/shader_runner"), "", 0o755);
        write(&piglit.join("lib/libpiglit.so"), "", 0o644);
        write(&root.join("runner/piglit-runner"), FAKE_RUNNER, 0o755);
        stamp_tree(&root.join("suite"), "suite", "v1");
        stamp_tree(&root.join("runner"), "runner", "v1");

        write(
            &root.join("config/gputest.toml"),
            &format!(
                "[store]\nroot = \"{}\"\n\n[driver.d]\nproject = \"driver\"\n\n\
                 [suite.p]\nproject = \"suite\"\nrunner = \"runner\"\ncase_list = [\"quick\"]\n\n\
                 [suite.p.piglit]\nfolder = \"lib64/piglit\"\n\n\
                 [case.c]\ndriver = \"d\"\nsuite = \"p\"\n\n\
                 [layer.other]\nproject = \"layer\"\nenv = {{ LAYER_ENV = \"on\" }}\n\
                 libs = [\"{}\"]\n\n\
                 [layer.same]\nproject = \"driver\"\nenv = {{ LAYER_ENV = \"same\" }}\n\n\
                 [case.other]\ndriver = \"d\"\nlayer = \"other\"\nsuite = \"p\"\n\n\
                 [case.same]\ndriver = \"d\"\nlayer = \"same\"\nsuite = \"p\"\n",
                root.join("store").display(),
                root.join("layer-libs").display()
            ),
            0o644,
        );
        (
            Workspace::load_from(&projects).unwrap(),
            Config::load_from(&root.join("config")).unwrap(),
        )
    }

    fn run(s: &Session<'_>, if_changed: bool) -> CaseReport {
        let mut claimed = None;
        run_case(s, "c", if_changed, &mut claimed)
            .unwrap_or_else(|err| CaseReport::error("c", claimed, &err))
    }

    /// The caller `env::scrub` would leave, without scrubbing the test process.
    fn scrubbed_caller() -> wits_actions::Env {
        std::env::vars_os()
            .filter(|(name, _)| !crate::env::is_scrubbed(name))
            .collect()
    }

    #[test]
    fn runs_compare_carry_failures_by_their_own_status_and_skip_or_compare_only() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let _runtime = crate::testutil::EnvGuard::set("XDG_RUNTIME_DIR", root.join("run"));
        let _state = crate::testutil::EnvGuard::set("XDG_STATE_HOME", root.join("state"));
        let (ws, config) = fixture(root);
        let raw = root.join("runner/raw.csv");
        let device = Device {
            pci: "0000:01:00.0".to_owned(),
            id: "1002:150e".to_owned(),
            drm: "amdgpu".to_owned(),
            kernel: "test".to_owned(),
        };
        let declared = ProfileFlags::default();
        let session = |profile| Session {
            config: &config,
            ws: &ws,
            profile,
            here: Some(root.to_path_buf()),
            caller: scrubbed_caller(),
            device: device.clone(),
        };
        let s = session(&declared);
        let lineage = || state::read("c", "1002-150e").unwrap().unwrap();
        let counts = |new, carried, improved| {
            Some(Counts {
                new,
                carried,
                improved,
            })
        };

        // A first run has no baseline: every failure is new, and the evidence
        // keeps each failing test by its own status.
        fs::write(&raw, "t1,Pass\nt2,Fail\nt3,Timeout\n").unwrap();
        let first = run(&s, false);
        assert_eq!(first.verdict, Verdict::Regression, "{first}");
        assert_eq!(first.counts, counts(2, 0, 0));
        let first_id = first.id.clone().unwrap();
        assert_eq!(lineage().run, first_id);
        assert_eq!(lineage().baseline, None);
        let archive = store::find(&config.store, "c", &first_id).unwrap();
        let record = archive::read_run(&archive.path).unwrap();
        assert_eq!(record.verdict, Verdict::Regression);
        assert_eq!(record.env.get("WITS_DEVENV"), Some(&"driver".to_owned()));
        assert_eq!(record.suite.project, "suite");
        assert_eq!(record.content.identity, lineage().hash);
        let mut failing = Vec::new();
        assert!(archive::copy_member(&archive.path, report::FAILING, &mut failing).unwrap());
        assert_eq!(String::from_utf8(failing).unwrap(), "t2,Fail\nt3,Timeout\n");
        assert!(
            fs::read_dir(root.join("run/gputest/c"))
                .unwrap()
                .next()
                .is_none(),
            "the run directory outlived its archive"
        );

        // The same identity with --if-changed records nothing.
        let skipped = run(&s, true);
        assert_eq!(skipped.verdict, Verdict::Skipped);
        assert_eq!(lineage().run, first_id);

        // A changed identity compares with the last run. The carried timeout
        // stays a Timeout in the next baseline, so a third run carries it too
        // rather than calling it new.
        stamp_tree(&root.join("runner"), "runner", "v2");
        let second = run(&s, false);
        assert_eq!(second.verdict, Verdict::Pass, "{second}");
        assert_eq!(second.counts, counts(0, 2, 0));
        assert_eq!(lineage().baseline.as_deref(), Some(first_id.as_str()));
        stamp_tree(&root.join("runner"), "runner", "v3");
        let third = run(&s, false);
        assert_eq!(third.verdict, Verdict::Pass, "{third}");
        assert_eq!(third.counts, counts(0, 2, 0));
        let third_id = third.id.clone().unwrap();

        // Profile flags make a run compare-only: it compares with the lineage's
        // last run and is archived, but does not move the lineage.
        let review = ProfileFlags {
            build_dir: Some(root.join("build")),
            ..ProfileFlags::default()
        };
        fs::write(&raw, "t1,Fail\nt2,Fail\nt3,Timeout\n").unwrap();
        stamp_tree(&root.join("runner"), "runner", "v4");
        let compared = run(&session(&review), false);
        assert_eq!(compared.verdict, Verdict::Regression, "{compared}");
        assert!(compared.compare_only);
        assert_eq!(lineage().run, third_id);
        let compared_record = archive::read_run(
            &store::find(&config.store, "c", compared.id.as_deref().unwrap())
                .unwrap()
                .path,
        )
        .unwrap();
        assert!(compared_record.compare_only);
        assert_eq!(compared_record.baseline.as_deref(), Some(third_id.as_str()));

        // A fixed test is an improvement, and leaves the baseline.
        fs::write(&raw, "t1,Pass\nt2,Pass\nt3,Timeout\n").unwrap();
        stamp_tree(&root.join("runner"), "runner", "v5");
        let fixed = run(&s, false);
        assert_eq!(fixed.verdict, Verdict::Improved, "{fixed}");
        assert_eq!(fixed.counts, counts(0, 1, 1));
        assert_eq!(store::archives(&config.store, "c").unwrap().len(), 5);
    }

    #[test]
    fn a_layer_brings_its_own_build_only_from_another_project() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (ws, config) = fixture(root);
        let profile = ProfileFlags::default();
        let s = Session {
            config: &config,
            ws: &ws,
            profile: &profile,
            here: Some(root.to_path_buf()),
            caller: scrubbed_caller(),
            device: Device {
                pci: String::new(),
                id: "1002:150e".to_owned(),
                drm: String::new(),
                kernel: String::new(),
            },
        };
        let var = |prepared: &resolve::Prepared, name: &str| {
            prepared
                .env
                .get(std::ffi::OsStr::new(name))
                .map(|value| value.to_string_lossy().into_owned())
        };

        // Another project's build lands beneath the driver's, whose value wins.
        let other = resolve::prepare(&s, "other").unwrap();
        assert_eq!(var(&other, "SHARED").as_deref(), Some("driver"));
        assert_eq!(var(&other, "LAYER_ONLY").as_deref(), Some("1"));
        assert_eq!(var(&other, "LAYER_ENV").as_deref(), Some("on"));
        assert_eq!(var(&other, "WITS_DEVENV").as_deref(), Some("driver"));
        let record = other.layer.as_ref().unwrap();
        assert_eq!(record.project, "layer");
        assert_eq!(
            record.build.as_ref().unwrap().build_dir,
            root.join("layer-build")
        );

        // The driver's own project carries the layer in the driver's build.
        let same = resolve::prepare(&s, "same").unwrap();
        assert_eq!(var(&same, "LAYER_ONLY"), None);
        assert_eq!(var(&same, "LAYER_ENV").as_deref(), Some("same"));
        assert!(same.layer.as_ref().unwrap().build.is_none());

        // What a layer lists is its own component.
        let plain = resolve::prepare(&s, "c").unwrap();
        assert!(plain.layer.is_none());
        assert_eq!(plain.content.layer_libs, same.content.layer_libs);
        assert_ne!(plain.content.layer_libs, other.content.layer_libs);
        assert_eq!(plain.content.driver_libs, other.content.driver_libs);
    }

    #[test]
    fn a_runner_that_does_not_complete_is_archived_as_an_error_and_moves_nothing() {
        let _lock = crate::testutil::lock();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let _runtime = crate::testutil::EnvGuard::set("XDG_RUNTIME_DIR", root.join("run"));
        let _state = crate::testutil::EnvGuard::set("XDG_STATE_HOME", root.join("state"));
        let (ws, config) = fixture(root);
        write(
            &root.join("runner/piglit-runner"),
            "#!/bin/sh\nexit 0\n",
            0o755,
        );
        let profile = ProfileFlags::default();
        let s = Session {
            config: &config,
            ws: &ws,
            profile: &profile,
            here: Some(root.to_path_buf()),
            caller: scrubbed_caller(),
            device: Device {
                pci: String::new(),
                id: "1002:150e".to_owned(),
                drm: String::new(),
                kernel: String::new(),
            },
        };
        let report = run(&s, false);
        assert_eq!(report.verdict, Verdict::Error);
        assert!(
            report
                .detail
                .as_deref()
                .unwrap()
                .contains("without writing results.csv"),
            "{report}"
        );
        assert!(state::read("c", "1002-150e").unwrap().is_none());
        let archived = store::find(&config.store, "c", report.id.as_deref().unwrap()).unwrap();
        assert_eq!(
            archive::read_run(&archived.path).unwrap().verdict,
            Verdict::Error
        );
    }
}
