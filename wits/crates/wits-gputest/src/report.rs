//! The one-line-per-case report, and the two counts it carries.
//!
//! The runner's exit status is the verdict; the CSVs provide the counts.
//! `failures.csv` holds the unexpected results, including
//! `UnexpectedImprovement(...)` entries that are not failures; `failures-all.csv`
//! additionally holds `ExpectedFail` entries, which are the failures carried
//! from the baseline.

use std::path::Path;

use anyhow::Result;

use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Regression,
    Improved,
    Skipped,
    Error,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Regression => "regression",
            Verdict::Improved => "improved",
            Verdict::Skipped => "skipped",
            Verdict::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub new: u32,
    pub carried: u32,
}

#[derive(Debug, Clone)]
pub struct CaseReport {
    pub case: String,
    pub id: Option<String>,
    pub verdict: Verdict,
    pub counts: Counts,
    pub detail: Option<String>,
}

impl CaseReport {
    pub fn print(&self) {
        let id = self.id.as_deref().unwrap_or("-");
        let mut line = format!(
            "{}  {}  {}  new={} carried={}",
            self.case,
            id,
            self.verdict.as_str(),
            self.counts.new,
            self.counts.carried
        );
        if let Some(detail) = &self.detail {
            line.push_str("  ");
            line.push_str(detail);
        }
        println!("{line}");
    }
}

/// Count the unexpected failures and the carried expected failures.
pub fn counts(run_dir: &Path) -> Result<Counts> {
    let new = match util::read_text(&run_dir.join("failures.csv")) {
        Ok(text) => text
            .lines()
            .filter_map(status_of)
            .filter(|status| !status.starts_with("UnexpectedImprovement"))
            .count() as u32,
        Err(_) => 0,
    };
    let carried = match util::read_text(&run_dir.join("failures-all.csv")) {
        Ok(text) => text
            .lines()
            .filter_map(status_of)
            .filter(|status| *status == "ExpectedFail")
            .count() as u32,
        Err(_) => 0,
    };
    Ok(Counts { new, carried })
}

fn status_of(line: &str) -> Option<&str> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() {
        return None;
    }
    // The runner writes `test,status` for the failure files. A test name is a
    // dEQP/Piglit identifier and carries no comma; the second field is the
    // status.
    line.split_once(',').map(|(_, status)| status.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn new_counts_unexpected_failures_and_carried_counts_expected_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("failures.csv"),
            "a,Fail\nb,Crash\nc,UnexpectedImprovement(Pass)\nd,Missing\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("failures-all.csv"),
            "a,Fail\nb,Crash\nc,UnexpectedImprovement(Pass)\nd,Missing\ne,ExpectedFail\nf,ExpectedFail\n",
        )
        .unwrap();
        assert_eq!(counts(dir.path()).unwrap(), Counts { new: 3, carried: 2 });
    }

    #[test]
    fn a_missing_file_counts_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(counts(dir.path()).unwrap(), Counts::default());
    }

    #[test]
    fn a_report_line_names_the_case_id_verdict_and_counts() {
        let report = CaseReport {
            case: "radv-vk".to_owned(),
            id: Some("20261026T213012Z".to_owned()),
            verdict: Verdict::Regression,
            counts: Counts { new: 2, carried: 3 },
            detail: None,
        };
        let id = report.id.clone().unwrap();
        assert_eq!(
            format!(
                "{}  {}  {}  new={} carried={}",
                report.case,
                id,
                report.verdict.as_str(),
                report.counts.new,
                report.counts.carried
            ),
            "radv-vk  20261026T213012Z  regression  new=2 carried=3"
        );
    }
}
