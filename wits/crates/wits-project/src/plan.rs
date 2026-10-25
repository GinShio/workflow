//! The full plan: the path stage plus the build-configuration layers.
//!
//! The layering is strictly one-directional — toolchain → org → project →
//! presets → CLI — and the toolchain's native env and definitions are injected
//! first, so a later layer that sets the same key wins; nothing is ever
//! re-asserted or recomputed. Everything here is read-only.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::context::{self, Ctx};
use crate::decl::{BuildSystem, DevenvOp};
use crate::paths::{resolve_paths, PathInput, ResolvedPaths};
use crate::presets;
use crate::profile::Profile;
use crate::workspace::{Project, Workspace};

/// A toolchain after selection: canonical fields plus verbatim pass-through
/// blocks. Backends translate the canonical fields into native form ("Single
/// source of truth for compilers, realised by the backend"); the pass-through
/// blocks are applied as-is.
#[derive(Debug, Clone, Default)]
pub struct Toolchain {
    pub name: String,
    pub cc: Option<String>,
    pub cxx: Option<String>,
    pub rustc: Option<String>,
    pub ar: Option<String>,
    pub nm: Option<String>,
    pub ranlib: Option<String>,
    pub strip: Option<String>,
    pub linker: Option<String>,
    pub launcher: Option<String>,
    pub c_flags: Vec<String>,
    pub cxx_flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub definitions: Vec<(String, minijinja::Value)>,
}

/// The accumulated, resolved build configuration produced by the pipeline.
/// `definitions` keep their type (bool/int/string) so a backend can spell each
/// one the way its tool expects.
///
/// `devenv` is the runtime twin of `environment`, kept apart because the two
/// must never mix: a build-time value (`CC`, `CFLAGS`) has no business in the
/// program under test, and a runtime selector none in a configure step.
#[derive(Debug, Clone, Default)]
pub struct LogicalConfig {
    pub environment: Vec<(String, String)>,
    pub definitions: Vec<(String, minijinja::Value)>,
    pub devenv: Vec<DevenvEntry>,
    pub extra_config_args: Vec<String>,
    pub extra_build_args: Vec<String>,
    pub extra_install_args: Vec<String>,
}

/// One resolved `devenv` operation, applied at run time on top of the backend's
/// base environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevenvEntry {
    pub name: String,
    pub op: DevenvOp,
    /// The rendered parts, already joined with `separator`.
    pub value: String,
    pub separator: String,
}

impl DevenvEntry {
    /// Apply this operation to `env`. A `set` is dotenv-style: it supplies the
    /// variable's default and leaves a value the base already carries — the
    /// caller's own, or one the build system's developer environment composed —
    /// untouched. A prepend/append onto a variable that is absent yields the
    /// value alone, while one onto a present variable — even empty — joins with
    /// the separator: Meson's run-mode rule.
    pub fn apply(&self, env: &mut BTreeMap<std::ffi::OsString, std::ffi::OsString>) {
        let key = std::ffi::OsString::from(&self.name);
        let joined = match (self.op, env.get(&key)) {
            (DevenvOp::Set, Some(_)) => return,
            (DevenvOp::Set, None) | (_, None) => std::ffi::OsString::from(&self.value),
            (DevenvOp::Prepend, Some(current)) => {
                let mut out = std::ffi::OsString::from(&self.value);
                out.push(&self.separator);
                out.push(current);
                out
            }
            (DevenvOp::Append, Some(current)) => {
                let mut out = current.clone();
                out.push(&self.separator);
                out.push(&self.value);
                out
            }
        };
        env.insert(key, joined);
    }
}

impl LogicalConfig {
    /// Accumulate a `devenv` operation. A `set` discards the operations on its
    /// variable that came before it: under dotenv semantics it states the
    /// variable's default outright and takes the variable over from lower
    /// levels. Keeping their prepend/append would let a lower level resurface
    /// whenever the environment already supplies the variable, which is exactly
    /// what the set claimed. A prepend/append repeating one still in force is
    /// not added again, so a preset reached twice through `extends` cannot add
    /// the same entry twice.
    pub fn add_devenv(&mut self, entry: DevenvEntry) {
        if entry.op == DevenvOp::Set {
            self.devenv.retain(|e| e.name != entry.name);
        } else if self.devenv.contains(&entry) {
            return;
        }
        self.devenv.push(entry);
    }

    /// Set an environment variable, replacing any earlier value for the key.
    /// Order is preserved by keeping the first insertion position.
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        set_kv(&mut self.environment, key.into(), value.into());
    }

    pub fn env_entry(&self, key: &str) -> Option<&str> {
        self.environment
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_definition(&mut self, key: impl Into<String>, value: minijinja::Value) {
        set_kv(&mut self.definitions, key.into(), value);
    }

    pub fn has_definition(&self, key: &str) -> bool {
        self.definitions.iter().any(|(k, _)| k == key)
    }
}

fn set_kv<V>(list: &mut Vec<(String, V)>, key: String, value: V) {
    if let Some(slot) = list.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        list.push((key, value));
    }
}

/// The one build-system responsibility the pipeline needs: translate a selected
/// [`Toolchain`]'s canonical fields into a backend's native env/definitions at
/// L0 ("Single source of truth for compilers, realised by the backend"). This is
/// the *only* seam between the read-only core and the build systems.
pub trait ToolchainInjector {
    /// Merge the toolchain's native env/definitions into `cfg`. Runs at L0, so a
    /// later preset or CLI override of the same key wins.
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig);
}

