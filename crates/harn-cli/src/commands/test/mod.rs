use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{self, Stdio};

use clap::{error::ErrorKind, CommandFactory};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::cli::{Cli, TestArgs};
use crate::commands::run::{
    install_cli_llm_mock_mode, persist_cli_llm_mock_recording, CliLlmMockMode,
};
use crate::commands::{agents_conformance, protocol_conformance};
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{self, JsonEnvelope, JsonError};
use crate::test_report::{self, TestCaseReport, TestOutcome, TestReport};
use crate::test_runner;
use crate::{execute_with_skill_dirs, execute_with_skill_dirs_and_harness, ExecError};

mod conformance;
mod reporting;
mod watch;

use conformance::{
    run_conformance_determinism_tests, run_conformance_tests, ConformanceRunOptions,
};

pub(crate) use reporting::CONFORMANCE_TEST_SCHEMA_VERSION;
use reporting::{
    print_per_test_timing, print_test_results, user_test_progress, user_test_report_from_summary,
    ConformanceJsonOutcome, ConformanceJsonReport, ConformanceJsonResult, ConformanceJsonSummary,
};

pub(crate) async fn run_command(args: TestArgs) {
    let operator_approval_grant =
        harn_vm::orchestration::OperatorApprovalGrant::from_cli_operations(
            args.approve_risky.clone(),
        )
        .unwrap_or_else(|error| command_error(&error));
    if operator_approval_grant.is_some()
        && (args.determinism
            || args.evals
            || matches!(
                args.target.as_deref(),
                Some("agents-conformance" | "conformance" | "protocols")
            ))
    {
        command_error("--approve-risky is supported only for user-test execution");
    }
    if args.watch && (args.junit.is_some() || args.json_out.is_some()) {
        command_error(
            "`harn test --watch` cannot combine with --junit or --json-out; the watch loop never terminates so the report would never be written",
        );
    }

    if args.coverage || args.coverage_out.is_some() {
        if args.watch {
            command_error(
                "`harn test --coverage` cannot combine with --watch; the watch loop never terminates so no report would be written",
            );
        }
        if args.determinism || args.evals {
            command_error("`harn test --coverage` cannot combine with --determinism or --evals");
        }
        if matches!(
            args.target.as_deref(),
            Some("conformance") | Some("protocols") | Some("agents-conformance")
        ) {
            command_error(
                "`harn test --coverage` is supported for user test suites, not conformance / protocols / agents-conformance",
            );
        }
    }

    let shard_requested = args.shard_index.is_some() || args.shard_total.is_some();
    if args.target.as_deref() == Some("agents-conformance") {
        run_agents_conformance_command(args, shard_requested).await;
        return;
    }
    if args.target.as_deref() == Some("protocols") {
        run_protocols_command(args, shard_requested);
        return;
    }
    if args.evals {
        if shard_requested {
            command_error("--evals cannot be combined with test sharding");
        }
        if args.determinism || args.record || args.replay || args.watch {
            command_error(
                "--evals cannot be combined with --determinism, --record, --replay, or --watch",
            );
        }
        if args.target.as_deref() != Some("package") || args.selection.is_some() {
            command_error("package evals are run with `harn test package --evals`");
        }
        crate::run_package_evals();
    } else if args.determinism {
        run_determinism_command(args, shard_requested).await;
    } else {
        run_standard_command(args, operator_approval_grant).await;
    }
}

async fn run_agents_conformance_command(args: TestArgs, shard_requested: bool) {
    if args.selection.is_some() {
        command_error(
            "`harn test agents-conformance` does not accept a second positional target; use --category instead",
        );
    }
    if args.evals || args.determinism || args.record || args.replay || args.watch || shard_requested
    {
        command_error(
            "`harn test agents-conformance` cannot be combined with --evals, --determinism, --record, --replay, --watch, or test sharding",
        );
    }
    let Some(target_url) = args.agents_target.clone() else {
        command_error("`harn test agents-conformance` requires --target <url>");
    };
    agents_conformance::run_agents_conformance(agents_conformance::AgentsConformanceConfig {
        target_url,
        api_key: args.agents_api_key.clone(),
        categories: args.agents_category.clone(),
        timeout_ms: args.timeout,
        verbose: args.verbose,
        json: args.json,
        json_out: args.json_out.clone(),
        workspace_id: args.agents_workspace_id.clone(),
        session_id: args.agents_session_id.clone(),
    })
    .await;
}

