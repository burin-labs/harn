//! User-test report conversion and terminal timing output.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::test_report::{TestCaseReport, TestOutcome, TestReport};
use crate::test_runner;
use crate::test_timing::DurationSummary;

use super::{logical_path, UserTestOutputOptions};

pub(crate) const CONFORMANCE_TEST_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConformanceJsonOutcome {
    Pass,
    Fail,
    XfailExpected,
    XfailUnexpectedPass,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ConformanceJsonResult {
    pub(super) name: String,
    pub(super) outcome: ConformanceJsonOutcome,
    pub(super) duration_ms: u64,
    pub(super) message: Option<String>,
    pub(super) diagnostic_codes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
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
        }
    }

    pub(super) fn is_success(&self) -> bool {
        self.fail == 0 && self.xfail_unexpected_pass == 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ConformanceJsonReport {
    #[serde(rename = "snapshotKey")]
    snapshot_key: String,
    results: Vec<ConformanceJsonResult>,
    summary: ConformanceJsonSummary,
    timing: DurationSummary,
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
    println!("Per-test: avg={avg} ms  p50={p50} ms  p90={p90} ms  p95={p95} ms  p99={p99} ms");

    let mut by_time: Vec<&TestCaseReport> = report.cases.iter().collect();
    by_time.sort_by_key(|case| std::cmp::Reverse(case.duration_ms));
    let top_n = by_time.len().min(10);
    if top_n > 0 {
        println!();
        println!("Slowest {top_n} tests:");
        for case in &by_time[..top_n] {
            println!("  {:>6} ms  {}", case.duration_ms, case.name);
        }
    }
}

pub(super) fn user_test_report_from_summary(
    suite_root: &Path,
    summary: &test_runner::TestSummary,
) -> TestReport {
    let mut report = TestReport::new("user", Some(suite_root));
    for result in &summary.results {
        let outcome = if result.passed {
            TestOutcome::Passed
        } else if result.timeout.is_some() {
            TestOutcome::TimedOut
        } else {
            TestOutcome::Failed
        };
        let file_path = PathBuf::from(&result.file);
        let relative = file_path
            .strip_prefix(suite_root)
            .ok()
            .map(logical_path)
            .unwrap_or_else(|| result.file.clone());
        report.push(TestCaseReport {
            name: result.name.clone(),
            file: relative.clone(),
            classname: relative,
            outcome,
            duration_ms: result.duration_ms,
            timeout: result.timeout,
            phases: result.phases,
            message: result.error.clone(),
            captured_output: result.captured_output.clone(),
        });
    }
    report.set_duration_ms(summary.duration_ms);
    report.set_execution_metrics(summary.timing, summary.aggregate);
    report
}

/// Prints a case's buffered `log`/`print`/`println` output beneath its
/// result line, indented one level past the `{line}` convention already
/// used for error text so the two blocks stay visually distinct.
pub(super) fn print_captured_output(output: &str) {
    println!("        captured output:");
    for line in output.lines() {
        println!("          {line}");
    }
}

pub(super) fn user_test_progress(verbose: bool) -> test_runner::TestRunProgress {
    std::sync::Arc::new(move |event| match event {
        test_runner::TestRunEvent::SuiteDiscovered {
            total_tests,
            total_files,
            parallel,
            workers,
        } => {
            if total_tests > 0 {
                let mode = if parallel { "dynamic" } else { "sequential" };
                println!(
                    "Running {} test{} from {} file{} with {} worker{} ({mode} scheduling)\n",
                    total_tests,
                    if total_tests == 1 { "" } else { "s" },
                    total_files,
                    if total_files == 1 { "" } else { "s" },
                    workers,
                    if workers == 1 { "" } else { "s" },
                );
            }
        }
        test_runner::TestRunEvent::LargeSequentialSuite {
            total_tests,
            total_files,
        } => {
            println!(
                "\x1b[33mwarning\x1b[0m: large suite discovered ({total_tests} tests across {total_files} files); running sequentially. Use `--parallel` to enable the bounded worker pool.\n"
            );
        }
        test_runner::TestRunEvent::TestStarted {
            name,
            file,
            test_index,
            total_tests,
        } => {
            if verbose {
                println!("    RUN   {name} [{file}] ({test_index}/{total_tests})");
            }
        }
        test_runner::TestRunEvent::TestFinished(result) => {
            if result.passed {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                    result.name, result.file, result.duration_ms
                );
                // Passing tests stay quiet by default; --verbose is the
                // existing "I want detail even when green" escape hatch,
                // so probes placed to trace a passing path are visible too.
                if verbose {
                    if let Some(output) = &result.captured_output {
                        print_captured_output(output);
                    }
                }
            } else {
                println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
                if verbose {
                    if let Some(err) = &result.error {
                        for line in err.lines() {
                            println!("        {line}");
                        }
                    }
                    if let Some(output) = &result.captured_output {
                        print_captured_output(output);
                    }
                }
            }
        }
    })
}

