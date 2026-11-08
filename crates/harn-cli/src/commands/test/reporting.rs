//! User-test report conversion and terminal timing output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::test_report::{TestCaseReport, TestOutcome, TestReport};
use crate::test_runner;
use crate::test_timing::DurationSummary;

use super::{logical_path, UserTestOutputOptions};

pub(crate) const CONFORMANCE_TEST_SCHEMA_VERSION: u32 = 3;

pub(super) fn extract_diagnostic_codes(message: &str) -> Vec<String> {
    let re = Regex::new(r"\bHARN-[A-Z0-9]+(?:-[A-Z0-9]+)*-[0-9]{3}\b")
        .expect("diagnostic code regex compiles");
    let mut codes = BTreeSet::new();
    for capture in re.find_iter(message) {
        codes.insert(capture.as_str().to_string());
    }
    codes.into_iter().collect()
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConformanceJsonOutcome {
    Pass,
    Fail,
    XfailExpected,
    XfailUnexpectedPass,
    Skipped,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct ConformanceJsonResult {
    pub(super) name: String,
    pub(super) outcome: ConformanceJsonOutcome,
    pub(super) duration_ms: u64,
    pub(super) message: Option<String>,
    pub(super) diagnostic_codes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct ConformanceJsonSummary {
    pub(super) pass: u64,
    pub(super) fail: u64,
    pub(super) xfail_expected: u64,
    pub(super) xfail_unexpected_pass: u64,
    pub(super) skipped: u64,
}

impl ConformanceJsonSummary {
    pub(super) fn record(&mut self, outcome: ConformanceJsonOutcome) {
        match outcome {
            ConformanceJsonOutcome::Pass => self.pass += 1,
            ConformanceJsonOutcome::Fail => self.fail += 1,
            ConformanceJsonOutcome::XfailExpected => self.xfail_expected += 1,
            ConformanceJsonOutcome::XfailUnexpectedPass => self.xfail_unexpected_pass += 1,
            ConformanceJsonOutcome::Skipped => self.skipped += 1,
        }
    }

    pub(super) fn is_success(&self) -> bool {
        self.fail == 0 && self.xfail_unexpected_pass == 0
    }

    /// Whether any test actually ran.
    ///
    /// [`is_success`] cannot answer this: it is a "nothing failed" test, and
    /// an all-zero summary satisfies it trivially. At the exit-code boundary
    /// that makes "ran nothing" indistinguishable from "passed everything",
    /// so every consumer gating on the exit status — a CI step, a script, an
    /// author's shell loop — reads a vacuous run as a green one.
    ///
    /// [`is_success`]: Self::is_success
    pub(super) fn ran_anything(&self) -> bool {
        self.pass + self.fail + self.xfail_expected + self.xfail_unexpected_pass + self.skipped > 0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct ConformanceJsonReport {
    #[serde(rename = "snapshotKey")]
    pub(super) snapshot_key: String,
    pub(super) results: Vec<ConformanceJsonResult>,
    pub(super) summary: ConformanceJsonSummary,
    pub(super) timing: DurationSummary,
}

impl ConformanceJsonReport {
    pub(super) fn new(
        snapshot_key: String,
        results: Vec<ConformanceJsonResult>,
        summary: ConformanceJsonSummary,
    ) -> Self {
        let timing = DurationSummary::from_samples(
            &results
                .iter()
                .map(|result| result.duration_ms)
                .collect::<Vec<_>>(),
        );
        Self {
            snapshot_key,
            results,
            summary,
            timing,
        }
    }
}

pub(super) fn print_per_test_timing(report: &TestReport) {
    let timing = DurationSummary::from_samples(
        &report
            .cases
            .iter()
            .map(|case| case.duration_ms)
            .collect::<Vec<_>>(),
    );
    let (Some(avg), Some(p50), Some(p90), Some(p95), Some(p99)) = (
        timing.average_ms,
        timing.p50_ms,
        timing.p90_ms,
        timing.p95_ms,
        timing.p99_ms,
    ) else {
        return;
    };
    println!(
        "{}",
        crate::localization::formatted!(
            "Per-test: avg={avg} ms  p50={p50} ms  p90={p90} ms  p95={p95} ms  p99={p99} ms",
            avg = avg,
            p50 = p50,
            p90 = p90,
            p95 = p95,
            p99 = p99
        )
    );

    let mut by_time: Vec<&TestCaseReport> = report.cases.iter().collect();
    by_time.sort_by_key(|case| std::cmp::Reverse(case.duration_ms));
    let top_n = by_time.len().min(10);
    if top_n > 0 {
        println!();
        println!(
            "{}",
            crate::localization::formatted!("Slowest tests: {top_n}", top_n = top_n)
        );
        for case in &by_time[..top_n] {
            println!(
                "{}",
                crate::localization::formatted!("  {:>6} ms  {}", case.duration_ms, case.name)
            );
        }
    }
}

pub(super) fn user_test_report_from_summary(
    suite_root: &Path,
    summary: &test_runner::TestSummary,
) -> TestReport {
    let mut report = TestReport::new("user", Some(suite_root));
    for result in &summary.results {
        let outcome = user_test_outcome(result);
        let relative = report_relative_path(suite_root, &result.file);
        report.push(TestCaseReport {
            name: result.name.clone(),
            file: relative.clone(),
            classname: relative,
            outcome,
            duration_ms: result.duration_ms,
            timeout: result.timeout,
            phases: result.phases,
            work: result.work.as_deref().copied(),
            timing_spans: result.timing_spans.clone(),
            message: result.skip_reason.clone().or_else(|| result.error.clone()),
            captured_output: result.captured_output.clone(),
        });
    }
    report.set_duration_ms(summary.duration_ms);
    report.set_execution_metrics(summary.timing, summary.aggregate);
    let mut shard_plan = summary.shard_plan.clone();
    if let Some(matrix) = shard_plan.as_mut().and_then(|plan| plan.matrix.as_mut()) {
        matrix.source_confirmed = matrix.source.is_some()
            && matrix.source == crate::test_shard_coverage::clean_source(suite_root);
        for case in &mut matrix.cases {
            case.file = report_relative_path(suite_root, &case.file);
        }
        // Wire order follows portable relative paths, not a host's absolute
        // paths or separator byte, so identical plans hash identically.
        matrix
            .cases
            .sort_by(|a, b| (&a.file, &a.name).cmp(&(&b.file, &b.name)));
    }
    if let Some(dominant) = shard_plan
        .as_mut()
        .and_then(|plan| plan.dominant_case.as_mut())
    {
        dominant.file = report_relative_path(suite_root, &dominant.file);
    }
    let mut cost_regressions = summary.cost_regressions.clone();
    for regression in &mut cost_regressions {
        regression.file = report_relative_path(suite_root, &regression.file);
    }
    report.set_timing_policy(
        summary.timing_environment.clone(),
        shard_plan,
        cost_regressions,
    );
    report
}

fn report_relative_path(suite_root: &Path, file: &str) -> String {
    let file_path = PathBuf::from(file);
    file_path
        .strip_prefix(suite_root)
        .ok()
        .map(logical_path)
        .unwrap_or_else(|| file.to_string())
}

fn user_test_outcome(result: &test_runner::TestResult) -> TestOutcome {
    if result.passed {
        TestOutcome::Passed
    } else if result.skip_reason.is_some() {
        TestOutcome::Skipped
    } else if result.timeout.is_some() {
        TestOutcome::TimedOut
    } else {
        TestOutcome::Failed
    }
}

#[derive(Clone, Copy)]
pub(super) struct TestConsole {
    terminal: bool,
    verbose: bool,
}

impl TestConsole {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            terminal: std::io::stderr().is_terminal(),
            verbose,
        }
    }

    pub(super) fn progress(self, message: fmt::Arguments<'_>) {
        if self.terminal {
            eprintln!("{message}");
        }
    }

    pub(super) fn case(
        self,
        outcome: TestOutcome,
        label: fmt::Arguments<'_>,
        message: Option<&str>,
        captured: Option<&str>,
    ) {
        let (status, color) = match outcome {
            TestOutcome::Passed if !self.verbose => return,
            TestOutcome::Passed => ("PASS", 32),
            TestOutcome::Skipped => ("SKIP", 33),
            TestOutcome::TimedOut => ("TIMEOUT", 31),
            TestOutcome::Failed => ("FAIL", 31),
        };
        if self.terminal {
            eprintln!("  \x1b[{color}m{status}\x1b[0m  {label}");
        } else {
            eprintln!("  {status}  {label}");
        }
        let message = match outcome {
            TestOutcome::Failed | TestOutcome::TimedOut => {
                message.or(Some("failed without diagnostic message"))
            }
            _ => message,
        };
        if let Some(message) = message {
            for line in message.lines() {
                eprintln!("        {line}");
            }
        }
        if let Some(output) = captured {
            eprintln!(
                "{}",
                crate::localization::formatted!("        captured output:")
            );
            for line in output.lines() {
                eprintln!("          {line}");
            }
        }
    }
}

pub(super) fn user_test_progress(verbose: bool) -> test_runner::TestRunProgress {
    let console = TestConsole::new(verbose);
    std::sync::Arc::new(move |event| match event {
        test_runner::TestRunEvent::SuiteDiscovered {
            total_tests,
            total_files,
            parallel,
            workers,
        } => {
            if total_tests > 0 {
                let mode = if parallel { "dynamic" } else { "sequential" };
                console.progress(format_args!(
                    "{}",
                    crate::localization::formatted!(
                        "Running tests: {}, files: {}, workers: {} ({mode} scheduling)\n",
                        total_tests,
                        total_files,
                        workers,
                        mode = mode
                    )
                ));
            }
        }
        test_runner::TestRunEvent::LargeSequentialSuite {
            total_tests,
            total_files,
        } => {
            cli_diagnostic!(
                "warning: large suite discovered ({total_tests} tests across {total_files} files); running sequentially. Use `--parallel` to enable the bounded worker pool."
            );
        }
        test_runner::TestRunEvent::TestStarted {
            name,
            file,
            test_index,
            total_tests,
        } => {
            if verbose {
                console.progress(format_args!(
                    "{}",
                    crate::localization::formatted!(
                        "    RUN   {name} [{file}] ({test_index}/{total_tests})",
                        file = file,
                        name = name,
                        test_index = test_index,
                        total_tests = total_tests
                    )
                ));
            }
        }
        test_runner::TestRunEvent::TestFinished(_) => {}
    })
}

pub(super) fn print_test_results(
    summary: &test_runner::TestSummary,
    options: UserTestOutputOptions,
) {
    let console = TestConsole::new(options.verbose);
    for result in &summary.results {
        console.case(
            user_test_outcome(result),
            format_args!(
                "{} [{}] ({} ms)",
                result.name, result.file, result.duration_ms
            ),
            result.skip_reason.as_deref().or(result.error.as_deref()),
            result.captured_output.as_deref(),
        );
    }

    if summary.failed > 0 {
        println!(
            "{} passed, {} failed, {} skipped, {} total ({} ms)",
            summary.passed, summary.failed, summary.skipped, summary.total, summary.duration_ms
        );
    } else if summary.total == 0 {
        println!(
            "{}",
            crate::localization::formatted!("No test pipelines found")
        );
    } else {
        println!(
            "{} passed, {} skipped, {} total ({} ms)",
            summary.passed, summary.skipped, summary.total, summary.duration_ms
        );
    }

    match (summary.timing.p50_ms, summary.timing.p90_ms) {
        (Some(p50), Some(p90)) => println!(
            "{}",
            crate::localization::formatted!(
                "Latency: p50={p50} ms  p90={p90} ms",
                p50 = p50,
                p90 = p90
            )
        ),
        _ => println!(
            "{}",
            crate::localization::formatted!("Latency: p50=n/a  p90=n/a (0 samples)")
        ),
    }

    if options.timing {
        print_user_test_timing(summary);
    }
}

pub(super) fn print_conformance_results(
    report: &ConformanceJsonReport,
    verbose: bool,
    timing: Option<&TestReport>,
) {
    let console = TestConsole::new(verbose);
    for result in &report.results {
        let outcome = match result.outcome {
            ConformanceJsonOutcome::Pass => TestOutcome::Passed,
            ConformanceJsonOutcome::Skipped | ConformanceJsonOutcome::XfailExpected => {
                TestOutcome::Skipped
            }
            ConformanceJsonOutcome::Fail | ConformanceJsonOutcome::XfailUnexpectedPass => {
                TestOutcome::Failed
            }
        };
        console.case(
            outcome,
            format_args!("{} ({} ms)", result.name, result.duration_ms),
            result.message.as_deref(),
            None,
        );
    }
    let passed = report.summary.pass;
    let failed = report.summary.fail + report.summary.xfail_unexpected_pass;
    let skipped = report.summary.skipped + report.summary.xfail_expected;
    let total = passed + failed + skipped;
    println!("{passed} passed, {failed} failed, {skipped} skipped, {total} total");
    if let Some(timing) = timing {
        println!();
        println!(
            "{}",
            crate::localization::formatted!("Total time: {} ms", timing.duration_ms)
        );
        print_per_test_timing(timing);
    }
}

fn print_user_test_timing(summary: &test_runner::TestSummary) {
    if summary.total == 0 {
        return;
    }

    println!();
    println!(
        "{}",
        crate::localization::formatted!("Total time: {} ms", summary.duration_ms)
    );
    if let (Some(avg), Some(p95), Some(p99)) = (
        summary.timing.average_ms,
        summary.timing.p95_ms,
        summary.timing.p99_ms,
    ) {
        println!(
            "{}",
            crate::localization::formatted!(
                "Per-test detail: avg={avg} ms  p95={p95} ms  p99={p99} ms",
                avg = avg,
                p95 = p95,
                p99 = p99
            )
        );
    }

    let mut by_test = summary.results.iter().collect::<Vec<_>>();
    by_test.sort_by_key(|result| std::cmp::Reverse(result.duration_ms));
    let top_test_count = by_test.len().min(10);
    if top_test_count > 0 {
        println!();
        println!(
            "{}",
            crate::localization::formatted!(
                "Slowest tests: {top_test_count}",
                top_test_count = top_test_count
            )
        );
        for result in &by_test[..top_test_count] {
            println!(
                "{}",
                crate::localization::formatted!(
                    "  {:>6} ms  {} [{}]",
                    result.duration_ms,
                    result.name,
                    result.file
                )
            );
        }
    }

    let mut file_totals = BTreeMap::<&str, u64>::new();
    for result in &summary.results {
        *file_totals.entry(result.file.as_str()).or_default() += result.duration_ms;
    }
    let mut by_file = file_totals.into_iter().collect::<Vec<_>>();
    by_file.sort_by_key(|(_, duration_ms)| std::cmp::Reverse(*duration_ms));
    let top_file_count = by_file.len().min(10);
    if top_file_count > 0 {
        println!();
        println!(
            "{}",
            crate::localization::formatted!(
                "Slowest files: {top_file_count}",
                top_file_count = top_file_count
            )
        );
        for (file, duration_ms) in &by_file[..top_file_count] {
            println!(
                "{}",
                crate::localization::formatted!(
                    "  {duration_ms:>6} ms  {file}",
                    duration_ms = duration_ms,
                    file = file
                )
            );
        }
    }

    let aggregate = summary.aggregate;
    println!();
    println!(
        "{}",
        crate::localization::formatted!(
            "Phase totals: collection={} ms  setup={} ms  compile={} ms  admission={} ms  execute={} ms  teardown={} ms",
            aggregate.collection_ms,
            aggregate.setup_ms,
            aggregate.compile_ms,
            aggregate.admission_ms,
            aggregate.execute_ms,
            aggregate.teardown_ms,
        )
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "Test-file entries: compile={} ms ({} files, {} entries)",
            aggregate.test_file_compile_ms,
            aggregate.test_files_compiled,
            aggregate.test_entries_compiled,
        )
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "Module attribution (overlaps phases): compile={} ms ({} modules)  load={} ms ({} modules)",
            aggregate.modules.module_compile_ms,
            aggregate.modules.modules_compiled,
            aggregate.modules.module_load_ms,
            aggregate.modules.modules_loaded,
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_runner::{AggregateTimings, PhaseTimings, TestPhase, TestResult, TestTimeout};

    /// The exact confusion the emptiness check exists to break: an all-zero
    /// summary is "success" to `is_success`, so without a separate question
    /// the exit-code boundary cannot tell a vacuous run from a green one.
    #[test]
    fn empty_summary_is_success_but_did_not_run_anything() {
        let empty = ConformanceJsonSummary::default();
        assert!(empty.is_success());
        assert!(!empty.ran_anything());
    }

    /// Every outcome the runner can record counts as having run, including the
    /// ones that are not passes. A run that skipped everything still executed
    /// its selection; only a run that recorded nothing at all is vacuous.
    #[test]
    fn any_recorded_outcome_counts_as_having_run() {
        for outcome in [
            ConformanceJsonOutcome::Pass,
            ConformanceJsonOutcome::Fail,
            ConformanceJsonOutcome::XfailExpected,
            ConformanceJsonOutcome::XfailUnexpectedPass,
            ConformanceJsonOutcome::Skipped,
        ] {
            let mut summary = ConformanceJsonSummary::default();
            summary.record(outcome);
            assert!(
                summary.ran_anything(),
                "{outcome:?} should count as having run"
            );
        }
    }

    #[test]
    fn user_report_conversion_pins_v5_execution_metrics() {
        let modules = harn_vm::ModulePhaseStats::default();
        let phases = PhaseTimings {
            execute_ms: 30,
            modules,
            ..PhaseTimings::default()
        };
        let summary = test_runner::TestSummary {
            results: vec![
                TestResult {
                    name: "test_timeout".into(),
                    file: "/suite/test_timeout.harn".into(),
                    passed: false,
                    skip_reason: None,
                    error: Some("execute phase timed out after 30ms".into()),
                    captured_output: Some("[harn] before the deadline\n".into()),
                    timeout: Some(TestTimeout {
                        phase: TestPhase::Execute,
                        limit_ms: 30,
                    }),
                    duration_ms: 30,
                    phases: Some(phases),
                    work: Some(Box::new(harn_vm::VmWork { vm_steps: 12 })),
                    timing_spans: Vec::new(),
                },
                TestResult {
                    name: "<file error>".into(),
                    file: "/suite/broken.harn".into(),
                    passed: false,
                    skip_reason: None,
                    error: Some("parse failed".into()),
                    captured_output: None,
                    timeout: None,
                    duration_ms: 0,
                    phases: None,
                    work: None,
                    timing_spans: Vec::new(),
                },
            ],
            passed: 0,
            failed: 2,
            skipped: 0,
            total: 2,
            duration_ms: 31,
            timing: DurationSummary::from_samples(&[30]),
            aggregate: AggregateTimings {
                execute_ms: 30,
                modules: phases.modules,
                ..AggregateTimings::default()
            },
            timing_environment: None,
            shard_plan: None,
            cost_regressions: Vec::new(),
        };

        let value =
            serde_json::to_value(user_test_report_from_summary(Path::new("/suite"), &summary))
                .expect("report serializes");

        assert_eq!(value["schemaVersion"], 5);
        assert_eq!(value["cases"][0]["work"]["vm_steps"], 12);
        assert!(value["cases"][1].get("work").unwrap().is_null());
        assert_eq!(value["timing"]["sample_count"], 1);
        assert_eq!(value["aggregate"]["modules"]["modules_loaded"], 0);
        assert_eq!(value["cases"][0]["timeout"]["phase"], "execute");
        assert_eq!(
            value["cases"][0]["phases"]["modules"]["modules_compiled"],
            0
        );
        assert_eq!(
            value["cases"][0]["captured_output"],
            "[harn] before the deadline\n"
        );
        assert!(value["cases"][1].get("phases").is_none());
        assert!(value["cases"][1].get("captured_output").is_none());
    }
}
