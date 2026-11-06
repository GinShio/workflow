//! The run archive: one `tar.zst` per run that reached the runner.
//!
//! `run.toml` is the first member, so `history` and `list` read it and stop;
//! the CSVs behind it are opened only when a person asks, or when a later run
//! takes this one's `failing.csv` as its baseline.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::content::Content;
use crate::device::Device;
use crate::report::{self, Counts, Verdict};
use crate::stamp::Source;

pub const RUN_TOML: &str = "run.toml";

/// The members kept after `run.toml`, whichever exist: the evidence every
/// runner in the deqp-runner project writes (`failures-all.csv` only since
/// 0.24), and the baseline gputest derived for the next run.
const MEMBERS: &[&str] = &[
    "results.csv",
    "failures.csv",
    "failures-all.csv",
    "expectations.json",
    report::FAILING,
];

/// `run.toml`: what ran, what it was compared with, what it found, and the
/// content and provenance behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// The run id: the UTC second the run started.
    pub started: String,
    /// Wall-clock seconds the runner took.
    pub duration: u64,
    /// The runner's exit code, or the negated signal that killed it.
    pub exit: i32,
    pub verdict: Verdict,
    /// Why the run is an `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The archive this run compared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
    /// Profile flags chose another build than the case's own, so the run left
    /// the case's state alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compare_only: bool,
    pub argv: Vec<String>,
    /// Names the composed environment removed from the caller's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unset: Vec<String>,
    /// Present when the run completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<Counts>,
    /// What the composed environment set, relative to the caller's.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub device: Device,
    pub driver: BuildRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerRecord>,
    pub suite: Source,
    pub runner: Source,
    pub content: Content,
}

/// A build the run used: the project, the checkout it came from and the build
/// tree whose environment the run used. The commits are the checkout's,
/// recorded as provenance; the content hashes say what was actually loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRecord {
    pub project: String,
    pub focus: String,
    /// `None` for a detached build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub build_type: String,
    pub build_dir: PathBuf,
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
}

/// The case's layer. It has a build of its own only when it comes from
/// another project than the driver's; otherwise it ran from the driver's.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerRecord {
    pub name: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildRecord>,
}

impl RunRecord {
    /// The verdict and its counts, or why it is an error.
    pub fn summary(&self) -> String {
        report::summary(
            self.verdict,
            self.counts,
            self.detail.as_deref(),
            self.compare_only,
        )
    }

    /// The driver commit a history line shows: the focus repo's if recorded,
    /// else the first one there is.
    pub fn driver_commit(&self) -> Option<&str> {
        self.driver
            .commits
            .get(&self.driver.focus)
            .or_else(|| self.driver.commits.values().next())
            .map(String::as_str)
    }
}

/// Write the archive: `run.toml` first, then every member `run_dir` holds. The
/// file is synced before this returns, because the store renames it into place
/// and a crash must leave either no archive or a whole one.
pub fn write(path: &Path, record: &RunRecord, run_dir: &Path) -> Result<()> {
    let text = toml::to_string(record).context("serializing run.toml")?;
    let file =
        File::create(path).with_context(|| format!("creating archive {}", path.display()))?;
    let encoder = zstd::stream::write::Encoder::new(file, 0)
        .with_context(|| format!("starting the zstd stream for {}", path.display()))?;
    let mut builder = tar::Builder::new(encoder);

    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, RUN_TOML, text.as_bytes())
        .context("writing run.toml as the first archive member")?;
    for name in MEMBERS {
        let member = run_dir.join(name);
        if member.is_file() {
            builder
                .append_path_with_name(&member, name)
                .with_context(|| format!("adding {} to the archive", member.display()))?;
        }
    }

    let file = builder
        .into_inner()
        .context("finishing the archive")?
        .finish()
        .context("finishing the zstd stream")?;
    file.sync_all()
        .with_context(|| format!("syncing archive {}", path.display()))
}

/// The first member's raw bytes, which must be `run.toml`. `show` prints it as
/// written; everything else parses it with [`read_run`].
pub fn read_first(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("opening archive {}", path.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .with_context(|| format!("starting the zstd stream for {}", path.display()))?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive
        .entries()
        .with_context(|| format!("reading archive {}", path.display()))?;
    let mut entry = entries
        .next()
        .transpose()
        .with_context(|| format!("reading archive {}", path.display()))?
        .with_context(|| format!("archive {} has no members", path.display()))?;
    let name = entry
        .path()
        .with_context(|| format!("reading the first member's name in {}", path.display()))?
        .into_owned();
    if name != Path::new(RUN_TOML) {
        bail!(
            "archive {} begins with {}, not {RUN_TOML}",
            path.display(),
            name.display()
        );
    }
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {RUN_TOML} from {}", path.display()))?;
    Ok(bytes)
}

/// One archive's `run.toml`, without touching the CSVs behind it.
pub fn read_run(path: &Path) -> Result<RunRecord> {
    let bytes = read_first(path)?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("{RUN_TOML} in {} is not UTF-8", path.display()))?;
    toml::from_str(text).with_context(|| format!("reading {RUN_TOML} in {}", path.display()))
}

