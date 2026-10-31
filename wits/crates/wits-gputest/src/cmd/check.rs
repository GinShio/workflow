//! `wits gputest check` — validate config, registry, assets and stamps without
//! composing a driver or running anything.
//!
//! It reports every class of problem it can, not just the first: a broken
//! config or suite should fail before a three-hour run.

use anyhow::{bail, Result};

use wits_project::{Project, Workspace};

use crate::config::{Config, Suite};
use crate::resolve;
use crate::stamp;
use crate::util;

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let ws = Workspace::load()?;
    let mut problems = Vec::new();

    for driver in config.drivers.values() {
        if let Err(err) = ws.project(&driver.project) {
            problems.push(format!(
                "driver '{}': project '{}' is not in the registry: {err:#}",
                driver.name, driver.project
            ));
        }
    }

    for suite in config.suites.values() {
        let suite_project = match ws.project(&suite.project) {
            Ok(project) => Some(project),
            Err(err) => {
                problems.push(format!(
                    "suite '{}': project '{}' is not in the registry: {err:#}",
                    suite.name, suite.project
                ));
                None
            }
        };
        let runner_project = match ws.project(&suite.runner) {
            Ok(project) => Some(project),
            Err(err) => {
                problems.push(format!(
                    "suite '{}': runner project '{}' is not in the registry: {err:#}",
                    suite.name, suite.runner
                ));
                None
            }
        };
        if let (Some(suite_project), Some(runner_project)) = (&suite_project, &runner_project) {
            if let (Ok(suite_root), Ok(runner_root)) = (
                resolve::install_root(&ws, suite_project),
                resolve::install_root(&ws, runner_project),
            ) {
                if let Err(err) = resolve::ensure_separate_roots(&suite_root, &runner_root) {
                    problems.push(format!("suite '{}': {err:#}", suite.name));
                }
            }
        }
        if let Some(project) = suite_project {
            check_suite_assets(&ws, project, suite, &mut problems);
        }
        if let Some(project) = runner_project {
            check_runner(&ws, project, suite, &mut problems);
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    for problem in &problems {
        eprintln!("error: {problem}");
    }
    bail!("{} problem(s) found", problems.len())
}

fn check_suite_assets(
    ws: &Workspace,
    project: &Project,
    suite: &Suite,
    problems: &mut Vec<String>,
) {
    let root = match resolve::install_root(ws, project) {
        Ok(root) => root,
        Err(err) => {
            problems.push(format!("suite '{}': {err:#}", suite.name));
            return;
        }
    };
    if let Err(err) = stamp::read(&root, &project.key()) {
        problems.push(format!("suite '{}': {err:#}", suite.name));
        return;
    }
    if let Err(err) = suite.kind.check_assets(suite, &root) {
        problems.push(format!("suite '{}': {err:#}", suite.name));
    }
}

fn check_runner(ws: &Workspace, project: &Project, suite: &Suite, problems: &mut Vec<String>) {
    let root = match resolve::install_root(ws, project) {
        Ok(root) => root,
        Err(err) => {
            problems.push(format!("suite '{}': runner: {err:#}", suite.name));
            return;
        }
    };
    if let Err(err) = stamp::read(&root, &project.key()) {
        problems.push(format!("suite '{}': runner: {err:#}", suite.name));
        return;
    }
    let binary = root.join(suite.kind.runner_bin());
    if !binary.is_file() {
        problems.push(format!(
            "suite '{}': runner binary {} is missing; install with `wits gputest install {}`",
            suite.name,
            binary.display(),
            suite.name
        ));
    } else {
        match util::is_executable(&binary) {
            Ok(true) => {}
            Ok(false) => problems.push(format!(
                "suite '{}': runner binary {} is not executable",
                suite.name,
                binary.display()
            )),
            Err(err) => problems.push(format!(
                "suite '{}': runner binary {}: {err:#}",
                suite.name,
                binary.display()
            )),
        }
    }
}
