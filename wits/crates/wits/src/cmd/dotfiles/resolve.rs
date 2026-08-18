//! Turning the manifests into the exact set of Dotdrop inputs, and saying so
//! when they cannot be turned into anything sensible.
//!
//! Resolution is one pass with three independent selectors, and keeping them
//! independent is the whole design:
//!
//! - a **plane** partitions output — each one becomes its own catalog and its
//!   own entrypoints, because two planes cannot share a Dotdrop run;
//! - a **capability** filters install units within a plane and changes nothing
//!   else;
//! - an **overlay** multiplies each surviving unit by the content layers that
//!   actually exist on disk, and layers variables in the host's stated order.
//!
//! The output is a [`Plan`]: everything decided, nothing rendered. It is not
//! written to disk — the emitter is the only consumer — but it is a real value
//! rather than a pile of locals so that the "what gets deployed" decisions stay
//! separable from the "what does Dotdrop want to read" decisions, which is the
//! seam a second backend would use.
//!
//! ## Why the per-overlay aggregates look redundant
//!
//! Dotdrop merges `import_variables` **shallowly**: the last file to mention a
//! top-level key replaces that whole key. So a private overlay that sets
//! `testing.result_dir` cannot simply contribute that one leaf — it would erase
//! the sibling `testing.*` values the shared file defines. Each per-overlay
//! aggregate therefore carries the fully merged value of *every top-level key it
//! touches*, and nothing else. That is the minimum that survives a shallow
//! merge, and it keeps the encrypted files down to the keys that genuinely
//! involve a secret.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use toml::{Table, Value};

use super::layout::{match_fragment, relative, FragmentName};
use super::model::{
    leaves, merge_into, template_references, Composition, Fragment, Globals, Install, Manifest,
};
use super::tree::{is_contained, Repo};

/// Everything the emitter needs, with every selection already made.
pub struct Plan {
    /// Plaintext variables shared by every host: globals plus every module's.
    pub variables: Table,
    pub dynvariables: Table,
    pub actions: Table,
    /// Per overlay, the values that overlay changes. Absent for overlays nobody
    /// overrides.
    pub overlay_variables: BTreeMap<String, Aggregate>,
    /// One per plane × host, in emission order.
    pub entrypoints: Vec<Entrypoint>,
    /// Non-fatal observations: dead installs, unused fragments, stale output.
    pub notes: Vec<String>,
}

pub struct Dotfile {
    pub dst: String,
    pub src: String,
    pub link: Option<String>,
    pub chmod: Option<String>,
    pub actions: Vec<String>,
}

pub struct Profile {
    /// In deployment order: module, then install, then the host's overlay order
    /// — so a later overlay's content lands on top of an earlier one's.
    pub dotfiles: Vec<String>,
    pub variables: Table,
}

/// One Dotdrop config, self-contained: settings, the dotfiles this host
/// deploys, and its profile.
///
/// Self-contained rather than a thin file importing a shared per-plane catalog,
/// for two reasons that only show up once something is templated. A `dst` like
/// `{{@@ sysenv.amd_config_dir @@}}/…` is rendered by whichever config *declares*
/// it, using only that config's own variables — so a catalog living in an
/// imported file cannot see the host's variables, and a shared catalog could
/// only ever render against the shared ones. And a shared profile file would put
/// every host's variables in front of every other host, which is the wrong
/// default when a host variable can be a secret.
pub struct Entrypoint {
    pub plane: String,
    pub host: String,
    /// Where this config lands, root-relative, per the layout's template.
    pub path: PathBuf,
    pub config: Table,
    /// Only what this host deploys, keyed by Dotdrop id.
    pub dotfiles: BTreeMap<String, Dotfile>,
    pub profile: Profile,
}

/// One overlay's contribution, in the shape Dotdrop can actually consume.
///
/// Dotdrop applies two rules to an imported variables file that together decide
/// this shape. It merges the file **shallowly**, so a fragment that changes
/// `testing.result_dir` must republish the whole `testing` table or it erases
/// the siblings. And it resolves the file's templates with a templater built
/// from **that file alone**, so a republished value like
/// `runner_dir = "{{@@ testing_runner_dir @@}}"` drags its referent in too.
///
/// The alternative — copying every shared variable into every aggregate, which
/// is what a hand-maintained bundle ends up doing — is correct but expensive in
/// the one place it hurts: these files are encrypted, encrypted blobs do not
/// merge, and a wholesale copy means every edit to a shared variable rewrites
/// every private file. Carrying only what an overlay changes, plus what those
/// values need to resolve, keeps a shared edit out of them entirely. It is also
/// what lets a host stack two private overlays without the later one's copy of
/// the shared defaults silently reverting the earlier one's overrides.
pub struct Aggregate {
    pub variables: Table,
    pub dynvariables: Table,
}

/// Pull in every shared value the aggregate's own templates refer to, until
/// nothing new appears. A referent may itself be a template, hence the fixpoint
/// rather than a single pass.
fn close_over_references(aggregate: &mut Aggregate, shared: &Table, shared_dyn: &Table) {
    loop {
        let mut wanted = BTreeSet::new();
        for value in aggregate.variables.values() {
            template_references(value, &mut wanted);
        }
        for value in aggregate.dynvariables.values() {
            template_references(value, &mut wanted);
        }

        let mut grew = false;
        for name in wanted {
            if aggregate.variables.contains_key(&name) || aggregate.dynvariables.contains_key(&name)
            {
                continue;
            }
            if let Some(value) = shared.get(&name) {
                aggregate.variables.insert(name, value.clone());
                grew = true;
            } else if let Some(value) = shared_dyn.get(&name) {
                aggregate.dynvariables.insert(name, value.clone());
                grew = true;
            }
        }
        if !grew {
            return;
        }
    }
}

