//! The path stage: turn a project + a [`Profile`] + a branch into concrete
//! checkouts and output paths, with no build layering applied.
//!
//! This is the stage a git hook, a branch cleanup, or `branch-build-dirs` asks:
//! it selects the toolchain (a path template may name it) and resolves every
//! path, but binds no org/project/preset/CLI configuration. `info --get`
//! deliberately stays on the full plan instead, because its contract includes
//! those layers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use minijinja::Value;

use crate::context::{self, Ctx};
use crate::decl::BranchStrategy;
use crate::layout;
use crate::plan::Toolchain;
use crate::profile::{anchor_of, identity_repo, BranchIdentity, Profile};
use crate::toolchain::{resolve_toolchain, select_toolchain};
use crate::workspace::{Project, Workspace};

/// Inputs that vary per invocation but are not part of the file model.
///
/// The branch and output-path overrides are *axes of the path stage*, which is
/// why they live here rather than on [`Profile`].
pub struct PathInput<'a> {
    pub profile: &'a Profile,
    /// The target branch (from `--branch` or the caller's git read). `None`
    /// explicitly selects the current detached `HEAD`; callers must never use it
    /// merely because branch discovery failed.
    pub branch: Option<&'a str>,
    /// A verbatim `--work-dir` override: use this checkout as the build base,
    /// bypassing the strategy's own resolution. Applies to the build repo only.
    pub work_dir: Option<&'a Path>,
    /// Verbatim CLI output-path overrides.
    pub build_dir: Option<&'a Path>,
    pub install_dir: Option<&'a Path>,
}

/// Everything the path stage resolves.
pub struct ResolvedPaths {
    pub focus: String,
    /// The repo the build sources from (the focus's `anchor`, or the focus).
    pub build_repo: String,
    /// The nearest own-git repo from the focus — what carries branch identity
    /// and is switched to the target branch.
    pub identity_repo: Option<String>,
    pub strategy: BranchStrategy,
    /// `None` means the caller explicitly requested a detached-HEAD build.
    pub branch: Option<BranchIdentity>,
    pub build_type: String,
    pub generator: Option<String>,
    pub toolchain: Option<Toolchain>,
    /// The resolved checkout of `build_repo`, also exposed to templates as
    /// `repos.<build_repo>.workdir`.
    pub work_dir: PathBuf,
    /// Every repo's resolved checkout, by name — the same values bound as
    /// `repos.<name>.workdir`, kept typed. A repo's `path` may be a bare git-dir;
    /// this is always a checkout.
    pub work_dirs: BTreeMap<String, PathBuf>,
    /// Where the backend configures from: the build repo's `source_dir`
    /// template, or the build repo's workdir when unset.
    pub source_dir: PathBuf,
    /// The resolved focus-over-anchor `build_dir`, if either declared one.
    pub build_dir: Option<PathBuf>,
    /// The resolved focus-over-anchor `install_dir`, if either declared one.
    pub install_dir: Option<PathBuf>,
    /// The path-stage context. `Clone` drops the memo and is safe to reuse.
    pub(crate) ctx: Ctx,
}

impl ResolvedPaths {
    /// One repo's resolved checkout — see [`work_dirs`](Self::work_dirs).
    pub fn work_dir_of(&self, repo: &str) -> Option<&Path> {
        self.work_dirs.get(repo).map(PathBuf::as_path)
    }

