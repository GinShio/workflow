//! Loading the project registry: find the config root, scan it, parse each file
//! into declarations, route the sections, and validate — no git, no path
//! rendering.
//!
//! Configuration is content-addressed (`docs/reference/project-design.rst`,
//! "Configuration topology"): files live anywhere under one root and declare
//! what they are by their sections, so loading is "read every `*.toml`, look at
//! what's inside, file it accordingly". A project's *name* is its file stem; its
//! *org* is explicit (`project.org`). The same `(org, name)` twice is a
//! conflict, not a silent override — cross-file layering of one project was a
//! foot-gun we don't reproduce.
//!
//! Validation here is about the declarations themselves: names, required
//! sections, remote roles, strategy fields, and the shape of a borrow. Resolving
//! a borrow to a checkout — and everything that needs git or path rendering —
//! belongs to the workspace and layout stages.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_core::config::Root;

use crate::decl::{BranchStrategy, FileDecl, OrgDecl, RepoDecl, SettingsDecl, ToolchainDecl};

/// Where the registry keeps its config tree: `$WITS_PROJECT_CONFIG`, then
/// `$XDG_CONFIG_HOME/wits/project`, then `$HOME/.wits/project`.
pub(crate) const CONFIG_ROOT: Root<'static> = Root {
    env: "WITS_PROJECT_CONFIG",
    xdg: "wits/project",
    home: ".wits/project",
};

/// One project as declared, before borrow resolution.
pub(crate) struct ProjectDecl {
    pub(crate) name: String,
    pub(crate) org: Option<String>,
    pub(crate) source: PathBuf,
    pub(crate) settings: SettingsDecl,
    pub(crate) repos: BTreeMap<String, RepoDecl>,
}

