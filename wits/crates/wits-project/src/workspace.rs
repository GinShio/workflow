//! The resolved registry: every project, org, and toolchain, with each repo's
//! declaration completed and its checkout located.
//!
//! A [`Project`] owns its repos as [`Repo`] values: the parsed [`RepoDecl`]
//! (with a borrow's travelling fields filled from its source), the inferred
//! [`Kind`], the resolved `borrowed_path` when it borrows, and the
//! [`RepoKey`] whose namespace its path templates render in. Nothing here
//! renders a template except [`Project::repo_abs_path`], and nothing queries git
//! except the reverse path lookups, which read the live worktree inventory.
//!
//! The declaration loader ([`crate::load`]) has already validated names, remote
//! roles, strategy fields, and borrow shapes; this module resolves the one thing
//! a borrow cannot answer from the file alone — where the source checkout is —
//! and then answers the reverse questions: which project owns a path, which
//! checkout a repo is on, and what a repo's own path resolves to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_core::git::Repository;

use crate::context::{self, Ctx};
use crate::decl::{infer_kind, is_nested, Kind, OrgDecl, RepoDecl, SettingsDecl, ToolchainDecl};
use crate::layout;
use crate::load;

/// A repo's identity: the project that *owns* the checkout, and the repo name
/// its own path templates render under there.
///
/// A borrowed repo carries its source's key, so `worktree_dir` and any other
/// owner-namespaced template keep rendering in the namespace they were written
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoKey {
    /// Canonical project key: `org/name`, or the bare name when unscoped.
    pub project: String,
    /// The repo name in the owning project.
    pub repo: String,
}

/// One project, fully resolved.
pub struct Project {
    pub(crate) name: String,
    pub(crate) org: Option<String>,
    pub(crate) source: PathBuf,
    pub(crate) settings: SettingsDecl,
    pub(crate) repos: BTreeMap<String, Repo>,
}

/// One repo with its declaration completed.
pub struct Repo {
    pub(crate) name: String,
    pub(crate) decl: RepoDecl,
    pub(crate) kind: Kind,
    /// The source's primary path, for a borrow; `None` for an own repo, whose
    /// path lives in [`RepoDecl::path`].
    pub(crate) borrowed_path: Option<PathBuf>,
    pub(crate) template_owner: RepoKey,
}

impl Repo {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn decl(&self) -> &RepoDecl {
        &self.decl
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Is this repo borrowed from another project (`from`)? The borrower may
    /// build it but does not own it — so `update` leaves it to its owner, and it
    /// never wins a path lookup.
    pub fn is_borrowed(&self) -> bool {
        self.decl.from.is_some()
    }
}

/// The whole registry, resolved.
pub struct Workspace {
    pub(crate) projects: BTreeMap<String, Project>,
    /// Bare name → the keys that carry it, for ambiguity detection.
    by_name: BTreeMap<String, Vec<String>>,
    pub(crate) orgs: BTreeMap<String, OrgDecl>,
    pub(crate) toolchains: BTreeMap<String, ToolchainDecl>,
}

impl Workspace {
    /// Load the registry from the resolved config root.
    pub fn load() -> Result<Self> {
        Self::from_registry(load::Registry::load()?)
    }

    /// Like [`load`](Self::load), but `Ok(None)` when there is no config tree at
    /// all — for a caller that *consults* the registry where it exists rather
    /// than requiring one.
    pub fn load_optional() -> Result<Option<Self>> {
        match load::Registry::load_optional()? {
            Some(registry) => Self::from_registry(registry).map(Some),
            None => Ok(None),
        }
    }

    /// Load from an explicit config root; the test and embedder entry point.
    pub fn load_from(root: &Path) -> Result<Self> {
        Self::from_registry(load::Registry::load_from(root)?)
    }

