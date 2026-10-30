//! `project info` — describe one project, or project one resolved value out of
//! its plan with `--get`.

use anyhow::{Context, Result};
use clap::Args;

use wits_build::backend_for;
use wits_core::git;
use wits_project::{
    anchor_of, layout, resolve_plan, value_to_string, DevenvOp, Project, Workspace,
    DEVENV_SEPARATOR,
};

use super::common::{branch_or_current, paths_input, plan_for_query, resolve_repo};
use crate::cmd::identity::ProfileArgs;

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Project name, or a path inside one (default: the project owning the
    /// current directory).
    #[arg(value_name = "NAME|PATH")]
    pub target: Option<String>,
    /// Print one resolved value instead of the description: a dotted path into
    /// the same template context the config files are written against, so
    /// `build_dir`, `repos.main.workdir`, `toolchain.linker` and
    /// `repo.main_branch` are all spelled here exactly as a template spells
    /// them. Repeatable — each path prints one line, in the order given, so a
    /// script reads several answers out of one resolve instead of paying for a
    /// registry load per question.
    ///
    /// The value must be a scalar: a table or a list is not one line, and
    /// rendering it as several would break reading the lines back positionally.
    /// Use the description for those.
    #[arg(long = "get", value_name = "PATH")]
    pub get: Vec<String>,
}

pub(super) fn run(ws: &Workspace, args: &InfoArgs, profile: &ProfileArgs) -> Result<()> {
    if !args.get.is_empty() {
        return get_paths(ws, args, profile);
    }
    let (project, repo) = resolve_repo(ws, args.target.as_deref(), profile.focus.as_deref())?;
    describe(ws, project, &repo, profile)
}

/// `info --get <path>…`: one resolved value per path, one line each, in the
/// order given.
///
/// The path is a dotted lookup into the plan's own template context — the same
/// namespace the config files are written against — so there is no second
/// vocabulary to define, document, or keep from drifting: `build_dir`,
/// `repos.main.workdir` and `repo.main_branch` mean here exactly what they mean
/// in a `[repos.*]` table. That also makes every value a *resolved* one, since
/// the context resolves a binding that is itself a template on the way out.
///
/// Two rules, both in service of the single-line contract that lets a caller
/// read N lines back positionally:
///
///   - a non-scalar is refused rather than flattened. A table or a list is not
///     one line, and joining one would hand back a string that no longer
///     round-trips to the value it came from;
///   - one failing path fails the whole invocation. Printing a blank line for it
///     would keep the *positions* right while leaving the caller unable to tell
///     an empty value from an absent one, which is worse than not answering.
fn get_paths(ws: &Workspace, args: &InfoArgs, profile: &ProfileArgs) -> Result<()> {
    let (project, plan) = plan_for_query(ws, args.target.as_deref(), profile)?;
    // Resolve every path before printing any: a later failure must not leave a
    // caller holding a partial, positionally-misread answer on stdout.
    let mut values = Vec::with_capacity(args.get.len());
    for path in &args.get {
        let value = plan
            .get_scalar(path)
            .with_context(|| format!("project '{}': --get {path}", project.key()))?;
        values.push(value);
    }
    for value in values {
        println!("{value}");
    }

    Ok(())
}

