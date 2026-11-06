//! A run's outcome, read from the runner's `results.csv` against the baseline
//! the run used: the verdict, its counts, and the next run's baseline.
//!
//! The runner applies `--baseline` per test and writes the result as its own
//! status (deqp-runner's `RunnerStatus`): `Fail`, `Crash`, `Timeout` and
//! `Missing` are new failures, `ExpectedFail` is a failure the baseline already
//! had, and `UnexpectedImprovement(<status>)` is a baseline failure that no
//! longer fails that way. The verdict is counted from those rows and the
//! runner's exit status only has to agree with them, so every deqp-runner
//! version reads the same way.
//!
//! The next baseline is `failing.csv`: every test that failed, with the status
//! it actually had. The runner's own `failures-all.csv` cannot be it, because
//! it records `ExpectedFail` and `UnexpectedImprovement(Fail)` as statuses, and
//! deqp-runner's `with_baseline` matches a `Timeout` only against `Timeout` and
//! a `Fail` only against `Fail` or `ExpectedFail`: a persistent timeout would
//! alternate between expected and new, and a crash that became a failure would
//! come back as new.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, IntoInnerError, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Every result the runner produced, one `test,status,duration` line each.
const RESULTS: &str = "results.csv";

/// The failing tests by their own status: the next run's baseline.
pub const FAILING: &str = "failing.csv";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Regression,
    Improved,
    Skipped,
    Error,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Regression => "regression",
            Verdict::Improved => "improved",
            Verdict::Skipped => "skipped",
            Verdict::Error => "error",
        }
    }

    /// The `run` exit status this verdict asks for; an error outranks a
    /// regression.
    pub fn exit_code(self) -> u8 {
        match self {
            Verdict::Error => 2,
            Verdict::Regression => 1,
            Verdict::Pass | Verdict::Improved | Verdict::Skipped => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    /// Failures the baseline did not have.
    pub new: u32,
    /// Failures the baseline already had.
    pub carried: u32,
    /// Baseline failures that no longer fail the same way.
    pub improved: u32,
}

impl Counts {
    pub fn verdict(self) -> Verdict {
        if self.new > 0 {
            Verdict::Regression
        } else if self.improved > 0 {
            Verdict::Improved
        } else {
            Verdict::Pass
        }
    }
}

/// Read a finished run: refuse a runner that did not complete, count
/// `results.csv` against `baseline`, and write `failing.csv` beside it.
///
/// `exit` is the runner's exit code, or the negated signal that killed it.
pub fn evaluate(exit: i32, run_dir: &Path, baseline: Option<&Path>) -> Result<Counts> {
    if exit < 0 {
        bail!("runner was killed by signal {}", -exit);
    }
    if exit > 2 {
        bail!("runner exited with code {exit}");
    }
    let results = run_dir.join(RESULTS);
    if !results.is_file() {
        bail!("runner exited {exit} without writing {RESULTS}");
    }
    let expected = match baseline {
        Some(path) => read_baseline(path)?,
        None => HashMap::new(),
    };
    let counts = derive(&results, &expected, &run_dir.join(FAILING))?;
    // deqp-runner exits 0 exactly when every result is a success, and 1 (or 2
    // for improvements alone, since 0.24) otherwise.
    if (exit == 0) != (counts.new == 0 && counts.improved == 0) {
        bail!(
            "runner exited {exit}, but {RESULTS} holds {} new failures and {} improvements",
            counts.new,
            counts.improved
        );
    }
    Ok(counts)
}

fn derive(results: &Path, expected: &HashMap<String, String>, failing: &Path) -> Result<Counts> {
    let reader = BufReader::new(
        File::open(results).with_context(|| format!("opening {}", results.display()))?,
    );
    let mut out = BufWriter::new(
        File::create(failing).with_context(|| format!("creating {}", failing.display()))?,
    );
    let mut counts = Counts::default();
    for (index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading {}", results.display()))?;
        if line.is_empty() {
            continue;
        }
        let at = || format!("{} line {}", results.display(), index + 1);
        let mut fields = line.split(',');
        let (Some(test), Some(status)) = (fields.next(), fields.next()) else {
            bail!("{}: {line:?} is not test,status,duration", at());
        };
        let failing_as = match status {
            "Fail" | "Crash" | "Timeout" | "Missing" => {
                counts.new += 1;
                Some(status)
            }
            "ExpectedFail" => {
                counts.carried += 1;
                // The runner marks a test ExpectedFail only against an entry in
                // the baseline this run passed it.
                let status = expected.get(test).with_context(|| {
                    format!(
                        "{}: {test} is ExpectedFail, but the baseline does not list it",
                        at()
                    )
                })?;
                Some(status.as_str())
            }
            "UnexpectedImprovement(Fail)" => {
                counts.improved += 1;
                Some("Fail")
            }
            _ if status.starts_with("UnexpectedImprovement(") => {
                counts.improved += 1;
                None
            }
            "Pass" | "Skip" | "Warn" | "Flake" | "KnownFlake" => None,
            _ => bail!("{}: unknown runner status {status:?}", at()),
        };
        if let Some(status) = failing_as {
            writeln!(out, "{test},{status}")
                .with_context(|| format!("writing {}", failing.display()))?;
        }
    }
    out.into_inner()
        .map_err(IntoInnerError::into_error)
        .with_context(|| format!("writing {}", failing.display()))?;
    Ok(counts)
}

fn read_baseline(path: &Path) -> Result<HashMap<String, String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the baseline {}", path.display()))?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split_once(',')
                .map(|(test, status)| (test.to_owned(), status.to_owned()))
                .with_context(|| format!("{}: {line:?} is not test,status", path.display()))
        })
        .collect()
}

