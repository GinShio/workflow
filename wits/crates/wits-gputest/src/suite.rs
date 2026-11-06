//! A suite — one runner invocation — and what running it means for each runner
//! shape: the binary, the argv, the installed layout the suite needs, what a
//! selection of its groups runs, and the driver it accepts. The pipeline never
//! branches on the kind itself.
//!
//! Both shapes are binaries from the deqp-runner project and leave the same
//! evidence behind; `report.rs` reads it.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::{self, Deserializer};
use serde::Deserialize;

use wits_actions::Env;
use wits_project::DevenvEntry;

/// The marker the run directory is replaced with in the identity argv.
const RUN_DIR_MARKER: &str = "{run}";

#[derive(Debug, Clone)]
pub struct Suite {
    pub name: String,
    /// The project whose `install_dir` is the suite root.
    pub project: String,
    /// The project whose `install_dir` holds the runner binaries.
    pub runner: String,
    /// Named sets of the suite's tests, which a case and a narrowed run select
    /// from: caselists, directories of them or patterns for `deqp`, one
    /// profile each for `piglit`.
    pub groups: BTreeMap<String, Vec<String>>,
    pub jobs: Jobs,
    /// Seconds one test may run before the runner records a `Timeout`.
    pub timeout: u64,
    pub env: Vec<DevenvEntry>,
    pub kind: Kind,
}

/// What one run tests: the groups it selected, checked against the installed
/// tree. Only [`Suite::select`] makes one.
#[derive(Debug, Clone)]
pub struct Selection {
    /// The selected groups, in name order.
    pub groups: Vec<String>,
    /// What the groups name for the runner, group by group: caselists for
    /// `deqp`, relative to the suite root; for `piglit`, exactly one profile.
    entries: Vec<String>,
    /// What holds the selected tests — each caselist that names a test for
    /// `deqp`, the profile for `piglit` — and the group that selected it.
    sources: BTreeMap<String, String>,
}

/// The runner shape, carrying the declaration only that shape has. The config
/// names it by its table: `[suite.<name>.deqp]` or `[suite.<name>.piglit]`.
#[derive(Debug, Clone)]
pub enum Kind {
    Deqp(DeqpDecl),
    Piglit(PiglitDecl),
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
    /// The directory holding `tests/`, `bin/` and `lib/`.
    pub folder: String,
}

/// Parallel test processes. `"cores"` means physical cores, the right number
/// for CPU-bound shader compilation; a number is used verbatim.
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

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Deqp(_) => "deqp",
            Kind::Piglit(_) => "piglit",
        }
    }

    /// The binary the runner project installs for this shape.
    pub fn runner_bin(&self) -> &'static str {
        match self {
            Kind::Deqp(_) => "deqp-runner",
            Kind::Piglit(_) => "piglit-runner",
        }
    }
}

/// The inputs to one runner command line. Every path is absolute; the
/// selection's entries are relative to `suite_root`.
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    pub selection: &'a Selection,
    pub run_dir: &'a Path,
    pub baseline: Option<&'a Path>,
    pub suite_root: &'a Path,
    pub runner_root: &'a Path,
    pub jobs: u32,
}

