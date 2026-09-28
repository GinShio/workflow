//! Loading the project registry: find the config root, scan it, and route each
//! file's sections into projects, toolchains, and orgs.
//!
//! Configuration is content-addressed (`docs/reference/project-design.rst`,
//! "Configuration topology"; quoted section names below are from the same
//! document): files live anywhere under one root and declare what they are by
//! their sections, so loading is "read every `*.toml`, look at what's inside,
//! file it accordingly". A project's *name* is its file stem; its *org* is
//! explicit (`project.org`). The same `(org, name)`
//! twice is a conflict, not a silent override — cross-file layering of one
//! project was a foot-gun we don't reproduce.
//!
//! `repo.path` is a template resolved against a Profile-free context (`project.name`,
//! `project.org`, `env.*`, `system.*`; no `repos.*` to avoid circularity). This
//! lets paths like `~/Projects/{{project.org}}/{{project.name}}` work. Because
//! no Profile is required, [`Workspace::project_for_path`] — the reverse lookup
//! `wits stack` and git hooks lean on — stays answerable without a Profile.
//!
//! A repo declared with `from` **borrows** another project's repo. That is
//! resolved here, once, right after every file is ingested: the source repo's
//! git identity is copied into the borrower with its `path` already made
//! absolute, so nothing downstream — `repo_abs_path`, `repo_value`, `infer_kind`,
//! `update`, the resolver — needs to know a borrow ever happened. A borrow may
//! not itself be borrowed, which is what keeps this one pass rather than a graph
//! walk with cycle detection. Borrowed entries also never win
//! [`Workspace::repo_for_path`]: the project whose own `repos.main` a checkout is
//! owns it, so `cd` into a shared component and you land on *its* project —
//! unless that owner says the checkout is currently developed as one of its
//! borrowers ([`Workspace::developed_as`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::context::Ctx;
use crate::git::Repository;

use super::model::{
    infer_kind, is_nested, parse_borrow, BranchStrategy, Kind, RawFile, RawPreset, RawProject,
    RawRepo, RawToolchain,
};

/// Org-level data retained after loading: shared environment/definitions plus
/// the org's declared presets. The environment/definitions are the org's
/// unconditional contribution to every project that joins it — inherited by the
/// build pipeline below the project's own layer — and are *also* exposed as
/// `org.environment.*` / `org.definitions.*` for templates that want to name one
/// directly. The presets, by contrast, only apply when selected by name.
pub struct OrgData {
    pub environment: std::collections::BTreeMap<String, toml::Value>,
    pub definitions: std::collections::BTreeMap<String, toml::Value>,
    pub presets: std::collections::BTreeMap<String, RawPreset>,
}

/// One project as loaded from disk (still raw / unresolved).
pub struct ProjectData {
    pub name: String,
    pub org: Option<String>,
    pub source: PathBuf,
    pub project: RawProject,
    pub repos: BTreeMap<String, RawRepo>,
}

impl ProjectData {
    /// The canonical key: `org/name`, or the bare name when unscoped.
    pub fn key(&self) -> String {
        match &self.org {
            Some(org) => format!("{org}/{}", self.name),
            None => self.name.clone(),
        }
    }

    /// The focus repo's name: `--focus` override → `project.focus` → `"main"`.
    pub fn focus_name<'a>(&'a self, override_focus: Option<&'a str>) -> &'a str {
        override_focus
            .or(self.project.focus.as_deref())
            .unwrap_or("main")
    }

    pub fn kind_of(&self, repo_name: &str) -> Option<Kind> {
        self.repos.get(repo_name).map(|r| infer_kind(repo_name, r))
    }

    /// Is this repo borrowed from another project (`from`)? The borrower may
    /// build it but does not own it — so `update` leaves it to its owner, and it
    /// never wins a path lookup (module docs).
    pub fn is_borrowed(&self, repo_name: &str) -> bool {
        self.repos.get(repo_name).is_some_and(|r| r.from.is_some())
    }

