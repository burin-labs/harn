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

use serde::{Deserialize, Serialize};

use crate::test_runner::{
    AggregateTimings, CostRegression, PhaseTimings, ShardPlan, TestTimeout, TestTimingSpan,
    TimingBaseline,
};
use crate::test_timing::DurationSummary;

pub const USER_TEST_REPORT_SCHEMA_VERSION: u32 = 4;

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
    pub timeout: Option<TestTimeout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<PhaseTimings>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timing_spans: Vec<TestTimingSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Everything the case wrote via `log`/`print`/`println`/etc, present
    /// regardless of outcome — a passing case's probes are as valid a
    /// reason to want this in a JSON/JUnit consumer as a failing case's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_output: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<DurationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AggregateTimings>,
    #[serde(rename = "timingEnvironment", skip_serializing_if = "Option::is_none")]
    pub timing_environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shard_plan: Option<ShardPlan>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cost_regressions: Vec<CostRegression>,
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
            timing: None,
            aggregate: None,
            timing_environment: None,
            shard_plan: None,
            cost_regressions: Vec::new(),
            summary: TestReportSummary::default(),
            cases: Vec::new(),
        }
    }

    pub fn set_execution_metrics(&mut self, timing: DurationSummary, aggregate: AggregateTimings) {
        self.timing = Some(timing);
        self.aggregate = Some(aggregate);
    }

    pub fn set_timing_policy(
        &mut self,
        environment: Option<String>,
        shard_plan: Option<ShardPlan>,
        cost_regressions: Vec<CostRegression>,
    ) {
        self.timing_environment = environment;
        self.shard_plan = shard_plan;
        self.cost_regressions = cost_regressions;
    }

    pub fn push(&mut self, case: TestCaseReport) {
        self.summary.record(case.outcome);
        self.cases.push(case);
    }

    pub fn set_duration_ms(&mut self, duration_ms: u64) {
        self.duration_ms = duration_ms;
    }
}

#[derive(Deserialize)]
struct BaselineReceipt {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    suite: String,
    #[serde(rename = "timingEnvironment")]
    timing_environment: Option<String>,
    shard_plan: Option<serde_json::Value>,
    #[serde(default)]
    cost_regressions: Vec<serde_json::Value>,
    summary: BaselineSummary,
    cases: Vec<BaselineCase>,
}

#[derive(Deserialize)]
struct BaselineSummary {
    failed: u64,
    timed_out: u64,
}

#[derive(Deserialize)]
struct BaselineCase {
    file: String,
    name: String,
    outcome: String,
    phases: Option<BaselinePhases>,
}

#[derive(Deserialize)]
struct BaselinePhases {
    execute_ms: u64,
}

