//! The gputest configuration tree.
//!
//! Files live anywhere under one root and declare what they contribute by their
//! sections, the way the project registry works. The root is
//! `$WITS_GPUTEST_CONFIG`, else `$XDG_CONFIG_HOME/wits/gputest`, else
//! `$HOME/.wits/gputest`.
//!
//! Strictness is the point: an unknown section, an unknown key, a second
//! definition of one name, a path that climbs out of its suite root, or a
//! caselist cardinality the kind cannot honour is a load-time error. A config
//! typo that silently does nothing is the one failure mode an unattended test
//! run cannot afford.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use wits_core::config::Root;
use wits_core::path::expand_tilde;
use wits_project::{literal_entry, DevenvDecl, DevenvEntry};

use crate::suite::{DeqpDecl, Jobs, Kind, PiglitDecl, Suite};

const CONFIG_ROOT: Root<'static> = Root {
    env: "WITS_GPUTEST_CONFIG",
    xdg: "wits/gputest",
    home: ".wits/gputest",
};

#[derive(Debug, Clone)]
pub struct Config {
    pub store: Store,
    pub drivers: BTreeMap<String, Driver>,
    pub suites: BTreeMap<String, Suite>,
    pub cases: BTreeMap<String, Case>,
}

#[derive(Debug, Clone)]
pub struct Store {
    /// `<root>/<case>/<device>/<id>.tar.zst`; absolute.
    pub root: PathBuf,
    /// The age `prune` removes archives at.
    pub archive_days: u32,
}