/// `regression  new=2 carried=3 improved=0`, or the verdict and why.
pub fn summary(
    verdict: Verdict,
    counts: Option<Counts>,
    detail: Option<&str>,
    compare_only: bool,
) -> String {
    let mut out = verdict.as_str().to_owned();
    if let Some(Counts {
        new,
        carried,
        improved,
    }) = counts
    {
        out.push_str(&format!(
            "  new={new} carried={carried} improved={improved}"
        ));
    }
    if let Some(detail) = detail {
        out.push_str("  ");
        out.push_str(&detail.replace('\n', "; "));
    }
    if compare_only {
        out.push_str("  (compare-only)");
    }
    out
}

/// One case's line in `run`'s report.
#[derive(Debug, Clone)]
pub struct CaseReport {
    pub case: String,
    pub id: Option<String>,
    pub verdict: Verdict,
    pub counts: Option<Counts>,
    pub detail: Option<String>,
    pub compare_only: bool,
}

impl CaseReport {
    pub fn error(case: &str, id: Option<String>, err: &anyhow::Error) -> Self {
        Self {
            case: case.to_owned(),
            id,
            verdict: Verdict::Error,
            counts: None,
            detail: Some(format!("{err:#}")),
            compare_only: false,
        }
    }

    pub fn skipped(case: &str, why: &str) -> Self {
        Self {
            case: case.to_owned(),
            id: None,
            verdict: Verdict::Skipped,
            counts: None,
            detail: Some(why.to_owned()),
            compare_only: false,
        }
    }
}