impl ProjectDecl {
    /// The canonical key: `org/name`, or the bare name when unscoped.
    pub(crate) fn key(&self) -> String {
        match &self.org {
            Some(org) => format!("{org}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The declarations, parsed and validated: every project, org, and toolchain the
/// registry declares.
#[derive(Default)]
pub(crate) struct Registry {
    /// Keyed by canonical key (`org/name`, or bare `name`).
    pub(crate) projects: BTreeMap<String, ProjectDecl>,
    /// Bare name → the keys that carry it, for ambiguity detection.
    by_name: BTreeMap<String, Vec<String>>,
    pub(crate) orgs: BTreeMap<String, OrgDecl>,
    pub(crate) toolchains: BTreeMap<String, ToolchainDecl>,
}

impl Registry {
    /// Load the registry from the resolved config root.
    pub(crate) fn load() -> Result<Self> {
        let root = wits_core::config::resolve_root(&CONFIG_ROOT)?;
        Self::load_from(&root)
    }

    /// Like [`load`](Self::load), but `Ok(None)` when there is no config tree at
    /// all — for a caller that *consults* the registry where it exists rather
    /// than requiring one.
    ///
    /// A machine with no projects declared is an ordinary state, not a failure:
    /// `wits stack` and `wits review` work in any git repository, and asking them
    /// to fail because nobody has written a project file yet would be absurd.
    /// What stays an error is a config tree that exists and cannot be read, since
    /// then a declaration *may* be going unseen.
    pub(crate) fn load_optional() -> Result<Option<Self>> {
        match wits_core::config::find_root(&CONFIG_ROOT)? {
            Some(root) => Self::load_from(&root).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn load_from(root: &Path) -> Result<Self> {
        let mut registry = Registry::default();
        let files = wits_core::config::discover_toml(root)
            .with_context(|| format!("scanning config root {}", root.display()))?;
        for file in &files {
            registry
                .ingest(file)
                .with_context(|| format!("loading {}", file.display()))?;
        }
        registry.validate_borrows()?;
        Ok(registry)
    }

    fn ingest(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let file = FileDecl::parse(&text)?;

        // Toolchains and orgs are additive registries; a repeated *name* is a
        // conflict (spreading distinct entries across files is the additive part).
        for (name, toolchain) in file.toolchains {
            if self.toolchains.insert(name.clone(), toolchain).is_some() {
                bail!("toolchain '{name}' is defined more than once");
            }
        }
        if let Some(org) = file.org {
            let name = org.name.clone();
            if self.orgs.insert(name.clone(), org).is_some() {
                bail!("org '{name}' is declared more than once");
            }
        }

        match file.project {
            Some(project) => {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("project file has no usable stem")?
                    .to_owned();
                if !file.repos.contains_key("main") {
                    bail!("project '{name}' has no [repos.main] (it is a required root)");
                }
                if file.repos["main"].from.is_some() {
                    bail!(
                        "project '{name}': [repos.main] may not be borrowed — \
                         the project root is its own"
                    );
                }
                for (repo_name, repo) in &file.repos {
                    if repo.path.is_none() && repo.from.is_none() {
                        bail!("project '{name}', repo '{repo_name}': needs a 'path' or a 'from'");
                    }
                    crate::remotes::validate(repo_name, repo)
                        .with_context(|| format!("project '{name}'"))?;
                    validate_strategy(&name, repo_name, repo)?;
                }
                let project = ProjectDecl {
                    name: name.clone(),
                    org: project.org,
                    source: path.to_path_buf(),
                    settings: project.settings,
                    repos: file.repos,
                };
                let key = project.key();
                if self.projects.contains_key(&key) {
                    bail!("project '{key}' is defined in more than one file");
                }
                self.by_name.entry(name).or_default().push(key.clone());
                self.projects.insert(key, project);
            }
            None => {
                // The sections are routed by content, so `[repos.*]` without a
                // `[project]` has nothing to belong to and would be silently
                // dropped; a declaration typo must not disappear.
                if !file.repos.is_empty() {
                    bail!(
                        "a [repos.*] table needs a [project] table in the same file \
                         ({} repos declared)",
                        file.repos.len()
                    );
                }
            }
        }
        Ok(())
    }

    /// The cross-file half of declaration validation: every borrow's source
    /// project and repo exists, and is not itself borrowed.
    fn validate_borrows(&self) -> Result<()> {
        for (key, project) in &self.projects {
            for (repo_name, repo) in &project.repos {
                let Some(reference) = &repo.from else {
                    continue;
                };
                let context = format!(
                    "project '{key}', repo '{repo_name}': from '{}:{}'",
                    reference.project, reference.repo
                );
                let source_project = self.project(&reference.project).with_context(|| context)?;
                let source = source_project.repos.get(&reference.repo).with_context(|| {
                    format!(
                        "project '{}' has no repo '{}'",
                        source_project.key(),
                        reference.repo
                    )
                })?;
                if source.from.is_some() {
                    bail!(
                        "repo '{}' of '{}' is itself borrowed; a borrow may not be borrowed",
                        reference.repo,
                        source_project.key()
                    );
                }
            }
        }
        Ok(())
    }

    /// Resolve a name reference (`name` or `org/name`) to a project.
    pub(crate) fn project(&self, reference: &str) -> Result<&ProjectDecl> {
        if let Some(p) = self.projects.get(reference) {
            return Ok(p);
        }
        if reference.contains('/') {
            bail!("no project '{reference}'{}", self.available());
        }
        match self.by_name.get(reference).map(Vec::as_slice) {
            Some([only]) => Ok(&self.projects[only]),
            Some(many) if many.len() > 1 => bail!(
                "project '{reference}' is ambiguous across orgs ({}); qualify it as org/name",
                many.join(", ")
            ),
            _ => bail!("no project '{reference}'{}", self.available()),
        }
    }

    fn available(&self) -> String {
        if self.projects.is_empty() {
            ". No projects are configured.".to_string()
        } else {
            format!(
                ". Available: {}",
                self.projects.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        }
    }
}

/// Check the fields that define where a bare-backed strategy keeps its working
/// trees. There is no rendering here: whether `worktree_dir` *resolves* is the
/// workspace's question. Called once for the declarations and again for the
/// resolved repos after borrow copying, where a travelling strategy has landed.
pub(crate) fn validate_strategy(project: &str, repo_name: &str, repo: &RepoDecl) -> Result<()> {
    let strategy = repo.strategy;
    if strategy.is_bare_backed() && repo.worktree_dir.is_none() {
        bail!(
            "project '{project}', repo '{repo_name}': strategy '{}' requires worktree_dir",
            strategy.as_str()
        );
    }
    if strategy == BranchStrategy::Hybrid && repo.bootstrap_worktree_dir.is_none() {
        bail!(
            "project '{project}', repo '{repo_name}': hybrid strategy requires \
             bootstrap_worktree_dir"
        );
    }
    if strategy == BranchStrategy::InPlace && repo.bootstrap_worktree_dir.is_some() {
        bail!(
            "project '{project}', repo '{repo_name}': bootstrap_worktree_dir requires \
             branch_strategy = \"worktree\" or \"hybrid\""
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The whole error chain of a load that must fail. `{:#}` so a message the
    /// loader attached as *context* (which is where the borrow errors land) is
    /// part of what a test can assert on.
    fn load_err(dir: &Path) -> String {
        match Registry::load_from(dir) {
            Ok(_) => panic!("expected the load to fail"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn loads_and_resolves_by_name_and_org() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "mesa/lavapipe.toml",
            r#"
            [project]
            org = "mesa"
            [repos.main]
            path = "~/src/mesa"
            main_branch = "main"
            "#,
        );
        write(
            dir.path(),
            "hello.toml",
            r#"
            [project]
            [repos.main]
            path = "/tmp/hello"
            main_branch = "main"
            "#,
        );
        let registry = Registry::load_from(dir.path()).unwrap();
        assert_eq!(registry.projects.len(), 2);
        assert_eq!(registry.project("hello").unwrap().name, "hello");
        // bare name resolves through the org
        assert_eq!(
            registry.project("lavapipe").unwrap().org.as_deref(),
            Some("mesa")
        );
        assert_eq!(registry.project("mesa/lavapipe").unwrap().name, "lavapipe");
        assert!(registry.project("nope").is_err());
    }

    #[test]
    fn duplicate_project_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"
            [project]
            [repos.main]
            path = "/tmp/x"
            main_branch = "main"
            "#;
        write(dir.path(), "a/x.toml", body);
        write(dir.path(), "b/x.toml", body);
        assert!(load_err(dir.path()).contains("more than one file"));
    }

    #[test]
    fn duplicate_org_and_toolchain_are_rejected() {
        // The same org name in two files.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "one.toml",
            r#"
            [org]
            name = "acme"
            [toolchains.cc]
            cc = "clang"
            [project]
            org = "acme"
            [repos.main]
            path = "/tmp/x"
            main_branch = "main"
            "#,
        );
        write(dir.path(), "two.toml", "[org]\nname = \"acme\"\n");
        let err = load_err(dir.path());
        assert!(
            err.contains("org 'acme' is declared more than once"),
            "{err}"
        );

        // The same toolchain name in two files.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "one.toml", "[toolchains.cc]\ncc = \"clang\"\n");
        write(dir.path(), "two.toml", "[toolchains.cc]\ncc = \"clang\"\n");
        let err = load_err(dir.path());
        assert!(
            err.contains("toolchain 'cc' is defined more than once"),
            "{err}"
        );
    }

    #[test]
    fn a_project_needs_a_repos_main() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "x.toml",
            r#"
            [project]
            [repos.other]
            path = "/src/other"
            main_branch = "main"
            "#,
        );
        let err = load_err(dir.path());
        assert!(err.contains("no [repos.main]"), "{err}");
    }

    #[test]
    fn a_repo_needs_exactly_one_of_path_and_from() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "x.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/x"
            main_branch = "main"
            [repos.other]
            anchor = "main"
            "#,
        );
        let err = load_err(dir.path());
        assert!(err.contains("'path' or a 'from'"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "x.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/x"
            main_branch = "main"
            [repos.other]
            path = "/src/other"
            from = "x"
            "#,
        );
        // A borrow supplies the path, so declaring both is a borrow conflict,
        // rejected at parse time.
        let err = load_err(dir.path());
        assert!(err.contains("may not declare path"), "{err}");
    }

    /// The sections are routed by content: a `[repos.*]` table with no
    /// `[project]` has nothing to belong to and must not be silently dropped.
    #[test]
    fn repos_without_a_project_table_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "x.toml",
            r#"
            [repos.main]
            path = "/src/x"
            main_branch = "main"
            "#,
        );
        let err = load_err(dir.path());
        assert!(err.contains("needs a [project] table"), "{err}");
    }

    /// The project root is the project's own: a borrow supplies a component,
    /// not the identity of the project being declared.
    #[test]
    fn a_borrow_may_not_be_repos_main() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "work.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/work"
            main_branch = "main"
            "#,
        );
        write(
            dir.path(),
            "wrap.toml",
            r#"
            [project]
            [repos.main]
            from = "work"
            "#,
        );
        let err = load_err(dir.path());
        assert!(err.contains("[repos.main] may not be borrowed"), "{err}");
    }

    #[test]
    fn bare_backed_strategies_require_their_path_fields() {
        let cases = [
            (
                r#"
                branch_strategy = "worktree"
                "#,
                "requires worktree_dir",
            ),
            (
                r#"
                branch_strategy = "hybrid"
                worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
                "#,
                "requires bootstrap_worktree_dir",
            ),
            (
                r#"
                branch_strategy = "in-place"
                bootstrap_worktree_dir = "{{repo.path}}.primary"
                "#,
                "requires branch_strategy",
            ),
        ];
        for (fields, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "x.toml",
                &format!(
                    r#"
                    [project]
                    [repos.main]
                    path = "/src/x.git"
                    main_branch = "main"
                    {fields}
                    "#
                ),
            );
            let err = load_err(dir.path());
            assert!(err.contains(expected), "unexpected error: {err}");
        }
    }

