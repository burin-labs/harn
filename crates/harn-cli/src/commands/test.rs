use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{self, Stdio};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::run::{
    install_cli_llm_mock_mode, persist_cli_llm_mock_recording, CliLlmMockMode,
};
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{self, JsonEnvelope, JsonError};
use crate::test_runner;
use crate::{execute_with_skill_dirs, execute_with_skill_dirs_and_harness};

pub(crate) const CONFORMANCE_TEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConformanceJsonOutcome {
    Pass,
    Fail,
    XfailExpected,
    XfailUnexpectedPass,
}

#[derive(Debug, Clone, Serialize)]
struct ConformanceJsonResult {
    name: String,
    outcome: ConformanceJsonOutcome,
    duration_ms: u64,
    message: Option<String>,
    diagnostic_codes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ConformanceJsonSummary {
    pass: u64,
    fail: u64,
    xfail_expected: u64,
    xfail_unexpected_pass: u64,
    skipped: u64,
}

impl ConformanceJsonSummary {
    fn record(&mut self, outcome: ConformanceJsonOutcome) {
        match outcome {
            ConformanceJsonOutcome::Pass => self.pass += 1,
            ConformanceJsonOutcome::Fail => self.fail += 1,
            ConformanceJsonOutcome::XfailExpected => self.xfail_expected += 1,
            ConformanceJsonOutcome::XfailUnexpectedPass => self.xfail_unexpected_pass += 1,
        }
    }

    fn is_success(&self) -> bool {
        self.fail == 0 && self.xfail_unexpected_pass == 0
    }
}

#[derive(Debug, Clone, Serialize)]
struct ConformanceJsonReport {
    #[serde(rename = "snapshotKey")]
    snapshot_key: String,
    results: Vec<ConformanceJsonResult>,
    summary: ConformanceJsonSummary,
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

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_junit_xml(path: &str, results: &[(String, bool, String, u64)], announce: bool) {
    let total = results.len();
    let failures = results.iter().filter(|r| !r.1).count();
    let total_time: f64 = results.iter().map(|r| r.3 as f64 / 1000.0).sum();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"harn\" tests=\"{total}\" failures=\"{failures}\" time=\"{total_time:.3}\">\n"
    ));
    for (name, passed, error_msg, duration_ms) in results {
        let time = *duration_ms as f64 / 1000.0;
        let escaped_name = xml_escape(name);
        xml.push_str(&format!(
            "  <testcase name=\"{escaped_name}\" time=\"{time:.3}\""
        ));
        if *passed {
            xml.push_str(" />\n");
        } else {
            xml.push_str(">\n");
            let escaped = xml_escape(error_msg);
            xml.push_str(&format!(
                "    <failure message=\"test failed\">{escaped}</failure>\n"
            ));
            xml.push_str("  </testcase>\n");
        }
    }
    xml.push_str("</testsuite>\n");

    if let Err(e) = fs::write(path, &xml) {
        eprintln!("Failed to write JUnit XML to {path}: {e}");
    } else if announce {
        println!("JUnit XML written to {path}");
    }
}

fn collect_harn_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    super::collect_harn_files(dir, &mut files);
    files
}

fn conformance_llm_mock_mode(harn_file: &Path) -> CliLlmMockMode {
    let fixture = harn_file.with_extension("llm-mock.jsonl");
    if fixture.is_file() {
        CliLlmMockMode::Replay {
            fixture_path: fixture,
        }
    } else {
        CliLlmMockMode::Off
    }
}

/// Testbench sidecar activation config for a single conformance test.
///
/// Sidecars are optional files adjacent to the `.harn` test:
/// - `<name>.process-tape.json` → subprocess replay tape
/// - `<name>.fs-overlay/` → filesystem overlay root
/// - `<name>.testbench-tape` → expected event tape for fidelity check
/// - `<name>.annotations.jsonl` → annotation sidecar; runner validates
///   against the emitted event tape
/// - `<name>.harness.json` → install a `Harness::null()` / `Harness::mock()`
///   test handle and assert recorded calls or deny events
///
/// When any testbench sidecar is present the runner also activates a paused
/// clock (pinned at `CONFORMANCE_TESTBENCH_START_MS`) so clock-advancing replay
/// behaves deterministically.
struct TestbenchSidecarConfig {
    process_tape: Option<PathBuf>,
    fs_overlay: Option<PathBuf>,
    expected_tape: Option<PathBuf>,
    annotations: Option<PathBuf>,
    harness: Option<PathBuf>,
}

impl TestbenchSidecarConfig {
    fn is_empty(&self) -> bool {
        self.process_tape.is_none()
            && self.fs_overlay.is_none()
            && self.expected_tape.is_none()
            && self.annotations.is_none()
    }
}