    /// One resolved scalar from the path-stage context.
    ///
    /// Scalars only: a table or list is an error, and booleans spell `true`/
    /// `false` as TOML does. One failure fails the whole query.
    pub fn get_scalar(&self, path: &str) -> anyhow::Result<String> {
        self.ctx
            .get_scalar(path)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Resolve the paths of one project for one branch.
pub fn resolve_paths(
    ws: &Workspace,
    project: &Project,
    input: &PathInput<'_>,
) -> Result<ResolvedPaths> {
    let profile = input.profile;
    let focus = project.focus_name(profile.focus.as_deref()).to_owned();
    if !project.repos.contains_key(&focus) {
        bail!(
            "focus repo '{focus}' is not defined in project '{}'",
            project.name()
        );
    }
    let build_repo = anchor_of(project, &focus);
    if !project.repos.contains_key(&build_repo) {
        bail!("anchor repo '{build_repo}' (of focus '{focus}') is not defined");
    }
    let identity_repo = identity_repo(project, &focus);
    let strategy = project.repos[&build_repo].decl.strategy;

    let branch = input.branch.map(|raw| BranchIdentity {
        raw: raw.to_owned(),
        slug: context::path_slug(raw),
    });
    let build_type = profile
        .build_type
        .clone()
        .unwrap_or_else(|| "debug".to_owned());
    let generator = profile
        .generator
        .clone()
        .or_else(|| project.settings.generator.clone());

    // Toolchain *selection* always happens (path templates depend on the name).
    let toolchain = select_toolchain(ws, project, profile)?;

    // --- Base context ------------------------------------------------------
    let mut ctx = context::plan_base(ws, project, &focus);
    // The Profile-free context initially carries configured repository paths.
    // Once a full plan is available, nested repos under a bare-backed main must
    // instead point into main's bootstrap checkout.
    for repo in project.repos.values() {
        let primary = layout::primary_path(ws, project, repo)?;
        ctx.set(
            &format!("repos.{}.path", repo.name()),
            Value::from(primary.display().to_string()),
        );
        if repo.name() == focus {
            ctx.set("repo.path", Value::from(primary.display().to_string()));
        }
    }
    if let Some(branch) = &branch {
        ctx.set("branch.raw", Value::from(&branch.raw));
        ctx.set("branch.slug", Value::from(&branch.slug));
    }
    ctx.set("build_type", Value::from(&build_type));
    if let Some(gen) = &generator {
        ctx.set("generator", Value::from(gen));
    }
    // CLI-registered `--spec K=V` values, as the `spec.*` namespace. Only what
    // was passed is bound, so a template that references an unsupplied
    // `{{spec.X}}` fails loudly.
    for (key, value) in &profile.specs {
        ctx.set(&format!("spec.{key}"), Value::from(value));
    }

    // Resolve the toolchain against this context and expose it as toolchain.*.
    // A toolchain field may reference `{{build_type}}`, `{{generator}}`, or
    // `{{branch.*}}`, so this order is a contract.
    let toolchain = match toolchain {
        Some((name, raw)) => Some(resolve_toolchain(&mut ctx, name, &raw)?),
        None => None,
    };

    // --- Paths -------------------------------------------------------------
    let mut work_dirs = BTreeMap::new();
    for repo in project.repos.values() {
        let dir = match (input.work_dir, repo.name() == build_repo, input.branch) {
            (Some(dir), true, _) => dir.to_path_buf(),
            (_, _, Some(branch)) => layout::work_dir(ws, project, repo, branch)?,
            (_, _, None) => layout::detached_work_dir(ws, project, repo, &ctx)?,
        };
        ctx.set(
            &format!("repos.{}.workdir", repo.name()),
            Value::from(dir.display().to_string()),
        );
        // `repo.*` is the relative alias for the repo being resolved, so it
        // mirrors the focus's entry.
        if repo.name() == focus {
            ctx.set("repo.workdir", Value::from(dir.display().to_string()));
        }
        work_dirs.insert(repo.name().to_owned(), dir);
    }
    let work_dir = work_dirs[&build_repo].clone();
    if branch.is_none() {
        layout::validate_detached_head(project, identity_repo.as_deref(), &work_dirs)?;
    }

    // The anchor owns the configure source. Output paths default there too, but
    // the focus may override them. Each resolved output is bound back into the
    // context as it lands, so a later output may name an earlier one
    // (`install_dir = "{{build_dir}}"`). The order is therefore a contract:
    // source → build → install.
    let build = &project.repos[&build_repo];
    let focused = &project.repos[&focus];
    let source_dir = match &build.decl.source_dir {
        Some(tpl) => PathBuf::from(ctx.render(tpl)?),
        None => work_dir.clone(),
    };
    ctx.set("source_dir", Value::from(source_dir.display().to_string()));
    let build_template = focused
        .decl
        .build_dir
        .as_ref()
        .or(build.decl.build_dir.as_ref());
    let build_dir = match (input.build_dir, build_template) {
        (Some(dir), _) => Some(dir.to_path_buf()),
        (None, Some(tpl)) => Some(PathBuf::from(ctx.render(tpl)?)),
        (None, None) => None,
    };
    if let Some(dir) = &build_dir {
        ctx.set("build_dir", Value::from(dir.display().to_string()));
    }
    let install_template = focused
        .decl
        .install_dir
        .as_ref()
        .or(build.decl.install_dir.as_ref());
    let install_dir = match (input.install_dir, install_template) {
        (Some(dir), _) => Some(dir.to_path_buf()),
        (None, Some(tpl)) => Some(PathBuf::from(ctx.render(tpl)?)),
        (None, None) => None,
    };
    if let Some(dir) = &install_dir {
        ctx.set("install_dir", Value::from(dir.display().to_string()));
    }

    Ok(ResolvedPaths {
        focus,
        build_repo,
        identity_repo,
        strategy,
        branch,
        build_type,
        generator,
        toolchain,
        work_dir,
        work_dirs,
        source_dir,
        build_dir,
        install_dir,
        ctx,
    })
}

// --- branch-identified build directories ---------------------------------------

/// A build directory that exists on disk and is identified by one branch of one
/// checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchBuildDir {
    /// The owning project, as `org/name`.
    pub project: String,
    /// The resolved directory.
    pub path: PathBuf,
}

/// A build type substituted into a template purely to locate where the axis
/// lands in the rendered path. Chosen so it cannot collide with a real value or
/// a path separator.
const BUILD_TYPE_PROBE: &str = "\u{1}wits-build-type\u{1}";

/// A branch substituted into a template to test whether the result moves with
/// the branch at all.
const BRANCH_PROBE: &str = "wits-branch-probe-a9f3c1";

/// Every build directory on disk whose identity depends on `branch` of the
/// checkout at `repo_path` — across *all* projects, borrows included.
///
/// Two derived conditions must hold: the branch must identify the project's
/// build (its focus's checkout is the one in question), and the resolved path
/// must actually move with the branch (a probe branch renders elsewhere). Only
/// existing directories are returned, with the `build_type` axis expanded
/// across whatever is present.
pub fn branch_build_dirs(ws: &Workspace, repo_path: &Path, branch: &str) -> Vec<BranchBuildDir> {
    let mut out = Vec::new();
    for project in ws.projects() {
        let focus = project.focus_name(None);
        let Some(identity) = identity_repo(project, focus) else {
            continue;
        };
        if !ws.repo_checkout_contains(project, &identity, repo_path) {
            continue;
        }
        let Some(rendered) = build_dir_for(ws, project, focus, branch, BUILD_TYPE_PROBE) else {
            continue;
        };
        // Branch-independent templates resolve to the same path for any branch,
        // so a probe that lands somewhere else proves the dependence.
        match build_dir_for(ws, project, focus, BRANCH_PROBE, BUILD_TYPE_PROBE) {
            Some(probe) if probe != rendered => {}
            _ => continue,
        }
        for path in expand_build_type(&rendered) {
            out.push(BranchBuildDir {
                project: project.key(),
                path,
            });
        }
    }
    out
}

/// One project's `build_dir` resolved for a branch and build type, or `None`
/// when the project declares none or the paths do not resolve.
fn build_dir_for(
    ws: &Workspace,
    project: &Project,
    focus: &str,
    branch: &str,
    build_type: &str,
) -> Option<PathBuf> {
    let profile = Profile {
        focus: Some(focus.to_owned()),
        build_type: Some(build_type.to_owned()),
        ..Default::default()
    };
    let input = PathInput {
        profile: &profile,
        branch: Some(branch),
        work_dir: None,
        build_dir: None,
        install_dir: None,
    };
    resolve_paths(ws, project, &input).ok()?.build_dir
}

/// The directories matching `rendered` once the [`BUILD_TYPE_PROBE`] in it is
/// replaced by whatever build types are present on disk.
fn expand_build_type(rendered: &Path) -> Vec<PathBuf> {
    let as_str = rendered.to_string_lossy();
    if !as_str.contains(BUILD_TYPE_PROBE) {
        return if rendered.is_dir() {
            vec![rendered.to_path_buf()]
        } else {
            Vec::new()
        };
    }

    let mut head = PathBuf::new();
    let mut component = None;
    let mut tail = PathBuf::new();
    for part in rendered.components() {
        let part = part.as_os_str().to_string_lossy().into_owned();
        match &component {
            None if part.contains(BUILD_TYPE_PROBE) => component = Some(part),
            None => head.push(part),
            Some(_) => tail.push(part),
        }
    }
    let Some(component) = component else {
        return Vec::new();
    };
    let Some((prefix, suffix)) = component.split_once(BUILD_TYPE_PROBE) else {
        return Vec::new();
    };

    let Ok(entries) = std::fs::read_dir(&head) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() <= prefix.len() + suffix.len()
            || !name.starts_with(prefix)
            || !name.ends_with(suffix)
        {
            continue;
        }
        // What the name implies the build type was. It has to look like one: a
        // build type is a lowercase word, so a value carrying a separator means
        // this directory belongs to a *different* branch whose slug merely
        // starts with ours.
        let implied = &name[prefix.len()..name.len() - suffix.len()];
        if !implied
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            continue;
        }
        let mut candidate = head.join(&name);
        if !tail.as_os_str().is_empty() {
            candidate.push(&tail);
        }
        if candidate.is_dir() {
            out.push(candidate);
        }
    }
    out.sort();
    out
}