    #[test]
    fn borrow_rejects_a_locally_declared_travelling_field() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "work.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/work"
            main_branch = "main"
            "#,
        );
        write(
            dir.path(),
            "wrap.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/wrap"
            main_branch = "main"
            [repos.w]
            from = "work"
            main_branch = "mine"
            "#,
        );
        let err = load_err(dir.path());
        assert!(err.contains("main_branch"), "unexpected error: {err}");
    }

    #[test]
    fn a_borrow_may_not_be_borrowed() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "work.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/work"
            main_branch = "main"
            "#,
        );
        write(
            dir.path(),
            "mid.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/mid"
            main_branch = "main"
            [repos.w]
            from = "work"
            "#,
        );
        write(
            dir.path(),
            "top.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/top"
            main_branch = "main"
            [repos.w]
            from = "mid:w"
            "#,
        );
        let err = load_err(dir.path());
        assert!(
            err.contains("may not be borrowed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn borrow_of_an_unknown_project_or_repo_is_rejected() {
        let base = r#"
            [project]
            [repos.main]
            path = "/src/work"
            main_branch = "main"
            "#;
        for (bad, needle) in [
            ("from = \"ghost\"", "no project"),
            ("from = \"work:nope\"", "no repo"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "work.toml", base);
            write(
                dir.path(),
                "wrap.toml",
                &format!(
                    r#"
                    [project]
                    [repos.main]
                    path = "/src/wrap"
                    main_branch = "main"
                    [repos.w]
                    {bad}
                    "#
                ),
            );
            let err = load_err(dir.path());
            assert!(err.contains(needle), "for {bad}: unexpected error: {err}");
        }
    }

    #[test]
    fn remote_role_rules_are_checked() {
        let load = |remotes: &str| -> Result<(), String> {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "x.toml",
                &format!(
                    r#"
                    [project]
                    [repos.main]
                    path = "/src/x"
                    main_branch = "main"
                    {remotes}
                    "#
                ),
            );
            Registry::load_from(dir.path())
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        };

        let err = load("[repos.main.remotes]\norigin = \"\"\n").unwrap_err();
        assert!(err.contains("url is empty"), "{err}");

        let err =
            load("[repos.main.remotes]\norigin = { url = \"https://x\", role = \"upstream\" }\n")
                .unwrap_err();
        assert!(err.contains("named after the 'origin' role"), "{err}");

        let err = load(
            "[repos.main.remotes]\norigin = \"https://x\"\nfork = { url = \"https://y\", role = \"origin\" }\n",
        )
        .unwrap_err();
        assert!(err.contains("hold the 'origin' role"), "{err}");

        // A free name holding a role, and mirrors, are both legal.
        load(
            "[repos.main.remotes]\nfork = { url = \"https://y\", role = \"origin\", mirrors = [\"https://z\"] }\n",
        )
        .unwrap();
    }
}
