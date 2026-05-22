//! Machine-readable test reports for `harn test`.
//!
//! User and conformance suites both feed the same writer so the JUnit
//! XML and `--json-out` payloads have a single source of truth. CI
//! systems get a uniform schema; performance audits get per-file and
//! per-test timing without scraping ANSI-coloured terminal output.
//!
//! The writers fail loudly: a missing or unwritable destination
//! returns an error so the CLI can exit non-zero rather than silently
//! succeed (issue #2146).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const USER_TEST_REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    Failed,
    TimedOut,
    Skipped,
}

impl TestOutcome {
    fn is_failure(self) -> bool {
        matches!(self, TestOutcome::Failed | TestOutcome::TimedOut)
    }

    fn is_skipped(self) -> bool {
        matches!(self, TestOutcome::Skipped)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TestCaseReport {
    pub name: String,
    pub file: String,
    pub classname: String,
    pub outcome: TestOutcome,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TestReportSummary {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub skipped: u64,
}

impl TestReportSummary {
    fn record(&mut self, outcome: TestOutcome) {
        self.total += 1;
        match outcome {
            TestOutcome::Passed => self.passed += 1,
            TestOutcome::Failed => self.failed += 1,
            TestOutcome::TimedOut => self.timed_out += 1,
            TestOutcome::Skipped => self.skipped += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub suite: String,
    pub root: Option<String>,
    pub duration_ms: u64,
    pub summary: TestReportSummary,
    pub cases: Vec<TestCaseReport>,
}

impl TestReport {
    pub fn new(suite: impl Into<String>, root: Option<&Path>) -> Self {
        Self {
            schema_version: USER_TEST_REPORT_SCHEMA_VERSION,
            suite: suite.into(),
            root: root.map(|p| p.display().to_string()),
            duration_ms: 0,
            summary: TestReportSummary::default(),
            cases: Vec::new(),
        }
    }

    pub fn push(&mut self, case: TestCaseReport) {
        self.summary.record(case.outcome);
        self.cases.push(case);
    }

    pub fn set_duration_ms(&mut self, duration_ms: u64) {
        self.duration_ms = duration_ms;
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn ensure_parent_writable(path: &Path) -> Result<(), String> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        if !parent.exists() {
            return Err(format!(
                "report directory does not exist: {}",
                parent.display()
            ));
        }
        if !parent.is_dir() {
            return Err(format!(
                "report directory is not a directory: {}",
                parent.display()
            ));
        }
    }
    Ok(())
}

pub fn write_junit(path: &str, report: &TestReport) -> Result<PathBuf, String> {
    let path_buf = PathBuf::from(path);
    ensure_parent_writable(&path_buf)?;

    let suite_time = report.duration_ms as f64 / 1000.0;
    let suite_name = xml_escape(&report.suite);
    let tests = report.summary.total;
    let failures = report.summary.failed + report.summary.timed_out;
    let skipped = report.summary.skipped;

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuites name=\"{suite_name}\" tests=\"{tests}\" failures=\"{failures}\" skipped=\"{skipped}\" time=\"{suite_time:.3}\">\n"
    ));
    xml.push_str(&format!(
        "  <testsuite name=\"{suite_name}\" tests=\"{tests}\" failures=\"{failures}\" skipped=\"{skipped}\" time=\"{suite_time:.3}\">\n"
    ));
    for case in &report.cases {
        let time = case.duration_ms as f64 / 1000.0;
        let escaped_name = xml_escape(&case.name);
        let escaped_classname = xml_escape(&case.classname);
        let escaped_file = xml_escape(&case.file);
        xml.push_str(&format!(
            "    <testcase name=\"{escaped_name}\" classname=\"{escaped_classname}\" file=\"{escaped_file}\" time=\"{time:.3}\""
        ));
        if case.outcome == TestOutcome::Passed {
            xml.push_str(" />\n");
            continue;
        }
        xml.push_str(">\n");
        let body = case.message.as_deref().unwrap_or_default();
        let escaped_body = xml_escape(body);
        if case.outcome.is_skipped() {
            xml.push_str(&format!("      <skipped message=\"{escaped_body}\" />\n"));
        } else if case.outcome.is_failure() {
            let kind = if matches!(case.outcome, TestOutcome::TimedOut) {
                "timeout"
            } else {
                "AssertionError"
            };
            xml.push_str(&format!(
                "      <failure type=\"{kind}\" message=\"test failed\">{escaped_body}</failure>\n"
            ));
        }
        xml.push_str("    </testcase>\n");
    }
    xml.push_str("  </testsuite>\n");
    xml.push_str("</testsuites>\n");

    fs::write(&path_buf, &xml).map_err(|error| {
        format!(
            "failed to write JUnit XML to {}: {error}",
            path_buf.display()
        )
    })?;
    Ok(path_buf)
}

pub fn write_json(path: &str, report: &TestReport) -> Result<PathBuf, String> {
    let path_buf = PathBuf::from(path);
    ensure_parent_writable(&path_buf)?;
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize test report JSON: {error}"))?;
    fs::write(&path_buf, rendered).map_err(|error| {
        format!(
            "failed to write JSON report to {}: {error}",
            path_buf.display()
        )
    })?;
    Ok(path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> TestReport {
        let mut report = TestReport::new("user", None);
        report.push(TestCaseReport {
            name: "test_alpha".into(),
            file: "suite/a.harn".into(),
            classname: "suite/a.harn".into(),
            outcome: TestOutcome::Passed,
            duration_ms: 12,
            message: None,
        });
        report.push(TestCaseReport {
            name: "test_beta".into(),
            file: "suite/b.harn".into(),
            classname: "suite/b.harn".into(),
            outcome: TestOutcome::Failed,
            duration_ms: 34,
            message: Some("expected 1 == 2".into()),
        });
        report.push(TestCaseReport {
            name: "test_gamma".into(),
            file: "suite/c.harn".into(),
            classname: "suite/c.harn".into(),
            outcome: TestOutcome::TimedOut,
            duration_ms: 30_000,
            message: Some("timed out after 30000ms".into()),
        });
        report.push(TestCaseReport {
            name: "test_delta".into(),
            file: "suite/d.harn".into(),
            classname: "suite/d.harn".into(),
            outcome: TestOutcome::Skipped,
            duration_ms: 0,
            message: Some("xfail: flaky".into()),
        });
        report.set_duration_ms(100);
        report
    }

    #[test]
    fn summary_counts_outcomes() {
        let report = sample_report();
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.timed_out, 1);
        assert_eq!(report.summary.skipped, 1);
    }

    #[test]
    fn write_junit_renders_failure_and_skip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.xml");
        let report = sample_report();
        write_junit(path.to_str().unwrap(), &report).unwrap();
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("<testsuites"));
        assert!(xml.contains(r#"tests="4" failures="2" skipped="1""#));
        assert!(xml.contains(r#"name="test_alpha""#));
        assert!(xml.contains(r#"<failure type="AssertionError""#));
        assert!(xml.contains(r#"<failure type="timeout""#));
        assert!(xml.contains("<skipped"));
    }

    #[test]
    fn write_json_round_trips_through_serde() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.json");
        let report = sample_report();
        write_json(path.to_str().unwrap(), &report).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schemaVersion"], USER_TEST_REPORT_SCHEMA_VERSION);
        assert_eq!(value["summary"]["total"], 4);
        assert_eq!(value["summary"]["passed"], 1);
        assert_eq!(value["summary"]["failed"], 1);
        assert_eq!(value["summary"]["timed_out"], 1);
        assert_eq!(value["summary"]["skipped"], 1);
        let cases = value["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[0]["outcome"], "passed");
        assert_eq!(cases[1]["outcome"], "failed");
        assert_eq!(cases[2]["outcome"], "timed_out");
        assert_eq!(cases[3]["outcome"], "skipped");
    }

    #[test]
    fn missing_parent_directory_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("does/not/exist/report.xml");
        let err = write_junit(path.to_str().unwrap(), &sample_report()).unwrap_err();
        assert!(
            err.contains("report directory does not exist"),
            "unexpected error: {err}"
        );
        let err = write_json(path.to_str().unwrap(), &sample_report()).unwrap_err();
        assert!(
            err.contains("report directory does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parent_must_be_a_directory() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("notadir");
        std::fs::write(&parent, "x").unwrap();
        let path = parent.join("report.xml");
        let err = write_junit(path.to_str().unwrap(), &sample_report()).unwrap_err();
        assert!(
            err.contains("is not a directory"),
            "unexpected error: {err}"
        );
    }
}