    fn from_registry(registry: load::Registry) -> Result<Self> {
        let mut projects = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, decl) in registry.projects {
            let repos = decl
                .repos
                .into_iter()
                .map(|(name, repo_decl)| {
                    let kind = infer_kind(&name, &repo_decl);
                    let owner = RepoKey {
                        project: key.clone(),
                        repo: name.clone(),
                    };
                    (
                        name.clone(),
                        Repo {
                            name,
                            decl: repo_decl,
                            kind,
                            borrowed_path: None,
                            template_owner: owner,
                        },
                    )
                })
                .collect();
            by_name
                .entry(decl.name.clone())
                .or_default()
                .push(key.clone());
            projects.insert(
                key,
                Project {
                    name: decl.name,
                    org: decl.org,
                    source: decl.source,
                    settings: decl.settings,
                    repos,
                },
            );
        }

        let mut ws = Workspace {
            projects,
            by_name,
            orgs: registry.orgs,
            toolchains: registry.toolchains,
        };
        ws.resolve_borrows()?;
        ws.validate_layout()?;
        Ok(ws)
    }

    /// Fill in every `from` borrow from the project that owns the repo.
    ///
    /// Two passes over one immutable snapshot: gather, then apply — so a borrow
    /// can never observe another borrow's resolution, which is what makes "a
    /// borrow may not be borrowed" enforceable rather than order-dependent.
    /// (The loader has already rejected a borrow of a borrow; the second pass
    /// would refuse one anyway.)
    fn resolve_borrows(&mut self) -> Result<()> {
        struct Pending {
            borrower: String,
            repo: String,
            path: PathBuf,
            owner: RepoKey,
            decl: RepoDecl,
        }

        let mut pending = Vec::new();
        for (key, project) in &self.projects {
            for (repo_name, repo) in &project.repos {
                let Some(reference) = &repo.decl.from else {
                    continue;
                };
                let source_project = self.project(&reference.project).with_context(|| {
                    format!(
                        "project '{key}', repo '{repo_name}': from '{}'",
                        reference.project
                    )
                })?;
                let source = source_project.repos.get(&reference.repo).with_context(|| {
                    format!(
                        "project '{}' has no repo '{}'",
                        source_project.key(),
                        reference.repo
                    )
                })?;
                if source.is_borrowed() {
                    bail!(
                        "repo '{}' of '{}' is itself borrowed; a borrow may not be borrowed",
                        reference.repo,
                        source_project.key()
                    );
                }
                let path = layout::primary_path(self, source_project, source)?;
                pending.push(Pending {
                    borrower: key.clone(),
                    repo: repo_name.clone(),
                    path,
                    owner: source.template_owner.clone(),
                    decl: source.decl.clone(),
                });
            }
        }

        for entry in pending {
            let project = self
                .projects
                .get_mut(&entry.borrower)
                .with_context(|| format!("project '{}' disappeared", entry.borrower))?;
            let repo = project
                .repos
                .get_mut(&entry.repo)
                .with_context(|| format!("repo '{}' disappeared", entry.repo))?;
            // Travelling fields: the component's git identity. `strategy_declared`
            // travels with the strategy it qualifies, so a report attributes the
            // declaration to the project that wrote it.
            repo.decl.main_branch = entry.decl.main_branch;
            repo.decl.strategy = entry.decl.strategy;
            repo.decl.strategy_declared = entry.decl.strategy_declared;
            repo.decl.worktree_dir = entry.decl.worktree_dir;
            repo.decl.bootstrap_worktree_dir = entry.decl.bootstrap_worktree_dir;
            repo.decl.skip = entry.decl.skip;
            repo.decl.remotes = entry.decl.remotes;
            repo.decl.hooks = entry.decl.hooks;
            // The declaration stays honest: the resolved path is separate.
            repo.decl.path = None;
            repo.borrowed_path = Some(entry.path);
            // A borrowed repo is standalone from this project's side.
            repo.kind = Kind::Standalone;
            repo.template_owner = entry.owner;
        }
        Ok(())
    }