impl Suite {
    /// One declared group's entries.
    pub fn group(&self, name: &str) -> Result<&[String]> {
        self.groups.get(name).map(Vec::as_slice).with_context(|| {
            format!(
                "suite '{}' has no group '{name}'; its groups: {}",
                self.name,
                self.groups.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
    }

    /// Refuse a selection of `count` groups that one invocation of this shape
    /// cannot run: piglit-runner takes exactly one profile.
    pub fn check_group_count(&self, count: usize) -> Result<()> {
        match self.kind {
            Kind::Piglit(_) if count != 1 => bail!(
                "suite '{}': piglit takes one profile per invocation, so a run selects one group \
                 (got {count})",
                self.name
            ),
            Kind::Deqp(_) | Kind::Piglit(_) => Ok(()),
        }
    }

    /// The named groups, which must be at least one, as the installed suite
    /// tree at `root` holds them. A deqp selection reads every caselist it
    /// reaches and hands the runner each of them once (`caselist_set`), so a
    /// caselist that is missing, empty or outside the root is an error before
    /// the runner starts; a piglit one checks that its profile exists.
    ///
    /// A repeated name counts once, and the groups are taken in name order, so
    /// one set of groups always reaches the runner in one order. The order is
    /// not cosmetic: deqp-runner shuffles its tests with a fixed seed, so it
    /// decides which tests share a dEQP process, and the identity hashes it as
    /// part of the argv.
    pub fn select(&self, root: &Path, groups: &[String]) -> Result<Selection> {
        let groups: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
        self.check_group_count(groups.len())?;
        let mut named = Vec::new();
        for &group in &groups {
            for entry in self.group(group)? {
                named.push((group, entry.as_str()));
            }
        }
        let (entries, sources) = match &self.kind {
            Kind::Deqp(_) => caselist_set(root, &named)?,
            Kind::Piglit(piglit) => {
                // One group, and the loader allows a piglit group one profile.
                let (group, profile) = named[0];
                let tests = root.join(&piglit.folder).join("tests");
                if !profile_candidates(profile)
                    .iter()
                    .any(|candidate| tests.join(candidate).is_file())
                {
                    bail!(
                        "Piglit profile '{profile}' does not exist under {} in any spelling \
                         piglit-runner reads",
                        tests.display()
                    );
                }
                (
                    vec![profile.to_owned()],
                    BTreeMap::from([(profile.to_owned(), group.to_owned())]),
                )
            }
        };
        Ok(Selection {
            groups: groups.into_iter().map(str::to_owned).collect(),
            entries,
            sources,
        })
    }

    /// The complete command line for one run. The args are a list, never a
    /// shell string, so nothing is word-split or quoted.
    pub fn argv(&self, ctx: &RunContext<'_>) -> Result<Vec<String>> {
        let mut argv = vec![
            path_text(&ctx.runner_root.join(self.kind.runner_bin()))?,
            "run".to_owned(),
            "--output".to_owned(),
            path_text(ctx.run_dir)?,
        ];
        match &self.kind {
            Kind::Deqp(_) => {
                for entry in &ctx.selection.entries {
                    argv.push("--caselist".to_owned());
                    argv.push(path_text(&ctx.suite_root.join(entry))?);
                }
            }
            Kind::Piglit(piglit) => {
                argv.push("--piglit-folder".to_owned());
                argv.push(path_text(&ctx.suite_root.join(&piglit.folder))?);
                argv.push("--profile".to_owned());
                argv.push(ctx.selection.entries[0].clone());
            }
        }
        if let Some(baseline) = ctx.baseline {
            argv.push("--baseline".to_owned());
            argv.push(path_text(baseline)?);
        }
        argv.push("--jobs".to_owned());
        argv.push(ctx.jobs.to_string());
        argv.push("--timeout".to_owned());
        argv.push(self.timeout.to_string());
        if let Kind::Deqp(deqp) = &self.kind {
            if let Some(n) = deqp.tests_per_group {
                argv.push("--tests-per-group".to_owned());
                argv.push(n.to_string());
            }
            argv.push("--deqp".to_owned());
            argv.push(path_text(&ctx.suite_root.join(&deqp.exe))?);
            argv.push("--".to_owned());
            argv.extend(deqp.args.iter().cloned());
            if let Some(waiver) = &deqp.waiver {
                argv.push(format!(
                    "--deqp-waiver-file={}",
                    path_text(&ctx.suite_root.join(waiver))?
                ));
            }
        }
        Ok(argv)
    }

    /// The argv that identity hashes: the run directory replaced by a marker
    /// and no baseline, so a first run and a rerun of one build hash the same
    /// command line.
    pub fn identity_argv(
        &self,
        selection: &Selection,
        suite_root: &Path,
        runner_root: &Path,
        jobs: u32,
    ) -> Result<Vec<String>> {
        self.argv(&RunContext {
            selection,
            run_dir: Path::new(RUN_DIR_MARKER),
            baseline: None,
            suite_root,
            runner_root,
            jobs,
        })
    }

    /// Verify the installed layout this shape needs under the suite root. What
    /// a group names is checked when a run selects it instead, so a broken
    /// group fails only the runs that select it.
    pub fn check_assets(&self, suite_root: &Path) -> Result<()> {
        match &self.kind {
            Kind::Deqp(deqp) => self.check_deqp_assets(deqp, suite_root),
            Kind::Piglit(piglit) => self.check_piglit_assets(piglit, suite_root),
        }
    }

    /// The API-specific driver guard. dEQP Vulkan must load exactly one
    /// selected ICD; Piglit GL needs no ICD, and reaches one through a layer
    /// such as zink when it does.
    pub fn check_driver(&self, env: &Env) -> Result<()> {
        match self.kind {
            Kind::Deqp(_) => crate::icd::guard(env),
            Kind::Piglit(_) => Ok(()),
        }
    }

    fn check_deqp_assets(&self, deqp: &DeqpDecl, root: &Path) -> Result<()> {
        let exe = root.join(&deqp.exe);
        if !exe.is_file() {
            bail!("dEQP executable {} is missing", exe.display());
        }
        if !crate::util::is_executable(&exe)? {
            bail!("dEQP executable {} is not executable", exe.display());
        }
        if let Some(waiver) = &deqp.waiver {
            non_empty_file(&root.join(waiver), "waiver")?;
        }
        let data = root.join("vulkan");
        if !data.is_dir() {
            bail!("dEQP data directory {} is missing", data.display());
        }
        Ok(())
    }

    fn check_piglit_assets(&self, piglit: &PiglitDecl, root: &Path) -> Result<()> {
        let folder = root.join(&piglit.folder);
        let tests = folder.join("tests");
        if !tests.is_dir() {
            bail!("Piglit profile directory {} is missing", tests.display());
        }
        for (dir, holds) in [("bin", "test executables"), ("lib", "shared libraries")] {
            let path = folder.join(dir);
            let mut entries = std::fs::read_dir(&path).with_context(|| {
                format!("Piglit {holds} directory {} is missing", path.display())
            })?;
            if entries.next().is_none() {
                bail!("Piglit {holds} directory {} is empty", path.display());
            }
        }
        Ok(())
    }
}

impl Selection {
    /// Refuse a narrowed run that selects tests `case` does not run. Its
    /// baseline is the case's last run, which never judged them, so each of
    /// their known failures would count as new.
    pub fn check_within(&self, case_selection: &Selection, case: &str) -> Result<()> {
        for (source, group) in &self.sources {
            if !case_selection.sources.contains_key(source) {
                bail!(
                    "group '{group}' selects {source}, which case '{case}' does not run; its \
                     baseline cannot judge those tests"
                );
            }
        }
        Ok(())
    }
}

/// One deqp selection as a set: the entries to hand the runner, in the order
/// `named` gives them, and every caselist that names a selected test with the
/// group that selected it. A directory or pattern entry counts as the lists it
/// expands to, each an entry of its own.
///
/// deqp-runner neither deduplicates its test list nor notices a cycle: a test
/// listed twice runs twice, and `RunnerResults::record_result` (deqp-runner
/// 0.24) records the second result as a failure. So each caselist reaches the
/// runner once. An entry whose caselists another entry reaches too is
/// dropped and the larger kept, which makes `all` with any group inside it
/// `all`, and a list that two groups name a single entry. The runner reads an
/// entry whole, so nothing may be reached twice through one entry, and two
/// entries that share caselists without one holding the other are refused;
/// only two indexes can do that.
///
/// Overlap is judged by caselist, not by test: two caselists that name one
/// test are not noticed. VK-GL-CTS's mustpass lists partition its tests, so
/// for the lists it installs the two are the same.
fn caselist_set(
    root: &Path,
    named: &[(&str, &str)],
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    let mut walked = Vec::new();
    for &(group, entry) in named {
        let path = inside_root(entry).with_context(|| {
            format!("group '{group}' names {entry}, which is outside the suite root")
        })?;
        let lists = if is_pattern(entry) {
            let pattern = match (entry.ends_with('/'), path.is_empty()) {
                (false, _) => path,
                (true, true) => "*".to_owned(),
                (true, false) => format!("{path}/*"),
            };
            let lists = expand(root, &pattern)
                .with_context(|| format!("group '{group}': expanding {entry}"))?;
            if lists.is_empty() {
                bail!("group '{group}': {entry} matches no caselist");
            }
            lists
        } else {
            vec![path]
        };
        for list in lists {
            let mut walk = Walk {
                root,
                reached: BTreeSet::new(),
                sources: BTreeSet::new(),
            };
            walk.visit(list.clone())
                .with_context(|| format!("group '{group}'"))?;
            walked.push((group, list, walk.sources));
        }
    }
    // The largest first, so of two nested entries the inner one is dropped; the
    // sort is stable, so of two equal ones the first in name order stays.
    let mut order: Vec<usize> = (0..walked.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(walked[index].2.len()));
    let mut covered: BTreeMap<&str, usize> = BTreeMap::new();
    let mut kept = vec![false; walked.len()];
    for index in order {
        let (group, entry, sources) = &walked[index];
        let Some(shared) = sources
            .iter()
            .find(|source| covered.contains_key(source.as_str()))
        else {
            covered.extend(sources.iter().map(|source| (source.as_str(), index)));
            kept[index] = true;
            continue;
        };
        if sources
            .iter()
            .all(|source| covered.contains_key(source.as_str()))
        {
            continue;
        }
        let (other_group, other_entry, _) = &walked[covered[shared.as_str()]];
        bail!(
            "group '{group}' reaches caselist {shared} through {entry}, and group \
             '{other_group}' through {other_entry}, but neither entry holds the other; \
             deqp-runner reads each whole and would run those tests twice"
        );
    }
    let entries = walked
        .iter()
        .zip(&kept)
        .filter(|(_, kept)| **kept)
        .map(|((_, entry, _), _)| entry.clone())
        .collect();
    let sources = covered
        .into_iter()
        .map(|(source, index)| (source.to_owned(), walked[index].0.to_owned()))
        .collect();
    Ok((entries, sources))
}

/// Whether a deqp group entry names several caselists: a directory, which ends
/// in `/`, or a pattern holding `*` or `?`.
pub fn is_pattern(entry: &str) -> bool {
    entry.ends_with('/') || entry.contains(['*', '?'])
}

/// The `.txt` files a pattern relative to the suite root matches, in byte
/// order. Each component is matched against the names in the directory the
/// components before it reached; a component with no wildcard names itself.
/// Only directories continue a match and only regular files end one. A name
/// that is not UTF-8 cannot be named by the config, literally or by a
/// pattern, and is passed over.
fn expand(root: &Path, pattern: &str) -> Result<Vec<String>> {
    let parts: Vec<&str> = pattern.split('/').collect();
    let mut reached = vec![String::new()];
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let mut next = Vec::new();
        for base in &reached {
            let names = if part.contains(['*', '?']) {
                let dir = root.join(base);
                let reading = || format!("reading directory {}", dir.display());
                let mut names = Vec::new();
                for found in std::fs::read_dir(&dir).with_context(reading)? {
                    if let Some(name) = found.with_context(reading)?.file_name().to_str() {
                        if wildcard_matches(part, name) {
                            names.push(name.to_owned());
                        }
                    }
                }
                names
            } else {
                vec![(*part).to_owned()]
            };
            for name in names {
                let path = if base.is_empty() {
                    name
                } else {
                    format!("{base}/{name}")
                };
                let full = root.join(&path);
                let meta = match std::fs::metadata(&full) {
                    Ok(meta) => meta,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => {
                        return Err(err).with_context(|| format!("reading {}", full.display()))
                    }
                };
                let keep = if last {
                    meta.is_file() && path.ends_with(".txt")
                } else {
                    meta.is_dir()
                };
                if keep {
                    next.push(path);
                }
            }
        }
        reached = next;
    }
    reached.sort();
    reached.dedup();
    Ok(reached)
}

/// Whether one path component matches a pattern in which `*` stands for any
/// run of characters and `?` for exactly one.
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    let (mut p, mut n) = (0, 0);
    // The last `*` seen, and how far into `name` its run reaches so far.
    let mut star = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((at, run_end)) = star {
            star = Some((at, run_end + 1));
            p = at + 1;
            n = run_end + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// One entry's caselists, read the way deqp-runner 0.24 reads them
/// (`read_lines` in its `src/lib.rs`): a line that ends in `.txt` names another
/// caselist beside the one holding it, and any other line names a test. Every
/// caselist must be a non-empty file inside the suite root, as every suite
/// path must: a run never silently tests fewer cases, and the install stamp
/// hashes only that tree.
struct Walk<'a> {
    root: &'a Path,
    /// Every caselist reached so far, relative to the root.
    reached: BTreeSet<String>,
    /// The caselists among them that name a test.
    sources: BTreeSet<String>,
}

impl Walk<'_> {
    fn visit(&mut self, list: String) -> Result<()> {
        if !self.reached.insert(list.clone()) {
            bail!(
                "caselist {list} is reached twice through one entry, which deqp-runner reads \
                 whole; it would run those tests twice and record each second result as a \
                 failure"
            );
        }
        let path = self.root.join(&list);
        // deqp-runner opens `<caselist>.zst` in place of a caselist beside
        // which one exists, and this walk would not see what it holds.
        let mut compressed = OsString::from(path.as_os_str());
        compressed.push(".zst");
        let compressed = PathBuf::from(compressed);
        if compressed.exists() {
            bail!(
                "{} exists, and deqp-runner would read it in place of caselist {list}; \
                 gputest reads caselists only as plain text",
                compressed.display()
            );
        }
        non_empty_file(&path, "caselist")?;
        let (names_tests, named) = read_caselist(&path, &list)?;
        if names_tests {
            self.sources.insert(list);
        }
        for sub in named {
            self.visit(sub)?;
        }
        Ok(())
    }
}

/// Whether the caselist at `path` names a test, and the caselists it names,
/// relative to the suite root. `list` is the same caselist relative to the
/// root, which its lines are resolved beside. Lines end the way `BufRead`
/// ends them for deqp-runner: at `\n`, with a `\r` before it dropped too.
fn read_caselist(path: &Path, list: &str) -> Result<(bool, Vec<String>)> {
    let reading = || format!("reading caselist {}", path.display());
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path).with_context(reading)?);
    let dir = list.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut names_tests = false;
    let mut named = Vec::new();
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).with_context(reading)? > 0 {
        let text = match line.strip_suffix(b"\n") {
            Some(text) => text.strip_suffix(b"\r").unwrap_or(text),
            None => &line,
        };
        if text.ends_with(b".txt") || text.ends_with(b".txt.zst") {
            let name = std::str::from_utf8(text)
                .with_context(|| format!("caselist {list} names a caselist that is not UTF-8"))?;
            if name.ends_with(".zst") {
                bail!(
                    "caselist {list} names the compressed caselist {name}; gputest reads \
                     caselists only as plain text"
                );
            }
            // An absolute line replaces the directory, as `Path::join` does
            // for deqp-runner.
            let beside = if dir.is_empty() || name.starts_with('/') {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            };
            named.push(inside_root(&beside).with_context(|| {
                format!("caselist {list} names {name}, which is outside the suite root")
            })?);
        } else {
            names_tests = true;
        }
        line.clear();
    }
    Ok((names_tests, named))
}

