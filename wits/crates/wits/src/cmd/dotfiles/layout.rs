//! Where everything is, declared rather than assumed.
//!
//! The *model* is fixed: a module owns a manifest, a per-overlay fragment
//! directory, and one content directory per overlay. The *paths* are not. Which
//! directory holds modules, what the manifest file is called, where generated
//! files land and what they are named — all of it is one repository's choice,
//! and baking it in would make this tool describe a single repository rather
//! than a shape of repository.
//!
//! That matters beyond taste. Every generated path is emitted **relative to the
//! file that references it**, and those relative paths are computed from the
//! layout, never spelled. Move the output directory one level and the `dotpath`
//! and imports follow; hard-code `..` and they quietly stop pointing anywhere.
//!
//! One thing has to be fixed or there is nothing to read: the name of this file.
//! `dotfiles.toml` at the repository root is the default, and `--config`,
//! `$WITS_DOTFILES_CONFIG`, or `wits.dotfiles.config` replaces it outright.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};
use serde::Deserialize;

/// The default name of the file that declares a repository's layout.
pub const CONFIG_NAME: &str = "dotfiles.toml";

/// `dotfiles.toml` — the whole of what this tool assumes about a repository.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub output: Output,
}

/// Where the source of truth lives.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// Directory holding one subdirectory per module.
    #[serde(default = "default_modules")]
    pub modules: PathBuf,
    /// A module's manifest, relative to the module directory.
    #[serde(default = "default_module_manifest")]
    pub module_manifest: PathBuf,
    /// A module's per-overlay fragments, relative to the module directory.
    /// Also the one name inside a module that is never an overlay.
    #[serde(default = "default_module_fragments")]
    pub module_fragments: PathBuf,
    /// The composition table: planes, hosts, and the backend's base settings.
    #[serde(default = "default_composition")]
    pub composition: PathBuf,
    /// Cross-module plaintext values. Optional; defaults to `globals.toml`
    /// beside the composition table.
    pub globals: Option<PathBuf>,
    /// Per-overlay values with no module owner. Optional; defaults to a
    /// directory named like `module_fragments`, beside the composition table.
    pub fragments: Option<PathBuf>,
}

/// Where generated files go, and what they are called.
///
/// The keys name *roles*, not the backend's vocabulary, because the roles are
/// what a different deployment tool would still need: one config per execution
/// context and machine, one shared value file, one file per overlay that
/// changes something.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Root of generated output. Optional; defaults to the directory holding
    /// the composition table.
    pub dir: Option<PathBuf>,
    /// One config per plane × host. `{plane}` and `{host}` are required; both
    /// may sit in directory positions, so `{plane}/{host}.toml` is a layout.
    #[serde(default = "default_entrypoint")]
    pub entrypoint: String,
    #[serde(default = "default_variables")]
    pub variables: String,
    #[serde(default = "default_actions")]
    pub actions: String,
    /// One per overlay that overrides something. `{overlay}` is required — it is
    /// what keeps each overlay's values under its own encryption key.
    #[serde(default = "default_overlay_variables")]
    pub overlay_variables: String,
}

fn default_modules() -> PathBuf {
    PathBuf::from("modules")
}
fn default_module_manifest() -> PathBuf {
    PathBuf::from("manifest.toml")
}
fn default_module_fragments() -> PathBuf {
    PathBuf::from("manifest")
}
fn default_composition() -> PathBuf {
    PathBuf::from("hosts.toml")
}
fn default_entrypoint() -> String {
    "{plane}.{host}.toml".into()
}
fn default_variables() -> String {
    "variables.toml".into()
}
fn default_actions() -> String {
    "actions.toml".into()
}
fn default_overlay_variables() -> String {
    "{overlay}/variables.toml".into()
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            modules: default_modules(),
            module_manifest: default_module_manifest(),
            module_fragments: default_module_fragments(),
            composition: default_composition(),
            globals: None,
            fragments: None,
        }
    }
}

impl Default for Output {
    fn default() -> Self {
        Self {
            dir: None,
            entrypoint: default_entrypoint(),
            variables: default_variables(),
            actions: default_actions(),
            overlay_variables: default_overlay_variables(),
        }
    }
}

