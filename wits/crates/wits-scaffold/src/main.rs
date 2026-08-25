//! `wits-scaffold` — generate the boilerplate a new extension needs across a
//! target tree.
//!
//! Supporting one new Vulkan or SPIR-V extension means repeating facts from its
//! specification across a target tree. The work is mechanical, which makes it a
//! better fit for generation than hand editing.
//!
//! ## The shape
//!
//! ```text
//!   spec source  ──extract──>  descriptor  ──generate──>  patch or files
//!  (grammar/xml/adoc)         (TOML, editable)
//! ```
//!
//! The descriptor in the middle is the point. Reading a hand-written asciidoc is
//! a heuristic that can be wrong, so its result is a document to review and fix,
//! and generation from a descriptor never looks at a spec again.
//!
//! ## What is configuration and what is code
//!
//! Ingest is code: the sources are Khronos-defined, closed, and few. Output is
//! configuration: one TOML catalogue per target tree, because the set of trees is
//! open and each one's conventions drift with its own refactors. Following a
//! rename in someone else's tree should be a pattern edit, not a tool change.
//!
//! ## No idempotence
//!
//! Running twice inserts twice. The safety net is that `--patch` is the default:
//! the review step is a diff you read, and `git apply` refuses one whose context
//! has moved. `--write` is there when you would rather edit and review in place.

mod anchor;
mod catalog;
mod emit;
mod ingest;
mod kinds;
mod model;
mod overlay;
mod plan;
mod render;
mod root;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use catalog::Catalog;
use ingest::Sources;
use kinds::KindTable;
use model::Extension;

#[derive(Debug, Parser)]
#[command(
    name = "wits-scaffold",
    version,
    about = "Scaffold the boilerplate a new Vulkan/SPIR-V extension needs across target trees.",
    arg_required_else_help = true
)]
struct Cli {
    /// Show what is happening as it happens.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Never write: force patch output even under `generate --write`.
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    /// Configuration tree (default: $WITS_SCAFFOLD_CONFIG, else
    /// $XDG_CONFIG_HOME/wits/scaffold).
    #[arg(long, global = true, value_name = "DIR")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read specifications into a descriptor you can review and correct.
    Extract(ExtractArgs),
    /// Apply a descriptor to the target trees.
    Generate(GenerateArgs),
    /// Check that every catalogue anchor still resolves against its tree.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// Extension names. A `SPV_` name fills the SPIR-V plane and a `VK_` name
    /// the Vulkan one, so pass one, the other, or both — the two are delivered
    /// independently and neither is derived from the other.
    #[arg(value_name = "NAME", required = true)]
    names: Vec<String>,

    /// Write the descriptor here instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Read this one specification file, and do not consult the grammar.
    ///
    /// Two cases need it: a draft that is not filed under `source.spv_spec` yet,
    /// and a local revision of an extension the grammar already covers — which
    /// would otherwise win, being the exact source.
    #[arg(long = "spec-file", value_name = "FILE")]
    spec_file: Option<PathBuf>,

    #[command(flatten)]
    sources: SourceArgs,
}

/// One flag per source, named exactly as `env.toml` and the reading module name
/// it, so a source has one name everywhere rather than three.
#[derive(Debug, Args)]
struct SourceArgs {
    /// Override `source.spv_grammar`.
    #[arg(long = "spv-grammar", value_name = "FILE")]
    spv_grammar: Option<PathBuf>,
    /// Override `source.spv_spec`.
    #[arg(long = "spv-spec", value_name = "DIR")]
    spv_spec: Option<PathBuf>,
    /// Override `source.vk_registry`.
    #[arg(long = "vk-registry", value_name = "FILE")]
    vk_registry: Option<PathBuf>,
}

impl SourceArgs {
    fn over(&self, mut base: Sources) -> Sources {
        if self.spv_grammar.is_some() {
            base.spv_grammar = self.spv_grammar.clone();
        }
        if self.spv_spec.is_some() {
            base.spv_spec = self.spv_spec.clone();
        }
        if self.vk_registry.is_some() {
            base.vk_registry = self.vk_registry.clone();
        }
        base
    }
}

#[derive(Debug, Args)]
struct GenerateArgs {
    /// The descriptor, or `-` for stdin.
    #[arg(short, long, value_name = "FILE")]
    descriptor: PathBuf,

    /// Only these targets (default: every catalogue whose plane the descriptor
    /// has).
    #[arg(short, long = "target", value_name = "NAME")]
    targets: Vec<String>,

    /// Edit the trees instead of printing a patch.
    #[arg(long)]
    write: bool,

    #[command(flatten)]
    bindings: BindingArgs,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// A representative descriptor used to render the anchors.
    #[arg(short, long, value_name = "FILE")]
    descriptor: PathBuf,

    #[arg(short, long = "target", value_name = "NAME")]
    targets: Vec<String>,