impl fmt::Display for CaseReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}  {}  {}",
            self.case,
            self.id.as_deref().unwrap_or("-"),
            summary(
                self.verdict,
                self.counts,
                self.detail.as_deref(),
                self.compare_only
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(results: &str, baseline: Option<&str>, exit: i32) -> (Result<Counts>, String) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(RESULTS), results).unwrap();
        let baseline_path = dir.path().join("baseline.csv");
        if let Some(text) = baseline {
            fs::write(&baseline_path, text).unwrap();
        }
        let counts = evaluate(exit, dir.path(), baseline.map(|_| baseline_path.as_path()));
        let failing = fs::read_to_string(dir.path().join(FAILING)).unwrap_or_default();
        (counts, failing)
    }

    #[test]
    fn a_first_run_counts_new_failures_and_keeps_their_status() {
        let (counts, failing) = run(
            "a,Pass,0.1\nb,Fail,0.2\nc,Timeout,300\nd,Crash,1\ne,Skip,0\nf,Missing,0\ng,Flake,1\n",
            None,
            1,
        );
        let counts = counts.unwrap();
        assert_eq!(
            counts,
            Counts {
                new: 4,
                carried: 0,
                improved: 0
            }
        );
        assert_eq!(counts.verdict(), Verdict::Regression);
        assert_eq!(failing, "b,Fail\nc,Timeout\nd,Crash\nf,Missing\n");
    }

    #[test]
    fn a_carried_failure_keeps_the_status_the_baseline_gave_it() {
        // A persistent timeout stays a Timeout in the next baseline, which the
        // runner matches against Timeout again; ExpectedFail would not match.
        let (counts, failing) = run(
            "b,ExpectedFail,0.2\nc,ExpectedFail,300\nd,Pass,0.1\n",
            Some("b,Fail\nc,Timeout\n"),
            0,
        );
        assert_eq!(
            counts.unwrap(),
            Counts {
                new: 0,
                carried: 2,
                improved: 0
            }
        );
        assert_eq!(failing, "b,Fail\nc,Timeout\n");
    }

    #[test]
    fn an_improvement_that_still_fails_stays_in_the_baseline_as_a_failure() {
        let (counts, failing) = run(
            "b,UnexpectedImprovement(Fail),0.2\nc,UnexpectedImprovement(Pass),0.1\n",
            Some("b,Crash\nc,Fail\n"),
            2,
        );
        let counts = counts.unwrap();
        assert_eq!(
            counts,
            Counts {
                new: 0,
                carried: 0,
                improved: 2
            }
        );
        assert_eq!(counts.verdict(), Verdict::Improved);
        assert_eq!(failing, "b,Fail\n");
        // deqp-runner before 0.24 exits 1 for improvements alone.
        assert!(
            run("c,UnexpectedImprovement(Pass),0.1\n", Some("c,Fail\n"), 1)
                .0
                .is_ok()
        );
    }

    #[test]
    fn an_incomplete_or_inconsistent_run_is_an_error() {
        let message = |results: &str, baseline: Option<&str>, exit: i32| {
            format!("{:#}", run(results, baseline, exit).0.unwrap_err())
        };
        assert!(message("a,Pass,0\n", None, -9).contains("signal 9"));
        assert!(message("a,Pass,0\n", None, 101).contains("code 101"));
        assert!(message("a,Fail,0\n", None, 0).contains("1 new failures"));
        assert!(message("a,Pass,0\n", None, 1).contains("0 new failures"));
        assert!(message("a,ExpectedFail,0\n", Some("b,Fail\n"), 0).contains("does not list it"));
        assert!(message("a,Bogus,0\n", None, 0).contains("unknown runner status"));

        let dir = tempfile::tempdir().unwrap();
        let err = evaluate(0, dir.path(), None).unwrap_err();
        assert!(
            format!("{err:#}").contains("without writing results.csv"),
            "{err:#}"
        );
    }

    #[test]
    fn a_report_line_names_the_case_id_verdict_and_counts() {
        let report = CaseReport {
            case: "radv-vk".to_owned(),
            id: Some("20261026T213012Z".to_owned()),
            verdict: Verdict::Regression,
            counts: Some(Counts {
                new: 2,
                carried: 3,
                improved: 0,
            }),
            detail: None,
            compare_only: true,
        };
        assert_eq!(
            report.to_string(),
            "radv-vk  20261026T213012Z  regression  new=2 carried=3 improved=0  (compare-only)"
        );
        let skipped = CaseReport::skipped("lvp-vk", "identity unchanged");
        assert_eq!(
            skipped.to_string(),
            "lvp-vk  -  skipped  identity unchanged"
        );
    }
}