    /// The resolved half of declaration validation: the strategy fields are
    /// re-checked after borrow copying (the source's fields have landed in the
    /// borrower), and a `bootstrap_worktree_dir` must render in the *owner's*
    /// namespace, must not name `branch.*` (it exists before any branch-specific
    /// worktree), and must not be empty.
    fn validate_layout(&self) -> Result<()> {
        for project in self.projects.values() {
            for (name, repo) in &project.repos {
                load::validate_strategy(&project.key(), name, &repo.decl)?;
                let Some(template) = repo.decl.bootstrap_worktree_dir.as_deref() else {
                    continue;
                };
                let owner_project = self
                    .projects
                    .get(&repo.template_owner.project)
                    .with_context(|| {
                        format!("template owner '{}' missing", repo.template_owner.project)
                    })?;
                let owner = owner_project
                    .repos
                    .get(&repo.template_owner.repo)
                    .with_context(|| {
                        format!("template owner repo '{}' missing", repo.template_owner.repo)
                    })?;
                let rendered =
                    Ctx::new(context::context_for_repo(self, owner_project, owner.name()))
                        .render_path(template)
                        .with_context(|| {
                            format!(
                                "project '{}', repo '{name}': resolving bootstrap_worktree_dir \
                             (it must not reference branch.*)",
                                project.key()
                            )
                        })?;
                if rendered.trim().is_empty() {
                    bail!(
                        "project '{}', repo '{name}': bootstrap_worktree_dir resolves to an \
                         empty path",
                        project.key()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.values()
    }

    /// Resolve a name reference (`name` or `org/name`) to a project.
    pub fn project(&self, reference: &str) -> Result<&Project> {
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

    /// The project that owns `path` — the one whose repo checkout is the deepest
    /// prefix of `path`.
    pub fn project_for_path(&self, path: &Path) -> Option<&Project> {
        self.repo_for_path(path).map(|(p, _)| p)
    }

    /// Like [`project_for_path`](Self::project_for_path), but also names the
    /// specific repo.
    ///
    /// **Borrowed repos are not candidates.** One checkout shared by N projects
    /// would otherwise be a tie at equal depth, decided by nothing better than
    /// map order. Since a borrow always points at a project that declares that
    /// checkout as its own, skipping borrows leaves exactly one owner.
    pub fn repo_for_path(&self, path: &Path) -> Option<(&Project, &Repo)> {
        let query = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let (owner, repo) = self.owning_repo_for_path(&query, &mut WorktreeMemo::default())?;
        Some(
            self.developed_as(owner, repo.name())
                .unwrap_or((owner, repo)),
        )
    }

    /// The project a checkout is currently developed *as*, and that project's
    /// borrow of it — `None` when the owner names none.
    ///
    /// The redirect follows the **borrow edges**, not the filesystem: among the
    /// named project's repos, the ones whose `from` resolves to this exact
    /// (project, repo) pair are the entries describing how this checkout is built
    /// there. Where more than one does, the project's focus wins.
    ///
    /// A value naming no known project, or one that names a project not borrowing
    /// this repo, leaves the structural answer standing: a registry half-deployed
    /// on some machine must not break every path query.
    pub fn developed_as<'a>(
        &'a self,
        owner: &'a Project,
        repo_name: &str,
    ) -> Option<(&'a Project, &'a Repo)> {
        let spec = owner.repos.get(repo_name)?.decl.developed_as.as_deref()?;
        let project = self.project(spec).ok()?;
        let owner_key = owner.key();
        let focus = project.focus_name(None);
        project
            .repos
            .iter()
            .filter(|(_, repo)| self.borrows(repo, &owner_key, repo_name))
            .max_by_key(|(name, _)| *name == focus)
            .map(|(_, repo)| (project, repo))
    }

    /// Does `repo` borrow the repo `repo_name` of the project keyed `owner_key`?
    ///
    /// The `from` reference is resolved through [`project`](Self::project) before
    /// comparing, so a borrow is recognised whichever way it spells the project.
    fn borrows(&self, repo: &Repo, owner_key: &str, repo_name: &str) -> bool {
        let Some(reference) = &repo.decl.from else {
            return false;
        };
        reference.repo == repo_name
            && self
                .project(&reference.project)
                .is_ok_and(|source| source.key() == owner_key)
    }

    /// Whether any checkout of `project`'s `repo_name` contains `path`.
    ///
    /// The single-repo question behind the path lookups: it includes borrows and
    /// must not be routed through [`repo_for_path`](Self::repo_for_path), whose
    /// job is to choose one repo among many.
    pub fn repo_checkout_contains(&self, project: &Project, repo_name: &str, path: &Path) -> bool {
        let query = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.repo_match_depth(project, repo_name, &query, &mut WorktreeMemo::default())
            .is_some()
    }

    fn owning_repo_for_path<'a>(
        &'a self,
        query: &Path,
        memo: &mut WorktreeMemo,
    ) -> Option<(&'a Project, &'a Repo)> {
        let mut best: Option<(&Project, &Repo, usize)> = None;
        for project in self.projects.values() {
            for repo in project.repos.values() {
                if repo.is_borrowed() {
                    continue;
                }
                let Some(depth) = self.repo_match_depth(project, repo.name(), query, memo) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(_, _, d)| depth > *d) {
                    best = Some((project, repo, depth));
                }
            }
        }
        best.map(|(p, repo, _)| (p, repo))
    }

    /// How deeply `query` sits inside any checkout of `repo`: its declared path,
    /// its primary path, and every live worktree of either.
    fn repo_match_depth(
        &self,
        project: &Project,
        repo_name: &str,
        query: &Path,
        memo: &mut WorktreeMemo,
    ) -> Option<usize> {
        let repo_path = project.repo_abs_path(repo_name).ok()?;
        // Attributing worktrees costs a strategy resolution per worktree, and it
        // can only change the answer where a sibling repo of this project
        // resolves to the same git dir. Compare the git dir, not the declared
        // path: a linked worktree and its bare host are different paths for one
        // repository.
        let contested = project.repos.values().any(|other| {
            other.name() != repo_name
                && project
                    .repo_abs_path(other.name())
                    .is_ok_and(|p| same_git_dir(memo, &p, &repo_path))
        });
        let mut roots = vec![repo_path];
        if let Some(repo) = project.repos.get(repo_name) {
            if let Ok(primary) = layout::primary_path(self, project, repo) {
                if !roots.contains(&primary) {
                    roots.push(primary);
                }
            }
        }
        let mut best: Option<usize> = None;
        let mut deepen = |depth: usize| {
            if best.is_none_or(|seen| depth > seen) {
                best = Some(depth);
            }
        };
        for root in roots {
            if let Some(depth) = containment_depth(query, &root) {
                deepen(depth);
            }
            for wt in memo.live_worktrees(&root) {
                // Containment before attribution: attribution costs a strategy
                // resolution, and most worktrees have nothing to do with the path.
                let Some(depth) = containment_depth(query, &wt.path) else {
                    continue;
                };
                if contested {
                    let Some(repo) = project.repos.get(repo_name) else {
                        continue;
                    };
                    if !layout::worktree_belongs_to(self, project, repo, wt) {
                        continue;
                    }
                }
                deepen(depth);
            }
        }
        best
    }

    pub fn toolchain(&self, name: &str) -> Option<&ToolchainDecl> {
        self.toolchains.get(name)
    }

    pub(crate) fn toolchains(&self) -> &BTreeMap<String, ToolchainDecl> {
        &self.toolchains
    }

    pub fn toolchain_names(&self) -> impl Iterator<Item = &str> {
        self.toolchains.keys().map(String::as_str)
    }

    pub fn org(&self, name: &str) -> Option<&OrgDecl> {
        self.orgs.get(name)
    }

    /// The preset names a project's flags can address at `focus`: the bare names
    /// reachable through the org/project/repo merge, plus each org preset's
    /// qualified `org/name` form.
    pub fn preset_names(&self, project: &Project, focus: &str) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> =
            crate::presets::candidate_preset_names(self, project, focus)
                .into_iter()
                .collect();
        if let Some(org) = &project.org {
            if let Some(org_data) = self.org(org) {
                for name in org_data.presets.keys() {
                    names.insert(format!("{org}/{name}"));
                }
            }
        }
        names.into_iter().collect()
    }

    pub fn repo_names(&self, project: &Project) -> Vec<String> {
        project.repos.keys().cloned().collect()
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

impl Project {
    /// The canonical key: `org/name`, or the bare name when unscoped.
    pub fn key(&self) -> String {
        match &self.org {
            Some(org) => format!("{org}/{}", self.name),
            None => self.name.clone(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn org(&self) -> Option<&str> {
        self.org.as_deref()
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    /// The focus repo's name: `--focus` override → `project.focus` → `"main"`.
    pub fn focus_name<'a>(&'a self, override_focus: Option<&'a str>) -> &'a str {
        override_focus
            .or(self.settings.focus.as_deref())
            .unwrap_or("main")
    }

    pub fn settings(&self) -> &SettingsDecl {
        &self.settings
    }

    pub fn repos(&self) -> impl Iterator<Item = (&str, &Repo)> {
        self.repos.iter().map(|(name, repo)| (name.as_str(), repo))
    }

    pub fn repo(&self, name: &str) -> Option<&Repo> {
        self.repos.get(name)
    }

    pub fn kind_of(&self, name: &str) -> Option<Kind> {
        self.repos.get(name).map(Repo::kind)
    }

    pub fn is_borrowed(&self, name: &str) -> bool {
        self.repos.get(name).is_some_and(Repo::is_borrowed)
    }

    /// The on-disk location of a repo. A borrow returns the path resolved at
    /// load; an own repo renders `path` as a template against a Profile-free
    /// context (`project.name`, `project.org`, `env.*`, `system.*`), then
    /// expands `~`. Nested (relative) paths join under `repos.main`.
    pub fn repo_abs_path(&self, name: &str) -> Result<PathBuf> {
        let repo = self
            .repos
            .get(name)
            .with_context(|| format!("repo '{name}' not found"))?;
        if let Some(path) = &repo.borrowed_path {
            return Ok(path.clone());
        }
        let rendered = self.rendered_path(name, repo)?;
        if is_nested(&rendered) && name != "main" {
            let main = self.repos.get("main").with_context(|| {
                format!("repo '{name}' has a nested path but 'main' is not declared")
            })?;
            let main_rendered = self.rendered_path("main", main)?;
            Ok(expand_tilde(&main_rendered).join(&rendered))
        } else {
            Ok(expand_tilde(&rendered))
        }
    }

    fn rendered_path(&self, repo_name: &str, repo: &Repo) -> Result<String> {
        let tpl = repo.decl.path.as_deref().with_context(|| {
            format!("repo '{repo_name}' has no path (an unresolved 'from' borrow?)")
        })?;
        render_path_template(tpl, &self.name, self.org.as_deref())
            .with_context(|| format!("resolving path template for repo '{repo_name}': {tpl:?}"))
    }
}

/// Do two repo paths name the same git dir? A linked worktree and its bare host
/// are different paths for one repository, which is the case sibling repos are
/// told apart by strategy in.
fn same_git_dir(memo: &mut WorktreeMemo, a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (memo.common_dir(a), memo.common_dir(b)) {
        (Some(a), Some(b)) => layout::same_path(&a, &b),
        // Neither path is a repository (yet): no worktree can exist under either.
        _ => false,
    }
}

/// How deeply `query` sits inside `candidate`, or `None` when it does not.
fn containment_depth(query: &Path, candidate: &Path) -> Option<usize> {
    let real = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    query.starts_with(&real).then(|| real.components().count())
}

/// Live worktrees per git dir, remembered for the duration of one path query.
///
/// Deliberately **not** stored on [`Workspace`]: a command that creates a
/// worktree and then asks a path question must see the new one.
#[derive(Default)]
struct WorktreeMemo {
    by_git_dir: std::collections::HashMap<PathBuf, Vec<wits_core::git::Worktree>>,
    common_dirs: std::collections::HashMap<PathBuf, Option<PathBuf>>,
}

impl WorktreeMemo {
    /// The common dir of one path, queried at most once per path query. Sibling
    /// attribution asks the same question for every repo of a project, so an
    /// uncached answer would spawn git per sibling per repo.
    fn common_dir(&mut self, path: &Path) -> Option<PathBuf> {
        self.common_dirs
            .entry(path.to_path_buf())
            .or_insert_with(|| Repository::new(path).git_common_dir())
            .clone()
    }

    fn live_worktrees(&mut self, root: &Path) -> &[wits_core::git::Worktree] {
        // The common dir is stable across a bare repository and its linked
        // worktrees, and across both spellings of a checkout, so one query per
        // git dir serves every root that reaches it. A path that is not a
        // repository has no common dir; key it by itself, as before.
        let key = self.common_dir(root).unwrap_or_else(|| root.to_path_buf());
        self.by_git_dir.entry(key).or_insert_with(|| {
            Repository::new(root)
                .worktrees()
                .into_iter()
                .filter(wits_core::git::Worktree::is_live)
                .collect()
        })
    }
}

/// Render a `repo.path` template against the shared Profile-free path context
/// (`project.name`, `project.org`, `system.*`, `env.*`; no `repos.*`, which would
/// be circular). Built by [`crate::context::path_context`] so this exact same
/// namespace backs `repo_abs_path` here and any other path resolve.
fn render_path_template(
    tpl: &str,
    project_name: &str,
    project_org: Option<&str>,
) -> Result<String> {
    let root = context::path_context(project_name, project_org);
    Ok(Ctx::new(root).render_path(tpl)?)
}

/// Expand a leading `~/` using `$HOME` (shells usually do this; a quoted path
/// might reach us intact).
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::looks_like_path;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The whole error chain of a load that must fail. `{:#}` so a message the
    /// loader attached as *context* (which is where the borrow errors land) is
    /// part of what a test can assert on.
    fn load_err(dir: &Path) -> String {
        match Workspace::load_from(dir) {
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
        let ws = Workspace::load_from(dir.path()).unwrap();
        assert_eq!(ws.projects().count(), 2);
        assert_eq!(ws.project("hello").unwrap().name, "hello");
        // bare name resolves through the org
        assert_eq!(ws.project("lavapipe").unwrap().org.as_deref(), Some("mesa"));
        assert_eq!(ws.project("mesa/lavapipe").unwrap().name, "lavapipe");
        assert!(ws.project("nope").is_err());
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
        assert!(Workspace::load_from(dir.path()).is_err());
    }

    #[test]
    fn project_for_path_finds_owner() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(checkout.join("src/sub")).unwrap();
        write(
            dir.path(),
            "proj.toml",
            &format!(
                r#"
                [project]
                [repos.main]
                path = "{}"
                main_branch = "main"
                "#,
                checkout.display()
            ),
        );
        let ws = Workspace::load_from(dir.path()).unwrap();
        let found = ws.project_for_path(&checkout.join("src/sub")).unwrap();
        assert_eq!(found.name, "proj");
        assert!(ws.project_for_path(Path::new("/nowhere")).is_none());
    }

    #[test]
    fn path_template_resolves_project_org_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Use an env var the template can reference without touching $HOME.
        let key = "WITS_TEST_PATH_BASE_TEMPLATE";
        std::env::set_var(key, base.to_str().unwrap());
        let checkout = base.join("acme").join("myproj");
        std::fs::create_dir_all(&checkout).unwrap();
        write(
            base,
            "myproj.toml",
            r#"
            [project]
            org = "acme"
            [repos.main]
            path = "{{env.WITS_TEST_PATH_BASE_TEMPLATE}}/{{project.org}}/{{project.name}}"
            main_branch = "main"
            "#,
        );
        let ws = Workspace::load_from(base).unwrap();
        let project = ws.project("acme/myproj").unwrap();
        let abs = project.repo_abs_path("main").unwrap();
        assert_eq!(abs, checkout);
        // project_for_path must resolve an inner path via the templated path.
        let found = ws.project_for_path(&checkout.join("src")).unwrap();
        assert_eq!(found.name, "myproj");
        std::env::remove_var(key);
    }

    #[test]
    fn malformed_path_template_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bad.toml",
            r#"
            [project]
            [repos.main]
            path = "{{no.such.var}}"
            main_branch = "main"
            "#,
        );
        let ws = Workspace::load_from(dir.path()).unwrap();
        let project = ws.project("bad").unwrap();
        assert!(project.repo_abs_path("main").is_err());
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
    fn hybrid_bootstrap_is_fixed_and_worktree_bootstrap_may_be_implicit() {
        let valid = tempfile::tempdir().unwrap();
        write(
            valid.path(),
            "worktree.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/w.git"
            main_branch = "main"
            branch_strategy = "worktree"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
            "#,
        );
        Workspace::load_from(valid.path()).unwrap();

        let invalid = tempfile::tempdir().unwrap();
        write(
            invalid.path(),
            "bad-hybrid.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/h.git"
            main_branch = "main"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.bootstrap/{{branch.slug}}"
            "#,
        );
        let err = load_err(invalid.path());
        assert!(
            err.contains("must not reference branch"),
            "unexpected error: {err}"
        );
    }

    /// The whole point of a borrow: the component's git identity is declared in
    /// the project that owns it, and every consumer resolves to that one checkout.
    #[test]
    fn borrow_imports_the_source_repos_git_identity() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "engine.toml",
            r#"
            [project]
            org = "acme"
            [repos.main]
            path = "/src/acme/engine"
            main_branch = "stable"
            branch_strategy = "hybrid"
            worktree_dir = "{{repo.path}}.wt/{{branch.slug}}"
            bootstrap_worktree_dir = "{{repo.path}}.primary"
            skip = ["/bigdata"]
            [repos.main.remotes]
            origin = "https://example.invalid/engine.git"
            "#,
        );
        write(
            dir.path(),
            "viewer.toml",
            r#"
            [project]
            org = "acme"
            focus = "engine"
            [repos.main]
            path = "/src/acme/viewer"
            main_branch = "main"
            skip = ["/third_party/engine"]
            [repos.engine]
            from = "acme/engine"
            anchor = "main"
            "#,
        );
        let ws = Workspace::load_from(dir.path()).unwrap();
        let consumer = ws.project("acme/viewer").unwrap();
        let borrowed = &consumer.repo("engine").unwrap();

        assert_eq!(
            consumer.repo_abs_path("engine").unwrap(),
            PathBuf::from("/src/acme/engine")
        );
        // The declaration stays honest: the resolved path is stored separately,
        // never written back into `path`.
        assert!(borrowed.decl().path.is_none());
        assert_eq!(borrowed.decl().main_branch.as_deref(), Some("stable"));
        assert_eq!(borrowed.decl().strategy.as_str(), "hybrid");
        assert_eq!(
            borrowed.decl().bootstrap_worktree_dir.as_deref(),
            Some("{{repo.path}}.primary")
        );
        assert_eq!(
            borrowed.decl().remotes["origin"].url,
            "https://example.invalid/engine.git"
        );
        // The component's own `skip` travels; the consumer's stays the consumer's.
        assert_eq!(borrowed.decl().skip, vec!["/bigdata"]);
        assert_eq!(
            consumer.repo("main").unwrap().decl().skip,
            vec!["/third_party/engine"]
        );
        // The borrower keeps what is about *its* build.
        assert_eq!(borrowed.decl().anchor.as_deref(), Some("main"));
        assert!(consumer.is_borrowed("engine") && !consumer.is_borrowed("main"));
        // An external checkout from this project's side, whatever it is in its own.
        assert_eq!(consumer.kind_of("engine"), Some(Kind::Standalone));
    }

    /// A borrow may name a *nested* repo, which is how "the component lives inside
    /// one of the projects that consumes it" is expressed. Its path must resolve
    /// against the **source** project's root, not the borrower's.
    #[test]
    fn borrowing_a_nested_repo_resolves_under_its_own_root() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "viewer.toml",
            r#"
            [project]
            [repos.main]
            path = "/src/viewer"
            main_branch = "main"
            [repos.engine]
            path = "third_party/engine"
            main_branch = "stable"
            anchor = "main"
            "#,
        );
        write(
            dir.path(),
            "editor.toml",
            r#"
            [project]
            focus = "engine"
            [repos.main]
            path = "/src/editor"
            main_branch = "main"
            skip = ["/vendor/engine"]
            [repos.engine]
            from = "viewer:engine"
            anchor = "main"
            "#,
        );
        let ws = Workspace::load_from(dir.path()).unwrap();
        let editor = ws.project("editor").unwrap();
        assert_eq!(
            editor.repo_abs_path("engine").unwrap(),
            PathBuf::from("/src/viewer/third_party/engine")
        );
        assert_eq!(
            editor.repo("engine").unwrap().decl().main_branch.as_deref(),
            Some("stable")
        );
    }

