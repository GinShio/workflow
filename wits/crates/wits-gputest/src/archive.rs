//! The run archive: one `tar.zst` per completed (or attempted) run.
//!
//! `run.toml` is the first member, so `history` reads the first member and
//! stops; the CSVs behind it are opened only when a person asks. The archive
//! records the component content hashes and the readable provenance, not a
//! verdict or a count — the runner's CSVs already carry both.
//!
//! The top-level `env` and `argv` keys are written as inline values before the
//! first table. If they were ordinary table fields, TOML would place whatever
//! followed inside whichever table preceded it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const RUN_TOML: &str = "run.toml";

/// The fixed evidence set. Every runner in the runner project writes the first
/// four; the archive keeps whichever the runner actually produced.
pub const EVIDENCE: &[&str] = &[
    "results.csv",
    "failures.csv",
    "failures-all.csv",
    "expectations.json",
];

/// The CSVs whose presence makes a 0/1/2 exit a verdict rather than an error.
pub const REQUIRED_CSVS: &[&str] = &["results.csv", "failures.csv", "failures-all.csv"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub started: String,
    pub duration: u64,
    pub exit: i32,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Names the overlay removed, relative to the scrubbed caller. TOML has no
    /// null in a table, so the removals are their own list; an empty list is
    /// omitted. Together with `env` this is the whole applied overlay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unset: Vec<String>,
    #[serde(default)]
    pub argv: Vec<String>,
    pub device: DeviceRecord,
    pub driver: DriverRecord,
    pub test: TestRecord,
    pub runner: RunnerRecord,
    pub content: ContentRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRecord {
    pub pci: String,
    pub id: String,
    pub drm: String,
    pub kernel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverRecord {
    pub project: String,
    pub focus: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub build_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestRecord {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerRecord {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentRecord {
    pub driver_icd: String,
    pub driver_libs: String,
    pub driver_config: String,
    pub suite: String,
    pub runner: String,
}

#[derive(Serialize)]
struct Tables<'a> {
    device: &'a DeviceRecord,
    driver: &'a DriverRecord,
    test: &'a TestRecord,
    runner: &'a RunnerRecord,
    content: &'a ContentRecord,
}

impl RunRecord {
    /// The TOML text placed as the first archive member.
    pub fn to_toml(&self) -> Result<String> {
        let mut out = String::new();
        out.push_str(&format!("started = {}\n", toml_string(&self.started)));
        out.push_str(&format!("duration = {}\n", self.duration));
        out.push_str(&format!("exit = {}\n", self.exit));
        out.push_str("env = {");
        for (index, (name, value)) in self.env.iter().enumerate() {
            if index == 0 {
                out.push(' ');
            } else {
                out.push_str(", ");
            }
            out.push_str(&toml_key(name));
            out.push_str(" = ");
            out.push_str(&toml_string(value));
        }
        if !self.env.is_empty() {
            out.push(' ');
        }
        out.push_str("}\n");
        if !self.unset.is_empty() {
            out.push_str("unset = [");
            for (index, name) in self.unset.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&toml_string(name));
            }
            out.push_str("]\n");
        }
        out.push_str("argv = [");
        for (index, arg) in self.argv.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&toml_string(arg));
        }
        out.push_str("]\n\n");
        out.push_str(
            &toml::to_string(&Tables {
                device: &self.device,
                driver: &self.driver,
                test: &self.test,
                runner: &self.runner,
                content: &self.content,
            })
            .context("serializing the run record's tables")?,
        );
        Ok(out)
    }

    /// The driver commit a history line shows: the focus repo's if it is
    /// recorded, else the first one there is.
    pub fn driver_commit(&self) -> Option<&str> {
        self.driver
            .commits
            .get(&self.driver.focus)
            .or_else(|| self.driver.commits.values().next())
            .map(String::as_str)
    }
}

