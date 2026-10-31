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
//! typo that silently does nothing is the one failure mode a test tool cannot
//! afford.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::{self, Deserializer};
use serde::Deserialize;

use wits_core::config::Root;
use wits_core::path::expand_tilde;
use wits_project::{literal_entry, DevenvDecl, DevenvEntry};

use crate::kinds::Kind;

/// Where the gputest config tree lives.
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
    pub root: PathBuf,
    pub archive_days: i64,
}

#[derive(Debug, Clone)]
pub struct Driver {
    pub name: String,
    pub project: String,
    pub build_type: String,
    pub branch: Option<String>,
    pub select: Option<String>,
    pub env: Vec<(String, DevenvEntry)>,
}

#[derive(Debug, Clone)]
pub struct Suite {
    pub name: String,
    pub kind: Kind,
    pub project: String,
    pub runner: String,
    pub case_list: Vec<String>,
    pub jobs: Jobs,
    pub timeout: u64,
    pub env: Vec<(String, DevenvEntry)>,
    pub deqp: Option<DeqpDecl>,
    pub piglit: Option<PiglitDecl>,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    pub driver: String,
    pub suite: String,
    pub env: Vec<(String, DevenvEntry)>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeqpDecl {
    pub exe: String,
    pub waiver: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub tests_per_group: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiglitDecl {
    pub folder: String,
}

/// Parallel test processes. `"cores"` means physical cores, which is the right
/// number for CPU-bound shader compilation; a number is used verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Jobs {
    #[default]
    Cores,
    Count(u32),
}

impl Jobs {
    pub fn number(self) -> u32 {
        match self {
            Jobs::Cores => crate::cores::physical_cores(),
            Jobs::Count(n) => n,
        }
    }
}

impl<'de> Deserialize<'de> for Jobs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Count(u32),
        }
        match Raw::deserialize(d)? {
            Raw::Word(word) if word == "cores" => Ok(Jobs::Cores),
            Raw::Word(word) => Err(de::Error::custom(format!(
                "jobs must be \"cores\" or a positive integer (got {word:?})"
            ))),
            Raw::Count(n) => Ok(Jobs::Count(n)),
        }
    }
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
    archive_days: i64,
}

