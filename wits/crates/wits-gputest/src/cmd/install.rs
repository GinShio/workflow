//! `wits gputest install` — the installation act.
//!
//! `run` never installs. This verb derives the project list from the suites
//! (there is no second list), always builds `release` on the project's main
//! branch, verifies the installed layout with the kind's `check_assets`, and
//! only then writes the stamp. The old stamp is removed before the build, so a
//! failed install leaves no stamp for a later run to mistake for a good tree.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};

use wits_actions::{build, BuildMode, BuildOptions, BuildRequest, Identity};
use wits_project::{resolve_paths, PathInput, Profile, Project, Workspace};

use crate::cmd::InstallArgs;
use crate::config::{Config, Suite};
use crate::content;
use crate::kinds::Kind;
use crate::resolve;
use crate::stamp::{self, Stamp, StampProject};
use crate::time;
use crate::util;

#[derive(Default)]
struct Target {
    suite_assets: Vec<String>,
    runner_kinds: BTreeSet<Kind>,
}

pub fn run(args: &InstallArgs) -> Result<()> {
    let config = Config::load()?;
    let ws = Workspace::load()?;

    let selected: Vec<String> = if args.suites.is_empty() {
        config.suites.keys().cloned().collect()
    } else {
        for name in &args.suites {
            config.suite(name)?;
        }
        args.suites
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    if selected.is_empty() {
        println!("no suites declared; nothing to install");
        return Ok(());
    }

    let mut targets: BTreeMap<String, Target> = BTreeMap::new();
    for suite_name in &selected {
        let suite = config.suite(suite_name)?;
        let suite_project = ws
            .project(&suite.project)
            .with_context(|| format!("suite '{}': project '{}'", suite.name, suite.project))?;
        targets
            .entry(suite_project.key())
            .or_default()
            .suite_assets
            .push(suite.name.clone());

        let runner_project = ws.project(&suite.runner).with_context(|| {
            format!("suite '{}': runner project '{}'", suite.name, suite.runner)
        })?;
        // Refuse an overlapping layout before any build or stamp removal. The
        // stamps hash whole install trees, so nested roots would make one
        // project's identity move when the other installs.
        let suite_root = resolve::install_root(&ws, suite_project)?;
        let runner_root = resolve::install_root(&ws, runner_project)?;
        resolve::ensure_separate_roots(&suite_root, &runner_root)
            .with_context(|| format!("suite '{}'", suite.name))?;

        targets
            .entry(runner_project.key())
            .or_default()
            .runner_kinds
            .insert(suite.kind);
    }

    for (key, target) in targets {
        let project = ws.project(&key)?;
        let main = resolve::project_main_branch(project)?;
        let root = resolve::install_root(&ws, project)?;
        if wits_core::log::is_dry_run() {
            println!(
                "would build: wits build '{}' -B release -b '{}' --install",
                project.key(),
                main
            );
            println!("would stamp: {}", stamp::path(&root).display());
            continue;
        }
        install_one(&ws, &config, project, &main, &root, &target)?;
    }
    Ok(())
}

fn install_one(
    ws: &Workspace,
    config: &Config,
    project: &Project,
    main: &str,
    root: &std::path::Path,
    target: &Target,
) -> Result<()> {
    // No stamp survives a new build until the layout check has passed.
    stamp::remove(root)?;

    let identity = Identity {
        profile: Profile {
            build_type: Some("release".to_owned()),
            ..Profile::default()
        },
        branch: Some(main.to_owned()),
        work_dir: None,
        build_dir: None,
        install_dir: None,
        here: None,
    };
    let request = BuildRequest {
        identity,
        options: BuildOptions {
            mode: BuildMode::Auto,
            install: true,
            ..BuildOptions::default()
        },
    };
    build(ws, project, &request)
        .with_context(|| format!("installing project '{}'", project.key()))?;

    for suite_name in &target.suite_assets {
        let suite: &Suite = config.suite(suite_name)?;
        suite
            .kind
            .check_assets(suite, root)
            .with_context(|| format!("checking the installed layout of suite '{}'", suite.name))?;
    }
    for kind in &target.runner_kinds {
        let binary = root.join(kind.runner_bin());
        if !binary.is_file() {
            bail!(
                "runner binary {} is missing after installing project '{}'",
                binary.display(),
                project.key()
            );
        }
        if !util::is_executable(&binary)? {
            bail!(
                "runner binary {} is not executable after installing project '{}'",
                binary.display(),
                project.key()
            );
        }
    }

    let paths = resolve_paths(
        ws,
        project,
        &PathInput {
            profile: &Profile {
                build_type: Some("release".to_owned()),
                ..Profile::default()
            },
            branch: Some(main),
            work_dir: None,
            build_dir: None,
            install_dir: None,
            here: None,
        },
    )
    .with_context(|| {
        format!(
            "resolving the installed paths of project '{}'",
            project.key()
        )
    })?;
    let stamp = Stamp {
        hash: content::hash_tree(root, &[stamp::path(root)])?,
        installed: time::now_run_id(),
        project: StampProject {
            key: project.key(),
            branch: Some(main.to_owned()),
            commits: resolve::commits(&paths.work_dirs, Some(main)),
        },
    };
    stamp::write(root, &stamp)?;
    println!("installed {} into {}", project.key(), root.display());
    Ok(())
}