/// Read every manifest and produce the plan, or the list of reasons there
/// isn't one. Errors that make the model incoherent are collected rather than
/// returned one at a time — fixing manifests one error per run is miserable.
pub fn plan(repo: &Repo) -> Result<Plan> {
    let inputs = Inputs::load(repo)?;
    let mut problems = Vec::new();
    let mut notes = Vec::new();

    inputs.validate(repo, &mut problems, &mut notes)?;
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("{problem}");
        }
        bail!("{} manifest problem(s)", problems.len());
    }

    let (variables, dynvariables, actions) = inputs.shared();
    let overlay_variables = inputs.overlay_variables(&variables, &dynvariables);
    let entrypoints = inputs.entrypoints(repo, &overlay_variables);

    Ok(Plan {
        variables,
        dynvariables,
        actions,
        overlay_variables,
        entrypoints,
        notes,
    })
}

/// Validate without building anything, for `check`. Returns `(problems, notes)`.
pub fn inspect(repo: &Repo) -> Result<(Vec<String>, Vec<String>)> {
    let inputs = Inputs::load(repo)?;
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    inputs.validate(repo, &mut problems, &mut notes)?;
    Ok((problems, notes))
}

// --- loaded inputs ------------------------------------------------------------

/// Every file the generator reads, parsed. Loading is separated from resolving
/// so that a locked or malformed fragment fails before any selection has run —
/// a half-resolved plan built from a fragment that silently read as empty is
/// exactly the outcome the encryption boundary makes possible.
struct Inputs {
    composition: Composition,
    globals: Globals,
    /// Module name -> its manifest, for modules that have one. Sorted, because
    /// the map order is the layering order.
    manifests: BTreeMap<String, Manifest>,
    /// Module-owned per-overlay values, already in merge order: by module name,
    /// then by the overlay's plain fragment before its named parts.
    fragments: Vec<Loaded>,
    /// Per-overlay values with no module owner at all, in the same order.
    shared_fragments: Vec<Loaded>,
    /// `.toml` files in a fragment directory that name no overlay any host uses.
    unclaimed: Vec<PathBuf>,
    /// Fragment file names that could belong to more than one overlay.
    ambiguous: Vec<(PathBuf, Vec<String>)>,
    /// Every overlay any host names, in no particular order beyond sorted.
    overlay_universe: BTreeSet<String>,
}

/// One fragment file, with enough provenance to name it in a report.
struct Loaded {
    overlay: String,
    path: PathBuf,
    fragment: Fragment,
}

/// Reads one fragment directory, sorting its files into the overlay each names.
///
/// The two rejects are carried out rather than dropped. A file naming no known
/// overlay is dead weight that reads like configuration, and a file naming two
/// is a value about to be filed under the wrong encryption key — both are worth
/// more than the silence that probing-by-name used to give them.
#[derive(Default)]
struct Scan {
    unclaimed: Vec<PathBuf>,
    ambiguous: Vec<(PathBuf, Vec<String>)>,
}

impl Scan {
    fn collect(
        &mut self,
        repo: &Repo,
        dir: &Path,
        overlays: &BTreeSet<String>,
        out: &mut Vec<Loaded>,
    ) -> Result<()> {
        let mut here = Vec::new();
        for (stem, path) in repo.fragments_in(dir)? {
            let mut matched = match_fragment(&stem, overlays.iter().map(String::as_str));
            match matched.len() {
                0 => self.unclaimed.push(path),
                1 => {
                    let name = matched.remove(0);
                    here.push((
                        FragmentName {
                            overlay: name.overlay,
                            part: name.part,
                        },
                        path,
                    ));
                }
                _ => self.ambiguous.push((
                    path,
                    matched.into_iter().map(|m| m.overlay.to_owned()).collect(),
                )),
            }
        }

        // Merge order within one directory: by overlay, then the plain fragment
        // before its named parts. Sorting by file name would invert that, since
        // `personal.identity` sorts before `personal`.
        here.sort();
        for (name, path) in here {
            out.push(Loaded {
                overlay: name.overlay.to_owned(),
                fragment: repo.read::<Fragment>(&path)?,
                path,
            });
        }
        Ok(())
    }
}