/// Stream one named member through `consume`. Returns false when the archive
/// does not carry it.
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
        let mut entry = entry.with_context(|| format!("reading archive {}", archive.display()))?;
        let found = entry
            .path()
            .with_context(|| format!("reading a member name in {}", archive.display()))?
            == Path::new(member);
        if found {
            consume(&mut entry)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copy one named member to `writer`. Returns false when it is absent.
pub fn copy_member(archive: &Path, member: &str, writer: &mut impl std::io::Write) -> Result<bool> {
    with_member(archive, member, |reader| {
        std::io::copy(reader, writer)
            .with_context(|| format!("copying {member} out of {}", archive.display()))?;
        Ok(())
    })
}

/// Extract one named member to `destination`. Returns false when it is absent.
pub fn extract(archive: &Path, member: &str, destination: &Path) -> Result<bool> {
    with_member(archive, member, |reader| {
        let mut file = File::create(destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        std::io::copy(reader, &mut file)
            .with_context(|| format!("writing {}", destination.display()))?;
        Ok(())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    pub(crate) fn record() -> RunRecord {
        RunRecord {
            started: "20261026T213012Z".to_owned(),
            duration: 812,
            exit: 1,
            verdict: Verdict::Regression,
            detail: None,
            baseline: Some("20261023T213012Z".to_owned()),
            compare_only: false,
            argv: vec!["runner".to_owned(), "--x".to_owned()],
            unset: vec!["LD_PRELOAD".to_owned()],
            counts: Some(Counts {
                new: 2,
                carried: 3,
                improved: 0,
            }),
            env: BTreeMap::from([
                ("VK_DRIVER_FILES".to_owned(), "/x/radeon.json".to_owned()),
                ("WEIRD KEY".to_owned(), "line\nnext".to_owned()),
            ]),
            device: Device {
                pci: "0000:c4:00.0".to_owned(),
                id: "1002:150e".to_owned(),
                drm: "amdgpu".to_owned(),
                kernel: "7.2.11".to_owned(),
            },
            driver: BuildRecord {
                project: "vendor/vulkan".to_owned(),
                focus: "main".to_owned(),
                branch: Some("main".to_owned()),
                build_type: "release".to_owned(),
                build_dir: PathBuf::from("/src/vulkan/_build/main-release"),
                commits: BTreeMap::from([("main".to_owned(), "abc".to_owned())]),
            },
            layer: Some(LayerRecord {
                name: "zink".to_owned(),
                project: "fd.o/mesa".to_owned(),
                build: Some(BuildRecord {
                    project: "fd.o/mesa".to_owned(),
                    focus: "main".to_owned(),
                    branch: Some("main".to_owned()),
                    build_type: "release".to_owned(),
                    build_dir: PathBuf::from("/src/mesa/_build/main-release"),
                    commits: BTreeMap::new(),
                }),
            }),
            suite: Source {
                project: "khronos/deqp".to_owned(),
                branch: "main".to_owned(),
                commits: BTreeMap::from([("main".to_owned(), "def".to_owned())]),
            },
            runner: Source {
                project: "fd.o/runner".to_owned(),
                branch: "main".to_owned(),
                commits: BTreeMap::new(),
            },
            content: Content {
                identity: "sha256:0".to_owned(),
                suite: "sha256:1".to_owned(),
                runner: "sha256:2".to_owned(),
                driver_icd: "sha256:3".to_owned(),
                driver_libs: "sha256:4".to_owned(),
                driver_config: "sha256:5".to_owned(),
                layer_libs: "sha256:6".to_owned(),
                layer_config: "sha256:7".to_owned(),
                argv: "sha256:8".to_owned(),
                env: "sha256:9".to_owned(),
            },
        }
    }

    #[test]
    fn an_archive_starts_with_run_toml_and_keeps_the_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("results.csv"), "t,Fail,1\n").unwrap();
        fs::write(run.join("failures.csv"), "t,Fail\n").unwrap();
        fs::write(run.join(report::FAILING), "t,Fail\n").unwrap();
        fs::write(run.join("unrelated.txt"), "not evidence").unwrap();
        let archive = dir.path().join("a.tar.zst");
        write(&archive, &record(), &run).unwrap();

        let read_back = read_run(&archive).unwrap();
        assert_eq!(read_back.verdict, Verdict::Regression);
        assert_eq!(read_back.counts, record().counts);
        assert_eq!(read_back.env["WEIRD KEY"], "line\nnext");
        assert_eq!(read_back.content, record().content);
        let layer = read_back.layer.as_ref().unwrap();
        assert_eq!(layer.name, "zink");
        assert_eq!(
            layer.build.as_ref().unwrap().build_dir,
            PathBuf::from("/src/mesa/_build/main-release")
        );
        assert_eq!(
            read_back.summary(),
            "regression  new=2 carried=3 improved=0"
        );

        let mut failures = Vec::new();
        assert!(copy_member(&archive, "failures.csv", &mut failures).unwrap());
        assert_eq!(failures, b"t,Fail\n");
        assert!(!copy_member(&archive, "expectations.json", &mut Vec::new()).unwrap());
        assert!(!copy_member(&archive, "unrelated.txt", &mut Vec::new()).unwrap());

        let baseline = dir.path().join("baseline.csv");
        assert!(extract(&archive, report::FAILING, &baseline).unwrap());
        assert_eq!(fs::read_to_string(baseline).unwrap(), "t,Fail\n");
    }
}