/// Stage inputs: the path axes plus the build layers.
pub struct BuildInput<'a> {
    pub paths: PathInput<'a>,
    /// Whether to inject the toolchain's env/definitions (skipped when trusting
    /// an already-configured build dir). Selection still happens.
    pub inject_toolchain: bool,
    /// The build system's toolchain translator. `None` for path-only resolves.
    pub injector: Option<&'a dyn ToolchainInjector>,
    /// L3 — verbatim overrides, applied last, at the highest priority.
    pub extra_config_args: &'a [String],
    pub extra_build_args: &'a [String],
    pub extra_install_args: &'a [String],
}

/// A fully resolved build plan: everything `build` executes and `info` reports.
pub struct BuildPlan {
    pub paths: ResolvedPaths,
    /// The project's declared build system.
    pub build_system: Option<BuildSystem>,
    /// The presets that applied, in application order — `default_presets`, then
    /// `applies_when` matches, then `--preset`.
    pub presets: Vec<String>,
    pub logical: LogicalConfig,
    /// The final context, so callers can resolve arbitrary templates or inspect
    /// one value. Private: the public entry point is [`get_scalar`](Self::get_scalar).
    ctx: Ctx,
}

impl BuildPlan {
    /// One resolved scalar from the full plan context — the same namespace the
    /// config files are written against.
    ///
    /// Scalars only: a table or list is an error, and booleans spell `true`/
    /// `false` as TOML does. One failure fails the whole query.
    pub fn get_scalar(&self, path: &str) -> anyhow::Result<String> {
        self.ctx
            .get_scalar(path)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Resolve one project into a full plan: [`resolve_paths`] then the layers.
pub fn resolve_plan(
    ws: &Workspace,
    project: &Project,
    input: &BuildInput<'_>,
) -> Result<BuildPlan> {
    let paths = resolve_paths(ws, project, &input.paths)?;
    // `Clone` drops the memo and is safe to reuse; the path-stage bindings are
    // the base every layer reads.
    let mut ctx = paths.ctx.clone();
    let profile: &Profile = input.paths.profile;

    let mut logical = LogicalConfig::default();

    // L0 — toolchain injection. A path-only resolve has no injector and skips
    // this layer entirely.
    if input.inject_toolchain {
        if let (Some(tc), Some(inj)) = (&paths.toolchain, input.injector) {
            inj.apply_toolchain(tc, &mut logical);
            context::fold_env(&mut ctx, &logical);
        }
    }

    // L0.5 — org config, inherited unconditionally by every member project.
    if let Some(org) = project.org() {
        if let Some(org_data) = ws.org(org) {
            context::apply_env_map(
                &mut ctx,
                &mut logical,
                "org.environment",
                &org_data.environment,
            )?;
            context::apply_def_map(
                &mut ctx,
                &mut logical,
                "org.definitions",
                &org_data.definitions,
            )?;
            context::apply_devenv_map(&ctx, &mut logical, &org_data.devenv)?;
        }
    }

    // L1 — project config.
    context::apply_env_map(
        &mut ctx,
        &mut logical,
        "project.environment",
        &project.settings.environment,
    )?;
    context::apply_def_map(
        &mut ctx,
        &mut logical,
        "project.definitions",
        &project.settings.definitions,
    )?;
    context::apply_devenv_map(&ctx, &mut logical, &project.settings.devenv)?;
    context::resolve_args(
        &ctx,
        &project.settings.extra_config_args,
        &mut logical.extra_config_args,
    )?;
    context::resolve_args(
        &ctx,
        &project.settings.extra_build_args,
        &mut logical.extra_build_args,
    )?;
    context::resolve_args(
        &ctx,
        &project.settings.extra_install_args,
        &mut logical.extra_install_args,
    )?;

    // L2 — presets.
    let presets = presets::applied_presets(
        ws,
        project,
        &paths.focus,
        profile,
        &paths.toolchain,
        &paths.build_type,
        &paths.generator,
    );
    for name in &presets {
        let mut seen = Vec::new();
        presets::resolve_preset_into(
            &mut ctx,
            &mut logical,
            ws,
            project,
            &paths.focus,
            name,
            &mut seen,
        )?;
    }

    // L3 — CLI extra args (verbatim, highest priority).
    logical
        .extra_config_args
        .extend(input.extra_config_args.iter().cloned());
    logical
        .extra_build_args
        .extend(input.extra_build_args.iter().cloned());
    logical
        .extra_install_args
        .extend(input.extra_install_args.iter().cloned());

    Ok(BuildPlan {
        build_system: project.settings.build_system,
        presets,
        logical,
        ctx,
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TemplateError;
    use crate::layout::bootstrap_worktree_dir;
    use crate::paths::branch_build_dirs;
    use minijinja::Value;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn devenv_entry(name: &str, op: DevenvOp, value: &str) -> DevenvEntry {
        DevenvEntry {
            name: name.into(),
            op,
            value: value.into(),
            separator: ":".into(),
        }
    }

    /// A path-only input: select but do not inject a toolchain, and apply no
    /// L3 overrides — the shape the read-only consumers use.
    fn paths_only<'a>(profile: &'a Profile, branch: &'a str) -> BuildInput<'a> {
        BuildInput {
            paths: PathInput {
                profile,
                branch: Some(branch),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        }
    }

    fn ws_with(body: &str, stem: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{stem}.toml")), body).unwrap();
        let ws = Workspace::load_from(dir.path()).unwrap();
        (dir, ws)
    }

    /// A stand-in for a real backend, so the pipeline can be tested without any
    /// build-system dependency: it echoes the toolchain's `cc` into one
    /// definition and one environment variable (the way a backend would) so we
    /// can assert L0 ran. Real per-backend translation is tested beside the
    /// backends themselves, in `wits-build`.
    struct MockInjector;
    impl ToolchainInjector for MockInjector {
        fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig) {
            if let Some(cc) = &tc.cc {
                cfg.set_definition("MOCK_CC", Value::from(cc.clone()));
                cfg.set_env("MOCK_CC_ENV", cc.clone());
            }
        }
    }

    /// The two ways a project that references a checkout still has nothing
    /// identified by its branch: it focuses somewhere else (so its build carries
    /// its *own* branch), or its `build_dir` does not move with a branch at all.
    /// Both are derived, so neither needs a registry annotation to stay true.
    #[test]
    fn branch_build_dirs_skip_projects_the_branch_does_not_identify() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let at = |rel: &str| dir.path().join(rel);
        let write = |stem: &str, body: &str| {
            let filled = body.replace("SHARED", at("shared").to_str().unwrap());
            std::fs::write(config.join(format!("{stem}.toml")), filled).unwrap();
        };

        // Owns the shared checkout and keys its build on its branch: included.
        write(
            "owner",
            r#"
[project]
[repos.main]
path = "SHARED"
main_branch = "main"
build_dir = "{{repos.main.workdir}}/_build/{{branch.slug}}"
"#,
        );
        // Borrows the shared checkout but focuses on its own repo, so its build is
        // identified by *its* branch and the shared branch says nothing about it.
        write(
            "bystander",
            &r#"
[project]
[repos.main]
path = "OWN"
main_branch = "main"
build_dir = "{{repos.main.workdir}}/_build/{{branch.slug}}"
[repos.lib]
from = "owner"
anchor = "main"
"#
            .replace("OWN", at("bystander").to_str().unwrap()),
        );
        // Focuses on the borrowed checkout, but its build directory is fixed.
        write(
            "fixed",
            &r#"
[project]
focus = "lib"
[repos.main]
path = "OWN"
main_branch = "main"
build_dir = "{{repos.main.workdir}}/_build/static"
[repos.lib]
from = "owner"
anchor = "main"
"#
            .replace("OWN", at("fixed").to_str().unwrap()),
        );

        for rel in [
            "shared/_build/feat",
            "bystander/_build/feat",
            "fixed/_build/static",
        ] {
            std::fs::create_dir_all(at(rel)).unwrap();
        }

        let ws = Workspace::load_from(&config).unwrap();
        let found = branch_build_dirs(&ws, &at("shared"), "feat");
        let paths: Vec<_> = found.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths, vec![at("shared/_build/feat")], "{found:?}");
    }

    #[test]
    fn detached_worktree_fallback_only_ignores_the_missing_branch_namespace() {
        let branch = TemplateError::UnknownPath("branch.slug".into());
        let typo = TemplateError::UnknownPath("brnach.slug".into());
        assert!(crate::layout::missing_branch_binding(&branch));
        assert!(!crate::layout::missing_branch_binding(&typo));
    }

    #[test]
    fn resolves_paths_and_injects_toolchain() {
        let body = r#"
            [project]
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{toolchain.name}}/{{build_type}}"

            [toolchains.clang]
            cc = "clang"
            cxx = "clang++"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile {
            build_type: Some("release".into()),
            ..Default::default()
        };
        let injector = MockInjector;
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: true,
            injector: Some(&injector),
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        assert_eq!(plan.paths.work_dir, PathBuf::from("/src/hello"));
        assert_eq!(
            plan.paths.build_dir.unwrap(),
            PathBuf::from("/src/hello/_build/clang/release")
        );
        // The injector ran at L0: the selected toolchain's `cc` was translated
        // into the backend-shaped definition and env var the mock emits.
        assert!(plan.logical.definitions.iter().any(|(k, _)| k == "MOCK_CC"));
        assert_eq!(plan.logical.env_entry("MOCK_CC_ENV"), Some("clang"));
    }

    /// The toolchain's values are injected at L0, so a later layer that sets
    /// the same key wins ("The toolchain hard constraint") — the property that
    /// lets the pass stay one-directional. A preset overrides both the
    /// definition and the env var the injector emits.
    #[test]
    fn a_preset_overriding_an_injected_toolchain_key_wins() {
        let body = r#"
            [project]
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"

            [toolchains.clang]
            cc = "clang"

            [project.presets.override]
            definitions = { MOCK_CC = "gcc" }
            environment = { MOCK_CC_ENV = "gcc" }
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile {
            presets: vec!["override".into()],
            ..Default::default()
        };
        let injector = MockInjector;
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: true,
            injector: Some(&injector),
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        let def = |key: &str| {
            plan.logical
                .definitions
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("no definition {key}"))
        };
        // The preset's values survived, not the injected ones.
        assert_eq!(def("MOCK_CC"), Value::from("gcc"));
        assert_eq!(plan.logical.env_entry("MOCK_CC_ENV"), Some("gcc"));
    }