/// `info`: everything known about one project, in four sections — what it is,
/// its repos and their git state, the path *templates* it declares, and the
/// resolution of those templates for one branch.
///
/// Templates and their resolution are both shown, rather than one or the other.
/// They answer different questions ("what did I write" versus "what does it come
/// out as"), and a mismatch between them is exactly the bug this output exists to
/// make visible; showing only the resolved form hides the declaration that
/// produced it, and only the template hides everything the profile contributes.
/// Resolution still needs a branch, so that section is what degrades when none
/// can be discovered — the templates are always printed. A path target resolves
/// to the repo containing it, exactly as `--get` does, so the description and a
/// projection out of the same plan cannot disagree about which repo is meant.
fn describe(ws: &Workspace, project: &Project, focus: &str, profile: &ProfileArgs) -> Result<()> {
    println!("project: {}", project.key());
    println!("  source:       {}", project.source().display());
    if let Some(org) = project.org() {
        println!("  org:          {org}");
    }
    println!("  focus:        {focus}");
    if let Some(bs) = project.settings().build_system {
        println!("  build_system: {}", bs.as_str());
    }
    if let Some(gen) = &project.settings().generator {
        println!("  generator:    {gen}");
    }
    if let Some(tc) = &project.settings().toolchain {
        println!("  toolchain:    {tc}");
    }
    if !project.settings().default_presets.is_empty() {
        println!(
            "  default_presets: {}",
            project.settings().default_presets.join(" ")
        );
    }

    println!();
    println!("repos:");
    for (name, repo) in project.repos() {
        let kind = project.kind_of(name).map(|k| k.as_str()).unwrap_or("?");
        // A path template that fails to resolve is a real config error; surface
        // it inline rather than letting `unwrap_or_default()` render an empty
        // path that then masquerades as a plain "<not cloned>" repo.
        let path = match layout::primary_path(ws, project, name) {
            Ok(path) => path,
            Err(e) => {
                println!("  {name:<12} {kind:<10} <path error: {e}>");
                continue;
            }
        };
        let git = git::Repository::new(&path);
        let state = if git.git_dir().is_some() {
            let branch = git.current_branch().unwrap_or_else(|| "-".into());
            let commit = git.head_commit().unwrap_or_else(|| "-".into());
            if git.is_bare() {
                format!("bare ({branch} @ {commit})")
            } else {
                format!("{branch} @ {commit}")
            }
        } else {
            "<not cloned>".into()
        };
        println!("  {name:<12} {kind:<10} {state:<24} {}", path.display());
        // Where a repo's identity came from, and what its checkout leaves out —
        // both invisible in the path alone, and both change what you are looking
        // at (a borrowed repo is someone else's to update).
        if let Some(from) = &repo.decl().from {
            let spec = if from.repo == "main" {
                from.project.clone()
            } else {
                format!("{}:{}", from.project, from.repo)
            };
            println!("      borrowed from {spec}");
        }
        if let Some(anchor) = &repo.decl().anchor {
            println!("      anchor        {anchor}");
        }
        if let Some(mb) = &repo.decl().main_branch {
            println!("      main_branch   {mb}");
        }
        // Report what the owning project declared, including an explicit
        // `in-place`: a repo that writes the default is stating an intent, and
        // leaving the line out would hide it. A repo that declares nothing keeps
        // the line out, since its behaviour is the default.
        if repo.decl().strategy_declared {
            let strategy = repo.decl().strategy.as_str();
            println!("      strategy      {strategy}");
        }
        // Remotes decide where `update` fetches from and where a stack pushes, and
        // a role held by a free name is invisible in the URL alone.
        for (remote, spec) in &repo.decl().remotes {
            let role = spec.role.map(|r| r.as_str()).unwrap_or("");
            let role = if role.is_empty() {
                String::new()
            } else {
                format!("  [{role}]")
            };
            println!("      remote        {remote:<10} {}{role}", spec.url);
            for mirror in &spec.mirrors {
                println!("        mirror      {mirror}");
            }
        }
        if !repo.decl().skip.is_empty() {
            println!("      skip          {}", repo.decl().skip.join(" "));
        }
        for wt in git.worktrees() {
            if wt.path != path {
                let b = wt.branch.as_deref().unwrap_or("-");
                println!("      worktree      {b:<16} {}", wt.path.display());
            }
        }
    }

    // The declared path templates, always — resolution may be impossible (no
    // branch), but what the file says never is. Read with the same
    // focus-over-anchor precedence `resolve` applies, so what is printed is the
    // template that would actually be used and not merely the one nearest to hand.
    let build_repo = anchor_of(project, focus);
    let anchor = project.repo(&build_repo).context("repo not found")?;
    let focused = project.repo(focus).context("repo not found")?;
    let templates = [
        ("source_dir", anchor.decl().source_dir.as_ref()),
        (
            "build_dir",
            focused
                .decl()
                .build_dir
                .as_ref()
                .or(anchor.decl().build_dir.as_ref()),
        ),
        (
            "install_dir",
            focused
                .decl()
                .install_dir
                .as_ref()
                .or(anchor.decl().install_dir.as_ref()),
        ),
    ];
    if templates.iter().any(|(_, t)| t.is_some()) {
        println!();
        println!("templates:");
        for (name, template) in templates {
            if let Some(template) = template {
                println!("  {name:<12} {template}");
            }
        }
    }

    // Resolution needs a branch, so this is the section that degrades: an
    // unresolvable branch says so rather than silently dropping the section,
    // since "no resolved paths" and "this project has none" are different facts.
    let branch = branch_or_current(ws, project, focus, profile.branch.as_deref()).ok();
    println!();
    let Some(branch) = branch else {
        println!("resolved: <no branch discovered — pass --branch to resolve>");
        return Ok(());
    };
    let mut resolved = profile.to_profile();
    resolved.focus = Some(focus.to_owned());
    let plan = resolve_plan(
        ws,
        project,
        &paths_input(&resolved, Some(&branch), profile.work_dir.as_deref()),
    )?;
    let identity = plan
        .paths
        .branch
        .as_ref()
        .context("branch path query resolved as detached")?;
    println!(
        "resolved (branch {}, build_type {}):",
        identity.raw, plan.paths.build_type
    );
    // focus / build_repo / identity_repo are three roles one repo usually fills
    // at once, which is exactly why they are printed separately: when they differ
    // (a borrowed focus, an anchored subtree) every path below follows a
    // different one of them.
    println!("  focus:         {}", plan.paths.focus);
    println!("  build_repo:    {}", plan.paths.build_repo);
    println!(
        "  identity_repo: {}",
        plan.paths.identity_repo.as_deref().unwrap_or("-")
    );
    println!("  strategy:      {}", plan.paths.strategy.as_str());
    println!("  branch.slug:   {}", identity.slug);
    if let Some(bs) = plan.build_system {
        println!("  build_system:  {}", bs.as_str());
    }
    if let Some(gen) = &plan.paths.generator {
        println!("  generator:     {gen}");
    }
    if let Some(tc) = &plan.paths.toolchain {
        println!("  toolchain:     {}", tc.name);
    }
    if !plan.presets.is_empty() {
        println!("  presets:       {}", plan.presets.join(" "));
    }
    println!("  source_dir:    {}", plan.paths.source_dir.display());
    if let Some(dir) = &plan.paths.build_dir {
        println!("  build_dir:     {}", dir.display());
    }
    if let Some(dir) = &plan.paths.install_dir {
        println!("  install_dir:   {}", dir.display());
    }
    for (name, dir) in &plan.paths.work_dirs {
        println!("  repos.{name}.workdir: {}", dir.display());
    }

    // The accumulated pipeline result. A path-only resolve skips L0 (there is no
    // backend to translate a toolchain for), so what shows here is the org,
    // project, and preset layers — which is what answers "where did this
    // definition come from", read together with `presets` above.
    let logical = &plan.logical;
    if !logical.definitions.is_empty() {
        println!();
        println!("definitions:");
        for (key, value) in &logical.definitions {
            // The backend's own spelling, so what is reported is what the build
            // receives — not minijinja's Jinja/Python rendering, in which a
            // boolean reads `True`.
            println!("  {key} = {}", value_to_string(value));
        }
    }
    if !logical.environment.is_empty() {
        println!();
        println!("environment:");
        for (key, value) in &logical.environment {
            println!("  {key} = {value}");
        }
    }
    // The runtime half, which `build` never applies: what `wits devenv` puts on
    // top of its base. The base is named rather than run — what a build system
    // puts in it is a fact of the configured tree, not of the registry.
    let runner = plan
        .build_system
        .zip(plan.paths.build_dir.as_deref())
        .and_then(|(bs, dir)| backend_for(bs).devenv_runner(dir));
    if runner.is_some() || !logical.devenv.is_empty() {
        println!();
        println!("devenv:");
        match (&runner, &plan.paths.build_dir) {
            (Some(argv), _) => println!("  base: {}", argv.join(" ")),
            (None, Some(_)) => println!("  base: the caller's environment"),
            // An org's devenv reaches every project joining it, built or not.
            (None, None) => println!("  base: none — no build_dir to run against"),
        }
        for entry in &logical.devenv {
            let separator = if entry.op != DevenvOp::Set && entry.separator != DEVENV_SEPARATOR {
                format!(" (separator {:?})", entry.separator)
            } else {
                String::new()
            };
            println!(
                "  {} {} {}{separator}",
                entry.name,
                entry.op.as_str(),
                entry.value
            );
        }
    }
    for (label, args) in [
        ("extra_config_args", &logical.extra_config_args),
        ("extra_build_args", &logical.extra_build_args),
        ("extra_install_args", &logical.extra_install_args),
    ] {
        if !args.is_empty() {
            println!();
            println!("{label}:");
            for arg in args {
                println!("  {arg}");
            }
        }
    }
    Ok(())
}