fn default_archive_days() -> i64 {
    360
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverDecl {
    project: String,
    #[serde(default = "default_build_type")]
    build_type: String,
    branch: Option<String>,
    select: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, DevenvDecl>,
}

fn default_build_type() -> String {
    "release".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteDecl {
    kind: Kind,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseDecl {
    driver: String,
    suite: String,
    #[serde(default)]
    env: BTreeMap<String, DevenvDecl>,
}

impl Config {
    /// Load the tree from the standard root.
    pub fn load() -> Result<Self> {
        let root = wits_core::config::resolve_root(&CONFIG_ROOT)?;
        Self::load_from(&root)
    }

    /// Load from an explicit root; the test and embedder entry point.
    pub fn load_from(root: &Path) -> Result<Self> {
        let files = wits_core::config::discover_toml(root)
            .with_context(|| format!("scanning config root {}", root.display()))?;

        let mut store: Option<Store> = None;
        let mut store_file: Option<PathBuf> = None;
        let mut drivers = BTreeMap::new();
        let mut driver_files: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut suites = BTreeMap::new();
        let mut suite_files: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut cases = BTreeMap::new();
        let mut case_files: BTreeMap<String, PathBuf> = BTreeMap::new();

        for file in files {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let parsed: ConfigFile =
                toml::from_str(&text).with_context(|| format!("loading {}", file.display()))?;

            if let Some(decl) = parsed.store {
                if let Some(first) = &store_file {
                    bail!(
                        "[store] is declared more than once ({} and {})",
                        first.display(),
                        file.display()
                    );
                }
                store = Some(Store {
                    root: expand_tilde(&decl.root),
                    archive_days: decl.archive_days,
                });
                store_file = Some(file.clone());
            }

            for (name, decl) in parsed.driver {
                if let Some(first) = driver_files.get(&name) {
                    bail!(
                        "driver '{name}' is defined more than once ({} and {})",
                        first.display(),
                        file.display()
                    );
                }
                driver_files.insert(name.clone(), file.clone());
                drivers.insert(name, decl);
            }
            for (name, decl) in parsed.suite {
                if let Some(first) = suite_files.get(&name) {
                    bail!(
                        "suite '{name}' is defined more than once ({} and {})",
                        first.display(),
                        file.display()
                    );
                }
                suite_files.insert(name.clone(), file.clone());
                suites.insert(name, decl);
            }
            for (name, decl) in parsed.case {
                if let Some(first) = case_files.get(&name) {
                    bail!(
                        "case '{name}' is defined more than once ({} and {})",
                        first.display(),
                        file.display()
                    );
                }
                case_files.insert(name.clone(), file.clone());
                cases.insert(name, decl);
            }
        }

        let store = store.context(
            "no [store] section in the gputest config tree; declare the archive root first",
        )?;
        if !store.root.is_absolute() {
            bail!(
                "store.root {} is not absolute; the store is a machine declaration, not a relative path",
                store.root.display()
            );
        }
        if store.archive_days < 0 {
            bail!(
                "store.archive_days must be non-negative (got {})",
                store.archive_days
            );
        }

        let mut config = Config {
            store,
            drivers: BTreeMap::new(),
            suites: BTreeMap::new(),
            cases: BTreeMap::new(),
        };

        for (name, decl) in drivers {
            validate_plain_name("driver", &name)?;
            if decl.project.is_empty() {
                bail!("driver '{name}': project is empty");
            }
            if decl.build_type.is_empty() {
                bail!("driver '{name}': build_type is empty");
            }
            if decl.branch.as_deref().is_some_and(str::is_empty) {
                bail!("driver '{name}': branch is empty");
            }
            validate_env(&format!("driver.{name}.env"), &decl.env)?;
            config.drivers.insert(
                name.clone(),
                Driver {
                    name,
                    project: decl.project,
                    build_type: decl.build_type,
                    branch: decl.branch,
                    select: decl.select,
                    env: resolve_env(decl.env),
                },
            );
        }

        for (name, decl) in suites {
            validate_plain_name("suite", &name)?;
            let suite = resolve_suite(&name, decl)?;
            config.suites.insert(name, suite);
        }

        for (name, decl) in cases {
            validate_case_name(&name)?;
            if !config.drivers.contains_key(&decl.driver) {
                bail!("case '{name}': driver '{}' is not declared", decl.driver);
            }
            if !config.suites.contains_key(&decl.suite) {
                bail!("case '{name}': suite '{}' is not declared", decl.suite);
            }
            validate_env(&format!("case.{name}.env"), &decl.env)?;
            config.cases.insert(
                name.clone(),
                Case {
                    name,
                    driver: decl.driver,
                    suite: decl.suite,
                    env: resolve_env(decl.env),
                },
            );
        }

        Ok(config)
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

fn resolve_suite(name: &str, decl: SuiteDecl) -> Result<Suite> {
    if decl.project.is_empty() {
        bail!("suite '{name}': project is empty");
    }
    if decl.runner.is_empty() {
        bail!("suite '{name}': runner is empty");
    }
    if decl.timeout == 0 {
        bail!("suite '{name}': timeout must be positive");
    }
    if let Jobs::Count(0) = decl.jobs {
        bail!("suite '{name}': jobs must be positive");
    }

    match decl.kind {
        Kind::Deqp => {
            if decl.piglit.is_some() {
                bail!("suite '{name}': kind is deqp but a [suite.{name}.piglit] table is present");
            }
            let deqp = decl.deqp.with_context(|| {
                format!("suite '{name}': kind is deqp but [suite.{name}.deqp] is missing")
            })?;
            if deqp.exe.is_empty() {
                bail!("suite '{name}': deqp.exe is empty");
            }
            if deqp.tests_per_group == Some(0) {
                bail!("suite '{name}': deqp.tests_per_group must be positive");
            }
            validate_suite_path(name, "deqp.exe", &deqp.exe)?;
            if let Some(waiver) = &deqp.waiver {
                validate_suite_path(name, "deqp.waiver", waiver)?;
            }
            for entry in &decl.case_list {
                validate_suite_path(name, "case_list entry", entry)?;
            }
            if decl.case_list.is_empty() {
                bail!(
                    "suite '{name}': case_list is empty; a deqp suite names at least one caselist"
                );
            }
            validate_env(&format!("suite.{name}.env"), &decl.env)?;
            Ok(Suite {
                name: name.to_owned(),
                kind: Kind::Deqp,
                project: decl.project,
                runner: decl.runner,
                case_list: decl.case_list,
                jobs: decl.jobs,
                timeout: decl.timeout,
                env: resolve_env(decl.env),
                deqp: Some(deqp),
                piglit: None,
            })
        }
        Kind::Piglit => {
            if decl.deqp.is_some() {
                bail!("suite '{name}': kind is piglit but a [suite.{name}.deqp] table is present");
            }
            let piglit = decl.piglit.with_context(|| {
                format!("suite '{name}': kind is piglit but [suite.{name}.piglit] is missing")
            })?;
            if piglit.folder.is_empty() {
                bail!("suite '{name}': piglit.folder is empty");
            }
            validate_suite_path(name, "piglit.folder", &piglit.folder)?;
            if decl.case_list.len() != 1 {
                bail!(
                    "suite '{name}': piglit takes one profile per invocation (got {} case_list entries)",
                    decl.case_list.len()
                );
            }
            validate_suite_path(name, "case_list profile", &decl.case_list[0])?;
            validate_env(&format!("suite.{name}.env"), &decl.env)?;
            Ok(Suite {
                name: name.to_owned(),
                kind: Kind::Piglit,
                project: decl.project,
                runner: decl.runner,
                case_list: decl.case_list,
                jobs: decl.jobs,
                timeout: decl.timeout,
                env: resolve_env(decl.env),
                deqp: None,
                piglit: Some(piglit),
            })
        }
    }
}

fn validate_plain_name(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("{kind} name is empty");
    }
    Ok(())
}

/// A case names a store and state directory, so it is a path component and
/// nothing else.
pub fn validate_case_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("case name '' is not a path component");
    }
    if name == "." || name == ".." {
        bail!("case name '{name}' is not a path component");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("case name '{name}' is not a path component (allowed: A-Z a-z 0-9 . _ -)");
    }
    Ok(())
}

/// A suite path is relative to the suite root and must not escape it.
fn validate_suite_path(suite: &str, key: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("suite '{suite}': {key} is empty");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        bail!("suite '{suite}': {key} '{value}' is absolute; suite paths are relative to the suite root");
    }
    for component in path.components() {
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

fn validate_env(table: &str, env: &BTreeMap<String, DevenvDecl>) -> Result<()> {
    for name in env.keys() {
        if name.is_empty() {
            bail!("{table}: environment variable name is empty");
        }
        if name.contains('=') || name.contains('\0') {
            bail!("{table}: {name:?} is not a usable environment variable name");
        }
    }
    Ok(())
}

fn resolve_env(env: BTreeMap<String, DevenvDecl>) -> Vec<(String, DevenvEntry)> {
    env.into_iter()
        .map(|(name, decl)| {
            let entry = literal_entry(&name, &decl);
            (name, entry)
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
kind = "deqp"
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
kind = "piglit"
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

    #[test]
    fn the_reference_configuration_loads() {
        let config = load(REFERENCE).unwrap();
        let suite = config.suite("deqp-vk").unwrap();
        assert_eq!(suite.kind, Kind::Deqp);
        assert_eq!(suite.case_list, ["mustpass/vk-default.txt"]);
        assert_eq!(suite.deqp.as_ref().unwrap().tests_per_group, Some(4096));
        assert_eq!(config.store.archive_days, 360);
        assert_eq!(config.store.root, expand_tilde("~/Public/results"));
        assert_eq!(config.suite("piglit").unwrap().env[0].0, "PIGLIT_PLATFORM");
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
        assert_eq!(env.len(), 3);
        let by_name = |name: &str| env.iter().find(|(key, _)| key == name).unwrap().1.clone();
        assert_eq!(by_name("GONE").op, wits_project::DevenvOp::Unset);
        assert_eq!(by_name("PREFIX").op, wits_project::DevenvOp::Prepend);
        assert_eq!(by_name("PREFIX").value, "x");
        assert_eq!(by_name("WORDS").value, "a:b");
    }

    #[test]
    fn unknown_sections_and_keys_are_load_errors() {
        let err = load(&format!("{REFERENCE}\n[typo]\nx = 1\n")).unwrap_err();
        assert!(format!("{err:#}").contains("unknown field"), "{err:#}");

        let err = load(&REFERENCE.replace("tests_per_group = 4096", "unknown = 1")).unwrap_err();
        assert!(format!("{err:#}").contains("unknown field"), "{err:#}");
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
            let err = load(&body).unwrap_err();
            assert!(
                format!("{err:#}").contains("path component"),
                "{name:?}: {err:#}"
            );
        }
    }

    #[test]
    fn a_suite_path_may_not_escape_its_root() {
        for value in ["/abs/case.txt", "../case.txt", "a/../../case.txt"] {
            let body = REFERENCE.replace(
                "case_list = [\"mustpass/vk-default.txt\"]",
                &format!("case_list = [{value:?}]"),
            );
            let err = load(&body).unwrap_err();
            assert!(
                format!("{err:#}").contains("suite root")
                    || format!("{err:#}").contains("absolute"),
                "{value:?}: {err:#}"
            );
        }
    }

    #[test]
    fn a_suite_has_exactly_the_subtable_its_kind_names() {
        let missing = REFERENCE.replace(
            "[suite.deqp-vk.deqp]\nexe = \"deqp-vk\"\nwaiver = \"waivers/vk.xml\"\nargs = [\"--deqp-log-images=disable\"]\ntests_per_group = 4096\n",
            "",
        );
        let err = load(&missing).unwrap_err();
        assert!(format!("{err:#}").contains("missing"), "{err:#}");

        let second = REFERENCE.replace(
            "[suite.deqp-vk.deqp]",
            "[suite.deqp-vk.piglit]\nfolder = \"lib64/piglit\"\n\n[suite.deqp-vk.deqp]",
        );
        let err = load(&second).unwrap_err();
        assert!(format!("{err:#}").contains("piglit"), "{err:#}");
    }

    #[test]
    fn piglit_takes_exactly_one_profile() {
        let body = REFERENCE.replace(
            "case_list = [\"quick\"]\njobs = 4",
            "case_list = [\"quick\", \"quick_gl\"]\njobs = 4",
        );
        let err = load(&body).unwrap_err();
        assert!(format!("{err:#}").contains("one profile"), "{err:#}");
    }

    #[test]
    fn jobs_accepts_cores_or_a_positive_number() {
        let suite = load(REFERENCE).unwrap().suite("piglit").unwrap().clone();
        assert_eq!(suite.jobs, Jobs::Count(4));
        let suite = load(REFERENCE).unwrap().suite("deqp-vk").unwrap().clone();
        assert_eq!(suite.jobs, Jobs::Cores);

        let zero = REFERENCE.replace("jobs = 4", "jobs = 0");
        let err = load(&zero).unwrap_err();
        assert!(
            format!("{err:#}").contains("jobs must be positive"),
            "{err:#}"
        );

        let bad = REFERENCE.replace("jobs = 4", "jobs = \"many\"");
        let err = load(&bad).unwrap_err();
        assert!(format!("{err:#}").contains("jobs"), "{err:#}");
    }

    #[test]
    fn the_store_root_must_be_absolute_and_retention_non_negative() {
        let err =
            load(&REFERENCE.replace("root = \"~/Public/results\"", "root = \"relative/results\""))
                .unwrap_err();
        assert!(format!("{err:#}").contains("not absolute"), "{err:#}");

        let err = load(&REFERENCE.replace("archive_days = 360", "archive_days = -1")).unwrap_err();
        assert!(format!("{err:#}").contains("non-negative"), "{err:#}");
    }
}