impl Inputs {
    fn load(repo: &Repo) -> Result<Self> {
        let layout = repo.layout();
        let composition: Composition = repo.read(&layout.composition)?;

        let globals: Globals = if repo.exists(&layout.globals) {
            repo.read(&layout.globals)?
        } else {
            Globals::default()
        };

        let overlay_universe: BTreeSet<String> = composition
            .hosts
            .values()
            .flat_map(|host| host.overlays.iter().cloned())
            .collect();

        let mut scan = Scan::default();
        let mut manifests = BTreeMap::new();
        let mut fragments = Vec::new();
        for app in repo.modules()? {
            let path = layout.manifest_of(&app);
            if repo.exists(&path) {
                manifests.insert(app.clone(), repo.read::<Manifest>(&path)?);
            }
            scan.collect(
                repo,
                &layout.fragments_of(&app),
                &overlay_universe,
                &mut fragments,
            )?;
        }

        let mut shared_fragments = Vec::new();
        scan.collect(
            repo,
            &layout.fragments,
            &overlay_universe,
            &mut shared_fragments,
        )?;

        Ok(Self {
            composition,
            globals,
            manifests,
            fragments,
            shared_fragments,
            unclaimed: scan.unclaimed,
            ambiguous: scan.ambiguous,
            overlay_universe,
        })
    }

    /// Every install, tagged with its module, in layering order.
    fn installs(&self) -> impl Iterator<Item = (&str, &Install)> {
        self.manifests
            .iter()
            .flat_map(|(app, m)| m.installs.iter().map(move |i| (app.as_str(), i)))
    }

    /// The fragments that apply to one overlay, in layering order: every
    /// module's, then the ownerless ones. Ownerless last because it is the
    /// escape hatch for a value with no module owner, and an escape hatch that
    /// loses to the thing it is escaping is useless.
    fn fragments_for<'a>(&'a self, overlay: &'a str) -> impl Iterator<Item = &'a Loaded> {
        self.fragments
            .iter()
            .chain(self.shared_fragments.iter())
            .filter(move |loaded| loaded.overlay == overlay)
    }

    /// Layers 1 and 2 of the merge order: globals, then every module's manifest
    /// in name order.
    fn shared(&self) -> (Table, Table, Table) {
        let mut variables = self.globals.variables.clone();
        let mut dynvariables = self.globals.dynvariables.clone();
        let mut actions = self.globals.actions.clone();
        for manifest in self.manifests.values() {
            merge_into(&mut variables, &manifest.variables);
            merge_into(&mut dynvariables, &manifest.dynvariables);
            merge_into(&mut actions, &manifest.actions);
        }
        (variables, dynvariables, actions)
    }

    /// Layers 3 and 4, packaged so each aggregate survives Dotdrop's shallow
    /// import merge *and* resolves on its own — see [`Aggregate`].
    fn overlay_variables(&self, shared: &Table, shared_dyn: &Table) -> BTreeMap<String, Aggregate> {
        let mut out = BTreeMap::new();
        for overlay in &self.overlay_universe {
            let contributions: Vec<&Table> = self
                .fragments_for(overlay)
                .map(|loaded| &loaded.fragment.variables)
                .filter(|t| !t.is_empty())
                .collect();
            if contributions.is_empty() {
                continue;
            }

            let touched: BTreeSet<&String> = contributions.iter().flat_map(|t| t.keys()).collect();
            let mut variables = Table::new();
            for key in touched {
                let mut value = shared
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| Value::Table(Table::new()));
                for contribution in &contributions {
                    let Some(incoming) = contribution.get(key) else {
                        continue;
                    };
                    match (&mut value, incoming) {
                        (Value::Table(base), Value::Table(over)) => merge_into(base, over),
                        _ => value = incoming.clone(),
                    }
                }
                variables.insert(key.clone(), value);
            }

            let mut aggregate = Aggregate {
                variables,
                dynvariables: Table::new(),
            };
            close_over_references(&mut aggregate, shared, shared_dyn);
            out.insert(overlay.clone(), aggregate);
        }
        out
    }

    /// Which planes a host deploys: its own list, or every declared plane.
    fn host_planes(&self, host: &str) -> Vec<String> {
        let declared = || self.composition.planes.keys().cloned().collect::<Vec<_>>();
        self.composition
            .hosts
            .get(host)
            .and_then(|h| h.planes.clone())
            .unwrap_or_else(declared)
    }

    /// The dotfiles one host deploys in one plane, and the order it deploys
    /// them in: module, then install, then the host's overlay order, so a later
    /// overlay's content lands on top of an earlier one's.
    fn select(
        &self,
        repo: &Repo,
        plane: &str,
        host_name: &str,
    ) -> (BTreeMap<String, Dotfile>, Vec<String>) {
        let host = self
            .composition
            .hosts
            .get(host_name)
            .expect("selection only runs for a declared host");

        let mut entries = BTreeMap::new();
        let mut order = Vec::new();
        for (app, install) in self.installs() {
            if !install.planes.iter().any(|p| p == plane) {
                continue;
            }
            // An install with no stated capability is unconditional; otherwise
            // one of its capabilities must be one the host claims.
            let wanted = install.capabilities.is_empty()
                || install
                    .capabilities
                    .iter()
                    .any(|c| host.capabilities.contains(c));
            let satisfied = install
                .requires_overlays
                .iter()
                .all(|o| host.overlays.contains(o));
            if !wanted || !satisfied {
                continue;
            }
            for overlay in &host.overlays {
                let Some(target) = repo.overlay_target(app, overlay, &install.path) else {
                    continue;
                };
                let mut src = format!("{app}/{overlay}");
                if install.path != "." {
                    src.push('/');
                    src.push_str(install.path.trim_end_matches('/'));
                }
                if target.is_dir {
                    src.push('/');
                }
                let id = format!("{}-{overlay}", install.id);
                entries.insert(
                    id.clone(),
                    Dotfile {
                        dst: install.dst.clone(),
                        src,
                        link: install.link.clone(),
                        chmod: install.chmod.clone(),
                        actions: install.actions.clone(),
                    },
                );
                order.push(id);
            }
        }
        (entries, order)
    }

    /// Layer 5: host defaults, then the machine's own overrides. The three
    /// derived lists are exposed as variables because templates legitimately
    /// branch on them (`'desktop' in capabilities`), and deriving them here
    /// means they cannot drift from the selection that just happened.
    fn host_variables(&self, host_name: &str) -> Table {
        let host = &self.composition.hosts[host_name];
        let mut variables = self.composition.defaults.clone();
        merge_into(&mut variables, &host.variables);
        variables.insert("capabilities".into(), to_array(&host.capabilities));
        variables.insert("overlays".into(), to_array(&host.overlays));
        variables.insert("planes".into(), to_array(&self.host_planes(host_name)));
        variables
    }

    fn entrypoints(
        &self,
        repo: &Repo,
        overlay_variables: &BTreeMap<String, Aggregate>,
    ) -> Vec<Entrypoint> {
        let layout = repo.layout();
        let mut out = Vec::new();
        for (plane_name, plane) in &self.composition.planes {
            for (host_name, host) in &self.composition.hosts {
                if !self.host_planes(host_name).iter().any(|p| p == plane_name) {
                    continue;
                }

                let mut config = self.composition.config.clone();
                merge_into(&mut config, &plane.config);

                // Every path below is expressed relative to where this config
                // actually lands, which the layout decides — so moving the output
                // directory moves these with it. They are also rejected in
                // `[config]` at check time, since a hand-written value here could
                // only ever be wrong.
                let path = layout.entrypoint_of(plane_name, host_name);
                let here = path.parent().unwrap_or(Path::new("")).to_path_buf();
                config.insert(
                    "dotpath".into(),
                    Value::String(relative(&here, &layout.modules)),
                );

                // Host overlay order is import order, so a later overlay's
                // aggregate wins the shallow merge — the same precedence the
                // content layers deploy with.
                let mut imports = vec![Value::String(relative(&here, &layout.variables_file()))];
                imports.extend(
                    host.overlays
                        .iter()
                        .filter(|overlay| overlay_variables.contains_key(*overlay))
                        .map(|overlay| {
                            Value::String(relative(&here, &layout.overlay_variables_file(overlay)))
                        }),
                );
                config.insert("import_variables".into(), Value::Array(imports));
                config.insert(
                    "import_actions".into(),
                    Value::Array(vec![Value::String(relative(&here, &layout.actions_file()))]),
                );

                let (dotfiles, order) = self.select(repo, plane_name, host_name);
                out.push(Entrypoint {
                    plane: plane_name.clone(),
                    host: host_name.clone(),
                    path,
                    config,
                    dotfiles,
                    profile: Profile {
                        dotfiles: order,
                        variables: self.host_variables(host_name),
                    },
                });
            }
        }
        out
    }
}