    /// The resolved outputs are bound back into the context in the order they
    /// land, so a later one may name an earlier one — which is how an install
    /// prefix that *is* the build tree avoids being a second copy of the same
    /// expression. `--get` reads the same bindings.
    #[test]
    fn resolved_output_paths_are_bound_back_into_the_context() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            source_dir = "{{repos.main.workdir}}/src"
            build_dir = "{{source_dir}}/../_build/{{branch.slug}}"
            install_dir = "{{build_dir}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile::default();
        let plan = resolve_plan(&ws, project, &paths_only(&profile, "main")).unwrap();

        assert_eq!(plan.paths.source_dir, PathBuf::from("/src/hello/src"));
        assert_eq!(
            plan.paths.build_dir.clone().unwrap(),
            PathBuf::from("/src/hello/src/../_build/main")
        );
        // install_dir named build_dir rather than repeating its expression.
        assert_eq!(plan.paths.install_dir, plan.paths.build_dir);

        // The same paths answer a one-path query, which is all `--get` does.
        // `repo.workdir` is there too: the relative alias mirrors the focus's
        // entry, so a caller that does not know the focus's name can still name
        // its checkout.
        for (path, want) in [
            ("source_dir", "/src/hello/src"),
            ("build_dir", "/src/hello/src/../_build/main"),
            ("install_dir", "/src/hello/src/../_build/main"),
            ("repo.workdir", "/src/hello"),
            ("repos.main.workdir", "/src/hello"),
        ] {
            assert_eq!(
                plan.ctx.get_scalar(path).ok().as_deref(),
                Some(want),
                "{path}"
            );
        }
    }

    /// Output paths are path templates: a whole-expression result that is a
    /// list or a map must be refused, not flattened into a plausible-looking
    /// path. `source_dir`, `build_dir`, and `install_dir` each go through the
    /// same renderer, so pin all three.
    #[test]
    fn output_path_templates_must_resolve_to_a_string() {
        for field in ["source_dir", "build_dir", "install_dir"] {
            let body = format!(
                r#"
                [project]
                build_system = "cmake"

                [repos.main]
                path = "/src/hello"
                main_branch = "main"
                {field} = "{{{{ ['a','b'] }}}}"
                "#
            );
            let (_d, ws) = ws_with(&body, "hello");
            let project = ws.project("hello").unwrap();
            let profile = Profile::default();
            let err = resolve_plan(&ws, project, &paths_only(&profile, "main"))
                .map(|_| ())
                .unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("non-string"), "{field}: {msg}");
        }
    }

    /// The same rule for a worktree path: a list-valued `worktree_dir` is not a
    /// checkout selector, and flattening it would silently produce one.
    #[test]
    fn worktree_dir_templates_must_resolve_to_a_string() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello.git"
            main_branch = "main"
            branch_strategy = "worktree"
            worktree_dir = "{{ ['a','b'] }}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile::default();
        let err = resolve_plan(&ws, project, &paths_only(&profile, "main"))
            .map(|_| ())
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("non-string"), "{msg}");
    }

    /// A project declaring no `build_dir` leaves the path *unbound*, so naming it
    /// is an unknown-path error rather than an empty string a caller could not
    /// tell apart from a configured empty value.
    #[test]
    fn an_undeclared_output_path_is_unbound_not_empty() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile::default();
        let plan = resolve_plan(&ws, project, &paths_only(&profile, "main")).unwrap();

        assert!(plan.paths.build_dir.is_none());
        assert!(matches!(
            plan.ctx.get("build_dir"),
            Err(TemplateError::UnknownPath(p)) if p == "build_dir"
        ));
        assert!(matches!(
            plan.ctx.get("install_dir"),
            Err(TemplateError::UnknownPath(p)) if p == "install_dir"
        ));
        // source_dir always resolves: it falls back to the build repo's workdir.
        assert_eq!(
            plan.ctx.get("source_dir").unwrap().as_str(),
            Some("/src/hello")
        );
    }

    #[test]
    fn bare_repo_uses_a_named_workdir_for_worktree_builds() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello.git"
            main_branch = "main"
            build_dir = "{{repos.other.workdir}}/_build/{{branch.slug}}"
            branch_strategy = "worktree"
            worktree_dir = "{{repo.path}}.worktrees/{{branch.slug}}"

            [repos.other]
            path = "/src/other.git"
            main_branch = "main"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.worktrees/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.primary"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let plan =
            resolve_plan(&ws, project, &paths_only(&Profile::default(), "feature/x")).unwrap();

        assert_eq!(
            plan.paths.work_dir,
            PathBuf::from("/src/hello.git.worktrees/feature_x")
        );
        assert_eq!(
            plan.paths.build_dir.unwrap(),
            PathBuf::from("/src/other.git.worktrees/feature_x/_build/feature_x")
        );
    }

    #[test]
    fn bootstrap_path_is_explicit_for_hybrid_and_main_branch_for_worktree() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/main.git"
            main_branch = "trunk"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.primary"

            [repos.other]
            path = "/src/other.git"
            main_branch = "stable/x"
            branch_strategy = "worktree"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"

            [repos.relative]
            path = "/src/relative.git"
            main_branch = "main"
            branch_strategy = "hybrid"
            worktree_dir = "/work/relative/{{branch.slug}}"
            bootstrap_worktree_dir = "main"

            [repos.anchored]
            path = "/src/anchored.git"
            main_branch = "main"
            branch_strategy = "worktree"
            worktree_dir = "worktrees/{{branch.slug}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, project.repo("main").unwrap()).unwrap(),
            PathBuf::from("/src/main.git.primary")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, project.repo("other").unwrap()).unwrap(),
            PathBuf::from("/src/other.git.wt/stable_x")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, project.repo("relative").unwrap()).unwrap(),
            PathBuf::from("/work/relative/main")
        );
        assert_eq!(
            bootstrap_worktree_dir(&ws, project, project.repo("anchored").unwrap()).unwrap(),
            PathBuf::from("/src/worktrees/main")
        );
    }

    #[test]
    fn top_level_work_dir_is_not_a_template_variable() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/hello.git"
            main_branch = "main"
            build_dir = "{{work.dir}}/_build"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let err = match resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")) {
            Ok(_) => panic!("expected the removed top-level work.dir to fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("work.dir"));
    }

    #[test]
    fn spec_vars_and_work_dir_override_resolve() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/mr-{{spec.mr}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile {
            specs: [("mr".to_owned(), "42".to_owned())].into_iter().collect(),
            ..Default::default()
        };
        // A checkout materialised elsewhere (e.g. a review worktree).
        let work_dir = PathBuf::from("/tmp/hello.review/mr-42");
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("feature"),
                work_dir: Some(&work_dir),
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        // The named repo workdir is the verbatim override, not the strategy's
        // path…
        assert_eq!(
            plan.paths.work_dir,
            PathBuf::from("/tmp/hello.review/mr-42")
        );
        // …and the build_dir template anchored on it and consumed `{{spec.mr}}`.
        assert_eq!(
            plan.paths.build_dir.unwrap(),
            PathBuf::from("/tmp/hello.review/mr-42/_build/mr-42")
        );
    }

    #[test]
    fn a_referenced_but_unsupplied_spec_is_a_hard_error() {
        let body = r#"
            [project]

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{spec.mr}}"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        // No `spec.mr` supplied — resolving the template must fail loudly.
        let err = match resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")) {
            Ok(_) => panic!("expected a hard error for the unsupplied spec"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("spec"),
            "error names the missing path: {err}"
        );
    }

    #[test]
    fn source_dir_defaults_to_repo_workdir_and_can_be_a_subdir() {
        let body = r#"
            [project]
            build_system = "cmake"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            source_dir = "{{repos.main.workdir}}/subdir"

            [repos.other]
            path = "/src/other"
            main_branch = "main"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let base = Profile::default();
        let input = |profile: &Profile| -> BuildPlan {
            resolve_plan(
                &ws,
                project,
                &BuildInput {
                    paths: PathInput {
                        profile,
                        branch: Some("main"),
                        work_dir: None,
                        build_dir: None,
                        install_dir: None,
                    },
                    inject_toolchain: false,
                    injector: None,
                    extra_config_args: &[],
                    extra_build_args: &[],
                    extra_install_args: &[],
                },
            )
            .unwrap()
        };
        // The named repo workdir stays the checkout root; source_dir is the
        // declared subdir.
        let plan = input(&base);
        assert_eq!(plan.paths.work_dir, PathBuf::from("/src/hello"));
        assert_eq!(plan.paths.source_dir, PathBuf::from("/src/hello/subdir"));
        // A repo without source_dir falls back to its workdir.
        let other = Profile {
            focus: Some("other".into()),
            ..Default::default()
        };
        let plan2 = input(&other);
        assert_eq!(plan2.paths.source_dir, plan2.paths.work_dir);
    }

    #[test]
    fn no_injector_skips_l0_but_still_resolves_paths() {
        let body = r#"
            [project]
            build_system = "cmake"
            toolchain = "clang"

            [repos.main]
            path = "/src/hello"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"

            [toolchains.clang]
            cc = "clang"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        let profile = Profile::default();
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: true, // requested, but no injector supplied,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        // Toolchain *selection* still happened (paths need the name)…
        assert_eq!(plan.paths.toolchain.as_ref().unwrap().name, "clang");
        // …but with no injector, L0 emitted nothing.
        assert!(plan.logical.definitions.is_empty());
    }

    #[test]
    fn org_config_is_inherited_and_the_project_overrides_it() {
        let body = r#"
            [org]
            name = "acme"
            [org.environment]
            SHARED_VAR = "from-org"
            UNTOUCHED_VAR = "org-only"
            OVERRIDDEN_VAR = "from-org"
            [org.definitions]
            ORG_LEVEL = 42
            ORG_FLAG = false

            [project]
            org = "acme"
            [project.environment]
            MY_ENV = "{{org.environment.SHARED_VAR}}"
            OVERRIDDEN_VAR = "from-project"
            [project.definitions]
            MY_DEF = "{{org.definitions.ORG_LEVEL}}"

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("acme/x").unwrap();
        let input = BuildInput {
            paths: PathInput {
                profile: &Profile::default(),
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        // Every org entry is inherited, whether or not anything referenced it.
        assert_eq!(plan.logical.env_entry("UNTOUCHED_VAR"), Some("org-only"));
        assert_eq!(plan.logical.env_entry("SHARED_VAR"), Some("from-org"));
        assert!(plan.logical.has_definition("ORG_LEVEL"));
        assert!(plan.logical.has_definition("ORG_FLAG"));
        // The org.* namespace still resolves, so existing references keep working.
        assert_eq!(plan.logical.env_entry("MY_ENV"), Some("from-org"));
        assert!(plan.logical.has_definition("MY_DEF"));
        // The org is the lowest layer: declaring a key at project level wins.
        assert_eq!(
            plan.logical.env_entry("OVERRIDDEN_VAR"),
            Some("from-project")
        );
    }

    /// An org's definitions keep their TOML type through inheritance, so a
    /// backend can still spell a bool as a bool rather than the string "false".
    #[test]
    fn inherited_org_definitions_keep_their_type() {
        let body = r#"
            [org]
            name = "acme"
            [org.definitions]
            ORG_FLAG = false
            ORG_COUNT = 8

            [project]
            org = "acme"

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("acme/x").unwrap();
        let plan = resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")).unwrap();
        let def = |key: &str| {
            plan.logical
                .definitions
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        // Inherited definitions keep their TOML type, which is what lets a backend
        // emit `ORG_FLAG` as a boolean rather than the string "false".
        assert_eq!(def("ORG_FLAG"), Value::from(false));
        assert_eq!(def("ORG_COUNT"), Value::from(8));
    }

    /// A project may name an org that no file declares. That is not fatal today
    /// (no validation rejects it), so inheritance must degrade to contributing
    /// nothing rather than panicking or erroring mid-plan.
    #[test]
    fn an_undeclared_org_contributes_nothing() {
        let body = r#"
            [project]
            org = "ghost"
            [project.definitions]
            OWN = true

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("ghost/x").unwrap();
        let plan = resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")).unwrap();
        assert!(plan.logical.has_definition("OWN"));
        assert_eq!(plan.logical.definitions.len(), 1);
    }

    /// The `org.*` namespace outlives inheritance: a preset that spells out
    /// `{{org.environment.X}}` binds it to a differently-named key, which is not
    /// something inheritance alone can express.
    #[test]
    fn org_namespace_referenceable_from_preset() {
        let body = r#"
            [org]
            name = "myorg"
            [org.environment]
            ORG_SETTING = "hello"

            [project]
            org = "myorg"
            default_presets = ["use-org"]

            [project.presets.use-org]
            environment = { FROM_ORG = "{{org.environment.ORG_SETTING}}" }

            [repos.main]
            path = "/src/y"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"
        "#;
        let (_d, ws) = ws_with(body, "y");
        let project = ws.project("myorg/y").unwrap();
        let input = BuildInput {
            paths: PathInput {
                profile: &Profile::default(),
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        assert_eq!(plan.logical.env_entry("FROM_ORG"), Some("hello"));
        // …and the source entry is inherited under its own name as well.
        assert_eq!(plan.logical.env_entry("ORG_SETTING"), Some("hello"));
    }

    #[test]
    fn preset_applies_when_and_override() {
        let body = r#"
            [project]
            build_system = "cmake"
            default_presets = ["warn"]

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/b"

            [project.presets.warn]
            definitions = { WERROR = true }

            [project.presets.dbg]
            applies_when = { build_type = "debug" }
            definitions = { ASSERTS = true }
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("x").unwrap();
        let profile = Profile::default();
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &[],
            extra_build_args: &[],
            extra_install_args: &[],
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        assert!(plan.logical.has_definition("WERROR"));
        assert!(plan.logical.has_definition("ASSERTS")); // auto-applied for debug
    }

    /// `devenv` accumulates through the same layers `environment` passes
    /// through — org, project, then presets with their `extends` first — but
    /// never enters `env.*` or the build environment, so an entry may read its
    /// own variable without a cycle.
    #[test]
    fn devenv_accumulates_through_the_layers_and_stays_out_of_the_build() {
        let body = r#"
            [org]
            name = "acme"
            [org.devenv]
            LIST = { append = "org" }
            [org.presets.shared.devenv]
            LIST = { append = "org-shared" }

            [project]
            org = "acme"
            default_presets = ["left", "right"]
            [project.devenv]
            LIST = { append = "{{build_dir}}" }
            VK = "{{install_dir}}/icd.json"
            PATH = "/opt/bin:{{env.PATH}}"

            [project.presets.base.devenv]
            LIST = { append = "base" }
            [project.presets.left]
            extends = ["base"]
            [project.presets.right]
            extends = ["base", "shared"]
            [project.presets.shared.devenv]
            LIST = { append = "project-shared" }

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build"
            install_dir = "{{build_dir}}/inst"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("acme/x").unwrap();
        let plan = resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")).unwrap();
        let ops = |name: &str| -> Vec<(&str, &str)> {
            plan.logical
                .devenv
                .iter()
                .filter(|e| e.name == name)
                .map(|e| (e.op.as_str(), e.value.as_str()))
                .collect()
        };

        // `base` is reached through both `left` and `right`; its append lands
        // once. The same-named `shared` carries its org and project levels in
        // that order.
        assert_eq!(
            ops("LIST"),
            vec![
                ("append", "org"),
                ("append", "/src/x/_build"),
                ("append", "base"),
                ("append", "org-shared"),
                ("append", "project-shared"),
            ]
        );
        assert_eq!(ops("VK"), vec![("set", "/src/x/_build/inst/icd.json")]);

        let caller_path = std::env::var("PATH").unwrap();
        assert_eq!(
            ops("PATH"),
            vec![("set", format!("/opt/bin:{caller_path}").as_str())]
        );
        // Neither the context nor the build environment learned of any of it.
        assert_eq!(plan.ctx.get_scalar("env.PATH").unwrap(), caller_path);
        assert!(plan.ctx.get("env.VK").is_err());
        assert!(plan.logical.env_entry("VK").is_none());
    }

    /// A `set` is dotenv-style: the base's value wins for a variable it already
    /// carries, and only an absent one is supplied. prepend/append still modify
    /// whatever is there.
    #[test]
    fn a_devenv_set_supplies_a_default_and_never_overrides() {
        let os = std::ffi::OsString::from;
        let mut env: BTreeMap<std::ffi::OsString, std::ffi::OsString> = BTreeMap::new();
        env.insert(os("X"), os("base"));

        devenv_entry("X", DevenvOp::Set, "declared").apply(&mut env);
        assert_eq!(
            env.get(&os("X")),
            Some(&os("base")),
            "a set overrode the base"
        );
        devenv_entry("X", DevenvOp::Append, "more").apply(&mut env);
        assert_eq!(env.get(&os("X")), Some(&os("base:more")));
        devenv_entry("X", DevenvOp::Prepend, "first").apply(&mut env);
        assert_eq!(env.get(&os("X")), Some(&os("first:base:more")));

        devenv_entry("Y", DevenvOp::Set, "supplied").apply(&mut env);
        assert_eq!(
            env.get(&os("Y")),
            Some(&os("supplied")),
            "a set on an absent variable did not apply"
        );
    }

    /// A `set` takes its variable over from lower levels, and an operation
    /// identical to one still in force is not added twice — otherwise a preset
    /// reached through two `extends` paths would append the same ICD twice.
    #[test]
    fn a_set_discards_earlier_operations_and_a_repeat_in_force_is_not_added() {
        let mut cfg = LogicalConfig::default();
        cfg.add_devenv(devenv_entry("P", DevenvOp::Prepend, "/a"));
        cfg.add_devenv(devenv_entry("Q", DevenvOp::Set, "q"));
        // A preset reached twice through `extends` repeats its prepend.
        cfg.add_devenv(devenv_entry("P", DevenvOp::Prepend, "/a"));
        assert_eq!(cfg.devenv.len(), 2);

        // A set wipes what came before it, so a later prepend that matches one
        // the set discarded is in force again rather than a repeat.
        cfg.add_devenv(devenv_entry("P", DevenvOp::Set, "/base"));
        cfg.add_devenv(devenv_entry("P", DevenvOp::Prepend, "/a"));
        let on_p: Vec<_> = cfg.devenv.iter().filter(|e| e.name == "P").collect();
        assert_eq!(
            on_p,
            vec![
                &devenv_entry("P", DevenvOp::Set, "/base"),
                &devenv_entry("P", DevenvOp::Prepend, "/a")
            ]
        );

        // Last writer wins for a set, however many times the value recurs.
        cfg.add_devenv(devenv_entry("Q", DevenvOp::Set, "other"));
        cfg.add_devenv(devenv_entry("Q", DevenvOp::Set, "q"));
        let on_q: Vec<_> = cfg.devenv.iter().filter(|e| e.name == "Q").collect();
        assert_eq!(on_q, vec![&devenv_entry("Q", DevenvOp::Set, "q")]);
    }

    /// Meson's run-mode rules: an absent variable stands alone, a present one —
    /// even empty — joins with the separator; a set on an absent variable
    /// supplies it.
    #[test]
    fn devenv_operations_apply_with_mesons_run_mode_semantics() {
        let mut env = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("EMPTY"), OsString::new()),
        ]);
        devenv_entry("PATH", DevenvOp::Prepend, "/b").apply(&mut env);
        devenv_entry("PATH", DevenvOp::Append, "/z").apply(&mut env);
        devenv_entry("FRESH", DevenvOp::Append, "/only").apply(&mut env);
        devenv_entry("EMPTY", DevenvOp::Prepend, "/e").apply(&mut env);
        devenv_entry("SET", DevenvOp::Set, "v").apply(&mut env);
        let get = |k: &str| env[&OsString::from(k)].to_str().unwrap().to_owned();
        assert_eq!(get("PATH"), "/b:/usr/bin:/z");
        assert_eq!(get("FRESH"), "/only");
        assert_eq!(get("EMPTY"), "/e:");
        assert_eq!(get("SET"), "v");
    }

    /// The anchor supplies output defaults, but a focus may name its own output
    /// context while still configuring through that anchor.
    #[test]
    fn focus_output_dirs_override_the_anchor_defaults() {
        let body = r#"
            [project]
            focus = "lib"

            [repos.main]
            path = "/src/root"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/root"
            install_dir = "{{repos.main.workdir}}/_install/root"

            [repos.lib]
            path = "/src/lib"
            main_branch = "main"
            anchor = "main"

            [repos.variant]
            path = "/src/variant"
            main_branch = "main"
            anchor = "main"
            build_dir = "{{repos.main.workdir}}/_build/variant"
            install_dir = "{{repos.main.workdir}}/_install/variant"

            [repos.tool]
            path = "/src/tool"
            main_branch = "main"
            build_dir = "{{repos.tool.workdir}}/_build/tool"
            install_dir = "{{repos.tool.workdir}}/_install/tool"
        "#;
        let (_d, ws) = ws_with(body, "hello");
        let project = ws.project("hello").unwrap();
        // Default focus is `lib`, which builds through `main`.
        let via_anchor =
            resolve_plan(&ws, project, &paths_only(&Profile::default(), "main")).unwrap();
        assert_eq!(via_anchor.paths.build_repo, "main");
        assert_eq!(
            via_anchor.paths.build_dir.unwrap(),
            PathBuf::from("/src/root/_build/root")
        );
        assert_eq!(
            via_anchor.paths.install_dir.unwrap(),
            PathBuf::from("/src/root/_install/root")
        );

        let overridden = resolve_plan(
            &ws,
            project,
            &paths_only(
                &Profile {
                    focus: Some("variant".into()),
                    ..Default::default()
                },
                "main",
            ),
        )
        .unwrap();
        assert_eq!(overridden.paths.build_repo, "main");
        assert_eq!(
            overridden.paths.build_dir.unwrap(),
            PathBuf::from("/src/root/_build/variant")
        );
        assert_eq!(
            overridden.paths.install_dir.unwrap(),
            PathBuf::from("/src/root/_install/variant")
        );

        let self_anchored = resolve_plan(
            &ws,
            project,
            &paths_only(
                &Profile {
                    focus: Some("tool".into()),
                    ..Default::default()
                },
                "main",
            ),
        )
        .unwrap();
        assert_eq!(self_anchored.paths.build_repo, "tool");
        assert_eq!(
            self_anchored.paths.build_dir.unwrap(),
            PathBuf::from("/src/tool/_build/tool")
        );
        assert_eq!(
            self_anchored.paths.install_dir.unwrap(),
            PathBuf::from("/src/tool/_install/tool")
        );
    }

    /// A toolchain field may name the profile bindings the path stage publishes
    /// before resolving the toolchain (`build_type`, `generator`, `branch.*`).
    /// This order is a contract, not an implementation detail.
    #[test]
    fn a_toolchain_field_resolves_after_branch_and_build_type_are_bound() {
        let body = r#"
            [project]
            toolchain = "t"

            [repos.main]
            path = "/src/x"
            main_branch = "main"

            [toolchains.t]
            cc = "cc-{{build_type}}"
            cxx = "cxx-{{branch.slug}}"
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("x").unwrap();
        let profile = Profile {
            build_type: Some("release".into()),
            ..Default::default()
        };
        let plan = resolve_plan(&ws, project, &paths_only(&profile, "feature/x")).unwrap();
        let tc = plan.paths.toolchain.as_ref().unwrap();
        assert_eq!(tc.cc.as_deref(), Some("cc-release"));
        assert_eq!(tc.cxx.as_deref(), Some("cxx-feature_x"));
    }

    /// L3 is the last layer: CLI `extra_*` args follow the project's and then
    /// the presets', and nothing is re-ordered behind them.
    #[test]
    fn cli_extra_args_are_applied_after_project_and_preset_layers() {
        let body = r#"
            [project]
            extra_config_args = ["-Dproject"]
            extra_build_args = ["project-build"]
            extra_install_args = ["project-install"]

            [repos.main]
            path = "/src/x"
            main_branch = "main"

            [project.presets.p]
            extra_config_args = ["-Dpreset"]
            extra_build_args = ["preset-build"]
            extra_install_args = ["preset-install"]
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("x").unwrap();
        let profile = Profile {
            presets: vec!["p".into()],
            ..Default::default()
        };
        let cfg = ["-Dcli".to_owned()];
        let build = ["cli-build".to_owned()];
        let install = ["cli-install".to_owned()];
        let input = BuildInput {
            paths: PathInput {
                profile: &profile,
                branch: Some("main"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
            inject_toolchain: false,
            injector: None,
            extra_config_args: &cfg,
            extra_build_args: &build,
            extra_install_args: &install,
        };
        let plan = resolve_plan(&ws, project, &input).unwrap();
        assert_eq!(
            plan.logical.extra_config_args,
            vec!["-Dproject", "-Dpreset", "-Dcli"]
        );
        assert_eq!(
            plan.logical.extra_build_args,
            vec!["project-build", "preset-build", "cli-build"]
        );
        assert_eq!(
            plan.logical.extra_install_args,
            vec!["project-install", "preset-install", "cli-install"]
        );
    }

    /// The path stage resolves checkouts and output paths; org/project/preset
    /// config does not exist there. This is what lets `branch-build-dirs`
    /// survive an unrelated broken preset.
    #[test]
    fn resolve_paths_does_not_apply_environment_or_presets() {
        let body = r#"
            [project]
            environment = { WITS_TEST_STAGE_PROJECT = "project" }
            definitions = { WITS_TEST_STAGE_PROJECT = "project" }

            [repos.main]
            path = "/src/x"
            main_branch = "main"
            build_dir = "{{repos.main.workdir}}/_build/{{branch.slug}}"

            [project.presets.p]
            environment = { WITS_TEST_STAGE_PRESET = "preset" }
        "#;
        let (_d, ws) = ws_with(body, "x");
        let project = ws.project("x").unwrap();
        let profile = Profile {
            presets: vec!["p".into()],
            ..Default::default()
        };
        let paths = crate::paths::resolve_paths(
            &ws,
            project,
            &PathInput {
                profile: &profile,
                branch: Some("feat"),
                work_dir: None,
                build_dir: None,
                install_dir: None,
            },
        )
        .unwrap();
        assert_eq!(
            paths.build_dir.clone().unwrap(),
            PathBuf::from("/src/x/_build/feat")
        );
        assert!(paths.get_scalar("env.WITS_TEST_STAGE_PROJECT").is_err());
        assert!(paths.get_scalar("env.WITS_TEST_STAGE_PRESET").is_err());
        assert!(paths
            .get_scalar("project.environment.WITS_TEST_STAGE_PROJECT")
            .is_err());
        assert!(paths
            .get_scalar("preset.p.environment.WITS_TEST_STAGE_PRESET")
            .is_err());
    }
}
