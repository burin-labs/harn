//! Process-isolated parallel execution for the conformance suite.

use std::path::{Path, PathBuf};
use std::process::{self, Stdio};

use crate::json_envelope::{self, JsonEnvelope, JsonError};
use crate::test_report::{TestCaseReport, TestOutcome, TestReport};

use super::*;

struct WorkerChild {
    index: usize,
    child: Option<tokio::process::Child>,
    _cleanup_guard: harn_vm::op_interrupt::ActiveProcessCleanupGuard,
}

pub(in crate::commands::test) async fn run_parallel_conformance_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    options: ConformanceRunOptions<'_>,
    jobs: Option<usize>,
) {
    let suite_root = canonical_suite_root_or_exit(dir, options.json);
    let selected_files = selected_files_or_exit(&suite_root, selection, filter, options.json);
    let workers =
        crate::test_runner::resolve_parallel_workers(jobs).min(selected_files.len().max(1));
    let snapshot_key = conformance_snapshot_key(&suite_root, &selected_files);
    let session_root = tempfile::Builder::new()
        .prefix("harn-conformance-workers-")
        .tempdir()
        .unwrap_or_else(|error| command_error(&format!("create worker session root: {error}")));

    if !options.json {
        eprintln!("harn conformance: running {workers} process-isolated worker(s)");
    }
    let started = std::time::Instant::now();
    let mut children = Vec::with_capacity(workers);
    for index in 1..=workers {
        let state_root = session_root.path().join(format!("worker-{index}"));
        fs::create_dir(&state_root).unwrap_or_else(|error| {
            command_error(&format!(
                "create state directory for conformance worker {index}: {error}"
            ))
        });
        children.push(
            spawn_worker(
                index,
                workers,
                dir,
                selection,
                filter,
                timeout_ms,
                &options,
                &state_root,
            )
            .await
            .unwrap_or_else(|error| command_error(&error)),
        );
    }

    let mut results = Vec::new();
    let mut infrastructure_error = None;
    for worker in &mut children {
        let child = worker.child.take().expect("worker child is present");
        match child.wait_with_output().await {
            Ok(output) => {
                if !output.stderr.is_empty() {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
                match parse_worker_report(worker.index, &output.stdout) {
                    Ok(mut report) => results.append(&mut report.results),
                    Err(error) => {
                        infrastructure_error = Some(error);
                        break;
                    }
                }
            }
            Err(error) => {
                infrastructure_error = Some(format!(
                    "conformance worker {} could not be reaped: {error}",
                    worker.index
                ));
                break;
            }
        }
    }
    if let Some(error) = infrastructure_error {
        terminate_workers(&mut children).await;
        command_error(&error);
    }

    results.sort_by(|left, right| left.name.cmp(&right.name));
    let mut summary = ConformanceJsonSummary::default();
    for result in &results {
        summary.record(result.outcome);
    }
    let duration_ms = started.elapsed().as_millis() as u64;
    let ok = summary.is_success();
    let report = ConformanceJsonReport::new(snapshot_key, results, summary);

    if options.json {
        print_json_report(&report, ok);
    } else {
        print_text_report(
            &suite_root,
            &report,
            duration_ms,
            options.verbose || options.timing,
        );
    }
    if let Some(path) = junit_path {
        let junit = test_report_from_conformance(&suite_root, duration_ms, &report.results);
        super::super::write_junit_xml_or_exit(path, &junit, !options.json);
    }
    if !ok {
        process::exit(1);
    }
}

fn canonical_suite_root_or_exit(dir: &str, json: bool) -> PathBuf {
    let path = PathBuf::from(dir);
    if !path.exists() {
        report_setup_error(
            json,
            "conformance_directory_not_found",
            format!("Directory not found: {dir}"),
        );
    }
    canonicalize_or_err(&path)
        .unwrap_or_else(|error| report_setup_error(json, "conformance_directory_error", error))
}

fn selected_files_or_exit(
    suite_root: &Path,
    selection: Option<&str>,
    filter: Option<&str>,
    json: bool,
) -> Vec<(PathBuf, String)> {
    resolve_conformance_selection(suite_root, selection)
        .unwrap_or_else(|error| report_setup_error(json, "conformance_selection_error", error))
        .into_iter()
        .filter_map(|path| {
            let relative = logical_path(path.strip_prefix(suite_root).unwrap_or(&path));
            conformance_filter_matches(&relative, filter).then_some((path, relative))
        })
        .collect()
}

fn report_setup_error(json: bool, code: &str, message: String) -> ! {
    if json {
        let envelope: JsonEnvelope<ConformanceJsonReport> =
            JsonEnvelope::err(CONFORMANCE_TEST_SCHEMA_VERSION, code, message);
        println!("{}", json_envelope::to_string_pretty(&envelope));
        process::exit(1);
    }
    command_error(&message)
}

async fn spawn_worker(
    index: usize,
    total: usize,
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    timeout_ms: u64,
    options: &ConformanceRunOptions<'_>,
    state_root: &Path,
) -> Result<WorkerChild, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve current harn executable: {error}"))?;
    let cleanup_token = harn_vm::op_interrupt::new_process_cleanup_token();
    let mut command = tokio::process::Command::new(executable);
    harn_vm::op_interrupt::configure_tokio_kill_group(&mut command);
    command
        .arg("test")
        .arg(dir)
        .arg("--json")
        .arg("--timeout")
        .arg(timeout_ms.to_string())
        .arg("--shard-index")
        .arg(index.to_string())
        .arg("--shard-total")
        .arg(total.to_string())
        .arg("--internal-conformance-worker")
        .arg(if options.json {
            "execute-xfail"
        } else {
            "skip-xfail"
        })
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env("HARN_SESSION_STORE_ROOT", state_root)
        .env(
            harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
            &cleanup_token,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(selection) = selection {
        command.arg(selection);
    }
    if let Some(filter) = filter {
        command.arg("--filter").arg(filter);
    }
    if options.verbose {
        command.arg("--verbose");
    }
    if options.differential_optimizations {
        command.arg("--differential-optimizations");
    }
    for directory in options.cli_skill_dirs {
        command.arg("--skill-dir").arg(directory);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("launch conformance worker {index}: {error}"))?;
    harn_vm::op_interrupt::record_tokio_process_owner_group(&mut child, &cleanup_token)
        .await
        .map_err(|error| format!("record conformance worker {index}: {error}"))?;
    let cleanup_guard =
        harn_vm::op_interrupt::register_active_process_cleanup(child.id(), &cleanup_token, None);
    Ok(WorkerChild {
        index,
        child: Some(child),
        _cleanup_guard: cleanup_guard,
    })
}

fn parse_worker_report(index: usize, stdout: &[u8]) -> Result<ConformanceJsonReport, String> {
    let envelope: JsonEnvelope<ConformanceJsonReport> =
        serde_json::from_slice(stdout).map_err(|e| {
            format!(
                "conformance worker {index} returned invalid JSON: {e}\n{}",
                String::from_utf8_lossy(stdout)
            )
        })?;
    if envelope.schema_version != CONFORMANCE_TEST_SCHEMA_VERSION {
        return Err(format!(
            "conformance worker {index} returned schema {}, expected {}",
            envelope.schema_version, CONFORMANCE_TEST_SCHEMA_VERSION
        ));
    }
    envelope.data.ok_or_else(|| {
        let detail = envelope
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "missing data and error".to_string());
        format!("conformance worker {index} failed before producing results: {detail}")
    })
}