    /// The on-disk location of a repo. `path` is resolved as a template against
    /// a Profile-free context (`project.name`, `project.org`, `env.*`, `system.*`),
    /// then `~` is expanded. Nested (relative) paths are joined under `repos.main`.
    /// Returns an error if the template is malformed or resolves to a non-string.
    pub fn repo_abs_path(&self, repo_name: &str) -> Result<PathBuf> {
        let repo = self
            .repos
            .get(repo_name)
            .with_context(|| format!("repo '{repo_name}' not found"))?;
        let rendered = self.rendered_path(repo_name, repo)?;
        if is_nested(&rendered) && repo_name != "main" {
            let main = self.repos.get("main").with_context(|| {
                format!("repo '{repo_name}' has a nested path but 'main' is not declared")
            })?;
            let main_rendered = self.rendered_path("main", main)?;
            Ok(expand_tilde(&main_rendered).join(&rendered))
        } else {
            Ok(expand_tilde(&rendered))
        }
    }

    fn rendered_path(&self, repo_name: &str, repo: &RawRepo) -> Result<String> {
        let tpl = repo.path.as_deref().with_context(|| {
            format!("repo '{repo_name}' has no path (an unresolved 'from' borrow?)")
        })?;
        render_path_template(tpl, &self.name, self.org.as_deref())
            .with_context(|| format!("resolving path template for repo '{repo_name}': {tpl:?}"))
    }
}

pub struct Workspace {
    /// Keyed by `org/name` (or bare `name`).
    projects: BTreeMap<String, ProjectData>,
    /// Bare name → the keys that carry it, for ambiguity detection.
    by_name: BTreeMap<String, Vec<String>>,
    toolchains: BTreeMap<String, RawToolchain>,
    orgs: BTreeMap<String, OrgData>,
}

/// How deeply `query` sits inside `candidate`, or `None` when it does not.
///
/// The depth is the matched path's component count, so a caller comparing
/// candidates can prefer the most specific one.
fn containment_depth(query: &Path, candidate: &Path) -> Option<usize> {
    let real = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    query.starts_with(&real).then(|| real.components().count())
}

/// Live worktrees per git dir, remembered for the duration of one path query.
///
/// Resolving a path walks every repo of every project, and many of them resolve
/// to the same git dir — a bare-backed component and each of its borrowers, every
/// repo of a monorepo. Asking git once per repo instead of once per git dir is
/// where a single path query comes to spawn a hundred `git worktree list`
/// processes.
///
/// Deliberately **not** stored on [`Workspace`]: a command that creates a
/// worktree and then asks a path question must see the new one, and a cache
/// living as long as the registry would answer from before the mutation. Scoped
/// to one query, there is no window in which it can be stale.
#[derive(Default)]
struct WorktreeMemo {
    by_git_dir: std::collections::HashMap<PathBuf, Vec<crate::git::Worktree>>,
}

impl WorktreeMemo {
    fn live_worktrees(&mut self, root: &Path) -> &[crate::git::Worktree] {
        self.by_git_dir
            .entry(root.to_path_buf())
            .or_insert_with(|| {
                Repository::new(root)
                    .worktrees()
                    .into_iter()
                    .filter(crate::git::Worktree::is_live)
                    .collect()
            })
    }
}

impl Workspace {
    pub fn toolchains(&self) -> &BTreeMap<String, RawToolchain> {
        &self.toolchains
    }

    pub fn org_presets(&self, org: &str) -> Option<&BTreeMap<String, RawPreset>> {
        self.orgs.get(org).map(|d| &d.presets)
    }

    pub fn org_base(&self, org: &str) -> Option<&OrgData> {
        self.orgs.get(org)
    }

    pub fn projects(&self) -> impl Iterator<Item = &ProjectData> {
        self.projects.values()
    }

    /// Load the registry from the resolved config root ("Config-root resolution").
    pub fn load() -> Result<Self> {
        let root = crate::config::resolve_root(&CONFIG_ROOT)?;
        Self::load_from(&root)
    }

    /// Like [`load`](Self::load), but `Ok(None)` when there is no config tree at
    /// all — for a caller that *consults* the registry where it exists rather than
    /// requiring one.
    ///
    /// A machine with no projects declared is an ordinary state, not a failure:
    /// `wits stack` and `wits review` work in any git repository, and asking them
    /// to fail because nobody has written a project file yet would be absurd. What
    /// stays an error is a config tree that exists and cannot be read, since then
    /// a declaration *may* be going unseen — see [`super::remotes::for_checkout`].
    pub fn load_optional() -> Result<Option<Self>> {
        match crate::config::find_root(&CONFIG_ROOT)? {
            Some(root) => Self::load_from(&root).map(Some),
            None => Ok(None),
        }
    }