/// Write the first member and every evidence file that exists.
pub fn write(path: &Path, record: &RunRecord, run_dir: &Path) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("creating archive {}", path.display()))?;
    let encoder = zstd::stream::write::Encoder::new(file, 0)
        .with_context(|| format!("starting the zstd stream for {}", path.display()))?;
    let mut builder = tar::Builder::new(encoder);

    let text = record.to_toml()?;
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, RUN_TOML, text.as_bytes())
        .context("writing run.toml as the first archive member")?;

    for name in EVIDENCE {
        let member = run_dir.join(name);
        if member.is_file() {
            builder
                .append_path_with_name(&member, name)
                .with_context(|| format!("adding {} to the archive", member.display()))?;
        }
    }

    builder.finish().context("finishing the archive")?;
    let encoder = builder.into_inner().context("finishing the archive")?;
    encoder.finish().context("finishing the zstd stream")?;
    Ok(())
}

/// The first member, as a name and bytes. `history` reads only this.
pub fn read_first(path: &Path) -> Result<(String, Vec<u8>)> {
    let file = File::open(path).with_context(|| format!("opening archive {}", path.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .with_context(|| format!("starting the zstd stream for {}", path.display()))?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive
        .entries()
        .with_context(|| format!("reading archive {}", path.display()))?;
    let mut entry = entries
        .next()
        .transpose()?
        .with_context(|| format!("archive {} has no members", path.display()))?;
    let name = entry
        .path()
        .with_context(|| format!("reading the first member's name in {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {} from {}", name, path.display()))?;
    Ok((name, bytes))
}

/// Copy one named member through a reader, without holding it in memory.
/// Returns false when the archive does not carry it.
fn with_member(
    archive: &Path,
    member: &str,
    consume: impl FnOnce(&mut dyn Read) -> Result<()>,
) -> Result<bool> {
    let file =
        File::open(archive).with_context(|| format!("opening archive {}", archive.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .with_context(|| format!("starting the zstd stream for {}", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar
        .entries()
        .with_context(|| format!("reading archive {}", archive.display()))?
    {
        let mut entry = entry?;
        let name = entry
            .path()
            .with_context(|| format!("reading a member name in {}", archive.display()))?
            .to_string_lossy()
            .into_owned();
        if name == member {
            consume(&mut entry)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copy one named member to `writer` without holding it in memory.
pub fn copy_member(archive: &Path, member: &str, writer: &mut impl std::io::Write) -> Result<bool> {
    with_member(archive, member, |reader| {
        std::io::copy(reader, writer)
            .with_context(|| format!("copying {} out of {}", member, archive.display()))?;
        Ok(())
    })
}

/// Extract the previous run's `failures-all.csv` as the run directory's
/// `baseline.csv`. Returns false when the archive does not carry it. The member
/// is streamed to the file, not held in memory: a baseline can be large.
pub fn extract_baseline(archive: &Path, run_dir: &Path) -> Result<bool> {
    let destination = run_dir.join("baseline.csv");
    with_member(archive, "failures-all.csv", |reader| {
        let mut file = File::create(&destination)
            .with_context(|| format!("creating baseline {}", destination.display()))?;
        std::io::copy(reader, &mut file)
            .with_context(|| format!("writing baseline {}", destination.display()))?;
        Ok(())
    })
}

/// Parse a `run.toml` from bytes.
pub fn parse_run(bytes: &[u8]) -> Result<RunRecord> {
    let text = std::str::from_utf8(bytes).context("run.toml is not UTF-8")?;
    toml::from_str(text).context("reading run.toml")
}

/// Read one archive's `run.toml`, without reading the CSVs.
pub fn read_run(path: &Path) -> Result<RunRecord> {
    let (name, bytes) = read_first(path)?;
    if name != RUN_TOML {
        bail!(
            "archive {} begins with {name:?}, not {RUN_TOML:?}",
            path.display()
        );
    }
    parse_run(&bytes)
}

/// Do the CSVs that make a 0/1/2 exit a verdict exist?
pub fn required_csvs_present(run_dir: &Path) -> bool {
    REQUIRED_CSVS
        .iter()
        .all(|name| run_dir.join(name).is_file())
}

/// Write `run.toml` into the run directory.
pub fn write_run_toml(run_dir: &Path, record: &RunRecord) -> Result<()> {
    let path = run_dir.join(RUN_TOML);
    let text = record.to_toml()?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        key.to_owned()
    } else {
        toml_string(key)
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn record() -> RunRecord {
        RunRecord {
            started: "20261026T213012Z".to_owned(),
            duration: 812,
            exit: 1,
            env: BTreeMap::from([
                ("VK_DRIVER_FILES".to_owned(), "/x/radeon.json".to_owned()),
                ("WEIRD KEY".to_owned(), "line\nnext".to_owned()),
            ]),
            unset: vec!["LD_PRELOAD".to_owned()],
            argv: vec!["runner".to_owned(), "--x".to_owned()],
            device: DeviceRecord {
                pci: "0000:c4:00.0".to_owned(),
                id: "1002:150e".to_owned(),
                drm: "amdgpu".to_owned(),
                kernel: "7.2.11".to_owned(),
            },
            driver: DriverRecord {
                project: "fd.o/mesa".to_owned(),
                focus: "main".to_owned(),
                branch: Some("main".to_owned()),
                build_type: "release".to_owned(),
                toolchain: Some("clang".to_owned()),
                generator: Some("ninja".to_owned()),
                presets: vec!["common".to_owned()],
                commits: BTreeMap::from([("main".to_owned(), "abc".to_owned())]),
            },
            test: TestRecord {
                project: "khronos/deqp".to_owned(),
                branch: Some("main".to_owned()),
                commits: BTreeMap::from([("main".to_owned(), "def".to_owned())]),
            },
            runner: RunnerRecord {
                project: "fd.o/runner".to_owned(),
                branch: Some("main".to_owned()),
                commits: BTreeMap::from([("main".to_owned(), "123".to_owned())]),
            },
            content: ContentRecord {
                driver_icd: "sha256:1".to_owned(),
                driver_libs: "sha256:2".to_owned(),
                driver_config: "sha256:3".to_owned(),
                suite: "sha256:4".to_owned(),
                runner: "sha256:5".to_owned(),
            },
        }
    }

    #[test]
    fn run_toml_parses_back_and_keeps_env_and_argv_top_level() {
        let text = record().to_toml().unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["env"]["WEIRD KEY"].as_str(), Some("line\nnext"));
        assert_eq!(parsed["unset"][0].as_str(), Some("LD_PRELOAD"));
        assert_eq!(parsed["argv"][0].as_str(), Some("runner"));
        assert_eq!(parsed["driver"]["commits"]["main"].as_str(), Some("abc"));
        assert_eq!(parsed["device"]["id"].as_str(), Some("1002:150e"));
        let read_back = parse_run(text.as_bytes()).unwrap();
        assert_eq!(read_back.started, record().started);
        assert_eq!(read_back.env.len(), 2);
    }

    #[test]
    fn an_archive_starts_with_run_toml_and_keeps_the_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("results.csv"), "t,Pass,1\n").unwrap();
        fs::write(run.join("failures.csv"), "").unwrap();
        fs::write(run.join("failures-all.csv"), "t,ExpectedFail\n").unwrap();
        let archive = dir.path().join("a.tar.zst");
        write(&archive, &record(), &run).unwrap();

        let (name, bytes) = read_first(&archive).unwrap();
        assert_eq!(name, RUN_TOML);
        assert_eq!(parse_run(&bytes).unwrap().exit, 1);
        let mut failures = Vec::new();
        assert!(copy_member(&archive, "failures-all.csv", &mut failures).unwrap());
        assert_eq!(failures, b"t,ExpectedFail\n");
        let mut expectations = Vec::new();
        assert!(!copy_member(&archive, "expectations.json", &mut expectations).unwrap());
        assert!(expectations.is_empty());

        let extracted = dir.path().join("out");
        fs::create_dir_all(&extracted).unwrap();
        assert!(extract_baseline(&archive, &extracted).unwrap());
        assert_eq!(
            fs::read_to_string(extracted.join("baseline.csv")).unwrap(),
            "t,ExpectedFail\n"
        );
    }

    #[test]
    fn required_csvs_are_the_three_a_verdict_needs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!required_csvs_present(dir.path()));
        for name in REQUIRED_CSVS {
            fs::write(dir.path().join(name), "").unwrap();
        }
        assert!(required_csvs_present(dir.path()));
    }
}