fn conformance_testbench_config(harn_file: &Path) -> TestbenchSidecarConfig {
    let process_tape = harn_file.with_extension("process-tape.json");
    let fs_overlay = harn_file.with_extension("fs-overlay");
    let expected_tape = harn_file.with_extension("testbench-tape");
    let annotations = harn_file.with_extension("annotations.jsonl");
    let harness = harn_file.with_extension("harness.json");
    TestbenchSidecarConfig {
        process_tape: process_tape.is_file().then_some(process_tape),
        fs_overlay: fs_overlay.is_dir().then_some(fs_overlay),
        expected_tape: expected_tape.is_file().then_some(expected_tape),
        annotations: annotations.is_file().then_some(annotations),
        harness: harness.is_file().then_some(harness),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HarnessSidecar {
    mode: HarnessSidecarMode,
    #[serde(default)]
    clock_at_unix_ms: Option<i64>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    fs_reads: BTreeMap<String, String>,
    #[serde(default)]
    net_gets: BTreeMap<String, String>,
    #[serde(default)]
    random_u64: Vec<u64>,
    #[serde(default)]
    expect_calls: Vec<HarnessEventExpectation>,
    #[serde(default)]
    expect_deny_events: Vec<HarnessEventExpectation>,
    #[serde(default)]
    expect_stdio: Option<String>,
    #[serde(default)]
    expect_stderr: Option<String>,
    #[serde(default)]
    stdin_lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HarnessSidecarMode {
    Null,
    Mock,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HarnessEventExpectation {
    sub_handle: String,
    method: String,
    #[serde(default)]
    args: Vec<String>,
}

impl HarnessSidecar {
    fn load(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|error| format!("read harness sidecar {}: {error}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|error| format!("parse harness sidecar {}: {error}", path.display()))
    }

    fn build_harness(&self) -> harn_vm::Harness {
        match self.mode {
            HarnessSidecarMode::Null => harn_vm::Harness::null(),
            HarnessSidecarMode::Mock => {
                let mut builder = harn_vm::Harness::mock();
                if let Some(unix_ms) = self.clock_at_unix_ms {
                    builder = builder.clock_at_unix_ms(unix_ms);
                }
                for (key, value) in &self.env {
                    builder = builder.env(key.as_str(), value.as_str());
                }
                for (path, value) in &self.fs_reads {
                    builder = builder.fs_read(path.as_str(), value.as_bytes().to_vec());
                }
                for (url, body) in &self.net_gets {
                    builder = builder.net_get(url.as_str(), body.as_str());
                }
                for value in &self.random_u64 {
                    builder = builder.random_u64(*value);
                }
                for line in &self.stdin_lines {
                    builder = builder.stdin_line(line.as_str());
                }
                builder.build()
            }
        }
    }

    fn validate(&self, harness: &harn_vm::Harness) -> Vec<String> {
        let mut errors = Vec::new();
        if !self.expect_calls.is_empty() {
            let actual = harness
                .calls()
                .into_iter()
                .map(event_from_call)
                .collect::<Vec<_>>();
            if actual != self.expect_calls {
                errors.push(format!(
                    "harness calls differed: expected {:?}, actual {:?}",
                    self.expect_calls, actual
                ));
            }
        }
        if !self.expect_deny_events.is_empty() {
            let actual = harness
                .deny_events()
                .into_iter()
                .map(event_from_deny)
                .collect::<Vec<_>>();
            if actual != self.expect_deny_events {
                errors.push(format!(
                    "harness deny events differed: expected {:?}, actual {:?}",
                    self.expect_deny_events, actual
                ));
            }
        }
        if let Some(expected) = &self.expect_stdio {
            let actual = harness.captured_stdio();
            if &actual != expected {
                errors.push(format!(
                    "harness captured stdio differed: expected {:?}, actual {:?}",
                    expected, actual
                ));
            }
        }
        if let Some(expected) = &self.expect_stderr {
            let actual = harness.captured_stderr();
            if &actual != expected {
                errors.push(format!(
                    "harness captured stderr differed: expected {:?}, actual {:?}",
                    expected, actual
                ));
            }
        }
        errors
    }
}

fn event_from_call(call: harn_vm::HarnessCall) -> HarnessEventExpectation {
    HarnessEventExpectation {
        sub_handle: harness_kind_name(call.sub_handle).to_string(),
        method: call.method,
        args: call.args,
    }
}

fn event_from_deny(event: harn_vm::DenyEvent) -> HarnessEventExpectation {
    HarnessEventExpectation {
        sub_handle: harness_kind_name(event.sub_handle).to_string(),
        method: event.method,
        args: event.args,
    }
}

fn harness_kind_name(kind: harn_vm::HarnessKind) -> &'static str {
    kind.field_name().unwrap_or("root")
}

enum ConformanceExecution {
    Completed(Result<String, String>),
    TimedOut,
}

struct ConformanceRun {
    execution: ConformanceExecution,
    duration_ms: u64,
    sidecar_error: Option<String>,
}

/// Pinned testbench clock start, same constant the CLI uses, so
/// conformance fixtures and CLI invocations are interchangeable.
const CONFORMANCE_TESTBENCH_START_MS: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

async fn execute_conformance_source(
    source: &str,
    harn_file: &Path,
    timeout_ms: u64,
    llm_mock_mode: &CliLlmMockMode,
    testbench: &TestbenchSidecarConfig,
    cli_skill_dirs: &[PathBuf],
) -> Result<ConformanceRun, String> {
    use harn_vm::testbench::{
        ClockConfig, FilesystemConfig, SubprocessConfig, TapeConfig, Testbench,
    };

    harn_vm::reset_thread_local_state();
    install_cli_llm_mock_mode(llm_mock_mode)
        .map_err(|error| format!("llm mock setup error: {error}"))?;
    let harness_sidecar = match testbench.harness.as_ref() {
        Some(path) => Some(HarnessSidecar::load(path)?),
        None => None,
    };
    let harness = harness_sidecar.as_ref().map(HarnessSidecar::build_harness);
    let harness_for_validation = harness.clone();

    // Activate testbench axes for any present sidecars. A paused clock is
    // included whenever any sidecar is active so subprocess duration_ms and
    // overlay timestamps stay deterministic.
    let tape_temp_dir = if testbench.expected_tape.is_some() || testbench.annotations.is_some() {
        Some(tempfile::tempdir().map_err(|e| format!("tempdir for tape: {e}"))?)
    } else {
        None
    };
    let tape_path = tape_temp_dir
        .as_ref()
        .map(|dir| dir.path().join("run.tape"));

    let bench = if !testbench.is_empty() {
        let clock = ClockConfig::Paused {
            starting_at_ms: CONFORMANCE_TESTBENCH_START_MS,
        };
        let subprocess = match &testbench.process_tape {
            Some(tape) => SubprocessConfig::Replay { tape: tape.clone() },
            None => SubprocessConfig::Real,
        };
        let filesystem = match &testbench.fs_overlay {
            Some(root) => FilesystemConfig::Overlay {
                worktree: root.clone(),
            },
            None => FilesystemConfig::Real,
        };
        let tape_cfg = match &tape_path {
            Some(path) => TapeConfig::Emit {
                path: path.clone(),
                argv: Vec::new(),
                script_path: Some(harn_file.to_string_lossy().into_owned()),
            },
            None => TapeConfig::Off,
        };
        Some(
            Testbench {
                clock,
                llm: harn_vm::testbench::LlmConfig::Real,
                filesystem,
                subprocess,
                network: harn_vm::testbench::NetworkConfig::Real,
                tape: tape_cfg,
            }
            .activate()
            .map_err(|e| format!("testbench activate: {e}"))?,
        )
    } else {
        None
    };

    let start = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
        match harness {
            Some(harness) => {
                execute_with_skill_dirs_and_harness(
                    source,
                    Some(harn_file),
                    cli_skill_dirs,
                    harness,
                )
                .await
            }
            None => execute_with_skill_dirs(source, Some(harn_file), cli_skill_dirs).await,
        }
    })
    .await;
    let duration_ms = start.elapsed().as_millis() as u64;
    harn_vm::llm::clear_cli_llm_mock_mode();

    // Finalize the testbench session before comparing tapes.
    if let Some(session) = bench {
        session
            .finalize()
            .map_err(|e| format!("testbench finalize: {e}"))?;
    }

    // Post-run tape fidelity check: compare emitted tape to the expected fixture.
    let mut sidecar_errors: Vec<String> = Vec::new();
    let actual_tape = match (&tape_path, &testbench.expected_tape) {
        (Some(tape_path), Some(expected_path)) => {
            use harn_vm::testbench::fidelity::{compare, FidelityMode};
            use harn_vm::testbench::tape::EventTape;
            match (EventTape::load(tape_path), EventTape::load(expected_path)) {
                (Ok(actual), Ok(expected)) => {
                    let report = compare(&expected, &actual, FidelityMode::ByteIdentical);
                    if !report.is_byte_identical() {
                        sidecar_errors.push(format!(
                            "tape fidelity: {} divergence(s) vs {}",
                            report.divergences.len(),
                            expected_path.display()
                        ));
                    }
                    Some(actual)
                }
                (Err(e), _) => {
                    sidecar_errors.push(format!("load emitted tape: {e}"));
                    None
                }
                (_, Err(e)) => {
                    sidecar_errors.push(format!(
                        "load expected tape {}: {e}",
                        expected_path.display()
                    ));
                    None
                }
            }
        }
        (Some(tape_path), None) => {
            use harn_vm::testbench::tape::EventTape;
            match EventTape::load(tape_path) {
                Ok(tape) => Some(tape),
                Err(e) => {
                    sidecar_errors.push(format!("load emitted tape: {e}"));
                    None
                }
            }
        }
        _ => None,
    };
    if let (Some(annotations_path), Some(actual)) = (&testbench.annotations, actual_tape.as_ref()) {
        use harn_vm::testbench::annotations::{validate_against_tape, AnnotationTape};
        match AnnotationTape::load(annotations_path) {
            Ok(annotations) => {
                let report = validate_against_tape(&annotations, actual);
                if !report.is_ok() {
                    sidecar_errors.push(format!(
                        "annotations: {} problem(s) in {}",
                        report.problems.len(),
                        annotations_path.display()
                    ));
                }
            }
            Err(e) => sidecar_errors.push(format!(
                "load annotations {}: {e}",
                annotations_path.display()
            )),
        }
    }
    if let (Some(sidecar), Some(harness)) = (&harness_sidecar, &harness_for_validation) {
        sidecar_errors.extend(sidecar.validate(harness));
    }
    let sidecar_error: Option<String> = if sidecar_errors.is_empty() {
        None
    } else {
        Some(sidecar_errors.join("; "))
    };

    let execution = match result {
        Ok(inner_result) => ConformanceExecution::Completed(inner_result),
        Err(_) => ConformanceExecution::TimedOut,
    };
    Ok(ConformanceRun {
        execution,
        duration_ms,
        sidecar_error,
    })
}

async fn verify_unoptimized_conformance_subprocess(
    harn_file: &Path,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) -> Result<u64, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current harn executable: {error}"))?;
    let start = std::time::Instant::now();
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("test")
        .arg("conformance")
        .arg(harn_file)
        .arg("--timeout")
        .arg(timeout_ms.to_string())
        .env(harn_vm::HARN_DISABLE_OPTIMIZATIONS_ENV, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for dir in cli_skill_dirs {
        command.arg("--skill-dir").arg(dir);
    }

    let wait_timeout = std::time::Duration::from_millis(timeout_ms.saturating_add(2_000));
    let output = match tokio::time::timeout(wait_timeout, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(format!("unoptimized subprocess launch failed: {error}"));
        }
        Err(_) => {
            return Err(format!(
                "unoptimized subprocess timed out after {}ms",
                wait_timeout.as_millis()
            ));
        }
    };
    let duration_ms = start.elapsed().as_millis() as u64;
    if output.status.success() {
        return Ok(duration_ms);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut message = format!(
        "unoptimized subprocess exited with status {}",
        output.status
    );
    if !stdout.trim().is_empty() {
        message.push_str("\nstdout:\n");
        message.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        message.push_str("\nstderr:\n");
        message.push_str(stderr.trim_end());
    }
    Err(message)
}

fn canonicalize_or_err(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("Failed to canonicalize {}: {error}", path.display()))
}

/// Look for `// @xfail: <reason>` in the first 50 lines of a conformance
/// test source file. Returns the reason if present.
fn read_xfail_marker(path: &Path) -> Option<String> {
    let source = fs::read_to_string(path).ok()?;
    parse_xfail_marker(&source)
}

fn parse_xfail_marker(source: &str) -> Option<String> {
    // Accept the marker in any of these comment forms within the first 50 lines:
    //   // @xfail: reason
    //   /** @xfail: reason */
    //   /**
    //    * @xfail: reason
    //    */
    // The Harn formatter sometimes converts a leading `//` comment that
    // precedes a `fn` or `pipeline` declaration into a `/** ... */` doc
    // comment, so we tolerate both shapes.
    for line in source.lines().take(50) {
        let mut s = line.trim_start();
        if let Some(rest) = s.strip_prefix("//") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("/**") {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else if let Some(rest) = s.strip_prefix('*') {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else {
            continue;
        }
        let s = s.trim();
        if let Some(reason) = s.strip_prefix("@xfail:") {
            let r = reason.trim().trim_end_matches("*/").trim();
            if !r.is_empty() {
                return Some(r.to_string());
            }
        }
    }
    None
}

fn resolve_conformance_selection(
    suite_root: &Path,
    selection: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let suite_root = canonicalize_or_err(suite_root)?;

    let Some(selection) = selection else {
        return Ok(collect_harn_files_sorted(&suite_root));
    };

    let raw = PathBuf::from(selection);
    let mut candidates = vec![raw.clone()];
    if !raw.is_absolute() && !raw.starts_with(&suite_root) {
        candidates.push(suite_root.join(&raw));
    }

    let Some(candidate) = candidates.into_iter().find(|path| path.exists()) else {
        return Err(format!(
            "Conformance target not found: {selection}. Expected a file or directory under {}",
            suite_root.display()
        ));
    };

    let canonical = canonicalize_or_err(&candidate)?;
    if !canonical.starts_with(&suite_root) {
        return Err(format!(
            "Conformance target must be inside {}: {}",
            suite_root.display(),
            candidate.display()
        ));
    }

    if canonical.is_file() {
        if canonical.extension().is_some_and(|ext| ext == "harn") {
            return Ok(vec![canonical]);
        }
        return Err(format!(
            "Conformance target must be a .harn file or directory: {}",
            candidate.display()
        ));
    }

    let files = collect_harn_files_sorted(&canonical);
    if files.is_empty() {
        return Err(format!(
            "No .harn conformance tests found under {}",
            candidate.display()
        ));
    }
    Ok(files)
}

fn conformance_filter_matches(rel_path: &str, filter: Option<&str>) -> bool {
    let Some(pattern) = filter else {
        return true;
    };
    if let Some(re_pat) = pattern.strip_prefix("re:") {
        Regex::new(re_pat).is_ok_and(|re| re.is_match(rel_path))
    } else if pattern.contains('|') {
        pattern.split('|').any(|p| rel_path.contains(p.trim()))
    } else if pattern.contains('*') || pattern.contains('?') {
        let escaped = regex::escape(pattern)
            .replace(r"\*", ".*")
            .replace(r"\?", ".");
        Regex::new(&escaped).is_ok_and(|re| re.is_match(rel_path))
    } else {
        rel_path.contains(pattern)
    }
}

#[derive(Debug, Clone)]
struct ConformanceCaseEvaluation {
    passed: bool,
    message: Option<String>,
    diagnostic_codes: Vec<String>,
    duration_ms: u64,
}

impl ConformanceCaseEvaluation {
    fn pass(duration_ms: u64) -> Self {
        Self {
            passed: true,
            message: None,
            diagnostic_codes: Vec::new(),
            duration_ms,
        }
    }

    fn fail(message: impl Into<String>, duration_ms: u64) -> Self {
        let message = message.into();
        Self {
            passed: false,
            diagnostic_codes: extract_diagnostic_codes(&message),
            message: Some(message),
            duration_ms,
        }
    }
}

fn extract_diagnostic_codes(message: &str) -> Vec<String> {
    let re = Regex::new(r"\bHARN-[A-Z0-9]+(?:-[A-Z0-9]+)*-[0-9]{3}\b")
        .expect("diagnostic code regex compiles");
    let mut codes = BTreeSet::new();
    for capture in re.find_iter(message) {
        codes.insert(capture.as_str().to_string());
    }
    codes.into_iter().collect()
}

fn target_triple_label() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown-target"
    }
}

fn hash_file_if_present(hasher: &mut blake3::Hasher, suite_root: &Path, path: &Path) {
    if !path.is_file() {
        return;
    }
    hasher.update(b"file\0");
    let rel = path.strip_prefix(suite_root).unwrap_or(path);
    hasher.update(logical_path(rel).as_bytes());
    hasher.update(b"\0");
    match fs::read(path) {
        Ok(bytes) => hasher.update(&bytes),
        Err(error) => hasher.update(format!("read-error:{error}").as_bytes()),
    };
    hasher.update(b"\0");
}

fn hash_dir_if_present(hasher: &mut blake3::Hasher, suite_root: &Path, path: &Path) {
    if !path.is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_files_recursive(path, &mut files);
    files.sort();
    for file in files {
        hash_file_if_present(hasher, suite_root, &file);
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_files_recursive(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
}

fn conformance_snapshot_key(suite_root: &Path, selected_files: &[(PathBuf, String)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\0");
    hasher.update(target_triple_label().as_bytes());
    hasher.update(b"\0");
    match harn_vm::orchestration::current_provider_catalog_hash_blake3() {
        Ok(hash) => hasher.update(hash.as_bytes()),
        Err(error) => hasher.update(format!("provider-catalog-error:{error}").as_bytes()),
    };
    hasher.update(b"\0");

    for (harn_file, rel_path) in selected_files {
        hasher.update(b"test\0");
        hasher.update(rel_path.as_bytes());
        hasher.update(b"\0");
        hash_file_if_present(&mut hasher, suite_root, harn_file);
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("expected"),
        );
        hash_file_if_present(&mut hasher, suite_root, &harn_file.with_extension("error"));
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("llm-mock.jsonl"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("process-tape.json"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("testbench-tape"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("annotations.jsonl"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("harness.json"),
        );
        hash_dir_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("fs-overlay"),
        );
    }

    hasher.finalize().to_hex().to_string()
}

pub(crate) struct ConformanceRunOptions<'a> {
    pub(crate) verbose: bool,
    pub(crate) timing: bool,
    pub(crate) differential_optimizations: bool,
    pub(crate) json: bool,
    pub(crate) cli_skill_dirs: &'a [PathBuf],
}

async fn evaluate_conformance_case(
    harn_file: &Path,
    expected_file: &Path,
    error_file: &Path,
    rel_path: &str,
    timeout_ms: u64,
    options: &ConformanceRunOptions<'_>,
) -> ConformanceCaseEvaluation {
    if expected_file.exists() {
        let source = match fs::read_to_string(harn_file) {
            Ok(s) => s,
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading source: {e}"),
                    0,
                );
            }
        };
        let expected = match fs::read_to_string(expected_file) {
            Ok(s) => normalize_expected_output(s.trim_end()),
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading expected: {e}"),
                    0,
                );
            }
        };

        let llm_mock_mode = conformance_llm_mock_mode(harn_file);
        let testbench_config = conformance_testbench_config(harn_file);
        let run = match execute_conformance_source(
            &source,
            harn_file,
            timeout_ms,
            &llm_mock_mode,
            &testbench_config,
            options.cli_skill_dirs,
        )
        .await
        {
            Ok(run) => run,
            Err(error) => {
                return ConformanceCaseEvaluation::fail(format!("{rel_path}: {error}"), 0);
            }
        };
        let duration_ms = run.duration_ms;
        if let Some(sidecar_error) = run.sidecar_error {
            return ConformanceCaseEvaluation::fail(
                format!("{rel_path}: {sidecar_error}"),
                duration_ms,
            );
        }

        return match run.execution {
            ConformanceExecution::Completed(Ok(output)) => {
                let actual = normalize_actual_output(output.trim_end());
                if actual == expected {
                    if options.differential_optimizations {
                        if let Err(error) = verify_unoptimized_conformance_subprocess(
                            harn_file,
                            timeout_ms,
                            options.cli_skill_dirs,
                        )
                        .await
                        {
                            return ConformanceCaseEvaluation::fail(
                                format!("{rel_path}: {error}"),
                                duration_ms,
                            );
                        }
                    }
                    ConformanceCaseEvaluation::pass(duration_ms)
                } else {
                    let diff = simple_diff(&expected, &actual);
                    let msg = if options.verbose {
                        format!(
                            "{rel_path}:\n  expected:\n    {}\n  actual:\n    {}\n  diff:\n{diff}",
                            expected.lines().collect::<Vec<_>>().join("\n    "),
                            actual.lines().collect::<Vec<_>>().join("\n    "),
                        )
                    } else {
                        format!("{rel_path}:\n{diff}")
                    };
                    ConformanceCaseEvaluation::fail(msg, duration_ms)
                }
            }
            ConformanceExecution::Completed(Err(e)) => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: runtime error: {e}"),
                duration_ms,
            ),
            ConformanceExecution::TimedOut => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: timed out after {timeout_ms}ms"),
                timeout_ms,
            ),
        };
    }

    if error_file.exists() {
        let source = match fs::read_to_string(harn_file) {
            Ok(s) => s,
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading source: {e}"),
                    0,
                );
            }
        };
        let expected_error = match fs::read_to_string(error_file) {
            Ok(s) => s.trim_end().to_string(),
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading expected error: {e}"),
                    0,
                );
            }
        };

        let llm_mock_mode = conformance_llm_mock_mode(harn_file);
        let testbench_config = conformance_testbench_config(harn_file);
        let run = match execute_conformance_source(
            &source,
            harn_file,
            timeout_ms,
            &llm_mock_mode,
            &testbench_config,
            options.cli_skill_dirs,
        )
        .await
        {
            Ok(run) => run,
            Err(error) => {
                return ConformanceCaseEvaluation::fail(format!("{rel_path}: {error}"), 0);
            }
        };
        let duration_ms = run.duration_ms;
        if let Some(sidecar_error) = run.sidecar_error {
            return ConformanceCaseEvaluation::fail(
                format!("{rel_path}: {sidecar_error}"),
                duration_ms,
            );
        }

        return match run.execution {
            ConformanceExecution::Completed(Err(ref err)) if error_matches(err, &expected_error) => {
                if options.differential_optimizations {
                    if let Err(error) = verify_unoptimized_conformance_subprocess(
                        harn_file,
                        timeout_ms,
                        options.cli_skill_dirs,
                    )
                    .await
                    {
                        return ConformanceCaseEvaluation::fail(
                            format!("{rel_path}: {error}"),
                            duration_ms,
                        );
                    }
                }
                ConformanceCaseEvaluation::pass(duration_ms)
            }
            ConformanceExecution::Completed(Err(err)) => ConformanceCaseEvaluation::fail(
                format!(
                    "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                ),
                duration_ms,
            ),
            ConformanceExecution::Completed(Ok(_)) => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: expected error containing '{expected_error}', but succeeded"),
                duration_ms,
            ),
            ConformanceExecution::TimedOut => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: timed out after {timeout_ms}ms"),
                timeout_ms,
            ),
        };
    }

    ConformanceCaseEvaluation::fail(format!("{rel_path}: missing .expected or .error file"), 0)
}