    /// One checkout consumed by N projects would tie at equal depth, so the owner
    /// has to win outright rather than by map order.
    #[test]
    fn path_lookup_answers_the_owner_not_a_borrower() {
        let dir = tempfile::tempdir().unwrap();
        let component = dir.path().join("engine");
        std::fs::create_dir_all(component.join("src")).unwrap();
        write(
            dir.path(),
            "engine.toml",
            &format!(
                r#"
                [project]
                [repos.main]
                path = "{}"
                main_branch = "stable"
                "#,
                component.display()
            ),
        );
        // Named to sort *before* the owner, so map order alone would pick it.
        write(
            dir.path(),
            "aaa-consumer.toml",
            r#"
            [project]
            focus = "engine"
            [repos.main]
            path = "/src/consumer"
            main_branch = "main"
            [repos.engine]
            from = "engine"
            anchor = "main"
            "#,
        );
        let ws = Workspace::load_from(dir.path()).unwrap();
        let (owner, repo) = ws.repo_for_path(&component.join("src")).unwrap();
        assert_eq!(owner.name, "engine");
        assert_eq!(repo.name(), "main");
    }

    /// The owner's counterpart to [`path_lookup_answers_the_owner_not_a_borrower`]:
    /// when it says the checkout is being developed as a borrower, the answer moves
    /// to that borrower's *entry* — the one carrying how the checkout is built
    /// there. Two of the borrower's repos borrow it, so returning the focus proves
    /// the focus preference rather than map order.
    #[test]
    fn developed_as_moves_the_answer_to_the_named_borrowers_focus() {
        let dir = tempfile::tempdir().unwrap();
        let component = dir.path().join("engine");
        std::fs::create_dir_all(component.join("src")).unwrap();
        let owner = |declaration: &str| {
            write(
                dir.path(),
                "engine.toml",
                &format!(
                    r#"
                    [project]
                    [repos.main]
                    path = "{}"
                    main_branch = "stable"
                    {declaration}
                    "#,
                    component.display()
                ),
            );
        };
        write(
            dir.path(),
            "consumer.toml",
            r#"
            [project]
            focus = "engine"
            [repos.main]
            path = "/src/consumer"
            main_branch = "main"
            [repos.engine]
            from = "engine"
            anchor = "main"
            [repos.engine-review]
            from = "engine"
            anchor = "main"
            "#,
        );
        let resolved = |dir: &Path| {
            let ws = Workspace::load_from(dir).unwrap();
            let (project, repo) = ws.repo_for_path(&component.join("src")).unwrap();
            (project.name.clone(), repo.name().to_owned())
        };

        owner(r#"developed_as = "consumer""#);
        assert_eq!(
            resolved(dir.path()),
            ("consumer".to_owned(), "engine".to_owned())
        );

        // Both ways the value can resolve to nothing leave the structural answer
        // standing, so one unreadable line cannot break every path query.
        for declaration in [
            r#"developed_as = "no-such-project""#,
            // A real project, but one that does not borrow this repo.
            r#"developed_as = "engine""#,
            "",
        ] {
            owner(declaration);
            assert_eq!(
                resolved(dir.path()),
                ("engine".to_owned(), "main".to_owned()),
                "declaration: {declaration}"
            );
        }
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
    fn a_repo_needs_a_path_or_a_from() {
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
        assert!(err.contains("'path' or a 'from'"), "unexpected: {err}");
    }

    #[test]
    fn path_classifier() {
        assert!(looks_like_path("."));
        assert!(looks_like_path("./sub"));
        assert!(looks_like_path("/abs"));
        assert!(looks_like_path("~/x"));
        assert!(!looks_like_path("hello"));
        assert!(!looks_like_path("mesa/lavapipe"));
    }
}