fn run_protocols_command(args: TestArgs, shard_requested: bool) {
    if args.evals || args.determinism || args.record || args.replay || args.watch {
        command_error(
            "`harn test protocols` cannot be combined with --evals, --determinism, --record, --replay, or --watch",
        );
    }
    if args.junit.is_some()
        || args.agents_target.is_some()
        || args.agents_api_key.is_some()
        || !args.agents_category.is_empty()
        || args.json
        || args.json_out.is_some()
        || args.agents_workspace_id.is_some()
        || args.agents_session_id.is_some()
        || args.parallel
        || args.fail_fast
        || shard_requested
        || !args.skill_dir.is_empty()
    {
        command_error(
            "`harn test protocols` accepts only --filter, --verbose, --timing, and an optional fixture selection",
        );
    }
    protocol_conformance::run_protocol_conformance(
        args.selection.as_deref(),
        args.filter.as_deref(),
        args.verbose || args.timing,
    );
}

async fn run_determinism_command(args: TestArgs, shard_requested: bool) {
    if shard_requested {
        command_error("--determinism cannot be combined with test sharding");
    }
    let cli_skill_dirs: Vec<PathBuf> = args.skill_dir.iter().map(PathBuf::from).collect();
    if args.watch {
        command_error("--determinism cannot be combined with --watch");
    }
    if args.record || args.replay {
        command_error("--determinism manages its own record/replay cycle");
    }
    if let Some(t) = args.target.as_deref() {
        if t == "conformance" {
            run_conformance_determinism_tests(
                t,
                args.selection.as_deref(),
                args.filter.as_deref(),
                args.timeout,
                &cli_skill_dirs,
            )
            .await;
        } else if args.selection.is_some() {
            command_error("only `harn test conformance` accepts a second positional target");
        } else {
            run_determinism_tests(t, args.filter.as_deref(), args.timeout, &cli_skill_dirs).await;
        }
    } else {
        let test_dir = default_test_dir_or_exit();
        if args.selection.is_some() {
            command_error("only `harn test conformance` accepts a second positional target");
        }
        run_determinism_tests(
            &test_dir,
            args.filter.as_deref(),
            args.timeout,
            &cli_skill_dirs,
        )
        .await;
    }
}

async fn run_standard_command(
    args: TestArgs,
    operator_approval_grant: Option<harn_vm::orchestration::OperatorApprovalGrant>,
) {
    let cli_skill_dirs: Vec<PathBuf> = args.skill_dir.iter().map(PathBuf::from).collect();
    if args.record {
        harn_vm::llm::set_replay_mode(harn_vm::llm::LlmReplayMode::Record, ".harn-fixtures");
    } else if args.replay {
        harn_vm::llm::set_replay_mode(harn_vm::llm::LlmReplayMode::Replay, ".harn-fixtures");
    }

    if let Some(t) = args.target.as_deref() {
        if t == "conformance" {
            run_conformance_tests(
                t,
                args.selection.as_deref(),
                args.filter.as_deref(),
                args.junit.as_deref(),
                args.timeout,
                ConformanceRunOptions {
                    verbose: args.verbose,
                    timing: args.timing,
                    differential_optimizations: args.differential_optimizations,
                    json: args.json,
                    shard: resolve_test_shard(args.shard_index, args.shard_total),
                    cli_skill_dirs: &cli_skill_dirs,
                },
            )
            .await;
        } else if args.selection.is_some() {
            command_error("only `harn test conformance` accepts a second positional target");
        } else {
            run_user_test_target(t, &args, &cli_skill_dirs, operator_approval_grant).await;
        }
    } else {
        let test_dir = default_test_dir_or_exit();
        if args.selection.is_some() {
            command_error("only `harn test conformance` accepts a second positional target");
        }
        run_user_test_target(&test_dir, &args, &cli_skill_dirs, operator_approval_grant).await;
    }
}

async fn run_user_test_target(
    path: &str,
    args: &TestArgs,
    cli_skill_dirs: &[PathBuf],
    operator_approval_grant: Option<harn_vm::orchestration::OperatorApprovalGrant>,
) {
    let run_args = UserTestRunArgs {
        filter: args.filter.as_deref(),
        timeout_ms: args.timeout,
        max_test_ms: args.max_test_ms,
        max_execute_ms: args.max_execute_ms,
        parallel: args.parallel,
        fail_fast: args.fail_fast,
        jobs: args.jobs,
        shard: resolve_test_shard(args.shard_index, args.shard_total),
        verbose: args.verbose,
        timing: args.timing,
        diagnose: args.diagnose,
        cli_skill_dirs,
        operator_approval_grant,
    };
    if args.watch {
        watch::run(path, run_args).await;
    } else {
        let coverage = (args.coverage || args.coverage_out.is_some()).then(|| CoverageOptions {
            out: args.coverage_out.clone(),
        });
        run_user_tests(
            path,
            run_args,
            UserTestReportConfig {
                junit_path: args.junit.as_deref(),
                json_out_path: args.json_out.as_deref(),
            },
            coverage,
        )
        .await;
    }
}