pub(crate) async fn run_conformance_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    options: ConformanceRunOptions<'_>,
) {
    let show_timing = options.verbose || options.timing;
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");
    let _force_optimized_parent = if options.differential_optimizations {
        Some(ScopedEnvVar::unset(harn_vm::HARN_DISABLE_OPTIMIZATIONS_ENV))
    } else {
        None
    };
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        if options.json {
            let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                CONFORMANCE_TEST_SCHEMA_VERSION,
                "conformance_directory_not_found",
                format!("Directory not found: {dir}"),
            );
            println!("{}", json_envelope::to_string_pretty(&envelope));
        } else {
            eprintln!("Directory not found: {dir}");
        }
        process::exit(1);
    }
    let suite_root = match canonicalize_or_err(&dir_path) {
        Ok(path) => path,
        Err(error) => {
            if options.json {
                let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                    CONFORMANCE_TEST_SCHEMA_VERSION,
                    "conformance_directory_error",
                    error,
                );
                println!("{}", json_envelope::to_string_pretty(&envelope));
            } else {
                eprintln!("{error}");
            }
            process::exit(1);
        }
    };

    let suite_start = std::time::Instant::now();

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut skipped_summary: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut json_results: Vec<ConformanceJsonResult> = Vec::new();
    let mut json_summary = ConformanceJsonSummary::default();
    let mut junit_results: Vec<(String, bool, String, u64)> = Vec::new();

    let harn_files = match resolve_conformance_selection(&suite_root, selection) {
        Ok(files) => files,
        Err(error) => {
            if options.json {
                let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                    CONFORMANCE_TEST_SCHEMA_VERSION,
                    "conformance_selection_error",
                    error,
                );
                println!("{}", json_envelope::to_string_pretty(&envelope));
            } else {
                eprintln!("{error}");
            }
            process::exit(1);
        }
    };

    let selected_harn_files: Vec<(PathBuf, String)> = harn_files
        .into_iter()
        .filter_map(|harn_file| {
            let rel_path = harn_file.strip_prefix(&suite_root).unwrap_or(&harn_file);
            let rel_path = logical_path(rel_path);
            conformance_filter_matches(&rel_path, filter).then_some((harn_file, rel_path))
        })
        .collect();

    for (harn_file, rel_path) in &selected_harn_files {
        let expected_file = harn_file.with_extension("expected");
        let error_file = harn_file.with_extension("error");

        // Honor `// @xfail: <reason>` markers in the first 50 lines of a
        // conformance test. Text mode preserves the historical skip behavior.
        // JSON mode executes the test so stale markers become
        // `xfail_unexpected_pass` failures that force marker cleanup.
        let xfail_reason = read_xfail_marker(harn_file);
        if !options.json {
            if let Some(reason) = xfail_reason.as_ref() {
                println!("  \x1b[33mSKIP\x1b[0m  {rel_path}  ({reason})");
                skipped_summary.push((rel_path.clone(), reason.clone()));
                skipped += 1;
                continue;
            }
        }

        if !expected_file.exists() && !error_file.exists() {
            continue;
        }

        let evaluation = evaluate_conformance_case(
            harn_file,
            &expected_file,
            &error_file,
            rel_path,
            timeout_ms,
            &options,
        )
        .await;

        if options.json {
            let outcome = match (&xfail_reason, evaluation.passed) {
                (Some(_), true) => ConformanceJsonOutcome::XfailUnexpectedPass,
                (Some(_), false) => ConformanceJsonOutcome::XfailExpected,
                (None, true) => ConformanceJsonOutcome::Pass,
                (None, false) => ConformanceJsonOutcome::Fail,
            };
            json_summary.record(outcome);

            let message = match (
                outcome,
                xfail_reason.as_deref(),
                evaluation.message.as_deref(),
            ) {
                (ConformanceJsonOutcome::XfailUnexpectedPass, Some(reason), _) => {
                    Some(format!("xfail marker is stale: {reason}"))
                }
                (ConformanceJsonOutcome::XfailExpected, Some(reason), Some(message)) => {
                    Some(format!("expected failure ({reason}): {message}"))
                }
                (ConformanceJsonOutcome::XfailExpected, Some(reason), None) => {
                    Some(format!("expected failure ({reason})"))
                }
                (_, _, Some(message)) => Some(message.to_string()),
                _ => None,
            };

            let junit_passed = matches!(
                outcome,
                ConformanceJsonOutcome::Pass | ConformanceJsonOutcome::XfailExpected
            );
            junit_results.push((
                rel_path.clone(),
                junit_passed,
                if junit_passed {
                    String::new()
                } else {
                    message.clone().unwrap_or_default()
                },
                evaluation.duration_ms,
            ));
            json_results.push(ConformanceJsonResult {
                name: rel_path.clone(),
                outcome,
                duration_ms: evaluation.duration_ms,
                message,
                diagnostic_codes: evaluation.diagnostic_codes,
            });
            continue;
        }

        if evaluation.passed {
            if show_timing {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {rel_path} ({} ms)",
                    evaluation.duration_ms
                );
            } else {
                println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
            }
            junit_results.push((
                rel_path.clone(),
                true,
                String::new(),
                evaluation.duration_ms,
            ));
            passed += 1;
        } else {
            if show_timing {
                println!(
                    "  \x1b[31mFAIL\x1b[0m  {rel_path} ({} ms)",
                    evaluation.duration_ms
                );
            } else {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
            }
            let msg = evaluation
                .message
                .unwrap_or_else(|| format!("{rel_path}: failed without diagnostic message"));
            errors.push(msg.clone());
            junit_results.push((rel_path.clone(), false, msg, evaluation.duration_ms));
            failed += 1;
        }
    }

    let total_duration_ms = suite_start.elapsed().as_millis() as u64;

    if options.json {
        if let Some(path) = junit_path {
            write_junit_xml(path, &junit_results, false);
        }
        let snapshot_key = conformance_snapshot_key(&suite_root, &selected_harn_files);
        let ok = json_summary.is_success();
        let error = (!ok).then(|| JsonError {
            code: "conformance_failed".to_string(),
            message: "one or more conformance tests failed or unexpectedly passed an xfail marker"
                .to_string(),
            details: serde_json::json!({
                "fail": json_summary.fail,
                "xfail_unexpected_pass": json_summary.xfail_unexpected_pass,
            }),
        });
        let envelope = JsonEnvelope {
            schema_version: CONFORMANCE_TEST_SCHEMA_VERSION,
            ok,
            data: Some(ConformanceJsonReport {
                snapshot_key,
                results: json_results,
                summary: json_summary,
            }),
            error,
            warnings: Vec::new(),
        };
        println!("{}", json_envelope::to_string_pretty(&envelope));
        if !ok {
            process::exit(1);
        }
        return;
    }

    println!();
    let total = passed + failed + skipped;
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    } else {
        println!(
            "\x1b[32m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    }

    if !skipped_summary.is_empty() {
        println!();
        println!("Skipped (xfail):");
        for (path, reason) in &skipped_summary {
            println!("  {path}  ({reason})");
        }
    }

    if show_timing {
        println!();
        println!("Total time: {total_duration_ms} ms");

        let mut durations: Vec<u64> = junit_results.iter().map(|r| r.3).collect();
        durations.sort();

        if !durations.is_empty() {
            let n = durations.len();
            let p50 = durations[n * 50 / 100];
            let p95 = durations[n * 95 / 100];
            let p99 = durations[(n * 99 / 100).min(n - 1)];
            let avg = durations.iter().sum::<u64>() / n as u64;
            println!("Per-test: avg={avg} ms  p50={p50} ms  p95={p95} ms  p99={p99} ms");
        }

        let mut by_time: Vec<&(String, bool, String, u64)> = junit_results.iter().collect();
        by_time.sort_by_key(|entry| std::cmp::Reverse(entry.3));
        let top_n = by_time.len().min(10);
        if top_n > 0 {
            println!();
            println!("Slowest {top_n} tests:");
            for entry in &by_time[..top_n] {
                println!("  {:>6} ms  {}", entry.3, entry.0);
            }
        }
    }

    if let Some(path) = junit_path {
        write_junit_xml(path, &junit_results, true);
    }

    if !errors.is_empty() {
        println!();
        println!("Failures:");
        for err in &errors {
            println!("  {err}");
        }
        process::exit(1);
    }
}

fn print_test_results(summary: &test_runner::TestSummary) {
    let file_count = summary
        .results
        .iter()
        .map(|r| r.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    if summary.total > 0 {
        println!(
            "Running {} test{} from {} file{}...\n",
            summary.total,
            if summary.total == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
        );
    }

    for result in &summary.results {
        if result.passed {
            println!(
                "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                result.name, result.file, result.duration_ms
            );
        } else {
            println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
            if let Some(err) = &result.error {
                for line in err.lines() {
                    println!("        {line}");
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
}

pub(crate) async fn run_user_tests(
    path_str: &str,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel, cli_skill_dirs).await;
    print_test_results(&summary);
    if summary.failed > 0 {
        process::exit(1);
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
        Ok(Err(error)) => Err(error),
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

pub(crate) async fn run_conformance_determinism_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) {
    let dir_path = PathBuf::from(dir);
    let suite_root = canonicalize_or_err(&dir_path).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let files = resolve_conformance_selection(&suite_root, selection).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = Vec::new();

    for path in files {
        let rel_path = path.strip_prefix(&suite_root).unwrap_or(&path);
        let rel_path = logical_path(rel_path);
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

pub(crate) async fn run_watch_tests(
    path_str: &str,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
) {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }

    println!("Watching {path_str} for changes... (Ctrl+C to stop)\n");

    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel, cli_skill_dirs).await;
    print_test_results(&summary);

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap_or_else(|e| {
        eprintln!("Failed to create file watcher: {e}");
        process::exit(1);
    });
    watcher
        .watch(&path, RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!("Failed to watch {path_str}: {e}");
            process::exit(1);
        });

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let is_harn = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|e| e == "harn"));
                if !is_harn {
                    continue;
                }

                // Debounce: drain any additional events within 100ms.
                while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}

                println!("\n\x1b[2m--- file changed, re-running tests ---\x1b[0m\n");
                let summary =
                    test_runner::run_tests(&path, filter, timeout_ms, parallel, cli_skill_dirs)
                        .await;
                print_test_results(&summary);
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_harn_files_sorted, evaluate_conformance_case, logical_path, parse_xfail_marker,
        resolve_conformance_selection, ConformanceRunOptions,
    };
    use std::fs;
    use std::path::Path;

    struct TempTestDir {
        dir: tempfile::TempDir,
    }

    impl TempTestDir {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("harn-cli-test-")
                .tempdir()
                .unwrap();
            Self { dir }
        }

        fn write(&self, relative: &str) {
            self.write_content(relative, "// test");
        }

        fn write_content(&self, relative: &str, content: &str) {
            let path = self.dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    #[test]
    fn collect_harn_files_sorted_descends_and_sorts() {
        let temp = TempTestDir::new();
        temp.write("suite/zeta.harn");
        temp.write("suite/alpha.harn");
        temp.write("suite/nested/beta.harn");
        fs::write(temp.path().join("suite/ignore.txt"), "").unwrap();

        let files = collect_harn_files_sorted(&temp.path().join("suite"));
        let relative: Vec<String> = files
            .iter()
            .map(|path| logical_path(path.strip_prefix(temp.path()).unwrap()))
            .collect();

        assert_eq!(
            relative,
            vec![
                "suite/alpha.harn",
                "suite/nested/beta.harn",
                "suite/zeta.harn"
            ]
        );
    }

    #[test]
    fn logical_path_uses_slashes_for_native_test_paths() {
        let path = Path::new("suite").join("nested").join("beta.harn");

        assert_eq!(logical_path(&path), "suite/nested/beta.harn");
    }

    #[test]
    fn resolve_conformance_selection_accepts_suite_relative_file() {
        let temp = TempTestDir::new();
        temp.write("conformance/tests/sample.harn");

        let files = resolve_conformance_selection(
            &temp.path().join("conformance"),
            Some("tests/sample.harn"),
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("conformance/tests/sample.harn"));
    }

    #[test]
    fn resolve_conformance_selection_rejects_paths_outside_suite_root() {
        let temp = TempTestDir::new();
        temp.write("conformance/tests/sample.harn");
        temp.write("outside.harn");

        let error = resolve_conformance_selection(
            &temp.path().join("conformance"),
            Some("../outside.harn"),
        )
        .unwrap_err();

        assert!(error.contains("must be inside"));
    }

    #[test]
    fn parse_xfail_marker_recognizes_top_of_file_marker() {
        let src = "// @xfail: tracked in #1240\npipeline main(task) {}\n";
        assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1240"));
    }

    #[test]
    fn parse_xfail_marker_recognizes_indented_marker() {
        let src = "    // @xfail: skill matching #1240\n";
        assert_eq!(
            parse_xfail_marker(src).as_deref(),
            Some("skill matching #1240")
        );
    }

    #[test]
    fn parse_xfail_marker_returns_none_when_absent() {
        let src = "// regular comment\npipeline main(task) {}\n";
        assert!(parse_xfail_marker(src).is_none());
    }

    #[test]
    fn parse_xfail_marker_ignores_marker_past_first_50_lines() {
        let mut src = String::new();
        for _ in 0..60 {
            src.push_str("// filler\n");
        }
        src.push_str("// @xfail: too late\n");
        assert!(parse_xfail_marker(&src).is_none());
    }

    #[test]
    fn parse_xfail_marker_ignores_empty_reason() {
        let src = "// @xfail:   \n";
        assert!(parse_xfail_marker(src).is_none());
    }

    #[test]
    fn parse_xfail_marker_recognizes_one_line_doc_comment() {
        let src = "/** @xfail: tracked in #1240 */\npipeline test() {}\n";
        assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1240"));
    }

    #[test]
    fn parse_xfail_marker_recognizes_multi_line_doc_comment() {
        let src = "/**\n * @xfail: tracked in #1238\n */\nfn foo() {}\n";
        assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1238"));
    }

    #[test]
    fn parse_xfail_marker_recognizes_block_comment() {
        let src = "/* @xfail: tracked in #1239 */\nfn foo() {}\n";
        assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1239"));
    }

    #[tokio::test]
    async fn conformance_harness_sidecar_error_fails_expected_error_fixture() {
        let temp = TempTestDir::new();
        temp.write_content(
            "conformance/tests/harness_sidecar_error.harn",
            r#"fn main(harness: Harness) {
  harness.env.get("TOKEN")
}
"#,
        );
        temp.write_content(
            "conformance/tests/harness_sidecar_error.error",
            "NullHarness denied",
        );
        temp.write_content(
            "conformance/tests/harness_sidecar_error.harness.json",
            r#"{
  "mode": "null",
  "expect_deny_events": [
    {
      "sub_handle": "env",
      "method": "wrong",
      "args": ["TOKEN"]
    }
  ]
}
"#,
        );

        let harn_file = temp
            .path()
            .join("conformance/tests/harness_sidecar_error.harn");
        let expected_file = harn_file.with_extension("expected");
        let error_file = harn_file.with_extension("error");
        let options = ConformanceRunOptions {
            verbose: false,
            timing: false,
            differential_optimizations: false,
            json: false,
            cli_skill_dirs: &[],
        };

        let evaluation = evaluate_conformance_case(
            &harn_file,
            &expected_file,
            &error_file,
            "tests/harness_sidecar_error.harn",
            2_000,
            &options,
        )
        .await;

        assert!(!evaluation.passed);
        let message = evaluation.message.unwrap_or_default();
        assert!(
            message.contains("harness deny events differed"),
            "unexpected message: {message}"
        );
    }
}