    #[command(flatten)]
    bindings: BindingArgs,
}

/// How a run feeds values to a catalogue's templates.
#[derive(Debug, Args)]
struct BindingArgs {
    /// Set a catalogue variable for this run, e.g. `--var default=OFF`.
    ///
    /// Optional for a variable `[target.vars]` already defaults; required for one
    /// it deliberately does not, since a template may not read a variable that
    /// has no value anywhere.
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,

    /// Target-specific metadata for this run.
    #[arg(long, value_name = "FILE")]
    overlay: Option<PathBuf>,
}

impl BindingArgs {
    fn options(&self) -> Result<plan::Options> {
        Ok(plan::Options {
            vars: parse_bindings("--var", &self.vars)?,
            overlay: self
                .overlay
                .as_deref()
                .map(overlay::Overlay::load)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    wits_util::log::init(cli.verbose, cli.dry_run);

    let config = match &cli.config {
        Some(path) => path.clone(),
        None => wits_util::config::resolve_root(&catalog::CONFIG_ROOT)
            .context("cannot find the scaffold config tree")?,
    };

    match &cli.command {
        Command::Extract(args) => extract(args, &config),
        Command::Generate(args) => generate(args, &config, cli.dry_run),
        Command::Doctor(args) => doctor(args, &config),
    }
}

/// Read every named plane into one descriptor.
fn extract(args: &ExtractArgs, config: &std::path::Path) -> Result<()> {
    let sources = args.sources.over(Sources::load(config)?);
    let table = KindTable::load(config)?;
    let mut ext = Extension::default();

    for name in &args.names {
        if name.starts_with("SPV_") {
            let (plane, notes) = read_spv(name, &sources, &table, args.spec_file.as_deref())?;
            ext.spv = Some(plane);
            ext.notes.extend(notes);
        } else if name.starts_with("VK_") {
            let path = sources
                .vk_registry
                .as_ref()
                .context("no Vulkan registry configured (env.toml: source.vk_registry)")?;
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            let (plane, notes) = ingest::vk_registry::extract(&text, name)?;
            ext.vk = Some(plane);
            ext.notes.extend(notes);
        } else {
            bail!("'{name}' is neither a SPV_ nor a VK_ extension name");
        }
    }

    let document = model::to_toml(&ext)?;
    match &args.output {
        Some(path) => {
            std::fs::write(path, &document)
                .with_context(|| format!("cannot write {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => std::io::stdout().write_all(document.as_bytes())?,
    }
    for note in &ext.notes {
        eprintln!("note: {note}");
    }
    Ok(())
}

/// The SPIR-V plane, preferring the exact source.
///
/// A grammar miss falls through to the prose specification. Which source
/// answered is reported because one is exact and the other is heuristic.
fn read_spv(
    name: &str,
    sources: &Sources,
    table: &KindTable,
    spec_override: Option<&std::path::Path>,
) -> Result<(model::SpvPlane, Vec<String>)> {
    if spec_override.is_none() {
        if let Some(path) = &sources.spv_grammar {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            match ingest::spv_grammar::extract(&text, name, table) {
                Ok(found) => return Ok(found),
                Err(error) => eprintln!("note: not in the grammar ({error}); reading the spec"),
            }
        }
    }

    let path = match spec_override {
        Some(path) => path.to_path_buf(),
        None => sources.find_spec(name)?,
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    ingest::spv_spec::extract(&text, Some(name), table)
        .with_context(|| format!("in {}", path.display()))
}

fn generate(args: &GenerateArgs, config: &std::path::Path, dry_run: bool) -> Result<()> {
    let ext = read_descriptor(&args.descriptor)?;
    let opts = args.bindings.options()?;
    let output = if args.write && !dry_run {
        emit::Output::Write
    } else {
        emit::Output::Patch
    };

    let mut ran = 0usize;
    for (path, catalog) in select(config, &args.targets, &opts.overlay)? {
        let named = !args.targets.is_empty();
        let Some(root) = tree_root(&catalog, &opts, named)? else {
            continue;
        };
        if !usable(&ext, &catalog, named)? {
            continue;
        }

        let edits = plan::build(&catalog, &ext, &opts)
            .with_context(|| format!("catalogue {}", path.display()))?;
        let report = emit::apply(&root, &edits, output)
            .with_context(|| format!("target '{}'", catalog.target.name))?;

        report_target(&catalog.target.name, &root, &report, output);
        print!("{}", report.patch);
        ran += 1;
    }

    if ran == 0 {
        bail!("no target matched; nothing was generated");
    }
    Ok(())
}

/// Whether the descriptor carries what a target consumes.
///
/// Selecting every catalogue is the default, so a target whose plane is absent is
/// expected there rather than a mistake; naming that target explicitly asked for
/// something impossible, and says so.
fn usable(ext: &Extension, catalog: &Catalog, named: bool) -> Result<bool> {
    if ext.has_plane(catalog.target.plane) {
        return Ok(true);
    }
    if named {
        bail!(
            "target '{}' needs the {} plane, which this descriptor does not have",
            catalog.target.name,
            catalog.target.plane
        );
    }
    eprintln!(
        "skip {}: the descriptor has no {} plane",
        catalog.target.name, catalog.target.plane
    );
    Ok(false)
}

/// A target's checked-out tree, or `None` when a sweep should pass it over.
///
/// The same asymmetry as [`usable`], for the same reason: one machine rarely
/// holds every tree the catalogues describe, so sweeping past an absent one is
/// normal, while naming it is a request that cannot be met.
fn tree_root(catalog: &Catalog, opts: &plan::Options, named: bool) -> Result<Option<PathBuf>> {
    let env = render::environment();
    let ctx = plan::target_context(catalog, opts, &env)?;
    let root = root::resolve(&catalog.target.root, &ctx, &env)
        .with_context(|| format!("target '{}'", catalog.target.name))?;
    if root.is_dir() {
        return Ok(Some(root));
    }
    if named {
        bail!(
            "target '{}': no tree at {}",
            catalog.target.name,
            root.display()
        );
    }
    eprintln!(
        "skip {}: no tree at {}",
        catalog.target.name,
        root.display()
    );
    Ok(None)
}

/// Resolve every anchor of every selected catalogue, reporting all failures
/// rather than stopping at the first.
///
/// `generate` already fails on a broken anchor, but it fails *once*. After
/// somebody refactors a tree, the useful question is which rules broke, all of
/// them, in one run — that is what makes a catalogue of patterns maintainable.
fn doctor(args: &DoctorArgs, config: &std::path::Path) -> Result<()> {
    let ext = read_descriptor(&args.descriptor)?;
    let opts = args.bindings.options()?;
    let mut broken = 0usize;

    for (_, catalog) in select(config, &args.targets, &opts.overlay)? {
        let named = !args.targets.is_empty();
        let Some(root) = tree_root(&catalog, &opts, named)? else {
            continue;
        };
        if !usable(&ext, &catalog, named)? {
            continue;
        }
        let edits = plan::build(&catalog, &ext, &opts)?;
        println!("{} ({})", catalog.target.name, root.display());

        for edit in &edits {
            // A `create` rule has no anchor to check, but it does have a
            // precondition worth the same report line: the file it authors must
            // not exist yet, which is the first thing a re-run trips over.
            let plan::Action::Insert { anchor, sort_line } = &edit.action else {
                if root.join(&edit.path).exists() {
                    broken += 1;
                    println!("  {:<6} {}: {}", "BROKEN", edit.path, edit.what);
                    println!("         already exists; a create rule will not overwrite it");
                } else {
                    println!("  {:<6} {}: {} (to create)", "ok", edit.path, edit.what);
                }
                continue;
            };
            let outcome = std::fs::read_to_string(root.join(&edit.path))
                .with_context(|| format!("cannot read {}", edit.path))
                .and_then(|text| anchor.locate(&text, sort_line.as_deref()));
            match outcome {
                Ok(placed) => {
                    println!("  {:<6} {}: {}", "ok", edit.path, edit.what);
                    for note in placed.notes {
                        println!("         note: {note}");
                    }
                }
                Err(error) => {
                    broken += 1;
                    println!("  {:<6} {}: {}", "BROKEN", edit.path, edit.what);
                    println!("         {error:#}");
                }
            }
        }
    }

    if broken > 0 {
        bail!("{broken} site(s) no longer fit their tree");
    }
    Ok(())
}

fn read_descriptor(path: &std::path::Path) -> Result<Extension> {
    let text = if path == std::path::Path::new("-") {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("cannot read descriptor {}", path.display()))?
    };
    model::from_toml(&text).context("descriptor is not a valid document")
}

/// The catalogues to act on, in discovery order.
fn select(
    config: &std::path::Path,
    wanted: &[String],
    overlay: &overlay::Overlay,
) -> Result<Vec<(PathBuf, Catalog)>> {
    let all = catalog::load_all(config)?;
    overlay.validate_targets(all.iter().map(|(_, catalog)| catalog.target.name.as_str()))?;
    if wanted.is_empty() {
        return Ok(all);
    }
    let known: Vec<String> = all
        .iter()
        .map(|(_, catalog)| catalog.target.name.clone())
        .collect();
    for name in wanted {
        if !known.contains(name) {
            bail!("no target '{name}'; known targets: {}", known.join(", "));
        }
    }
    Ok(all
        .into_iter()
        .filter(|(_, catalog)| wanted.contains(&catalog.target.name))
        .collect())
}

fn parse_bindings(flag: &str, raw: &[String]) -> Result<BTreeMap<String, String>> {
    raw.iter()
        .map(|entry| {
            entry
                .split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .with_context(|| format!("{flag} expects K=V, got '{entry}'"))
        })
        .collect()
}

fn report_target(name: &str, root: &std::path::Path, report: &emit::Report, output: emit::Output) {
    let verb = match output {
        emit::Output::Patch => "would change",
        emit::Output::Write => "changed",
    };
    eprintln!("{name} ({}): {verb}", root.display());
    for path in &report.created {
        eprintln!("  create  {path}");
    }
    for entry in &report.edited {
        eprintln!("  edit    {entry}");
    }
    for (site, note) in &report.notes {
        eprintln!("  note    {site}: {note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogues(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let targets = dir.path().join("target");
        std::fs::create_dir_all(&targets).unwrap();
        for name in names {
            std::fs::write(
                targets.join(format!("{name}.toml")),
                format!("[target]\nname=\"{name}\"\nroot=\"/fixtures/{name}\"\nplane=\"spv\"\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn bindings_parse_as_key_value() {
        let parsed = parse_bindings("--var", &["default=OFF".to_owned()]).unwrap();
        assert_eq!(parsed.get("default").unwrap(), "OFF");
    }

    #[test]
    fn a_value_may_contain_equals_signs() {
        let parsed = parse_bindings("--var", &["expr=a=b".to_owned()]).unwrap();
        assert_eq!(parsed.get("expr").unwrap(), "a=b");
    }

    #[test]
    fn a_binding_without_a_value_is_rejected_and_names_its_flag() {
        let err = parse_bindings("--var", &["lonely".to_owned()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--var"), "got: {err}");
    }

    #[test]
    fn source_overrides_win_over_the_env_file() {
        let base = Sources {
            vk_registry: Some(PathBuf::from("/from/env.toml")),
            ..Default::default()
        };
        let args = SourceArgs {
            spv_grammar: None,
            spv_spec: None,
            vk_registry: Some(PathBuf::from("/from/cli")),
        };
        assert_eq!(
            args.over(base).vk_registry.unwrap(),
            PathBuf::from("/from/cli")
        );
    }

    #[test]
    fn an_absent_override_leaves_the_env_file_alone() {
        let base = Sources {
            vk_registry: Some(PathBuf::from("/from/env.toml")),
            ..Default::default()
        };
        let args = SourceArgs {
            spv_grammar: None,
            spv_spec: None,
            vk_registry: None,
        };
        assert_eq!(
            args.over(base).vk_registry.unwrap(),
            PathBuf::from("/from/env.toml")
        );
    }

    #[test]
    fn an_unknown_target_lists_the_known_ones() {
        let dir = catalogues(&["alpha"]);
        let err = select(
            dir.path(),
            &["nope".to_owned()],
            &overlay::Overlay::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("known targets: alpha"), "got: {err}");
    }

    #[test]
    fn no_selection_means_every_catalogue() {
        let dir = catalogues(&["alpha", "beta"]);
        assert_eq!(
            select(dir.path(), &[], &overlay::Overlay::default())
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_named_target_with_no_tree_is_an_error_but_a_sweep_passes_over_it() {
        let dir = catalogues(&["alpha"]);
        let (_, catalog) = select(dir.path(), &[], &overlay::Overlay::default())
            .unwrap()
            .pop()
            .unwrap();
        let opts = plan::Options::default();

        // One machine rarely holds every tree the catalogues describe.
        assert!(tree_root(&catalog, &opts, false).unwrap().is_none());
        let err = tree_root(&catalog, &opts, true).unwrap_err().to_string();
        assert!(err.contains("/fixtures/alpha"), "got: {err}");
    }

    #[test]
    fn a_root_reading_an_undefined_variable_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let targets = dir.path().join("target");
        std::fs::create_dir_all(&targets).unwrap();
        std::fs::write(
            targets.join("alpha.toml"),
            "[target]\nname=\"alpha\"\nroot=\"{{ var.tree }}\"\nplane=\"spv\"\n",
        )
        .unwrap();
        let (_, catalog) = select(dir.path(), &[], &overlay::Overlay::default())
            .unwrap()
            .pop()
            .unwrap();

        let err = format!(
            "{:#}",
            tree_root(&catalog, &plan::Options::default(), false).unwrap_err()
        );
        assert!(err.contains("--var tree="), "got: {err}");

        let opts = plan::Options {
            vars: BTreeMap::from([("tree".to_owned(), "/fixtures/elsewhere".to_owned())]),
            ..Default::default()
        };
        // Defined now, so resolution gets as far as looking for the tree.
        let err = tree_root(&catalog, &opts, true).unwrap_err().to_string();
        assert!(err.contains("/fixtures/elsewhere"), "got: {err}");
    }
}