/// A [`Config`] with every optional path resolved, validated once so that no
/// later stage has to ask whether a path is present or legal.
///
/// Every path here is **relative to the repository root**, which is what makes
/// [`relative`] able to answer "how does this file refer to that one?" without
/// touching the filesystem or caring where the repository is checked out.
#[derive(Debug)]
pub struct Resolved {
    pub modules: PathBuf,
    pub module_manifest: PathBuf,
    pub module_fragments: PathBuf,
    pub composition: PathBuf,
    pub globals: PathBuf,
    pub fragments: PathBuf,
    pub output: PathBuf,
    entrypoint: String,
    variables: String,
    actions: String,
    overlay_variables: String,
}

impl Resolved {
    pub fn new(config: Config) -> Result<Self> {
        let Config { layout, output } = config;

        for (what, path) in [
            ("layout.modules", &layout.modules),
            ("layout.composition", &layout.composition),
        ] {
            check_contained(what, path)?;
        }
        for (what, name) in [
            ("layout.module_manifest", &layout.module_manifest),
            ("layout.module_fragments", &layout.module_fragments),
        ] {
            check_contained(what, name)?;
        }

        let beside = layout
            .composition
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let globals = layout
            .globals
            .unwrap_or_else(|| beside.join("globals.toml"));
        let fragments = layout
            .fragments
            .unwrap_or_else(|| beside.join(&layout.module_fragments));
        let out_dir = output.dir.unwrap_or(beside);

        check_contained("layout.globals", &globals)?;
        check_contained("layout.fragments", &fragments)?;
        check_contained("output.dir", &out_dir)?;

        // A template missing its placeholder collapses distinct files onto one
        // path, and the loser is simply absent — so it is refused up front
        // rather than discovered as a host that mysteriously never deploys.
        require_placeholders(
            "output.entrypoint",
            &output.entrypoint,
            &["{plane}", "{host}"],
        )?;
        require_placeholders(
            "output.overlay_variables",
            &output.overlay_variables,
            &["{overlay}"],
        )?;
        for (what, template) in [
            ("output.entrypoint", &output.entrypoint),
            ("output.variables", &output.variables),
            ("output.actions", &output.actions),
            ("output.overlay_variables", &output.overlay_variables),
        ] {
            check_contained(what, Path::new(template))?;
        }

        Ok(Self {
            modules: layout.modules,
            module_manifest: layout.module_manifest,
            module_fragments: layout.module_fragments,
            composition: layout.composition,
            globals,
            fragments,
            output: out_dir,
            entrypoint: output.entrypoint,
            variables: output.variables,
            actions: output.actions,
            overlay_variables: output.overlay_variables,
        })
    }

    pub fn manifest_of(&self, app: &str) -> PathBuf {
        self.modules.join(app).join(&self.module_manifest)
    }

    /// A module's fragment directory. Its contents are *scanned* rather than
    /// probed by name — see [`match_fragment`].
    pub fn fragments_of(&self, app: &str) -> PathBuf {
        self.modules.join(app).join(&self.module_fragments)
    }

    pub fn entrypoint_of(&self, plane: &str, host: &str) -> PathBuf {
        self.output.join(
            self.entrypoint
                .replace("{plane}", plane)
                .replace("{host}", host),
        )
    }

    pub fn variables_file(&self) -> PathBuf {
        self.output.join(&self.variables)
    }

    pub fn actions_file(&self) -> PathBuf {
        self.output.join(&self.actions)
    }

    pub fn overlay_variables_file(&self, overlay: &str) -> PathBuf {
        self.output
            .join(self.overlay_variables.replace("{overlay}", overlay))
    }