/// Coverage knobs for a user-test run. Present only when `--coverage` (or
/// `--coverage-out`) was requested.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoverageOptions {
    /// Optional LCOV output path.
    pub out: Option<String>,
}

fn default_test_dir_or_exit() -> String {
    if PathBuf::from("tests").is_dir() {
        "tests".to_string()
    } else {
        command_error("no path specified and no tests/ directory found");
    }
}

fn resolve_test_shard(
    shard_index: Option<usize>,
    shard_total: Option<usize>,
) -> Option<test_runner::TestShard> {
    match (shard_index, shard_total) {
        (None, None) => None,
        (Some(index), Some(total)) => match test_runner::TestShard::new(index, total) {
            Ok(shard) => Some(shard),
            Err(error) => command_error(&error),
        },
        _ => command_error("test sharding requires both --shard-index and --shard-total"),
    }
}

fn command_error(message: &str) -> ! {
    Cli::command()
        .error(ErrorKind::ValueValidation, message)
        .exit()
}

/// Report-writing options threaded into `run_user_tests`. Each `Some`
/// path triggers a write at end-of-run; a missing parent directory or
/// I/O error fails the run loudly instead of silently succeeding.
#[derive(Debug, Clone, Copy, Default)]
pub struct UserTestReportConfig<'a> {
    pub junit_path: Option<&'a str>,
    pub json_out_path: Option<&'a str>,
}

impl UserTestReportConfig<'_> {
    pub fn is_empty(&self) -> bool {
        self.junit_path.is_none() && self.json_out_path.is_none()
    }
}

/// Per-invocation knobs for `harn test <dir>` and `harn test --watch`.
/// Bundled so call sites share one parameter shape and the runner
/// signatures stay readable as new flags accrete.
#[derive(Clone)]
pub(crate) struct UserTestRunArgs<'a> {
    pub filter: Option<&'a str>,
    pub timeout_ms: u64,
    pub max_test_ms: Option<u64>,
    pub max_execute_ms: Option<u64>,
    pub parallel: bool,
    pub fail_fast: bool,
    pub jobs: Option<usize>,
    pub shard: Option<test_runner::TestShard>,
    pub verbose: bool,
    pub timing: bool,
    pub diagnose: bool,
    pub cli_skill_dirs: &'a [PathBuf],
    pub operator_approval_grant: Option<harn_vm::orchestration::OperatorApprovalGrant>,
}