pub fn load_timing_baseline(
    path: &Path,
    suite_root: &Path,
    environment: &str,
    max_regression_percent: u64,
) -> Result<TimingBaseline, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read timing baseline {}: {error}", path.display()))?;
    let receipt: BaselineReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "timing baseline {} is not a Harn user-test receipt: {error}",
            path.display()
        )
    })?;
    if receipt.schema_version != USER_TEST_REPORT_SCHEMA_VERSION || receipt.suite != "user" {
        return Err(format!(
            "timing baseline must be a Harn user-test receipt schema v{} (found suite {:?}, schema v{})",
            USER_TEST_REPORT_SCHEMA_VERSION, receipt.suite, receipt.schema_version
        ));
    }
    if receipt.timing_environment.as_deref() != Some(environment) {
        return Err(format!(
            "timing baseline environment mismatch: receipt has {:?}, enforcing run requires {:?}",
            receipt.timing_environment, environment
        ));
    }
    if receipt.shard_plan.is_some() {
        return Err("timing baseline must come from a complete unsharded run".to_string());
    }
    if receipt.summary.failed > 0
        || receipt.summary.timed_out > 0
        || !receipt.cost_regressions.is_empty()
    {
        return Err(
            "timing baseline must come from a passing run without cost regressions".to_string(),
        );
    }
    let suite_root = suite_root.canonicalize().map_err(|error| {
        format!(
            "failed to resolve timing baseline suite root {}: {error}",
            suite_root.display()
        )
    })?;
    let mut weights_ms = std::collections::BTreeMap::new();
    for case in receipt.cases {
        let relative = Path::new(&case.file);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "timing baseline case path must stay beneath the suite root: {}",
                case.file
            ));
        }
        let Some(phases) = case.phases else {
            if case.outcome == "skipped" {
                continue;
            }
            return Err(format!(
                "timing baseline case {}::{} has no executed-case timing receipt",
                case.file, case.name
            ));
        };
        let file = suite_root.join(relative).canonicalize().map_err(|error| {
            format!(
                "timing baseline case no longer exists: {}::{} ({error})",
                case.file, case.name
            )
        })?;
        if !file.starts_with(&suite_root) {
            return Err(format!(
                "timing baseline case resolves outside the suite root: {}",
                case.file
            ));
        }
        let key = format!("{}::{}", file.display(), case.name);
        if weights_ms
            .insert(key.clone(), phases.execute_ms.max(1))
            .is_some()
        {
            return Err(format!("timing baseline contains duplicate case: {key}"));
        }
    }
    Ok(TimingBaseline {
        environment: environment.to_string(),
        weights_ms,
        max_regression_percent,
    })
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
    let suite_name = crate::format::escape_html(&report.suite);
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
        let escaped_name = crate::format::escape_html(&case.name);
        let escaped_classname = crate::format::escape_html(&case.classname);
        let escaped_file = crate::format::escape_html(&case.file);
        xml.push_str(&format!(
            "    <testcase name=\"{escaped_name}\" classname=\"{escaped_classname}\" file=\"{escaped_file}\" time=\"{time:.3}\""
        ));
        if case.outcome == TestOutcome::Passed {
            xml.push_str(" />\n");
            continue;
        }
        xml.push_str(">\n");
        let body = case.message.as_deref().unwrap_or_default();
        let escaped_body = crate::format::escape_html(body);
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
            if let Some(output) = &case.captured_output {
                let escaped_output = crate::format::escape_html(output);
                xml.push_str(&format!(
                    "      <system-out>{escaped_output}</system-out>\n"
                ));
            }
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
            timeout: None,
            phases: None,
            timing_spans: vec![TestTimingSpan {
                name: "sweep.expensive_case".into(),
                duration_ms: 9,
                attributes: std::collections::BTreeMap::from([(
                    "case_id".into(),
                    serde_json::json!("alpha"),
                )]),
            }],
            message: None,
            captured_output: None,
        });
        report.push(TestCaseReport {
            name: "test_beta".into(),
            file: "suite/b.harn".into(),
            classname: "suite/b.harn".into(),
            outcome: TestOutcome::Failed,
            duration_ms: 34,
            timeout: None,
            phases: None,
            timing_spans: Vec::new(),
            message: Some("expected 1 == 2".into()),
            captured_output: Some("[harn] probing state\n".into()),
        });
        report.push(TestCaseReport {
            name: "test_gamma".into(),
            file: "suite/c.harn".into(),
            classname: "suite/c.harn".into(),
            outcome: TestOutcome::TimedOut,
            duration_ms: 30_000,
            timeout: Some(TestTimeout {
                phase: crate::test_runner::TestPhase::Execute,
                limit_ms: 30_000,
            }),
            phases: Some(PhaseTimings {
                execute_ms: 30_000,
                ..PhaseTimings::default()
            }),
            timing_spans: Vec::new(),
            message: Some("timed out after 30000ms".into()),
            captured_output: None,
        });
        report.push(TestCaseReport {
            name: "test_delta".into(),
            file: "suite/d.harn".into(),
            classname: "suite/d.harn".into(),
            outcome: TestOutcome::Skipped,
            duration_ms: 0,
            timeout: None,
            phases: None,
            timing_spans: Vec::new(),
            message: Some("xfail: flaky".into()),
            captured_output: None,
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
        assert!(xml.contains("<system-out>[harn] probing state\n</system-out>"));
        // The timed-out case carries no captured output in this fixture;
        // its `<testcase>` must not grow a stray `<system-out>`.
        assert_eq!(xml.matches("<system-out>").count(), 1);
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
        assert_eq!(cases[1]["captured_output"], "[harn] probing state\n");
        assert_eq!(cases[2]["outcome"], "timed_out");
        assert_eq!(cases[2]["timeout"]["phase"], "execute");
        assert_eq!(cases[2]["timeout"]["limit_ms"], 30_000);
        assert_eq!(cases[2]["phases"]["execute_ms"], 30_000);
        assert_eq!(cases[0]["timing_spans"][0]["name"], "sweep.expensive_case");
        assert_eq!(cases[3]["outcome"], "skipped");
    }

    #[test]
    fn timing_baseline_requires_current_harn_receipt_and_matching_environment() {
        let temp = tempfile::TempDir::new().unwrap();
        let suite = temp.path().join("suite");
        fs::create_dir_all(&suite).unwrap();
        fs::write(suite.join("a.harn"), "@test pipeline test_alpha(task) {}").unwrap();
        let receipt = temp.path().join("receipt.json");
        fs::write(
            &receipt,
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": USER_TEST_REPORT_SCHEMA_VERSION,
                "suite": "user",
                "timingEnvironment": "github-linux-x64",
                "summary": {"failed": 0, "timed_out": 0},
                "cases": [{
                    "file": "a.harn",
                    "name": "test_alpha",
                    "outcome": "passed",
                    "duration_ms": 120,
                    "phases": {"execute_ms": 100}
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let baseline = load_timing_baseline(&receipt, &suite, "github-linux-x64", 25).unwrap();
        assert_eq!(baseline.weights_ms.len(), 1);
        let mismatch = load_timing_baseline(&receipt, &suite, "local", 25).unwrap_err();
        assert!(mismatch.contains("environment mismatch"));

        fs::write(&receipt, br#"{"tests/a.harn::test_alpha":120}"#).unwrap();
        let untyped = load_timing_baseline(&receipt, &suite, "github-linux-x64", 25).unwrap_err();
        assert!(untyped.contains("not a Harn user-test receipt"));
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