async fn terminate_workers(workers: &mut [WorkerChild]) {
    #[cfg(unix)]
    let _ = harn_vm::op_interrupt::signal_ownerless_active_process_cleanups(libc::SIGTERM);
    #[cfg(not(unix))]
    let _ = harn_vm::op_interrupt::signal_ownerless_active_process_cleanups(15);
    for worker in workers {
        if let Some(mut child) = worker.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

fn print_json_report(report: &ConformanceJsonReport, ok: bool) {
    let error = (!ok).then(|| JsonError {
        code: "conformance_failed".to_string(),
        message: "one or more conformance tests failed or unexpectedly passed an xfail marker"
            .to_string(),
        details: serde_json::json!({
            "fail": report.summary.fail,
            "xfail_unexpected_pass": report.summary.xfail_unexpected_pass,
        }),
    });
    let envelope = JsonEnvelope {
        schema_version: CONFORMANCE_TEST_SCHEMA_VERSION,
        ok,
        data: Some(report.clone()),
        error,
        warnings: Vec::new(),
    };
    println!("{}", json_envelope::to_string_pretty(&envelope));
}

fn print_text_report(
    suite_root: &Path,
    report: &ConformanceJsonReport,
    duration_ms: u64,
    show_timing: bool,
) {
    for result in &report.results {
        let timing = show_timing.then(|| format!(" ({} ms)", result.duration_ms));
        match result.outcome {
            ConformanceJsonOutcome::Pass => {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {}{}",
                    result.name,
                    timing.unwrap_or_default()
                );
            }
            ConformanceJsonOutcome::Skipped | ConformanceJsonOutcome::XfailExpected => {
                let reason = result.message.as_deref().unwrap_or("expected failure");
                println!("  \x1b[33mSKIP\x1b[0m  {}  ({reason})", result.name);
            }
            ConformanceJsonOutcome::Fail | ConformanceJsonOutcome::XfailUnexpectedPass => {
                println!(
                    "  \x1b[31mFAIL\x1b[0m  {}{}",
                    result.name,
                    timing.unwrap_or_default()
                );
            }
        }
    }

    let passed = report.summary.pass;
    let failed = report.summary.fail + report.summary.xfail_unexpected_pass;
    let skipped = report.summary.skipped + report.summary.xfail_expected;
    let total = passed + failed + skipped;
    println!();
    if failed == 0 {
        println!(
            "\x1b[32m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    } else {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    }
    if show_timing {
        println!();
        println!("Total time: {duration_ms} ms");
        let test_report = test_report_from_conformance(suite_root, duration_ms, &report.results);
        print_per_test_timing(&test_report);
    }
    let failures = report.results.iter().filter(|result| {
        matches!(
            result.outcome,
            ConformanceJsonOutcome::Fail | ConformanceJsonOutcome::XfailUnexpectedPass
        )
    });
    let mut printed_header = false;
    for failure in failures {
        if !printed_header {
            println!();
            println!("Failures:");
            printed_header = true;
        }
        println!(
            "  {}",
            failure
                .message
                .as_deref()
                .unwrap_or("failed without diagnostic message")
        );
    }
}

fn test_report_from_conformance(
    suite_root: &Path,
    duration_ms: u64,
    results: &[ConformanceJsonResult],
) -> TestReport {
    let mut report = TestReport::new("conformance", Some(suite_root));
    report.set_duration_ms(duration_ms);
    for result in results {
        let outcome = match result.outcome {
            ConformanceJsonOutcome::Pass => TestOutcome::Passed,
            ConformanceJsonOutcome::Skipped | ConformanceJsonOutcome::XfailExpected => {
                TestOutcome::Skipped
            }
            ConformanceJsonOutcome::Fail | ConformanceJsonOutcome::XfailUnexpectedPass => {
                TestOutcome::Failed
            }
        };
        report.push(TestCaseReport {
            name: result.name.clone(),
            file: result.name.clone(),
            classname: result.name.clone(),
            outcome,
            duration_ms: result.duration_ms,
            timeout: None,
            phases: None,
            timing_spans: Vec::new(),
            message: result.message.clone(),
            captured_output: None,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_reports_are_parsed_from_failed_test_processes() {
        let report = ConformanceJsonReport::new(
            "a".repeat(64),
            vec![ConformanceJsonResult {
                name: "failure.harn".to_string(),
                outcome: ConformanceJsonOutcome::Fail,
                duration_ms: 4,
                message: Some("failed".to_string()),
                diagnostic_codes: Vec::new(),
            }],
            ConformanceJsonSummary {
                fail: 1,
                ..ConformanceJsonSummary::default()
            },
        );
        let envelope = JsonEnvelope {
            schema_version: CONFORMANCE_TEST_SCHEMA_VERSION,
            ok: false,
            data: Some(report),
            error: Some(JsonError {
                code: "conformance_failed".to_string(),
                message: "failed".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        };
        let encoded = serde_json::to_vec(&envelope).unwrap();

        let parsed = parse_worker_report(2, &encoded).unwrap();

        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].name, "failure.harn");
    }
}