fn normalize_expected_output(text: &str) -> String {
    text.lines()
        .map(normalize_output_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_actual_output(text: &str) -> String {
    text.lines()
        .map(normalize_output_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_output_line(line: &str) -> String {
    if let Some(prefix) = line.strip_suffix("ms") {
        if let Some((head, _millis)) = prefix.rsplit_once(": ") {
            if head.starts_with("[timer] ") {
                return format!("{head}: <ms>");
            }
        }
    }
    line.to_string()
}

fn logical_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                Some(component.as_os_str().to_string_lossy().into_owned())
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Produce a simple line diff between expected and actual.
fn simple_diff(expected: &str, actual: &str) -> String {
    let mut result = String::new();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let max = expected_lines.len().max(actual_lines.len());
    for i in 0..max {
        let exp = expected_lines.get(i).copied().unwrap_or("");
        let act = actual_lines.get(i).copied().unwrap_or("");
        if exp == act {
            result.push_str(&format!("  {exp}\n"));
        } else {
            result.push_str(&format!("\x1b[31m- {exp}\x1b[0m\n"));
            result.push_str(&format!("\x1b[32m+ {act}\x1b[0m\n"));
        }
    }
    result
}

/// Check whether an actual error message matches the expected error spec.
///
/// The `.error` file supports three modes:
/// - Plain text: substring match (backward compatible)
/// - `re:` prefix: regex match against the full error message
/// - Multiple lines: union — passes if any line matches
fn error_matches(actual_error: &str, expected_spec: &str) -> bool {
    let lines: Vec<&str> = expected_spec.lines().collect();
    if lines.len() > 1 {
        return lines
            .iter()
            .any(|line| error_line_matches(actual_error, line.trim()));
    }
    error_line_matches(actual_error, expected_spec.trim())
}

fn error_line_matches(actual_error: &str, pattern: &str) -> bool {
    if let Some(re_pattern) = pattern.strip_prefix("re:") {
        match Regex::new(re_pattern.trim()) {
            Ok(re) => re.is_match(actual_error),
            Err(_) => {
                eprintln!("    warning: invalid regex in .error file: {re_pattern}");
                false
            }
        }
    } else {
        actual_error.contains(pattern)
    }
}

/// Write a JUnit XML report or exit non-zero with a clear diagnostic.
/// Centralised so every `harn test` mode fails loudly on bad paths
/// (issue #2146).
fn write_junit_xml_or_exit(path: &str, report: &TestReport, announce: bool) {
    match test_report::write_junit(path, report) {
        Ok(written) => {
            if announce {
                println!("JUnit XML written to {}", written.display());
            }
        }
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn write_user_test_json_or_exit(path: &str, report: &TestReport, announce: bool) {
    match test_report::write_json(path, report) {
        Ok(written) => {
            if announce {
                println!("JSON report written to {}", written.display());
            }
        }
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct UserTestOutputOptions {
    timing: bool,
    progress: bool,
}

async fn run_user_tests_once(path: &Path, args: UserTestRunArgs<'_>) -> test_runner::TestSummary {
    run_user_tests_once_with_session(path, args, &test_runner::TestRunSession::default()).await
}

async fn run_user_tests_once_with_session(
    path: &Path,
    args: UserTestRunArgs<'_>,
    session: &test_runner::TestRunSession,
) -> test_runner::TestSummary {
    let options = test_runner::RunOptions {
        filter: args.filter.map(str::to_owned),
        timeout_ms: args.timeout_ms,
        max_test_ms: args.max_test_ms,
        max_execute_ms: args.max_execute_ms,
        parallel: args.parallel,
        fail_fast: args.fail_fast,
        jobs: args.jobs,
        shard: args.shard,
        cli_skill_dirs: args.cli_skill_dirs.to_vec(),
        progress: Some(user_test_progress(args.verbose)),
        diagnose: args.diagnose,
    };
    let summary = test_runner::run_tests_with_session_and_operator_grant(
        path,
        &options,
        session,
        args.operator_approval_grant.as_ref(),
    )
    .await;
    print_test_results(
        &summary,
        UserTestOutputOptions {
            timing: args.timing,
            progress: true,
        },
    );
    summary
}
pub(crate) async fn run_user_tests(
    path_str: &str,
    args: UserTestRunArgs<'_>,
    report_config: UserTestReportConfig<'_>,
    coverage: Option<CoverageOptions>,
) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    if !report_config.is_empty() {
        if let Some(p) = report_config.junit_path {
            preflight_report_path(p);
        }
        if let Some(p) = report_config.json_out_path {
            preflight_report_path(p);
        }
    }
    if let Some(opts) = &coverage {
        if let Some(out) = &opts.out {
            preflight_report_path(out);
        }
        // Arm coverage before any test worker spins up a VM so every isolate
        // the run constructs records into the shared report.
        harn_vm::coverage::begin_session();
    }

    let summary = run_user_tests_once(&path, args).await;

    if !report_config.is_empty() {
        let suite_root = path.canonicalize().unwrap_or(path.clone());
        let report = user_test_report_from_summary(&suite_root, &summary);
        if let Some(p) = report_config.junit_path {
            write_junit_xml_or_exit(p, &report, true);
        }
        if let Some(p) = report_config.json_out_path {
            write_user_test_json_or_exit(p, &report, true);
        }
    }

    // Emit coverage before the failure exit so a failing suite still reports
    // what it exercised.
    if let Some(opts) = &coverage {
        let report = harn_vm::coverage::end_session();
        emit_coverage_report(&report, opts.out.as_deref());
    }

    if summary.failed > 0 {
        process::exit(1);
    }
}

/// Print the per-file coverage summary and, when an LCOV path was requested,
/// write the tracefile. A write failure fails the run loudly.
fn emit_coverage_report(report: &harn_vm::coverage::Coverage, out: Option<&str>) {
    if report.is_empty() {
        eprintln!("\n[coverage] no executed source files were found on disk to report");
    } else {
        let (covered, total) = report.totals();
        println!(
            "\nLine coverage: {covered}/{total} ({:.1}%)\n{}",
            report.percent(),
            report.render_text(),
        );
    }
    // Honour an explicit --coverage-out even for an empty report: a CI step that
    // consumes the tracefile should find it rather than fail on a missing
    // artifact. An empty `Coverage` renders to a valid empty LCOV (zero
    // records).
    if let Some(path) = out {
        if let Err(error) = fs::write(path, report.render_lcov()) {
            eprintln!("failed to write coverage report to {path}: {error}");
            process::exit(1);
        }
        println!("LCOV tracefile written to {path}");
    }
}

/// Validate report destinations before running the suite so a typo
/// in the output path is caught up-front rather than after a long
/// test run. Mirrors the post-run write check inside the writers but
/// short-circuits "I just spent 10 minutes running tests and got no
/// report" cases.
fn preflight_report_path(path: &str) {
    let buf = PathBuf::from(path);
    if let Some(parent) = buf.parent().filter(|p| !p.as_os_str().is_empty()) {
        if !parent.exists() {
            eprintln!("report directory does not exist: {}", parent.display());
            process::exit(1);
        }
        if !parent.is_dir() {
            eprintln!("report directory is not a directory: {}", parent.display());
            process::exit(1);
        }
    }
}

fn collect_user_test_files(path_str: &str) -> Result<Vec<PathBuf>, String> {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        return Err(format!("Path not found: {path_str}"));
    }
    if path.is_file() {
        return Ok(vec![path]);
    }
    let files = collect_harn_files_sorted(&path);
    if files.is_empty() {
        return Err(format!("No .harn files found under {}", path.display()));
    }
    Ok(files)
}

fn collect_harn_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    crate::commands::collect_harn_files(dir, &mut files);
    files
}

fn sibling_llm_fixture(path: &Path) -> Option<PathBuf> {
    let fixture = path.with_extension("llm-mock.jsonl");
    fixture.is_file().then_some(fixture)
}

fn load_run_records(dir: &Path) -> Result<Vec<harn_vm::orchestration::RunRecord>, String> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .map_err(|error| format!("failed to read {}: {error}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            harn_vm::orchestration::load_run_record(path)
                .map_err(|error| format!("failed to load {}: {error}", path.display()))
        })
        .collect()
}

fn load_transcript_responses(dir: &Path) -> Result<Vec<Value>, String> {
    let path = dir.join("llm_transcript.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("provider_call_response"))
        .map(|event| {
            Ok(serde_json::json!({
                "provider": event.get("provider").cloned().unwrap_or(Value::Null),
                "model": event.get("model").cloned().unwrap_or(Value::Null),
                "text": event.get("text").cloned().unwrap_or(Value::Null),
                "tool_calls": event.get("tool_calls").cloned().unwrap_or(Value::Null),
                "input_tokens": event.get("input_tokens").cloned().unwrap_or(Value::Null),
                "output_tokens": event.get("output_tokens").cloned().unwrap_or(Value::Null),
                "thinking": event.get("thinking").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect()
}

async fn execute_determinism_run(
    source: &str,
    path: &Path,
    timeout_ms: u64,
    llm_mock_mode: &CliLlmMockMode,
    run_dir: &tempfile::TempDir,
    transcript_dir: &tempfile::TempDir,
    cli_skill_dirs: &[PathBuf],
) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    install_cli_llm_mock_mode(llm_mock_mode)?;
    let run_dir_guard = ScopedEnvVar::set(
        harn_vm::runtime_paths::HARN_RUN_DIR_ENV,
        &run_dir.path().to_string_lossy(),
    );
    let transcript_dir_guard = ScopedEnvVar::set(
        "HARN_LLM_TRANSCRIPT_DIR",
        &transcript_dir.path().to_string_lossy(),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        execute_with_skill_dirs(source, Some(path), cli_skill_dirs),
    )
    .await;
    let persist_result = persist_cli_llm_mock_recording(llm_mock_mode);
    harn_vm::llm::clear_cli_llm_mock_mode();
    drop(transcript_dir_guard);
    drop(run_dir_guard);
    persist_result?;
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!("timed out after {timeout_ms}ms")),
    }
}

fn compare_determinism_artifacts(
    path: &Path,
    left_runs: &[harn_vm::orchestration::RunRecord],
    right_runs: &[harn_vm::orchestration::RunRecord],
    left_responses: &[Value],
    right_responses: &[Value],
) -> Result<(), String> {
    if left_runs.len() != right_runs.len() {
        return Err(format!(
            "{} produced {} run record(s) on the first pass and {} on replay",
            path.display(),
            left_runs.len(),
            right_runs.len()
        ));
    }
    for (idx, (left, right)) in left_runs.iter().zip(right_runs.iter()).enumerate() {
        let diff = harn_vm::orchestration::diff_run_records(left, right);
        if !diff.identical
            || left.tool_recordings != right.tool_recordings
            || left.hitl_questions != right.hitl_questions
        {
            return Err(format!(
                "{} replay diverged for run #{idx}: identical={} tool_recordings_equal={} hitl_questions_equal={}",
                path.display(),
                diff.identical,
                left.tool_recordings == right.tool_recordings,
                left.hitl_questions == right.hitl_questions
            ));
        }
    }
    if left_responses != right_responses {
        return Err(format!(
            "{} replay changed provider_call_response output",
            path.display()
        ));
    }
    Ok(())
}

async fn run_determinism_case(
    path: &Path,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let recording_dir = tempfile::Builder::new()
        .prefix("harn-determinism-record-")
        .tempdir()
        .map_err(|error| format!("failed to create determinism tempdir: {error}"))?;
    let replay_dir = tempfile::Builder::new()
        .prefix("harn-determinism-replay-")
        .tempdir()
        .map_err(|error| format!("failed to create determinism tempdir: {error}"))?;
    let record_transcript = tempfile::Builder::new()
        .prefix("harn-determinism-record-llm-")
        .tempdir()
        .map_err(|error| format!("failed to create transcript tempdir: {error}"))?;
    let replay_transcript = tempfile::Builder::new()
        .prefix("harn-determinism-replay-llm-")
        .tempdir()
        .map_err(|error| format!("failed to create transcript tempdir: {error}"))?;
    let fixture_mode = sibling_llm_fixture(path);
    let fixture_path = fixture_mode
        .clone()
        .unwrap_or_else(|| recording_dir.path().join("fixture.jsonl"));
    let first_mode = fixture_mode
        .clone()
        .map(|fixture_path| CliLlmMockMode::Replay { fixture_path })
        .unwrap_or_else(|| CliLlmMockMode::Record {
            fixture_path: fixture_path.clone(),
        });
    let second_mode = CliLlmMockMode::Replay {
        fixture_path: fixture_path.clone(),
    };

    let first_output = execute_determinism_run(
        &source,
        path,
        timeout_ms,
        &first_mode,
        &recording_dir,
        &record_transcript,
        cli_skill_dirs,
    )
    .await?;
    let second_output = execute_determinism_run(
        &source,
        path,
        timeout_ms,
        &second_mode,
        &replay_dir,
        &replay_transcript,
        cli_skill_dirs,
    )
    .await?;

    if first_output != second_output {
        return Err(format!(
            "{} replay changed stdout\nfirst:\n{}\nsecond:\n{}",
            path.display(),
            first_output,
            second_output
        ));
    }

    let first_runs = load_run_records(recording_dir.path())?;
    let second_runs = load_run_records(replay_dir.path())?;
    let first_responses = load_transcript_responses(record_transcript.path())?;
    let second_responses = load_transcript_responses(replay_transcript.path())?;
    compare_determinism_artifacts(
        path,
        &first_runs,
        &second_runs,
        &first_responses,
        &second_responses,
    )
}

pub(crate) async fn run_determinism_tests(
    path_str: &str,
    filter: Option<&str>,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) {
    let files = collect_user_test_files(path_str).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = Vec::new();

    for path in files {
        let rel_path = path.display().to_string();
        if let Some(pattern) = filter {
            let matched = if let Some(re_pat) = pattern.strip_prefix("re:") {
                Regex::new(re_pat).is_ok_and(|re| re.is_match(&rel_path))
            } else {
                rel_path.contains(pattern)
            };
            if !matched {
                continue;
            }
        }

        match run_determinism_case(&path, timeout_ms, cli_skill_dirs).await {
            Ok(()) => {
                println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                passed += 1;
            }
            Err(error) => {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                failed += 1;
                errors.push(error);
            }
        }
    }

    println!();
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
        println!();
        println!("Failures:");
        for error in errors {
            println!("  {error}");
        }
        process::exit(1);
    }
    println!(
        "\x1b[32m{passed} passed, {failed} failed, {} total\x1b[0m",
        passed + failed
    );
}