fn to_array(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())
}

// --- validation ---------------------------------------------------------------

impl Inputs {
    /// Collect everything wrong with the manifests. `problems` block generation;
    /// `notes` describe things that are dead or suspicious but still coherent —
    /// a module you have not wired up yet must not stop you regenerating.
    fn validate(
        &self,
        repo: &Repo,
        problems: &mut Vec<String>,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        if self.composition.planes.is_empty() {
            problems.push("hosts.toml declares no [planes.*]".into());
        }
        if self.composition.hosts.is_empty() {
            problems.push("hosts.toml declares no [hosts.*]".into());
        }
        for key in Composition::RESERVED_CONFIG_KEYS {
            if self.composition.config.contains_key(key) {
                problems.push(format!(
                    "hosts.toml [config] sets '{key}', which the generator computes"
                ));
            }
        }
        for (name, plane) in &self.composition.planes {
            for key in Composition::RESERVED_CONFIG_KEYS {
                if plane.config.contains_key(key) {
                    problems.push(format!(
                        "hosts.toml [planes.{name}.config] sets '{key}', \
                         which the generator computes"
                    ));
                }
            }
        }
        for (name, host) in &self.composition.hosts {
            for plane in host.planes.iter().flatten() {
                if !self.composition.planes.contains_key(plane) {
                    problems.push(format!("host '{name}' selects undeclared plane '{plane}'"));
                }
            }
            if host.overlays.is_empty() {
                problems.push(format!("host '{name}' selects no overlays"));
            }
        }

        self.validate_names(problems);
        self.validate_installs(repo, problems, notes)?;
        self.validate_fragments(problems, notes);
        self.validate_variable_collisions(notes);
        Ok(())
    }

    /// Actions and dynvariables share one flat namespace across every module, so
    /// two modules claiming a name is not an override — it is a coin flip whose
    /// outcome depends on directory order. Variables are exempt because their
    /// nesting gives each module its own subtree by convention.
    fn validate_names(&self, problems: &mut Vec<String>) {
        let mut actions: HashMap<&str, &str> = self
            .globals
            .actions
            .keys()
            .map(|k| (k.as_str(), "globals.toml"))
            .collect();
        let mut dynvars: HashMap<&str, &str> = self
            .globals
            .dynvariables
            .keys()
            .map(|k| (k.as_str(), "globals.toml"))
            .collect();

        for (app, manifest) in &self.manifests {
            for name in manifest.actions.keys() {
                if let Some(owner) = actions.insert(name, app) {
                    problems.push(format!(
                        "action '{name}' is defined by both {owner} and {app}"
                    ));
                }
            }
            for name in manifest.dynvariables.keys() {
                if let Some(owner) = dynvars.insert(name, app) {
                    problems.push(format!(
                        "dynvariable '{name}' is defined by both {owner} and {app}"
                    ));
                }
            }
        }
    }