    pub fn load_from(root: &Path) -> Result<Self> {
        let mut ws = Workspace {
            projects: BTreeMap::new(),
            by_name: BTreeMap::new(),
            toolchains: BTreeMap::new(),
            orgs: BTreeMap::new(),
        };

        let files = crate::config::discover_toml(root)
            .with_context(|| format!("scanning config root {}", root.display()))?;

        for file in &files {
            ws.ingest(file)
                .with_context(|| format!("loading {}", file.display()))?;
        }
        ws.resolve_borrows()?;
        ws.validate_repo_strategies()?;
        Ok(ws)
    }

    /// Validate the fields that define where a strategy keeps its working
    /// trees. This runs after borrow resolution because those fields travel from
    /// the owning repo and must be judged in their resolved form.
    fn validate_repo_strategies(&self) -> Result<()> {
        for project in self.projects.values() {
            for (name, repo) in &project.repos {
                let strategy = BranchStrategy::parse(repo.branch_strategy.as_deref())
                    .with_context(|| format!("project '{}', repo '{name}'", project.key()))?;
                if strategy.is_bare_backed() && repo.worktree_dir.is_none() {
                    bail!(
                        "project '{}', repo '{name}': strategy '{}' requires worktree_dir",
                        project.key(),
                        repo.branch_strategy.as_deref().unwrap_or("in-place")
                    );
                }
                if strategy == BranchStrategy::Hybrid && repo.bootstrap_worktree_dir.is_none() {
                    bail!(
                        "project '{}', repo '{name}': hybrid strategy requires \
                         bootstrap_worktree_dir",
                        project.key()
                    );
                }
                let Some(template) = repo.bootstrap_worktree_dir.as_deref() else {
                    continue;
                };
                if strategy == BranchStrategy::InPlace {
                    bail!(
                        "project '{}', repo '{name}': bootstrap_worktree_dir requires \
                         branch_strategy = \"worktree\" or \"hybrid\"",
                        project.key()
                    );
                }
                let ctx = Ctx::new(super::context::context_for_repo(self, project, name));
                let rendered = ctx.render_path(template).with_context(|| {
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

    /// Fill in every `from` borrow from the project that owns the repo (module
    /// docs). Two passes over one immutable snapshot: gather, then apply — so a
    /// borrow can never observe another borrow's resolution, which is what makes
    /// "a borrow may not be borrowed" enforceable rather than order-dependent.
    fn resolve_borrows(&mut self) -> Result<()> {
        let mut resolved: Vec<(String, String, RawRepo)> = Vec::new();

        for (key, project) in &self.projects {
            for (repo_name, repo) in &project.repos {
                let Some(spec) = repo.from.as_deref() else {
                    continue;
                };
                let borrowed = self.borrow_source(spec, repo).with_context(|| {
                    format!("project '{key}', repo '{repo_name}': from {spec:?}")
                })?;
                resolved.push((key.clone(), repo_name.clone(), borrowed));
            }
        }

        for (key, repo_name, borrowed) in resolved {
            if let Some(project) = self.projects.get_mut(&key) {
                project.repos.insert(repo_name, borrowed);
            }
        }
        Ok(())
    }

    /// The borrower's repo, with the source repo's git identity filled in and its
    /// path made absolute. Absolute because a nested source resolves relative to
    /// *its* project's `repos.main`, which the borrower knows nothing about.
    fn borrow_source(&self, spec: &str, borrower: &RawRepo) -> Result<RawRepo> {
        let conflicts = borrower.borrowed_field_conflicts();
        if !conflicts.is_empty() {
            bail!(
                "a borrow may not declare {} — they belong to the project that owns the repo",
                conflicts.join(", ")
            );
        }
        let reference = parse_borrow(spec).map_err(anyhow::Error::msg)?;
        let source_project = self.project(reference.project)?;
        let source = source_project.repos.get(reference.repo).with_context(|| {
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
        let abs = super::resolve::repo_primary_path(self, source_project, reference.repo)?;

        let mut out = borrower.clone();
        out.path = Some(abs.display().to_string());
        out.main_branch = source.main_branch.clone();
        out.branch_strategy = source.branch_strategy.clone();
        out.worktree_dir = source.worktree_dir.clone();
        out.bootstrap_worktree_dir = source.bootstrap_worktree_dir.clone();
        out.skip = source.skip.clone();
        out.remotes = source.remotes.clone();
        out.hooks = source.hooks.clone();
        Ok(out)
    }

    fn ingest(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let raw: RawFile = toml::from_str(&text)?;

        // Toolchains and orgs are additive registries; a repeated *name* is a
        // conflict (spreading distinct entries across files is the additive part).
        for (name, tc) in raw.toolchains {
            if self.toolchains.insert(name.clone(), tc).is_some() {
                bail!("toolchain '{name}' is defined more than once");
            }
        }
        if let Some(org) = raw.org {
            let data = OrgData {
                environment: org.environment,
                definitions: org.definitions,
                presets: org.presets,
            };
            if self.orgs.insert(org.name.clone(), data).is_some() {
                bail!("org '{}' is declared more than once", org.name);
            }
        }

        if let Some(project) = raw.project {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("project file has no usable stem")?
                .to_owned();
            if !raw.repos.contains_key("main") {
                bail!("project '{name}' has no [repos.main] (it is a required root)");
            }
            for (repo_name, repo) in &raw.repos {
                if repo.path.is_none() && repo.from.is_none() {
                    bail!("project '{name}', repo '{repo_name}': needs a 'path' or a 'from'");
                }
                super::remotes::validate(repo_name, repo)
                    .with_context(|| format!("project '{name}'"))?;
            }
            let data = ProjectData {
                org: project.org.clone(),
                name: name.clone(),
                source: path.to_path_buf(),
                project,
                repos: raw.repos,
            };
            let key = data.key();
            if self.projects.contains_key(&key) {
                bail!("project '{key}' is defined in more than one file");
            }
            self.by_name.entry(name).or_default().push(key.clone());
            self.projects.insert(key, data);
        }

        Ok(())
    }

    /// Resolve a name reference (`name` or `org/name`) to a project.
    pub fn project(&self, reference: &str) -> Result<&ProjectData> {
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
    /// prefix of `path`. This is the reverse lookup consumers need to answer
    /// "which project am I standing in?".
    pub fn project_for_path(&self, path: &Path) -> Option<&ProjectData> {
        self.repo_for_path(path).map(|(p, _)| p)
    }

    /// Like [`project_for_path`](Self::project_for_path), but also names the
    /// specific repo whose checkout is the deepest prefix of `path` — so a caller
    /// standing in one repo of a multi-repo project (a submodule, a fork sibling)
    /// can ask about *that* repo, not just the project. The repo name is returned
    /// owned so the borrow of `self` stays tied only to the project.
    ///
    /// **Borrowed repos are not candidates.** One checkout shared by N projects
    /// would otherwise be a tie at equal depth, decided by nothing better than
    /// map order — a silently wrong answer. Since a borrow always points at a
    /// project that declares that checkout as its own, skipping borrows leaves
    /// exactly one owner, and the answer becomes "the project this component
    /// *is*", not "whichever project happens to sort first".
    ///
    /// The owner may redirect the answer to a project that *borrows* the repo,
    /// for a checkout several projects share — see
    /// [`developed_as`](Self::developed_as).
    pub fn repo_for_path(&self, path: &Path) -> Option<(&ProjectData, String)> {
        let query = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let (owner, repo) = self.owning_repo_for_path(&query, &mut WorktreeMemo::default())?;
        Some(self.developed_as(owner, &repo).unwrap_or((owner, repo)))
    }

    /// The project a checkout is currently developed *as*, and that project's
    /// borrow of it — `None` when the owner names none.
    ///
    /// A component several projects borrow is owned by exactly one of them, and
    /// [`owning_repo_for_path`](Self::owning_repo_for_path) answers with that
    /// owner. That is right while the owner is the project being built and wrong
    /// when you develop a borrower whose build is identified by the shared
    /// checkout's branch: every path query then names the owner's build tree
    /// rather than the one being built.
    ///
    /// So the owner declares which borrower the checkout currently serves, in
    /// [`RawRepo::developed_as`]. It is the owner's to declare because the
    /// borrowers have no grounds to prefer between themselves — N of them
    /// claiming it would restore the very tie that makes a borrow ineligible
    /// above, whereas the owner is unique by construction.
    ///
    /// The redirect follows the **borrow edges**, not the filesystem: among the
    /// named project's repos, the ones whose `from` resolves to this exact
    /// (project, repo) pair are the entries describing how this checkout is built
    /// there. Where more than one does, the project's focus wins — naming the
    /// project asks for *its build*, and the focus is the repo that build takes
    /// its identity from.
    ///
    /// A value naming no known project, or one that names a project not borrowing
    /// this repo, yields `None` and so leaves the structural answer standing: a
    /// registry half-deployed on some machine must not break every query that
    /// passes through path resolution. `project check` reports both, which is
    /// where a typo can be judged against the whole registry.
    pub fn developed_as(
        &self,
        owner: &ProjectData,
        repo_name: &str,
    ) -> Option<(&ProjectData, String)> {
        let spec = owner.repos.get(repo_name)?.developed_as.as_deref()?;
        let project = self.project(spec).ok()?;
        let owner_key = owner.key();
        let focus = project.focus_name(None);
        project
            .repos
            .iter()
            .filter(|(_, repo)| self.borrows(repo, &owner_key, repo_name))
            .map(|(name, _)| (name == focus, name.clone()))
            .max()
            .map(|(_, name)| (project, name))
    }

    /// Does `repo` borrow the repo `repo_name` of the project keyed `owner_key`?
    ///
    /// The `from` reference is resolved through [`project`](Self::project) before
    /// comparing, so a borrow is recognised whichever way it spells the project —
    /// bare name or `org/name`.
    fn borrows(&self, repo: &RawRepo, owner_key: &str, repo_name: &str) -> bool {
        let Some(spec) = repo.from.as_deref() else {
            return false;
        };
        let Ok(reference) = parse_borrow(spec) else {
            return false;
        };
        reference.repo == repo_name
            && self
                .project(reference.project)
                .is_ok_and(|source| source.key() == owner_key)
    }

    /// Whether any checkout of `project`'s `repo_name` contains `path`.
    ///
    /// The single-repo question behind the path lookups. A caller asking about one
    /// *named* repo — "is this project's focus checked out here?" — must not go
    /// through [`repo_for_path`](Self::repo_for_path), whose job is to choose one
    /// repo among many: it cannot answer about a borrow at all, and where a path
    /// matches two sibling repos of one project its choice between them decides
    /// by nothing meaningful. Asking about the repo you actually mean has neither
    /// problem.
    pub fn repo_checkout_contains(
        &self,
        project: &ProjectData,
        repo_name: &str,
        path: &Path,
    ) -> bool {
        let query = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.repo_match_depth(project, repo_name, &query, &mut WorktreeMemo::default())
            .is_some()
    }

    /// The project that declares `query` as its own, borrows excluded — the
    /// structural answer described on [`repo_for_path`](Self::repo_for_path).
    fn owning_repo_for_path(
        &self,
        query: &Path,
        memo: &mut WorktreeMemo,
    ) -> Option<(&ProjectData, String)> {
        let mut best: Option<(&ProjectData, String, usize)> = None;
        for project in self.projects.values() {
            for repo_name in project.repos.keys() {
                if project.is_borrowed(repo_name) {
                    continue;
                }
                let Some(depth) = self.repo_match_depth(project, repo_name, query, memo) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(_, _, d)| depth > *d) {
                    best = Some((project, repo_name.clone(), depth));
                }
            }
        }
        best.map(|(p, name, _)| (p, name))
    }

    /// How deeply `query` sits inside any checkout of `repo_name`: its declared
    /// path, its primary path, and every live worktree either of those places.
    /// The depth is the matched checkout's component count, so a caller comparing
    /// candidates picks the most specific one (a submodule over its
    /// superproject).
    fn repo_match_depth(
        &self,
        project: &ProjectData,
        repo_name: &str,
        query: &Path,
        memo: &mut WorktreeMemo,
    ) -> Option<usize> {
        let repo_path = project.repo_abs_path(repo_name).ok()?;
        // Attributing worktrees costs a strategy resolution per worktree, and it
        // can only change the answer where a sibling repo of this project resolves
        // to the same git dir. Deciding that from the registry first keeps the
        // ordinary one-repo-per-checkout case from paying for it at all.
        let contested = project.repos.keys().any(|other| {
            other != repo_name && project.repo_abs_path(other).is_ok_and(|p| p == repo_path)
        });
        let mut roots = vec![repo_path];
        if let Ok(primary) = super::resolve::repo_primary_path(self, project, repo_name) {
            if !roots.contains(&primary) {
                roots.push(primary);
            }
        }
        let mut best: Option<usize> = None;
        let mut deepen = |depth: usize| {
            if best.is_none_or(|seen| depth > seen) {
                best = Some(depth);
            }
        };
        for root in roots {
            // The declared and primary paths are this repo by definition.
            if let Some(depth) = containment_depth(query, &root) {
                deepen(depth);
            }
            for wt in memo.live_worktrees(&root) {
                // Containment is tested before attribution because attribution
                // costs a strategy resolution — several git invocations — while
                // most worktrees of a repository have nothing to do with the path
                // being resolved. Asking who placed a worktree that cannot match
                // is how a single path query comes to run dozens of them.
                let Some(depth) = containment_depth(query, &wt.path) else {
                    continue;
                };
                if contested && !self.repo_places_worktree(project, repo_name, wt) {
                    continue;
                }
                deepen(depth);
            }
        }
        best
    }

    /// Whether `repo_name`'s own branch strategy is what put `wt` where it is.
    ///
    /// Two repos may share one git repository and still be different checkouts of
    /// it: a bare-backed component and a review checkout of the same component
    /// each declare their own `worktree_dir`. `git worktree list` answers for the
    /// *repository*, so it reports both to both, and attributing every worktree
    /// to every repo that points at that git dir makes the two
    /// indistinguishable — a tie broken by map order, which is how a path query
    /// comes to answer with the wrong sibling.
    ///
    /// Re-resolving the strategy for the worktree's own branch says which repo
    /// would have placed it there, which is the discriminator the paths
    /// themselves cannot supply. A repo that declares no `worktree_dir` has no
    /// sibling to be confused with — its checkout *is* the repository — so it
    /// accepts what git reported and pays no resolution.
    fn repo_places_worktree(
        &self,
        project: &ProjectData,
        repo_name: &str,
        wt: &crate::git::Worktree,
    ) -> bool {
        let Some(repo) = project.repos.get(repo_name) else {
            return false;
        };
        if repo.worktree_dir.is_none() {
            return true;
        }
        // A detached worktree names no branch; the repo's own main branch stands
        // in, which is enough for a strategy whose path does not vary by branch.
        let Some(branch) = wt.branch.clone().or_else(|| repo.main_branch.clone()) else {
            return false;
        };
        let Ok(placed) = super::resolve::work_dir(self, project, repo_name, &branch) else {
            return false;
        };
        let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        real(&placed) == real(&wt.path)
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

/// Where `project` keeps its config tree ("Config-root resolution"):
/// `$WITS_PROJECT_CONFIG`, then `$XDG_CONFIG_HOME/wits/project`, then
/// `$HOME/.wits/project`.
const CONFIG_ROOT: crate::config::Root<'static> = crate::config::Root {
    env: "WITS_PROJECT_CONFIG",
    xdg: "wits/project",
    home: ".wits/project",
};

/// Classify a CLI positional as a filesystem path rather than a name: `.`/`..`
/// or a leading `.`, `/`, or `~` ("CLI contract"). Everything else is a name.
pub fn looks_like_path(token: &str) -> bool {
    token == "."
        || token == ".."
        || token.starts_with('.')
        || token.starts_with('/')
        || token.starts_with('~')
}

/// Render a `repo.path` template against the shared Profile-free path context
/// (`project.name`, `project.org`, `system.*`, `env.*`; no `repos.*`, which would
/// be circular). Built by [`super::context::path_context`] so this exact same
/// namespace backs `repo_abs_path` here and any other path resolve — they can't
/// drift apart.
fn render_path_template(
    tpl: &str,
    project_name: &str,
    project_org: Option<&str>,
) -> Result<String> {
    let root = super::context::path_context(project_name, project_org);
    Ok(Ctx::new(root).render_path(tpl)?)
}

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
        let borrowed = &consumer.repos["engine"];

        assert_eq!(
            consumer.repo_abs_path("engine").unwrap(),
            PathBuf::from("/src/acme/engine")
        );
        assert_eq!(borrowed.main_branch.as_deref(), Some("stable"));
        assert_eq!(borrowed.branch_strategy.as_deref(), Some("hybrid"));
        assert_eq!(
            borrowed.bootstrap_worktree_dir.as_deref(),
            Some("{{repo.path}}.primary")
        );
        assert_eq!(
            borrowed.remotes["origin"].url,
            "https://example.invalid/engine.git"
        );
        // The component's own `skip` travels; the consumer's stays the consumer's.
        assert_eq!(borrowed.skip, vec!["/bigdata"]);
        assert_eq!(consumer.repos["main"].skip, vec!["/third_party/engine"]);
        // The borrower keeps what is about *its* build.
        assert_eq!(borrowed.anchor.as_deref(), Some("main"));
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
            editor.repos["engine"].main_branch.as_deref(),
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
        assert_eq!(repo, "main");
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
            (project.name.clone(), repo)
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