#[derive(Debug, Clone)]
pub struct Driver {
    pub name: String,
    pub project: String,
    pub build_type: String,
    pub branch: Option<String>,
    /// `VK_LOADER_DRIVERS_SELECT`.
    pub select: Option<String>,
    pub env: Vec<DevenvEntry>,
    /// The libraries the driver loads besides its ICD's own, as files and
    /// directories; their content is `driver.libs`. Absolute.
    pub libs: Vec<PathBuf>,
    /// Libraries the driver's ICD links, each by the name a linked library
    /// carries alone or before its version (`libLLVM.so` for
    /// `libLLVM.so.23.1`); their content is `driver.libs` too.
    pub linked: Vec<String>,
    /// The settings the driver reads, as files and directories; their content
    /// is `driver.config`. Absolute.
    pub config: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    pub driver: String,
    pub suite: String,
    pub env: Vec<DevenvEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    store: Option<StoreDecl>,
    #[serde(default)]
    driver: BTreeMap<String, DriverDecl>,
    #[serde(default)]
    suite: BTreeMap<String, SuiteDecl>,
    #[serde(default)]
    case: BTreeMap<String, CaseDecl>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreDecl {
    root: String,
    #[serde(default = "default_archive_days")]
    archive_days: u32,
}

fn default_archive_days() -> u32 {
    360
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverDecl {
    project: String,
    #[serde(default = "default_build_type")]
    build_type: String,
    branch: Option<String>,
    select: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, DevenvDecl>,
    #[serde(default)]
    libs: Vec<String>,
    #[serde(default)]
    config: Vec<String>,
}

fn default_build_type() -> String {
    "release".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteDecl {
    project: String,
    #[serde(default = "default_runner")]
    runner: String,
    case_list: Vec<String>,
    #[serde(default)]
    jobs: Jobs,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(default)]
    env: BTreeMap<String, DevenvDecl>,
    deqp: Option<DeqpDecl>,
    piglit: Option<PiglitDecl>,
}

fn default_runner() -> String {
    "runner".to_owned()
}

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseDecl {
    driver: String,
    suite: String,
    #[serde(default)]
    env: BTreeMap<String, DevenvDecl>,
}

/// Every declaration of one section across the tree, each with the file that
/// declared it, so a second definition can name both.
struct Declared<T> {
    section: &'static str,
    items: BTreeMap<String, (T, PathBuf)>,
}

impl<T> Declared<T> {
    fn new(section: &'static str) -> Self {
        Self {
            section,
            items: BTreeMap::new(),
        }
    }

    fn add(&mut self, decls: BTreeMap<String, T>, file: &Path) -> Result<()> {
        for (name, decl) in decls {
            if let Some((_, first)) = self.items.get(&name) {
                bail!(
                    "{} '{name}' is defined more than once ({} and {})",
                    self.section,
                    first.display(),
                    file.display()
                );
            }
            self.items.insert(name, (decl, file.to_path_buf()));
        }
        Ok(())
    }
}

impl Config {
    /// Load the tree from the standard root.
    pub fn load() -> Result<Self> {
        let root = wits_core::config::resolve_root(&CONFIG_ROOT)?;
        Self::load_from(&root)
    }

    /// Load from an explicit root; the test entry point.
    pub fn load_from(root: &Path) -> Result<Self> {
        let files = wits_core::config::discover_toml(root)
            .with_context(|| format!("scanning config root {}", root.display()))?;

        let mut store: Option<(StoreDecl, PathBuf)> = None;
        let mut drivers = Declared::new("driver");
        let mut suites = Declared::new("suite");
        let mut cases = Declared::new("case");
        for file in files {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let parsed: ConfigFile =
                toml::from_str(&text).with_context(|| format!("loading {}", file.display()))?;
            if let Some(decl) = parsed.store {
                if let Some((_, first)) = &store {
                    bail!(
                        "[store] is declared more than once ({} and {})",
                        first.display(),
                        file.display()
                    );
                }
                store = Some((decl, file.clone()));
            }
            drivers.add(parsed.driver, &file)?;
            suites.add(parsed.suite, &file)?;
            cases.add(parsed.case, &file)?;
        }

        let (store, _) = store.context(
            "no [store] section in the gputest config tree; declare the archive root first",
        )?;
        let store = Store {
            root: expand_tilde(&store.root),
            archive_days: store.archive_days,
        };
        if !store.root.is_absolute() {
            bail!(
                "store.root {} is not absolute; the store is a machine declaration, not a relative path",
                store.root.display()
            );
        }

        let drivers = drivers
            .items
            .into_iter()
            .map(|(name, (decl, _))| Ok((name.clone(), resolve_driver(name, decl)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let suites = suites
            .items
            .into_iter()
            .map(|(name, (decl, _))| Ok((name.clone(), resolve_suite(name, decl)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let cases = cases
            .items
            .into_iter()
            .map(|(name, (decl, _))| {
                validate_case_name(&name)?;
                if !drivers.contains_key(&decl.driver) {
                    bail!("case '{name}': driver '{}' is not declared", decl.driver);
                }
                if !suites.contains_key(&decl.suite) {
                    bail!("case '{name}': suite '{}' is not declared", decl.suite);
                }
                let case = Case {
                    env: resolve_env(&format!("case.{name}.env"), decl.env)?,
                    name: name.clone(),
                    driver: decl.driver,
                    suite: decl.suite,
                };
                Ok((name, case))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;

        Ok(Config {
            store,
            drivers,
            suites,
            cases,
        })
    }

    pub fn case(&self, name: &str) -> Result<&Case> {
        self.cases.get(name).with_context(|| {
            format!(
                "unknown case '{name}'; declared cases: {}",
                self.cases.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
    }

    pub fn suite(&self, name: &str) -> Result<&Suite> {
        self.suites
            .get(name)
            .with_context(|| format!("suite '{name}' is not declared"))
    }

    pub fn driver(&self, name: &str) -> Result<&Driver> {
        self.drivers
            .get(name)
            .with_context(|| format!("driver '{name}' is not declared"))
    }
}

fn resolve_driver(name: String, decl: DriverDecl) -> Result<Driver> {
    // An empty value would resolve to a build directory nobody declared.
    if decl.build_type.is_empty() {
        bail!("driver '{name}': build_type is empty");
    }
    if decl.branch.as_deref() == Some("") {
        bail!("driver '{name}': branch is empty");
    }
    // A name without a `/` is searched for rather than opened, the way the
    // dynamic loader treats one.
    let (linked, libs): (Vec<String>, Vec<String>) = decl
        .libs
        .into_iter()
        .partition(|entry| !entry.contains('/'));
    if linked.iter().any(String::is_empty) {
        bail!("driver '{name}': a libs entry is empty");
    }
    Ok(Driver {
        env: resolve_env(&format!("driver.{name}.env"), decl.env)?,
        libs: absolute_paths(&name, "libs", &libs)?,
        linked,
        config: absolute_paths(&name, "config", &decl.config)?,
        name,
        project: decl.project,
        build_type: decl.build_type,
        branch: decl.branch,
        select: decl.select,
    })
}

/// A driver's declared paths with `~` expanded. The runner does not run where
/// gputest does, so a relative path would name a different file for each of
/// them.
fn absolute_paths(driver: &str, key: &str, paths: &[String]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            let expanded = expand_tilde(path);
            if !expanded.is_absolute() {
                bail!("driver '{driver}': {key} path '{path}' is not absolute");
            }
            Ok(expanded)
        })
        .collect()
}

fn resolve_suite(name: String, decl: SuiteDecl) -> Result<Suite> {
    if decl.timeout == 0 {
        bail!("suite '{name}': timeout must be positive");
    }
    if decl.jobs == Jobs::Count(0) {
        bail!("suite '{name}': jobs must be positive");
    }
    let kind = match (decl.deqp, decl.piglit) {
        (Some(deqp), None) => {
            validate_suite_path(&name, "deqp.exe", &deqp.exe)?;
            if let Some(waiver) = &deqp.waiver {
                validate_suite_path(&name, "deqp.waiver", waiver)?;
            }
            if deqp.tests_per_group == Some(0) {
                bail!("suite '{name}': deqp.tests_per_group must be positive");
            }
            if decl.case_list.is_empty() {
                bail!(
                    "suite '{name}': case_list is empty; a deqp suite names at least one caselist"
                );
            }
            for entry in &decl.case_list {
                validate_suite_path(&name, "case_list entry", entry)?;
            }
            Kind::Deqp(deqp)
        }
        (None, Some(piglit)) => {
            validate_suite_path(&name, "piglit.folder", &piglit.folder)?;
            let [profile] = decl.case_list.as_slice() else {
                bail!(
                    "suite '{name}': piglit takes one profile per invocation (got {} case_list entries)",
                    decl.case_list.len()
                );
            };
            validate_suite_path(&name, "case_list profile", profile)?;
            Kind::Piglit(piglit)
        }
        (None, None) => bail!(
            "suite '{name}': declare its runner shape with a [suite.{name}.deqp] or \
             [suite.{name}.piglit] table"
        ),
        (Some(_), Some(_)) => bail!(
            "suite '{name}': [suite.{name}.deqp] and [suite.{name}.piglit] are both present; \
             a suite has one runner shape"
        ),
    };
    Ok(Suite {
        env: resolve_env(&format!("suite.{name}.env"), decl.env)?,
        name,
        project: decl.project,
        runner: decl.runner,
        case_list: decl.case_list,
        jobs: decl.jobs,
        timeout: decl.timeout,
        kind,
    })
}

/// A case names a store and a state directory, so it is a path component and
/// nothing else.
fn validate_case_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("case name '{name}' is not a path component (allowed: A-Z a-z 0-9 . _ -)");
    }
    Ok(())
}

/// A suite path is relative to the suite root and stays inside it: the install
/// stamp hashes that tree, so a file outside it would be tested without being
/// part of the identity.
fn validate_suite_path(suite: &str, key: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("suite '{suite}': {key} is empty");
    }
    for component in Path::new(value).components() {
        match component {
            Component::ParentDir => bail!(
                "suite '{suite}': {key} '{value}' climbs out of the suite root with '..'"
            ),
            Component::RootDir | Component::Prefix(_) => bail!(
                "suite '{suite}': {key} '{value}' is absolute; suite paths are relative to the suite root"
            ),
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

fn resolve_env(table: &str, env: BTreeMap<String, DevenvDecl>) -> Result<Vec<DevenvEntry>> {
    env.into_iter()
        .map(|(name, decl)| {
            if name.is_empty() || name.contains('=') || name.contains('\0') {
                bail!("{table}: {name:?} is not a usable environment variable name");
            }
            Ok(literal_entry(&name, &decl))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const REFERENCE: &str = r#"
[store]
root = "~/Public/results"
archive_days = 360

[driver.radv]
project = "mesa"
select = "*radeon*"

[driver.lvp]
project = "mesa"
select = "*lvp*"

[driver.amdvlk]
project = "amdvlk"

[suite.deqp-vk]
project = "deqp"
case_list = ["mustpass/vk-default.txt"]
jobs = "cores"
timeout = 300

[suite.deqp-vk.deqp]
exe = "deqp-vk"
waiver = "waivers/vk.xml"
args = ["--deqp-log-images=disable"]
tests_per_group = 4096

[suite.piglit]
project = "piglit"
case_list = ["quick"]
jobs = 4
timeout = 300

[suite.piglit.piglit]
folder = "lib64/piglit"

[suite.piglit.env]
PIGLIT_PLATFORM = "gbm"
PYTHONDONTWRITEBYTECODE = "1"

[case.radv-vk]
driver = "radv"
suite = "deqp-vk"

[case.radv-zink-gl]
driver = "radv"
suite = "piglit"
env = { MESA_LOADER_DRIVER_OVERRIDE = "zink" }
"#;

    const DEQP_TABLE: &str = "[suite.deqp-vk.deqp]\nexe = \"deqp-vk\"\nwaiver = \"waivers/vk.xml\"\nargs = [\"--deqp-log-images=disable\"]\ntests_per_group = 4096\n";

    fn load(body: &str) -> Result<Config> {
        load_files(&[("config.toml", body)])
    }

    fn load_files(files: &[(&str, &str)]) -> Result<Config> {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        Config::load_from(dir.path())
    }

    fn error(body: &str) -> String {
        format!("{:#}", load(body).unwrap_err())
    }

    #[test]
    fn the_reference_configuration_loads() {
        let config = load(REFERENCE).unwrap();
        let suite = config.suite("deqp-vk").unwrap();
        assert_eq!(suite.kind.name(), "deqp");
        assert_eq!(suite.case_list, ["mustpass/vk-default.txt"]);
        let Kind::Deqp(deqp) = &suite.kind else {
            panic!("deqp-vk is not a deqp suite")
        };
        assert_eq!(deqp.tests_per_group, Some(4096));
        assert_eq!(suite.jobs, Jobs::Cores);
        assert_eq!(config.suite("piglit").unwrap().kind.name(), "piglit");
        assert_eq!(config.suite("piglit").unwrap().jobs, Jobs::Count(4));
        assert_eq!(config.store.archive_days, 360);
        assert_eq!(config.store.root, expand_tilde("~/Public/results"));
        assert_eq!(
            config.suite("piglit").unwrap().env[0].name,
            "PIGLIT_PLATFORM"
        );
        assert_eq!(config.case("radv-zink-gl").unwrap().env.len(), 1);
    }

    #[test]
    fn the_env_tables_use_the_devenv_declaration_vocabulary() {
        let body = format!(
            "{REFERENCE}\n[case.env-test]\ndriver = \"radv\"\nsuite = \"deqp-vk\"\n\
             env = {{ GONE = {{ unset = true }}, PREFIX = {{ prepend = \"x\" }}, \
             WORDS = [\"a\", \"b\"] }}\n"
        );
        let config = load(&body).unwrap();
        let env = &config.case("env-test").unwrap().env;
        let by_name = |name: &str| env.iter().find(|entry| entry.name == name).unwrap().clone();
        assert_eq!(by_name("GONE").op, wits_project::DevenvOp::Unset);
        assert_eq!(by_name("PREFIX").op, wits_project::DevenvOp::Prepend);
        assert_eq!(by_name("PREFIX").value, "x");
        assert_eq!(by_name("WORDS").value, "a:b");
    }

    #[test]
    fn unknown_sections_and_keys_are_load_errors() {
        assert!(error(&format!("{REFERENCE}\n[typo]\nx = 1\n")).contains("unknown field"));
        assert!(
            error(&REFERENCE.replace("tests_per_group = 4096", "unknown = 1"))
                .contains("unknown field")
        );
        // The runner shape is its table; there is no `kind` key to repeat it.
        assert!(error(
            &REFERENCE.replace("[suite.piglit]\n", "[suite.piglit]\nkind = \"piglit\"\n")
        )
        .contains("unknown field"));
    }

    #[test]
    fn driver_paths_are_absolute_and_a_bare_libs_name_is_a_linked_library() {
        let body = REFERENCE.replace(
            "[driver.lvp]\nproject = \"mesa\"\n",
            "[driver.lvp]\nproject = \"mesa\"\nlibs = [\"~/lib\", \"libLLVM.so\"]\n\
             config = [\"~/settings.cfg\"]\n",
        );
        let config = load(&body).unwrap();
        let lvp = config.driver("lvp").unwrap();
        assert_eq!(lvp.libs, [expand_tilde("~/lib")]);
        assert_eq!(lvp.linked, ["libLLVM.so"]);
        assert_eq!(lvp.config, [expand_tilde("~/settings.cfg")]);
        for (declared, key, path) in [
            ("~/lib", "libs", "lib/relative"),
            ("~/settings.cfg", "config", "relative"),
            ("~/settings.cfg", "config", ""),
        ] {
            let err = error(&body.replace(declared, path));
            assert!(
                err.contains(&format!("{key} path '{path}' is not absolute")),
                "{key} {path:?}: {err}"
            );
        }
        let err = error(&body.replace("libLLVM.so", ""));
        assert!(err.contains("a libs entry is empty"), "{err}");
    }

    #[test]
    fn a_name_may_not_be_defined_twice() {
        let err = load_files(&[
            ("a.toml", REFERENCE),
            ("b.toml", "[driver.radv]\nproject = \"other\"\n"),
        ])
        .unwrap_err();
        assert!(format!("{err:#}").contains("driver 'radv' is defined more than once"));
    }

    #[test]
    fn a_case_name_must_be_a_path_component() {
        for name in [".", "..", "a/b", "a b", ""] {
            let body =
                format!("{REFERENCE}\n[case.\"{name}\"]\ndriver = \"radv\"\nsuite = \"deqp-vk\"\n");
            assert!(error(&body).contains("path component"), "{name:?}");
        }
    }

    #[test]
    fn a_suite_path_may_not_escape_its_root() {
        for value in ["/abs/case.txt", "../case.txt", "a/../../case.txt"] {
            let body = REFERENCE.replace(
                "case_list = [\"mustpass/vk-default.txt\"]",
                &format!("case_list = [{value:?}]"),
            );
            let err = error(&body);
            assert!(err.contains("suite root"), "{value:?}: {err}");
        }
    }

    #[test]
    fn a_suite_declares_exactly_one_runner_shape() {
        let err = error(&REFERENCE.replace(DEQP_TABLE, ""));
        assert!(err.contains("declare its runner shape"), "{err}");

        let both = REFERENCE.replace(
            "[suite.deqp-vk.deqp]",
            "[suite.deqp-vk.piglit]\nfolder = \"lib64/piglit\"\n\n[suite.deqp-vk.deqp]",
        );
        assert!(error(&both).contains("both present"));
    }

    #[test]
    fn piglit_takes_exactly_one_profile() {
        let body = REFERENCE.replace(
            "case_list = [\"quick\"]",
            "case_list = [\"quick\", \"all\"]",
        );
        assert!(error(&body).contains("one profile"));
    }

    #[test]
    fn jobs_accepts_cores_or_a_positive_number() {
        assert!(error(&REFERENCE.replace("jobs = 4", "jobs = 0")).contains("jobs must be positive"));
        assert!(error(&REFERENCE.replace("jobs = 4", "jobs = \"many\"")).contains("jobs"));
    }

    #[test]
    fn the_store_root_must_be_absolute_and_retention_non_negative() {
        assert!(error(
            &REFERENCE.replace("root = \"~/Public/results\"", "root = \"relative/results\"")
        )
        .contains("not absolute"));
        assert!(load(&REFERENCE.replace("archive_days = 360", "archive_days = -1")).is_err());
    }
}