    fn validate_installs(
        &self,
        repo: &Repo,
        problems: &mut Vec<String>,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let known_actions: BTreeSet<&str> = self
            .globals
            .actions
            .keys()
            .map(String::as_str)
            .chain(
                self.manifests
                    .values()
                    .flat_map(|m| m.actions.keys().map(String::as_str)),
            )
            .collect();

        let mut ids: HashMap<&str, &str> = HashMap::new();
        for (app, install) in self.installs() {
            let at = format!("{app}/manifest.toml install '{}'", install.id);

            if let Some(owner) = ids.insert(&install.id, app) {
                problems.push(format!(
                    "install id '{}' is used by both {owner} and {app}",
                    install.id
                ));
            }
            if install.planes.is_empty() {
                problems.push(format!("{at} states no planes"));
            }
            for plane in &install.planes {
                if !self.composition.planes.contains_key(plane) {
                    problems.push(format!("{at} targets undeclared plane '{plane}'"));
                }
            }
            if !is_contained(&install.path) {
                problems.push(format!("{at} has a path that escapes its overlay"));
            }
            for action in &install.actions {
                if !known_actions.contains(action.as_str()) {
                    problems.push(format!("{at} references undefined action '{action}'"));
                }
            }
            for plane in &install.planes {
                let Some(spec) = self.composition.planes.get(plane) else {
                    continue;
                };
                if !spec.dst_prefixes.is_empty()
                    && !spec.dst_prefixes.iter().any(|p| install.dst.starts_with(p))
                {
                    problems.push(format!(
                        "{at} has dst '{}', outside plane '{plane}' prefixes {:?}",
                        install.dst, spec.dst_prefixes
                    ));
                }
            }
            for overlay in &install.requires_overlays {
                if !self.overlay_universe.contains(overlay) {
                    notes.push(format!(
                        "note: {at} requires overlay '{overlay}', which no host has"
                    ));
                }
            }

            let reachable = self
                .overlay_universe
                .iter()
                .any(|o| repo.overlay_target(app, o, &install.path).is_some());
            if !reachable {
                notes.push(format!(
                    "note: {at} has path '{}', which exists in no overlay",
                    install.path
                ));
            }
        }

        // Content with no manifest deploys nothing, silently. That is a normal
        // state for a module being written, so it is a note rather than an error.
        for app in repo.modules()? {
            if self.manifests.contains_key(&app) {
                continue;
            }
            let overlays = repo.overlays_of(&app).unwrap_or_default();
            if !overlays.is_empty() {
                notes.push(format!(
                    "note: module '{app}' has content ({}) but no manifest.toml, so it deploys nothing",
                    overlays.join(", ")
                ));
            }
        }
        Ok(())
    }

    fn validate_fragments(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        for (path, overlays) in &self.ambiguous {
            problems.push(format!(
                "{} names more than one overlay ({}) — rename it so only one can match",
                path.display(),
                overlays.join(", ")
            ));
        }
        for path in &self.unclaimed {
            notes.push(format!(
                "note: {} names no overlay any host selects",
                path.display()
            ));
        }

        // Splitting an overlay across files is what makes mixed encryption
        // possible, and it is also what makes one file able to quietly overwrite
        // another. Same class as two modules claiming a variable, one layer down.
        for overlay in &self.overlay_universe {
            let mut seen: HashMap<String, (&Path, Value)> = HashMap::new();
            for loaded in self.fragments_for(overlay) {
                let mut found = Vec::new();
                leaves(&loaded.fragment.variables, "", &mut found);
                for (path, value) in found {
                    if let Some((prior, prior_value)) = seen.get(&path) {
                        if *prior_value != value {
                            notes.push(format!(
                                "note: variable '{path}' is set by both {} and {}",
                                prior.display(),
                                loaded.path.display()
                            ));
                        }
                    }
                    seen.insert(path, (&loaded.path, value));
                }
            }
        }
    }

    /// Two modules writing the same variable path is last-writer-wins by
    /// directory order — legal under the merge rule, and almost never intended.
    fn validate_variable_collisions(&self, notes: &mut Vec<String>) {
        let mut seen: HashMap<String, (&str, Value)> = HashMap::new();
        let mut record = |owner: &'static str, table: &Table, notes: &mut Vec<String>| {
            let mut found = Vec::new();
            leaves(table, "", &mut found);
            for (path, value) in found {
                if let Some((prior, prior_value)) = seen.get(&path) {
                    if *prior_value != value {
                        notes.push(format!(
                            "note: variable '{path}' is set by both {prior} and {owner}"
                        ));
                    }
                }
                seen.insert(path, (owner, value));
            }
        };
        record("globals.toml", &self.globals.variables, notes);
        for (app, manifest) in &self.manifests {
            let owner: &'static str = Box::leak(app.clone().into_boxed_str());
            record(owner, &manifest.variables, notes);
        }
    }
}

