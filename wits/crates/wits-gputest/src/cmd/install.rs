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

#[derive(Debug, Default)]
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

    let targets = targets(&ws, &config, &selected)?;

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

/// Group the selected suites by the project that has to be installed: the
/// suite's project contributes the assets to check, the runner's project the
/// binary its kind names. One build per project, and no second list to keep in
/// step with the suites.
fn targets(
    ws: &Workspace,
    config: &Config,
    selected: &[String],
) -> Result<BTreeMap<String, Target>> {
    let mut targets: BTreeMap<String, Target> = BTreeMap::new();
    for suite_name in selected {
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
        let suite_root = resolve::install_root(ws, suite_project)?;
        let runner_root = resolve::install_root(ws, runner_project)?;
        resolve::ensure_separate_roots(&suite_root, &runner_root)
            .with_context(|| format!("suite '{}'", suite.name))?;

        targets
            .entry(runner_project.key())
            .or_default()
            .runner_kinds
            .insert(suite.kind());
    }
    Ok(targets)
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
            .shape
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// A registry with a suite and a runner project, and a config naming one
    /// piglit suite. The paths need not exist: `targets` only resolves them.
    fn fixture(root: &Path, runner_install: &Path) -> (Workspace, Config) {
        let projects = root.join("projects");
        fs::create_dir_all(&projects).unwrap();
        for (name, install) in [
            ("suite", root.join("suite-install")),
            ("runner", runner_install.to_path_buf()),
        ] {
            fs::write(
                projects.join(format!("{name}.toml")),
                format!(
                    "[project]\n[repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
                     install_dir = \"{}\"\n",
                    root.join(format!("{name}-src")).display(),
                    install.display()
                ),
            )
            .unwrap();
        }

        let config_root = root.join("config");
        fs::create_dir_all(&config_root).unwrap();
        fs::write(
            config_root.join("gputest.toml"),
            format!(
                "[store]\nroot = \"{}\"\n\n\
                 [suite.s]\nkind = \"piglit\"\nproject = \"suite\"\nrunner = \"runner\"\n\
                 case_list = [\"quick\"]\n\n[suite.s.piglit]\nfolder = \"lib64/piglit\"\n",
                root.join("store").display()
            ),
        )
        .unwrap();

        (
            Workspace::load_from(&projects).unwrap(),
            Config::load_from(&config_root).unwrap(),
        )
    }

    #[test]
    fn install_groups_suite_assets_and_runner_kinds_one_build_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (ws, config) = fixture(root, &root.join("runner-install"));

        let targets = targets(&ws, &config, &["s".to_owned()]).unwrap();
        assert_eq!(targets.len(), 2, "{targets:?}");
        assert_eq!(targets["suite"].suite_assets, ["s"]);
        assert!(targets["suite"].runner_kinds.is_empty());
        assert_eq!(
            targets["runner"]
                .runner_kinds
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [Kind::Piglit]
        );
    }

    #[test]
    fn install_refuses_overlapping_suite_and_runner_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (ws, config) = fixture(root, &root.join("suite-install/runner"));

        let err = targets(&ws, &config, &["s".to_owned()]).unwrap_err();
        assert!(format!("{err:#}").contains("overlap"), "{err:#}");
    }
}