pub(super) fn print_test_results(
    summary: &test_runner::TestSummary,
    options: UserTestOutputOptions,
) {
    let file_count = summary
        .results
        .iter()
        .map(|result| result.file.as_str())
        .collect::<HashSet<_>>()
        .len();

    if !options.progress && summary.total > 0 {
        println!(
            "Running {} test{} from {} file{}...\n",
            summary.total,
            if summary.total == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
        );
    }

    if !options.progress {
        for result in &summary.results {
            if result.passed {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                    result.name, result.file, result.duration_ms
                );
            } else {
                println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
                if let Some(error) = &result.error {
                    for line in error.lines() {
                        println!("        {line}");
                    }
                }
                if let Some(output) = &result.captured_output {
                    print_captured_output(output);
                }
            }
        }
    }

    println!();
    if summary.failed > 0 {
        println!(
            "\x1b[31m{} passed, {} failed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.failed, summary.total, summary.duration_ms
        );
    } else if summary.total == 0 {
        println!("No test pipelines found");
    } else {
        println!(
            "\x1b[32m{} passed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.total, summary.duration_ms
        );
    }

    match (summary.timing.p50_ms, summary.timing.p90_ms) {
        (Some(p50), Some(p90)) => println!("Latency: p50={p50} ms  p90={p90} ms"),
        _ => println!("Latency: p50=n/a  p90=n/a (0 samples)"),
    }

    if options.timing {
        print_user_test_timing(summary);
    }

    if options.progress && summary.failed > 0 {
        println!();
        println!("Failures:");
        for result in summary.results.iter().filter(|result| !result.passed) {
            if let Some(error) = &result.error {
                println!("  {} [{}]", result.name, result.file);
                for line in error.lines() {
                    println!("        {line}");
                }
                if let Some(output) = &result.captured_output {
                    print_captured_output(output);
                }
            }
        }
    }
}

fn print_user_test_timing(summary: &test_runner::TestSummary) {
    if summary.total == 0 {
        return;
    }

    println!();
    println!("Total time: {} ms", summary.duration_ms);
    if let (Some(avg), Some(p95), Some(p99)) = (
        summary.timing.average_ms,
        summary.timing.p95_ms,
        summary.timing.p99_ms,
    ) {
        println!("Per-test detail: avg={avg} ms  p95={p95} ms  p99={p99} ms");
    }

    let mut by_test = summary.results.iter().collect::<Vec<_>>();
    by_test.sort_by_key(|result| std::cmp::Reverse(result.duration_ms));
    let top_test_count = by_test.len().min(10);
    if top_test_count > 0 {
        println!();
        println!("Slowest {top_test_count} tests:");
        for result in &by_test[..top_test_count] {
            println!(
                "  {:>6} ms  {} [{}]",
                result.duration_ms, result.name, result.file
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
        println!("Slowest {top_file_count} files:");
        for (file, duration_ms) in &by_file[..top_file_count] {
            println!("  {duration_ms:>6} ms  {file}");
        }
    }

    let aggregate = summary.aggregate;
    println!();
    println!(
        "Phase totals: collection={} ms  setup={} ms  compile={} ms  execute={} ms  teardown={} ms",
        aggregate.collection_ms,
        aggregate.setup_ms,
        aggregate.compile_ms,
        aggregate.execute_ms,
        aggregate.teardown_ms,
    );
    println!(
        "Module attribution (overlaps phases): compile={} ms ({} modules)  load={} ms ({} modules)",
        aggregate.modules.module_compile_ms,
        aggregate.modules.modules_compiled,
        aggregate.modules.module_load_ms,
        aggregate.modules.modules_loaded,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_runner::{AggregateTimings, PhaseTimings, TestPhase, TestResult, TestTimeout};

    #[test]
    fn user_report_conversion_pins_v3_execution_metrics() {
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
                    error: Some("execute phase timed out after 30ms".into()),
                    captured_output: Some("[harn] before the deadline\n".into()),
                    timeout: Some(TestTimeout {
                        phase: TestPhase::Execute,
                        limit_ms: 30,
                    }),
                    duration_ms: 30,
                    phases: Some(phases),
                },
                TestResult {
                    name: "<file error>".into(),
                    file: "/suite/broken.harn".into(),
                    passed: false,
                    error: Some("parse failed".into()),
                    captured_output: None,
                    timeout: None,
                    duration_ms: 0,
                    phases: None,
                },
            ],
            passed: 0,
            failed: 2,
            total: 2,
            duration_ms: 31,
            timing: DurationSummary::from_samples(&[30]),
            aggregate: AggregateTimings {
                execute_ms: 30,
                modules: phases.modules,
                ..AggregateTimings::default()
            },
        };

        let value =
            serde_json::to_value(user_test_report_from_summary(Path::new("/suite"), &summary))
                .expect("report serializes");

        assert_eq!(value["schemaVersion"], 3);
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