/// `path`, relative to the suite root, with `.` and `..` resolved lexically;
/// `None` when it is absolute or climbs out of the root.
fn inside_root(path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// A run would silently test fewer cases with an empty caselist or waiver.
fn non_empty_file(path: &Path, what: &str) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("{what} {} is missing", path.display()))?;
    if !meta.is_file() || meta.len() == 0 {
        bail!("{what} {} is not a non-empty file", path.display());
    }
    Ok(())
}

/// The profile spellings piglit-runner's `read_profile_file` tries, in its
/// order. The runner has no process-isolation flag here, so it reads the
/// no-isolation forms first.
fn profile_candidates(profile: &str) -> Vec<PathBuf> {
    let base = Path::new(profile);
    [
        "no_isolation.meta.xml",
        "meta.xml",
        "no_isolation.xml.gz",
        "no_isolation.xml",
        "xml.gz",
        "xml",
    ]
    .into_iter()
    .map(|ext| base.with_extension(ext))
    .collect()
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn suite(deqp: bool) -> Suite {
        Suite {
            name: "s".to_owned(),
            project: "test".to_owned(),
            runner: "runner".to_owned(),
            groups: BTreeMap::from([(
                "all".to_owned(),
                vec![if deqp { "mustpass/vk.txt" } else { "quick" }.to_owned()],
            )]),
            jobs: Jobs::Count(4),
            timeout: 300,
            env: Vec::new(),
            kind: if deqp {
                Kind::Deqp(DeqpDecl {
                    exe: "deqp-vk".to_owned(),
                    waiver: Some("waivers/vk.xml".to_owned()),
                    args: vec!["--deqp-log-images=disable".to_owned()],
                    tests_per_group: Some(4096),
                })
            } else {
                Kind::Piglit(PiglitDecl {
                    folder: "lib64/piglit".to_owned(),
                })
            },
        }
    }

    /// The selection `Suite::select` makes of a suite's one group, without the
    /// installed tree it checks.
    fn selection(suite: &Suite) -> Selection {
        Selection {
            groups: vec!["all".to_owned()],
            entries: suite.groups["all"].clone(),
            sources: BTreeMap::new(),
        }
    }

    fn context<'a>(
        selection: &'a Selection,
        run_dir: &'a Path,
        baseline: Option<&'a Path>,
    ) -> RunContext<'a> {
        RunContext {
            selection,
            run_dir,
            baseline,
            suite_root: Path::new("/suite"),
            runner_root: Path::new("/runner"),
            jobs: 8,
        }
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A deqp suite over an installed tree laid out like VK-GL-CTS's mustpass:
    /// an index of two lists, one in a subdirectory, and a group for the index
    /// and for each list.
    fn mustpass(root: &Path) -> Suite {
        write(
            root,
            "mustpass/vk-default.txt",
            "vk-default/a.txt\r\nvk-default/sub/b.txt\n",
        );
        write(
            root,
            "mustpass/vk-default/a.txt",
            "dEQP-VK.a.1\ndEQP-VK.a.2\n",
        );
        write(root, "mustpass/vk-default/sub/b.txt", "dEQP-VK.b.1");
        let mut suite = suite(true);
        suite.groups = BTreeMap::from([
            ("all".to_owned(), vec!["mustpass/vk-default.txt".to_owned()]),
            ("a".to_owned(), vec!["mustpass/vk-default/a.txt".to_owned()]),
            (
                "b".to_owned(),
                vec!["./mustpass/vk-default/sub/b.txt".to_owned()],
            ),
        ]);
        suite
    }

    fn names(groups: &[&str]) -> Vec<String> {
        groups.iter().map(|group| (*group).to_owned()).collect()
    }

    fn select_error(suite: &Suite, root: &Path, groups: &[&str]) -> String {
        format!("{:#}", suite.select(root, &names(groups)).unwrap_err())
    }

    #[test]
    fn deqp_argv_is_absolute_and_ordered() {
        let suite = suite(true);
        let selection = selection(&suite);
        let argv = suite
            .argv(&context(
                &selection,
                Path::new("/run/1"),
                Some(Path::new("/run/1/baseline.csv")),
            ))
            .unwrap();
        assert_eq!(
            argv,
            [
                "/runner/deqp-runner",
                "run",
                "--output",
                "/run/1",
                "--caselist",
                "/suite/mustpass/vk.txt",
                "--baseline",
                "/run/1/baseline.csv",
                "--jobs",
                "8",
                "--timeout",
                "300",
                "--tests-per-group",
                "4096",
                "--deqp",
                "/suite/deqp-vk",
                "--",
                "--deqp-log-images=disable",
                "--deqp-waiver-file=/suite/waivers/vk.xml",
            ]
        );
    }

    #[test]
    fn the_identity_argv_omits_the_baseline_and_marks_the_run_dir() {
        let suite = suite(true);
        let selection = selection(&suite);
        let identity = suite
            .identity_argv(&selection, Path::new("/suite"), Path::new("/runner"), 8)
            .unwrap();
        assert!(!identity.iter().any(|a| a == "--baseline"));
        assert!(identity.iter().any(|a| a == RUN_DIR_MARKER));
        let with_baseline = suite
            .argv(&context(
                &selection,
                Path::new("/run/2"),
                Some(Path::new("/b")),
            ))
            .unwrap();
        assert_ne!(identity, with_baseline);
    }

    #[test]
    fn piglit_argv_uses_the_folder_and_one_profile() {
        let suite = suite(false);
        let selection = selection(&suite);
        let argv = suite
            .argv(&context(&selection, Path::new("/run"), None))
            .unwrap();
        assert_eq!(
            argv,
            [
                "/runner/piglit-runner",
                "run",
                "--output",
                "/run",
                "--piglit-folder",
                "/suite/lib64/piglit",
                "--profile",
                "quick",
                "--jobs",
                "8",
                "--timeout",
                "300",
            ]
        );
    }

    #[test]
    fn check_assets_accepts_the_deqp_layout_whatever_its_groups_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = suite(true);
        fs::create_dir_all(root.join("vulkan")).unwrap();
        write(root, "waivers/vk.xml", "<waivers/>");
        fs::write(root.join("deqp-vk"), "").unwrap();
        fs::set_permissions(root.join("deqp-vk"), fs::Permissions::from_mode(0o755)).unwrap();
        // The group's caselist is missing: only a run that selects it fails.
        suite.check_assets(root).unwrap();

        fs::write(root.join("waivers/vk.xml"), "").unwrap();
        let err = suite.check_assets(root).unwrap_err();
        assert!(
            format!("{err:#}").contains("waiver") && format!("{err:#}").contains("non-empty"),
            "{err:#}"
        );

        fs::remove_file(root.join("deqp-vk")).unwrap();
        let err = suite.check_assets(root).unwrap_err();
        assert!(format!("{err:#}").contains("dEQP executable"), "{err:#}");
    }

    #[test]
    fn check_assets_accepts_the_cmake_piglit_layout() {
        let dir = tempfile::tempdir().unwrap();
        let suite = suite(false);
        let folder = dir.path().join("lib64/piglit");
        fs::create_dir_all(folder.join("tests")).unwrap();
        write(&folder, "bin/shader_runner", "");
        write(&folder, "lib/libpiglit.so", "");
        suite.check_assets(dir.path()).unwrap();

        fs::remove_dir_all(folder.join("lib")).unwrap();
        let err = suite.check_assets(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("shared libraries"), "{err:#}");
    }

    #[test]
    fn a_deqp_selection_reads_an_index_the_way_deqp_runner_does() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = mustpass(root);

        // The index stays one entry for the runner; the lists it names, with
        // a CRLF line ending and one without a final newline, hold the tests.
        let all = suite.select(root, &names(&["all"])).unwrap();
        assert_eq!(all.groups, ["all"]);
        assert_eq!(all.entries, ["mustpass/vk-default.txt"]);
        assert_eq!(
            all.sources.keys().collect::<Vec<_>>(),
            ["mustpass/vk-default/a.txt", "mustpass/vk-default/sub/b.txt"]
        );

        // A narrowed run stays inside what the case runs, and only there.
        let a = suite.select(root, &names(&["a"])).unwrap();
        a.check_within(&all, "c").unwrap();
        let err = format!("{:#}", all.check_within(&a, "c").unwrap_err());
        assert!(
            err.contains(
                "group 'all' selects mustpass/vk-default/sub/b.txt, which case 'c' does not run"
            ),
            "{err}"
        );
    }

    #[test]
    fn groups_reach_the_runner_once_each_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let suite = mustpass(root);
        let selection = suite.select(root, &names(&["b", "a", "b"])).unwrap();
        assert_eq!(selection.groups, ["a", "b"]);
        assert_eq!(
            selection.entries,
            ["mustpass/vk-default/a.txt", "mustpass/vk-default/sub/b.txt"]
        );
        assert_eq!(
            select_error(&suite, root, &["nope"]),
            "suite 's' has no group 'nope'; its groups: a, all, b"
        );
    }

    #[test]
    fn a_selection_is_a_set_of_caselists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut suite = mustpass(root);
        let all = suite.select(root, &names(&["all"])).unwrap();

        // A group inside the index adds nothing, so the runner sees the index
        // alone, as it does for `all` by itself.
        let both = suite.select(root, &names(&["a", "all"])).unwrap();
        assert_eq!(both.groups, ["a", "all"]);
        assert_eq!(both.entries, all.entries);
        assert_eq!(both.sources, all.sources);

        // A list that two groups name reaches the runner once.
        suite.groups.insert(
            "a-too".to_owned(),
            vec!["mustpass/vk-default/./a.txt".to_owned()],
        );
        let shared = suite.select(root, &names(&["b", "a-too", "a"])).unwrap();
        assert_eq!(
            shared.entries,
            ["mustpass/vk-default/a.txt", "mustpass/vk-default/sub/b.txt"]
        );
        assert_eq!(shared.sources["mustpass/vk-default/a.txt"], "a");
    }

    #[test]
    fn what_the_runner_reads_whole_cannot_be_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut suite = mustpass(root);

        // An index that names a list twice, or names itself.
        write(
            root,
            "mustpass/twice.txt",
            "vk-default/a.txt\n./vk-default/a.txt\n",
        );
        write(root, "mustpass/loop.txt", "loop.txt\n");
        for (group, list) in [("twice", "twice.txt"), ("loop", "loop.txt")] {
            suite
                .groups
                .insert(group.to_owned(), vec![format!("mustpass/{list}")]);
            let err = select_error(&suite, root, &[group]);
            assert!(
                err.starts_with(&format!("group '{group}': ")) && err.contains("reached twice"),
                "{group}: {err}"
            );
        }

        // Two indexes that share a list, neither holding the other.
        write(root, "mustpass/vk-default/c.txt", "dEQP-VK.c.1\n");
        write(
            root,
            "mustpass/bc.txt",
            "vk-default/sub/b.txt\nvk-default/c.txt\n",
        );
        suite
            .groups
            .insert("bc".to_owned(), vec!["mustpass/bc.txt".to_owned()]);
        let err = select_error(&suite, root, &["all", "bc"]);
        assert!(
            err.contains("caselist mustpass/vk-default/sub/b.txt")
                && err.contains("neither entry holds the other"),
            "{err}"
        );
    }

    #[test]
    fn a_directory_or_a_pattern_names_the_lists_it_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut suite = mustpass(root);
        write(root, "mustpass/vk-default/c.txt", "dEQP-VK.c.1\n");
        write(root, "mustpass/vk-default/notes.md", "not a caselist\n");
        write(root, "mustpass/vk-default/sub/d.txt", "dEQP-VK.d.1\n");
        let top = ["mustpass/vk-default/a.txt", "mustpass/vk-default/c.txt"];
        let sub = [
            "mustpass/vk-default/sub/b.txt",
            "mustpass/vk-default/sub/d.txt",
        ];
        for (entry, expected) in [
            // A directory stands for its own lists, not those below it.
            ("mustpass/vk-default/", &top[..]),
            ("./mustpass/vk-default/*", &top[..]),
            ("mustpass/vk-default/?.txt", &top[..]),
            ("mustpass/*/sub/*.txt", &sub[..]),
            ("mustpass/vk-default/s?b/", &sub[..]),
            ("mustpass/vk-default/*/b*", &sub[..1]),
        ] {
            suite.groups.insert("p".to_owned(), vec![entry.to_owned()]);
            let selection = suite.select(root, &names(&["p"])).unwrap();
            assert_eq!(selection.entries, expected, "{entry}");
        }

        // Each list a pattern names counts on its own: `a` is inside `all`
        // and is dropped, `c` is not and stays.
        suite
            .groups
            .insert("p".to_owned(), vec!["mustpass/vk-default/*.txt".to_owned()]);
        let both = suite.select(root, &names(&["all", "p"])).unwrap();
        assert_eq!(
            both.entries,
            ["mustpass/vk-default.txt", "mustpass/vk-default/c.txt"]
        );

        for entry in [
            "mustpass/none/",
            "mustpass/vk-default/*.md.txt",
            "mustpass/vk-default/x?.txt",
            "mustpass/vk-default/a.txt/",
        ] {
            suite.groups.insert("p".to_owned(), vec![entry.to_owned()]);
            assert_eq!(
                select_error(&suite, root, &["p"]),
                format!("group 'p': {entry} matches no caselist")
            );
        }
    }

    #[test]
    fn a_wildcard_stays_within_one_name() {
        for (pattern, name, matched) in [
            ("*", "", true),
            ("*.txt", "a.txt", true),
            ("*.txt", "a.txt.zst", false),
            ("a*b", "axxb", true),
            ("a*b", "axxbx", false),
            ("a*b*c", "abxbc", true),
            ("?", "", false),
            ("?.txt", "ab.txt", false),
            ("*-*", "multisample-interpolation", true),
        ] {
            assert_eq!(wildcard_matches(pattern, name), matched, "{pattern} {name}");
        }
    }

    #[test]
    fn every_caselist_is_a_plain_non_empty_file_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut suite = mustpass(root);
        write(root, "mustpass/vk-default/empty.txt", "");
        for (index, expected) in [
            ("vk-default/gone.txt", "vk-default/gone.txt is missing"),
            ("vk-default/empty.txt", "is not a non-empty file"),
            (
                "../../outside.txt",
                "names ../../outside.txt, which is outside the suite root",
            ),
            ("/etc/x.txt", "outside the suite root"),
            (
                "vk-default/a.txt.zst",
                "names the compressed caselist vk-default/a.txt.zst",
            ),
        ] {
            write(root, "mustpass/bad.txt", &format!("{index}\n"));
            suite
                .groups
                .insert("bad".to_owned(), vec!["mustpass/bad.txt".to_owned()]);
            let err = select_error(&suite, root, &["bad"]);
            assert!(
                err.starts_with("group 'bad': ") && err.contains(expected),
                "{index}: {err}"
            );
        }

        write(root, "mustpass/vk-default/a.txt.zst", "");
        let err = select_error(&suite, root, &["a"]);
        assert!(
            err.contains("would read it in place of caselist mustpass/vk-default/a.txt"),
            "{err}"
        );
    }

    #[test]
    fn a_piglit_selection_is_one_group_whose_profile_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut suite = suite(false);
        suite
            .groups
            .insert("gpu".to_owned(), vec!["gpu".to_owned()]);
        write(root, "lib64/piglit/tests/quick.meta.xml", "<profile/>");

        let quick = suite.select(root, &names(&["all"])).unwrap();
        assert_eq!(quick.entries, ["quick"]);
        assert!(select_error(&suite, root, &["all", "gpu"]).contains("one profile per invocation"));
        assert!(select_error(&suite, root, &["gpu"]).contains("profile 'gpu' does not exist"));

        write(root, "lib64/piglit/tests/gpu.xml.gz", "");
        let gpu = suite.select(root, &names(&["gpu"])).unwrap();
        let err = format!("{:#}", gpu.check_within(&quick, "c").unwrap_err());
        assert!(
            err.contains("group 'gpu' selects gpu, which case 'c' does not run"),
            "{err}"
        );
    }
}