    /// The file extensions this layout emits, for telling generated output
    /// apart from the hand-written files that may share the directory.
    pub fn output_extensions(&self) -> Vec<String> {
        let mut out: Vec<String> = [
            &self.entrypoint,
            &self.variables,
            &self.actions,
            &self.overlay_variables,
        ]
        .iter()
        .filter_map(|t| Path::new(t).extension())
        .map(|e| e.to_string_lossy().into_owned())
        .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Module directory names this layout reserves. A directory under `modules`
    /// that holds the composition table, the globals, the shared fragments, or
    /// the output is not a module — which is how a repository can keep those
    /// files inside the module tree without inventing a privileged module name.
    pub fn reserved_module_names(&self) -> Vec<String> {
        let mut names: Vec<String> = [
            &self.composition,
            &self.globals,
            &self.fragments,
            &self.output,
        ]
        .iter()
        .filter_map(|path| path.strip_prefix(&self.modules).ok())
        .filter_map(|rest| rest.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Which overlay a fragment file belongs to, and where it sits in that
/// overlay's merge order.
///
/// `<overlay>.toml` is an overlay's plain fragment; `<overlay>.<name>.toml` is
/// an additional one. One overlay gets several files because **encryption is
/// decided per path**: values that belong to the same overlay but not to the
/// same secret cannot share a file, since `.gitattributes` can only mark the
/// whole of one. Splitting them is the only way to say so.
///
/// Flat rather than a directory per overlay, deliberately. The split is driven
/// by encryption, and encryption rules are written as path patterns — a suffix
/// keeps every fragment of a module in one place and one glob (`manifest/*.toml`)
/// away, where a directory would put the plain case behind an invented filename.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FragmentName<'a> {
    pub overlay: &'a str,
    /// `None` for `<overlay>.toml`, which merges before any named part. The
    /// plain fragment is the overlay's baseline; the named ones layer on it.
    pub part: Option<String>,
}

/// Every overlay a fragment file name could belong to.
///
/// More than one is only possible when overlay names nest (`work` and
/// `work.eu`, given `work.eu.toml`), which the caller reports rather than
/// resolving: guessing which was meant would silently file a secret under the
/// wrong key.
pub fn match_fragment<'a>(
    stem: &str,
    overlays: impl IntoIterator<Item = &'a str>,
) -> Vec<FragmentName<'a>> {
    overlays
        .into_iter()
        .filter_map(|overlay| {
            if stem == overlay {
                Some(FragmentName {
                    overlay,
                    part: None,
                })
            } else {
                stem.strip_prefix(overlay)
                    .and_then(|rest| rest.strip_prefix('.'))
                    .filter(|part| !part.is_empty())
                    .map(|part| FragmentName {
                        overlay,
                        part: Some(part.to_owned()),
                    })
            }
        })
        .collect()
}

/// How a file in `from_dir` refers to `target`, both being root-relative.
///
/// Always computed, never spelled. The generated configs are full of paths that
/// one file uses to reach another, and every one of them changes when the layout
/// does; a literal `..` is correct exactly until someone moves a directory, and
/// then it is wrong in a way that reads like a missing variable.
pub fn relative(from_dir: &Path, target: &Path) -> String {
    let from: Vec<Component<'_>> = from_dir.components().collect();
    let to: Vec<Component<'_>> = target.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();

    let mut parts: Vec<String> = vec!["..".to_owned(); from.len() - shared];
    parts.extend(
        to[shared..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

fn check_contained(what: &str, path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!(
            "{what} must be relative to the repository root, got '{}'",
            path.display()
        );
    }
    if path.components().any(|c| c == Component::ParentDir) {
        bail!("{what} escapes the repository root: '{}'", path.display());
    }
    Ok(())
}

fn require_placeholders(what: &str, template: &str, needed: &[&str]) -> Result<()> {
    for placeholder in needed {
        if !template.contains(placeholder) {
            bail!("{what} = '{template}' must contain {placeholder}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(toml: &str) -> Resolved {
        Resolved::new(toml::from_str(toml).unwrap()).unwrap()
    }

    /// The defaults describe a repository that keeps everything beside its
    /// composition table, so declaring that one path is a complete config.
    #[test]
    fn one_declared_path_implies_the_rest() {
        let r = resolved("[layout]\ncomposition = 'modules/dotdrop/hosts.toml'\n");

        assert_eq!(r.globals, Path::new("modules/dotdrop/globals.toml"));
        assert_eq!(r.fragments, Path::new("modules/dotdrop/manifest"));
        assert_eq!(r.output, Path::new("modules/dotdrop"));
        assert_eq!(r.manifest_of("git"), Path::new("modules/git/manifest.toml"));
        assert_eq!(r.fragments_of("git"), Path::new("modules/git/manifest"));
        assert_eq!(
            r.entrypoint_of("user", "strix"),
            Path::new("modules/dotdrop/user.strix.toml")
        );
    }

    #[test]
    fn a_relocated_layout_moves_every_derived_path() {
        let r = resolved(
            "[layout]\nmodules = 'apps'\nmodule_manifest = 'app.toml'\n\
             module_fragments = 'private'\ncomposition = 'etc/machines.toml'\n\
             [output]\ndir = 'build/dd'\nentrypoint = '{plane}/{host}.toml'\n",
        );

        assert_eq!(r.manifest_of("git"), Path::new("apps/git/app.toml"));
        assert_eq!(r.fragments_of("git"), Path::new("apps/git/private"));
        assert_eq!(r.globals, Path::new("etc/globals.toml"));
        assert_eq!(r.fragments, Path::new("etc/private"));
        assert_eq!(
            r.entrypoint_of("user", "strix"),
            Path::new("build/dd/user/strix.toml")
        );
        assert_eq!(
            r.overlay_variables_file("personal"),
            Path::new("build/dd/personal/variables.toml")
        );
    }

    /// The reason paths are computed rather than written down: an entrypoint one
    /// directory deeper has to reach one directory further back, and nothing in
    /// the config says so.
    #[test]
    fn references_are_computed_from_wherever_the_file_lands() {
        let flat = resolved("[layout]\ncomposition = 'modules/dotdrop/hosts.toml'\n");
        let entry = flat.entrypoint_of("user", "strix");
        assert_eq!(
            relative(entry.parent().unwrap(), &flat.modules),
            "..",
            "entrypoint sits one level below the module tree"
        );
        assert_eq!(
            relative(entry.parent().unwrap(), &flat.variables_file()),
            "variables.toml"
        );

        let nested = resolved(
            "[layout]\ncomposition = 'modules/dotdrop/hosts.toml'\n\
             [output]\nentrypoint = '{plane}/{host}.toml'\nvariables = 'bundle/variables.toml'\n",
        );
        // modules/dotdrop/user/ -> modules/
        let entry = nested.entrypoint_of("user", "strix");
        assert_eq!(relative(entry.parent().unwrap(), &nested.modules), "../..");
        assert_eq!(
            relative(entry.parent().unwrap(), &nested.variables_file()),
            "../bundle/variables.toml"
        );
    }

    #[test]
    fn a_directory_holding_the_composition_is_not_a_module() {
        let r = resolved("[layout]\ncomposition = 'modules/dotdrop/hosts.toml'\n");
        assert_eq!(r.reserved_module_names(), vec!["dotdrop".to_owned()]);

        let outside = resolved("[layout]\ncomposition = 'config/hosts.toml'\n");
        assert!(
            outside.reserved_module_names().is_empty(),
            "nothing is privileged when the config lives outside the module tree"
        );
    }

    #[test]
    fn a_fragment_name_is_an_overlay_and_an_optional_part() {
        let overlays = ["common", "personal"];

        assert_eq!(
            match_fragment("personal", overlays),
            vec![FragmentName {
                overlay: "personal",
                part: None
            }]
        );
        assert_eq!(
            match_fragment("personal.identity", overlays),
            vec![FragmentName {
                overlay: "personal",
                part: Some("identity".to_owned())
            }]
        );
        // Not a fragment of `personal`: the suffix separator is required, so an
        // overlay named `personal` does not swallow a `personal2` file.
        assert!(match_fragment("personal2", overlays).is_empty());
        assert!(match_fragment("personal.", overlays).is_empty());
        assert!(match_fragment("khronos3d", overlays).is_empty());
    }

    /// The plain fragment is the overlay's baseline and merges first; the named
    /// ones layer on it in name order. Plain lexicographic order would get this
    /// backwards, since `personal.identity` sorts before `personal`.
    #[test]
    fn the_plain_fragment_merges_before_its_named_parts() {
        let mut names = [
            FragmentName {
                overlay: "personal",
                part: Some("secret".to_owned()),
            },
            FragmentName {
                overlay: "personal",
                part: None,
            },
            FragmentName {
                overlay: "personal",
                part: Some("identity".to_owned()),
            },
        ];
        names.sort();

        let parts: Vec<Option<&str>> = names.iter().map(|n| n.part.as_deref()).collect();
        assert_eq!(parts, vec![None, Some("identity"), Some("secret")]);
    }

    /// Nested overlay names make a file name genuinely ambiguous. The caller is
    /// told about both rather than handed a guess, because guessing files a
    /// value under the wrong encryption key.
    #[test]
    fn a_name_matching_two_overlays_reports_both() {
        let matches = match_fragment("work.eu", ["work", "work.eu"]);
        assert_eq!(matches.len(), 2, "{matches:?}");
    }

    #[test]
    fn a_template_without_its_placeholders_is_refused() {
        let err = Resolved::new(toml::from_str("[output]\nentrypoint = '{host}.toml'\n").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("{plane}"), "got: {err}");
    }

    #[test]
    fn paths_may_not_escape_the_repository() {
        let err = Resolved::new(
            toml::from_str("[layout]\ncomposition = '../elsewhere/hosts.toml'\n").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("escapes"), "got: {err}");
    }
}
