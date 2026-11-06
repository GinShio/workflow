//! `wits gputest install` — the installation act.
//!
//! `run` never installs. This verb derives the projects from the selected
//! suites, the runner projects included; builds each as `release` on its main
//! branch with the install phase; checks the installed layout; and only then
//! writes the stamp. The old stamp goes before the build, so a failed install
//! leaves no stamp for a run to mistake for a good tree. Each project installs
//! on its own: one that fails does not keep the others from installing, the
//! way one case's failure does not stop a run.

use std::collections::btree_map::{BTreeMap, Entry};

use anyhow::{bail, Context, Result};

use wits_actions::{build, BuildMode, BuildOptions, BuildRequest, Identity};
use wits_project::{Profile, Project, Workspace};

use crate::cmd::InstallArgs;
use crate::config::Config;
use crate::content;
use crate::resolve::{self, InstallPaths};
use crate::stamp::{self, Source, Stamp};
use crate::suite::Suite;
use crate::time;

/// One project to install, and what its installed tree must hold.
struct Target<'a> {
    project: &'a Project,
    paths: InstallPaths,
    /// Suites whose assets the tree carries.
    suites: Vec<&'a Suite>,
    /// Suites whose runner binary the tree carries.
    runs: Vec<&'a Suite>,
}

pub fn run(args: &InstallArgs) -> Result<()> {
    let config = Config::load()?;
    let ws = Workspace::load()?;
    let selected: Vec<&Suite> = if args.suites.is_empty() {
        config.suites.values().collect()
    } else {
        args.suites
            .iter()
            .map(|name| config.suite(name))
            .collect::<Result<_>>()?
    };
    if selected.is_empty() {
        println!("no suites declared; nothing to install");
        return Ok(());
    }
    let targets = targets(&ws, &selected)?;

    if wits_core::log::is_dry_run() {
        for target in &targets {
            println!(
                "would build: wits build '{}' -B release -b '{}' --install",
                target.project.key(),
                target.paths.branch
            );
            println!("would stamp: {}", stamp::path(&target.paths.root).display());
        }
        return Ok(());
    }

    let mut failed = 0;
    for target in &targets {
        match install(&ws, target) {
            Ok(()) => println!(
                "installed {} into {}",
                target.project.key(),
                target.paths.root.display()
            ),
            Err(err) => {
                eprintln!(
                    "error: installing project '{}': {err:#}",
                    target.project.key()
                );
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} project(s) failed to install", targets.len());
    }
    Ok(())
}

/// One target per project, with the suites that need its tree. The roots are
/// checked pairwise before anything builds: each stamp hashes its whole tree,
/// so a root inside another would make that project's identity move whenever
/// the other one installs.
fn targets<'a>(ws: &'a Workspace, suites: &[&'a Suite]) -> Result<Vec<Target<'a>>> {
    let mut by_key: BTreeMap<String, Target<'a>> = BTreeMap::new();
    for &suite in suites {
        for (name, runner) in [(&suite.project, false), (&suite.runner, true)] {
            let project = ws.project(name).with_context(|| {
                format!(
                    "suite '{}': project '{name}' is not in the registry",
                    suite.name
                )
            })?;
            let target = match by_key.entry(project.key()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(Target {
                    project,
                    paths: resolve::install_paths(ws, project)?,
                    suites: Vec::new(),
                    runs: Vec::new(),
                }),
            };
            if runner {
                target.runs.push(suite);
            } else {
                target.suites.push(suite);
            }
        }
    }
    let targets: Vec<Target<'a>> = by_key.into_values().collect();
    for (index, a) in targets.iter().enumerate() {
        for b in &targets[index + 1..] {
            let (root_a, root_b) = (
                wits_core::path::real(&a.paths.root),
                wits_core::path::real(&b.paths.root),
            );
            if root_a.starts_with(&root_b) || root_b.starts_with(&root_a) {
                bail!(
                    "the install roots of '{}' ({}) and '{}' ({}) overlap; each stamp hashes \
                     its whole tree, so give every suite and runner project its own install_dir, \
                     for example $XDG_RUNTIME_DIR/wits/gputest/<name>",
                    a.project.key(),
                    a.paths.root.display(),
                    b.project.key(),
                    b.paths.root.display()
                );
            }
        }
    }
    Ok(targets)
}

fn install(ws: &Workspace, target: &Target<'_>) -> Result<()> {
    let root = &target.paths.root;
    stamp::remove(root)?;
    build(
        ws,
        target.project,
        &BuildRequest {
            identity: Identity {
                profile: Profile {
                    build_type: Some("release".to_owned()),
                    ..Profile::default()
                },
                branch: Some(target.paths.branch.clone()),
                work_dir: None,
                build_dir: None,
                install_dir: None,
                here: None,
            },
            options: BuildOptions {
                mode: BuildMode::Auto,
                install: true,
                ..BuildOptions::default()
            },
        },
    )?;
    for suite in &target.suites {
        suite
            .check_assets(root)
            .with_context(|| format!("checking the installed layout of suite '{}'", suite.name))?;
    }
    for suite in &target.runs {
        resolve::check_runner(root, suite)?;
    }
    let stamp_path = stamp::path(root);
    stamp::write(
        root,
        &Stamp {
            hash: content::hash_tree(root, Some(&stamp_path))?,
            installed: time::now_run_id(),
            source: Source {
                project: target.project.key(),
                branch: target.paths.branch.clone(),
                commits: resolve::commits(&target.paths.work_dirs, Some(&target.paths.branch)),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// A registry with a suite and a runner project and a config naming one
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
                 [suite.s]\nproject = \"suite\"\nrunner = \"runner\"\n\n\
                 [suite.s.groups]\nquick = [\"quick\"]\n\n\
                 [suite.s.piglit]\nfolder = \"lib64/piglit\"\n",
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
    fn install_builds_each_project_once_with_what_its_tree_must_hold() {
        let dir = tempfile::tempdir().unwrap();
        let (ws, config) = fixture(dir.path(), &dir.path().join("runner-install"));
        let suites: Vec<&Suite> = config.suites.values().collect();
        let targets = targets(&ws, &suites).unwrap();
        let summary: Vec<_> = targets
            .iter()
            .map(|t| {
                (
                    t.project.key(),
                    t.suites.len(),
                    t.runs.len(),
                    t.paths.branch.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("runner".to_owned(), 0, 1, "main".to_owned()),
                ("suite".to_owned(), 1, 0, "main".to_owned()),
            ]
        );
    }

    #[test]
    fn install_refuses_overlapping_roots() {
        let dir = tempfile::tempdir().unwrap();
        let (ws, config) = fixture(dir.path(), &dir.path().join("suite-install/runner"));
        let suites: Vec<&Suite> = config.suites.values().collect();
        let err = targets(&ws, &suites)
            .err()
            .expect("overlapping roots were accepted");
        assert!(format!("{err:#}").contains("overlap"), "{err:#}");
    }
}