/// Report Dotdrop configs under `modules/dotdrop/` that this plan does not
/// produce — the residue of a renamed host, a dropped plane, or the previous
/// format.
///
/// Scoped to the two config extensions on purpose. The generator does not own
/// that tree outright: `.gitattributes` and prose live there too, and a tool
/// that calls a hand-written file stale teaches you to ignore it.
pub fn stale(repo: &Repo, plan: &Plan) -> Result<Vec<String>> {
    let layout = repo.layout();
    let mut expected: BTreeSet<PathBuf> = plan.entrypoints.iter().map(|e| e.path.clone()).collect();
    expected.insert(layout.composition.clone());
    expected.insert(layout.globals.clone());
    expected.insert(layout.variables_file());
    expected.insert(layout.actions_file());
    for overlay in plan.overlay_variables.keys() {
        expected.insert(layout.overlay_variables_file(overlay));
    }

    // The output directory is not owned outright — a repository may keep prose
    // or `.gitattributes` beside its generated files, and a tool that calls a
    // hand-written file stale teaches you to ignore it. What the generator does
    // own is the extensions its own templates produce.
    let owned = layout.output_extensions();
    let mut out = Vec::new();
    let mut walk = vec![layout.output.clone()];
    while let Some(dir) = walk.pop() {
        let absolute = repo.abs(&dir);
        // A repository that has never been generated has no output tree, and
        // nothing about that is stale.
        if !absolute.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&absolute)
            .with_context(|| format!("reading {}", absolute.display()))?
        {
            let entry = entry?;
            let path = dir.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                walk.push(path);
                continue;
            }
            let generated = path
                .extension()
                .is_some_and(|e| owned.iter().any(|o| o.as_str() == e));
            if generated && !expected.contains(&path) {
                out.push(format!("note: stale generated file {}", path.display()));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a repository whose shape exercises every selector: two planes, two
    /// hosts with different overlays, a capability-gated install, and an install
    /// that only one overlay has content for.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(super::super::layout::CONFIG_NAME),
            "[layout]\ncomposition = 'modules/dotdrop/hosts.toml'\n\
             [output]\nvariables = 'bundle/variables.toml'\n\
             actions = 'bundle/actions.toml'\n\
             overlay_variables = 'bundle/{overlay}/variables.toml'\n",
        )
        .unwrap();
        let m = dir.path().join("modules");
        let dd = m.join("dotdrop");
        std::fs::create_dir_all(&dd).unwrap();
        std::fs::write(
            dd.join("hosts.toml"),
            r#"
[config]
backup = true

[planes.user]

[planes.system]
dst_prefixes = ['/etc/']
[planes.system.config]
workdir = '/var/lib/dotdrop'

[defaults]
shared_flag = false

[hosts.alpha]
capabilities = ['develop']
overlays = ['common', 'personal']
planes = ['user']

[hosts.beta]
capabilities = ['develop', 'desktop']
overlays = ['common', 'work']
[hosts.beta.variables]
shared_flag = true
"#,
        )
        .unwrap();
        std::fs::write(
            dd.join("globals.toml"),
            r#"
[variables]
runner_root = "{{@@ env['XDG_RUNTIME_DIR'] @@}}/run"

[variables.testing]
runner = '{{@@ runner_root @@}}/r'
result = 'b'

[variables.unrelated]
k = 1

[actions]
reload = 'true'
"#,
        )
        .unwrap();

        // git: content in every overlay, plus a private identity per overlay.
        for overlay in ["common", "personal", "work"] {
            let d = m.join("git").join(overlay);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("config"), "").unwrap();
        }
        std::fs::write(
            m.join("git").join("manifest.toml"),
            "[[install]]\nid = 'git'\ndst = '~/.config/git/'\npath = '.'\n\
             capabilities = ['develop']\nplanes = ['user']\n",
        )
        .unwrap();
        std::fs::create_dir_all(m.join("git").join("manifest")).unwrap();
        std::fs::write(
            m.join("git").join("manifest").join("personal.toml"),
            "[variables.testing]\nresult = 'private'\n",
        )
        .unwrap();

        // sshd: system plane, common only.
        let d = m.join("sshd").join("common");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("sshd_config"), "").unwrap();
        std::fs::write(
            m.join("sshd").join("manifest.toml"),
            "[[install]]\nid = 'sshd'\ndst = '/etc/ssh/sshd_config'\npath = 'sshd_config'\n\
             planes = ['system']\nactions = ['reload']\n",
        )
        .unwrap();

        // desktop-only install, content in common.
        let d = m.join("mpv").join("common");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mpv.conf"), "").unwrap();
        std::fs::write(
            m.join("mpv").join("manifest.toml"),
            "[[install]]\nid = 'mpv'\ndst = '~/.config/mpv/'\npath = '.'\n\
             capabilities = ['desktop']\nplanes = ['user']\n",
        )
        .unwrap();

        dir
    }

    fn planned(dir: &tempfile::TempDir) -> Plan {
        plan(&Repo::open(None, Some(dir.path())).unwrap()).unwrap()
    }

    fn entry<'a>(plan: &'a Plan, plane: &str, host: &str) -> &'a Entrypoint {
        plan.entrypoints
            .iter()
            .find(|e| e.plane == plane && e.host == host)
            .expect("entrypoint should exist")
    }

    #[test]
    fn a_host_only_gets_dotfiles_for_the_overlays_it_names() {
        let dir = fixture();
        let plan = planned(&dir);

        assert_eq!(
            entry(&plan, "user", "alpha").profile.dotfiles,
            vec!["git-common", "git-personal"],
            "alpha lacks the desktop capability, so mpv is out; it has no work overlay"
        );
        assert_eq!(
            entry(&plan, "user", "beta").profile.dotfiles,
            vec!["git-common", "git-work", "mpv-common"]
        );
    }

    /// An entrypoint carries exactly what its host deploys. Nothing wider: an
    /// entry for an overlay the host cannot read would name a source it cannot
    /// decrypt, and a `dst` template it cannot resolve.
    #[test]
    fn an_entrypoint_carries_nothing_its_host_does_not_deploy() {
        let dir = fixture();
        let plan = planned(&dir);
        let alpha = entry(&plan, "user", "alpha");

        let ids: Vec<&str> = alpha.dotfiles.keys().map(String::as_str).collect();
        assert_eq!(ids, vec!["git-common", "git-personal"]);
        assert_eq!(alpha.dotfiles["git-common"].src, "git/common/");
    }

    #[test]
    fn a_file_install_keeps_its_name_and_a_dir_install_gets_a_slash() {
        let dir = fixture();
        let plan = planned(&dir);
        assert_eq!(
            entry(&plan, "system", "beta").dotfiles["sshd-common"].src,
            "sshd/common/sshd_config"
        );
    }

    #[test]
    fn a_host_that_names_no_planes_deploys_all_of_them() {
        let dir = fixture();
        let plan = planned(&dir);
        let pairs: Vec<(&str, &str)> = plan
            .entrypoints
            .iter()
            .map(|e| (e.plane.as_str(), e.host.as_str()))
            .collect();

        assert!(pairs.contains(&("system", "beta")));
        assert!(
            !pairs.contains(&("system", "alpha")),
            "alpha named only the user plane"
        );
    }

    /// The point of the per-overlay aggregate: the private fragment overrides
    /// one leaf, so the aggregate must republish the sibling leaves too, or
    /// Dotdrop's shallow merge would drop them.
    #[test]
    fn an_overlay_aggregate_republishes_the_keys_it_touches() {
        let dir = fixture();
        let plan = planned(&dir);
        let personal = &plan.overlay_variables["personal"].variables;

        assert_eq!(personal["testing"]["result"].as_str(), Some("private"));
        assert_eq!(
            personal["testing"]["runner"].as_str(),
            Some("{{@@ runner_root @@}}/r"),
            "the sibling leaf must survive the shallow import merge"
        );
        assert!(
            !plan.overlay_variables.contains_key("work"),
            "an overlay nobody overrides earns no aggregate"
        );
    }

    /// Dotdrop resolves each imported variables file with a templater built from
    /// that file alone, so a republished template must arrive with whatever it
    /// refers to — and with nothing else, or every shared edit would rewrite
    /// every encrypted file.
    #[test]
    fn an_aggregate_carries_its_referents_and_no_more() {
        let dir = fixture();
        let plan = planned(&dir);
        let personal = &plan.overlay_variables["personal"].variables;

        assert!(
            personal.contains_key("runner_root"),
            "testing.runner refers to it, so it has to travel along: {personal:?}"
        );
        assert!(
            !personal.contains_key("unrelated"),
            "nothing refers to it, so it stays in the shared file"
        );
    }

    #[test]
    fn entrypoints_import_only_the_aggregates_their_host_can_read() {
        let dir = fixture();
        let plan = planned(&dir);

        let beta = entry(&plan, "user", "beta");
        let imports = beta.config["import_variables"].as_array().unwrap();
        assert_eq!(
            imports.len(),
            1,
            "beta's overlays have no aggregate but the shared one"
        );
        assert_eq!(imports[0].as_str(), Some("bundle/variables.toml"));
        assert_eq!(beta.path, Path::new("modules/dotdrop/user.beta.toml"));

        let alpha = entry(&plan, "user", "alpha");
        let imports = alpha.config["import_variables"].as_array().unwrap();
        assert_eq!(
            imports.last().unwrap().as_str(),
            Some("bundle/personal/variables.toml"),
            "the private overlay imports last, so it wins the shallow merge"
        );
    }

    #[test]
    fn a_plane_layers_its_config_over_the_base() {
        let dir = fixture();
        let plan = planned(&dir);
        let system = entry(&plan, "system", "beta");

        assert_eq!(system.config["backup"].as_bool(), Some(true));
        assert_eq!(
            system.config["workdir"].as_str(),
            Some("/var/lib/dotdrop"),
            "the plane's own workdir is what makes it a distinct execution context"
        );
    }

    /// A host's variables belong to that host's config and nowhere else: they
    /// can be secrets, and a shared profile file would hand them to every other
    /// machine.
    #[test]
    fn host_variables_beat_defaults_and_stay_in_their_own_entrypoint() {
        let dir = fixture();
        let plan = planned(&dir);
        let alpha = &entry(&plan, "user", "alpha").profile.variables;
        let beta = &entry(&plan, "user", "beta").profile.variables;

        assert_eq!(alpha["shared_flag"].as_bool(), Some(false));
        assert_eq!(beta["shared_flag"].as_bool(), Some(true));
        assert_eq!(alpha["overlays"].as_array().unwrap().len(), 2);
        assert_eq!(
            alpha["planes"].as_array().unwrap()[0].as_str(),
            Some("user")
        );
    }

    #[test]
    fn a_dst_outside_its_planes_prefixes_is_a_problem() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("modules/sshd/manifest.toml"),
            "[[install]]\nid = 'sshd'\ndst = '~/sshd_config'\npath = 'sshd_config'\n\
             planes = ['system']\n",
        )
        .unwrap();
        let (problems, _) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("outside plane 'system'")),
            "got: {problems:?}"
        );
    }

    #[test]
    fn undeclared_planes_and_unknown_actions_are_problems() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("modules/mpv/manifest.toml"),
            "[[install]]\nid = 'mpv'\ndst = '~/.config/mpv/'\npath = '.'\n\
             planes = ['boot']\nactions = ['nope']\n",
        )
        .unwrap();
        let (problems, _) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(problems
            .iter()
            .any(|p| p.contains("undeclared plane 'boot'")));
        assert!(problems
            .iter()
            .any(|p| p.contains("undefined action 'nope'")));
    }

    #[test]
    fn two_modules_claiming_one_action_name_is_a_problem() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("modules/mpv/manifest.toml"),
            "[[install]]\nid = 'mpv'\ndst = '~/.config/mpv/'\npath = '.'\nplanes = ['user']\n\
             [actions]\nreload = 'false'\n",
        )
        .unwrap();
        let (problems, _) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("action 'reload'")),
            "got: {problems:?}"
        );
    }

    /// One overlay across several files is what lets values with different
    /// encryption treatment live in the same overlay, since `.gitattributes`
    /// marks whole files. They merge as one layer, plain fragment first.
    #[test]
    fn an_overlay_may_be_split_across_files() {
        let dir = fixture();
        let manifest = dir.path().join("modules/git/manifest");
        // `personal.toml` already sets testing.result; these layer on top.
        std::fs::write(
            manifest.join("personal.identity.toml"),
            "[variables.git.identity]\nemail = 'me@example.com'\n",
        )
        .unwrap();
        std::fs::write(
            manifest.join("personal.secret.toml"),
            "[variables.testing]\nresult = 'from the later part'\n",
        )
        .unwrap();

        let plan = planned(&dir);
        let personal = &plan.overlay_variables["personal"].variables;

        assert_eq!(
            personal["git"]["identity"]["email"].as_str(),
            Some("me@example.com")
        );
        assert_eq!(
            personal["testing"]["result"].as_str(),
            Some("from the later part"),
            "a named part layers over the overlay's plain fragment"
        );
        assert_eq!(
            personal["testing"]["runner"].as_str(),
            Some("{{@@ runner_root @@}}/r"),
            "and the shallow-merge republishing still holds across the split"
        );
    }

    /// Two files of one overlay writing the same value is last-writer-wins by
    /// file name — legal, and almost never what a split was for.
    #[test]
    fn two_parts_of_one_overlay_writing_the_same_value_is_noted() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("modules/git/manifest/personal.other.toml"),
            "[variables.testing]\nresult = 'a different value'\n",
        )
        .unwrap();

        let (problems, notes) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(
            notes.iter().any(|n| n.contains("testing.result")),
            "{notes:?}"
        );
    }

    #[test]
    fn a_fragment_naming_no_overlay_is_noted_and_one_naming_two_is_a_problem() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("modules/git/manifest/staging.toml"),
            "[variables.x]\ny = 1\n",
        )
        .unwrap();

        let (problems, notes) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(
            notes.iter().any(|n| n.contains("staging.toml")),
            "{notes:?}"
        );

        // `personal` and `personal.identity` as overlay names make
        // `personal.identity.toml` mean two different things.
        let hosts = dir.path().join("modules/dotdrop/hosts.toml");
        let text = std::fs::read_to_string(&hosts).unwrap();
        std::fs::write(
            &hosts,
            text.replace(
                "overlays = ['common', 'personal']",
                "overlays = ['common', 'personal', 'personal.identity']",
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("modules/git/manifest/personal.identity.toml"),
            "[variables.x]\ny = 1\n",
        )
        .unwrap();

        let (problems, _) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("more than one overlay")),
            "{problems:?}"
        );
    }

    #[test]
    fn content_without_a_manifest_is_noted_not_fatal() {
        let dir = fixture();
        let d = dir.path().join("modules/emacs-next/common");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("init.el"), "").unwrap();

        let (problems, notes) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(problems.is_empty(), "got: {problems:?}");
        assert!(notes.iter().any(|n| n.contains("emacs-next")));
    }

    #[test]
    fn a_reserved_config_key_is_rejected() {
        let dir = fixture();
        let hosts = dir.path().join("modules/dotdrop/hosts.toml");
        let text = std::fs::read_to_string(&hosts).unwrap();
        std::fs::write(&hosts, text.replace("backup = true", "dotpath = 'x'")).unwrap();

        let (problems, _) = inspect(&Repo::open(None, Some(dir.path())).unwrap()).unwrap();
        assert!(problems.iter().any(|p| p.contains("'dotpath'")));
    }
}
