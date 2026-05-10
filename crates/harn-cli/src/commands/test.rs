use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{self, Stdio};

use regex::Regex;
use serde_json::Value;

use crate::commands::run::{
    install_cli_llm_mock_mode, persist_cli_llm_mock_recording, CliLlmMockMode,
};
use crate::env_guard::ScopedEnvVar;
use crate::execute;
use crate::test_runner;

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

fn write_junit_xml(path: &str, results: &[(String, bool, String, u64)]) {
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
    } else {
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
///
/// When any sidecar is present the runner also activates a paused clock
/// (pinned at `CONFORMANCE_TESTBENCH_START_MS`) so clock-advancing
/// replay behaves deterministically.
struct TestbenchSidecarConfig {
    process_tape: Option<PathBuf>,
    fs_overlay: Option<PathBuf>,
    expected_tape: Option<PathBuf>,
    annotations: Option<PathBuf>,
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
    TestbenchSidecarConfig {
        process_tape: process_tape.is_file().then_some(process_tape),
        fs_overlay: fs_overlay.is_dir().then_some(fs_overlay),
        expected_tape: expected_tape.is_file().then_some(expected_tape),
        annotations: annotations.is_file().then_some(annotations),
    }
}

enum ConformanceExecution {
    Completed(Result<String, String>),
    TimedOut,
}

struct ConformanceRun {
    execution: ConformanceExecution,
    duration_ms: u64,
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
) -> Result<ConformanceRun, String> {
    use harn_vm::testbench::{
        ClockConfig, FilesystemConfig, SubprocessConfig, TapeConfig, Testbench,
    };

    harn_vm::reset_thread_local_state();
    install_cli_llm_mock_mode(llm_mock_mode)
        .map_err(|error| format!("llm mock setup error: {error}"))?;

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
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        execute(source, Some(harn_file)),
    )
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
    let sidecar_error: Option<String> = if sidecar_errors.is_empty() {
        None
    } else {
        Some(sidecar_errors.join("; "))
    };

    let execution = match result {
        // Surface the script error first when both happen — sidecar
        // divergences are usually a downstream symptom of a script failure.
        Ok(inner_result) => match (inner_result, sidecar_error) {
            (Err(error), _) => ConformanceExecution::Completed(Err(error)),
            (Ok(_), Some(sidecar_err)) => ConformanceExecution::Completed(Err(sidecar_err)),
            (Ok(output), None) => ConformanceExecution::Completed(Ok(output)),
        },
        Err(_) => ConformanceExecution::TimedOut,
    };
    Ok(ConformanceRun {
        execution,
        duration_ms,
    })
}

async fn verify_unoptimized_conformance_subprocess(
    harn_file: &Path,
    timeout_ms: u64,
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

pub(crate) async fn run_conformance_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    verbose: bool,
    timing: bool,
    differential_optimizations: bool,
) {
    let show_timing = verbose || timing;
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");
    let _force_optimized_parent = if differential_optimizations {
        Some(ScopedEnvVar::unset(harn_vm::HARN_DISABLE_OPTIMIZATIONS_ENV))
    } else {
        None
    };
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        eprintln!("Directory not found: {dir}");
        process::exit(1);
    }
    let suite_root = canonicalize_or_err(&dir_path).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });

    let suite_start = std::time::Instant::now();

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut skipped_summary: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut junit_results: Vec<(String, bool, String, u64)> = Vec::new();

    let harn_files =
        resolve_conformance_selection(&suite_root, selection).unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(1);
        });

    for harn_file in &harn_files {
        let expected_file = harn_file.with_extension("expected");
        let error_file = harn_file.with_extension("error");

        let rel_path = harn_file.strip_prefix(&suite_root).unwrap_or(harn_file);
        let rel_path = logical_path(rel_path);

        // Filter syntax: `re:<regex>`, `foo|bar` (OR), `*_runtime*` (glob),
        // or plain substring match.
        if let Some(pattern) = filter {
            let matched = if let Some(re_pat) = pattern.strip_prefix("re:") {
                Regex::new(re_pat).is_ok_and(|re| re.is_match(&rel_path))
            } else if pattern.contains('|') {
                pattern.split('|').any(|p| rel_path.contains(p.trim()))
            } else if pattern.contains('*') || pattern.contains('?') {
                let escaped = regex::escape(pattern)
                    .replace(r"\*", ".*")
                    .replace(r"\?", ".");
                Regex::new(&escaped).is_ok_and(|re| re.is_match(&rel_path))
            } else {
                rel_path.contains(pattern)
            };
            if !matched {
                continue;
            }
        }

        // Honor `// @xfail: <reason>` markers in the first 50 lines of a
        // conformance test. Skipped tests are reported but do not count as
        // failures. Use sparingly, always with a tracking issue link.
        if let Some(reason) = read_xfail_marker(harn_file) {
            println!("  \x1b[33mSKIP\x1b[0m  {rel_path}  ({reason})");
            skipped_summary.push((rel_path.clone(), reason));
            skipped += 1;
            continue;
        }

        if expected_file.exists() {
            let source = match fs::read_to_string(harn_file) {
                Ok(s) => s,
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading source: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let expected = match fs::read_to_string(&expected_file) {
                Ok(s) => normalize_expected_output(s.trim_end()),
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading expected: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
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
            )
            .await
            {
                Ok(run) => run,
                Err(error) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: {error}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let duration_ms = run.duration_ms;

            match run.execution {
                ConformanceExecution::Completed(Ok(output)) => {
                    let actual = normalize_actual_output(output.trim_end());
                    if actual == expected {
                        if differential_optimizations {
                            if let Err(error) =
                                verify_unoptimized_conformance_subprocess(harn_file, timeout_ms)
                                    .await
                            {
                                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                                let msg = format!("{rel_path}: {error}");
                                errors.push(msg.clone());
                                junit_results.push((rel_path, false, msg, duration_ms));
                                failed += 1;
                                continue;
                            }
                        }
                        if show_timing {
                            println!("  \x1b[32mPASS\x1b[0m  {rel_path} ({duration_ms} ms)");
                        } else {
                            println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                        }
                        junit_results.push((rel_path, true, String::new(), duration_ms));
                        passed += 1;
                    } else {
                        if show_timing {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                        } else {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        }
                        let diff = simple_diff(&expected, &actual);
                        let msg = if verbose {
                            format!(
                                "{rel_path}:\n  expected:\n    {}\n  actual:\n    {}\n  diff:\n{diff}",
                                expected.lines().collect::<Vec<_>>().join("\n    "),
                                actual.lines().collect::<Vec<_>>().join("\n    "),
                            )
                        } else {
                            format!("{rel_path}:\n{diff}")
                        };
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, duration_ms));
                        failed += 1;
                    }
                }
                ConformanceExecution::Completed(Err(e)) => {
                    if verbose {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    }
                    let msg = format!("{rel_path}: runtime error: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                ConformanceExecution::TimedOut => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, timeout_ms));
                    failed += 1;
                }
            }
        } else if error_file.exists() {
            let source = match fs::read_to_string(harn_file) {
                Ok(s) => s,
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading source: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let expected_error = match fs::read_to_string(&error_file) {
                Ok(s) => s.trim_end().to_string(),
                Err(e) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: IO error reading expected error: {e}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
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
            )
            .await
            {
                Ok(run) => run,
                Err(error) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: {error}");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, 0));
                    failed += 1;
                    continue;
                }
            };
            let duration_ms = run.duration_ms;

            match run.execution {
                ConformanceExecution::Completed(Err(ref err))
                    if error_matches(err, &expected_error) =>
                {
                    if differential_optimizations {
                        if let Err(error) =
                            verify_unoptimized_conformance_subprocess(harn_file, timeout_ms).await
                        {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                            let msg = format!("{rel_path}: {error}");
                            errors.push(msg.clone());
                            junit_results.push((rel_path, false, msg, duration_ms));
                            failed += 1;
                            continue;
                        }
                    }
                    if verbose {
                        println!("  \x1b[32mPASS\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                    }
                    junit_results.push((rel_path, true, String::new(), duration_ms));
                    passed += 1;
                }
                ConformanceExecution::Completed(Err(err)) => {
                    if verbose {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path} ({duration_ms} ms)");
                    } else {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    }
                    let msg = format!(
                        "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                    );
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                ConformanceExecution::Completed(Ok(_)) => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!(
                        "{rel_path}: expected error containing '{expected_error}', but succeeded"
                    );
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, duration_ms));
                    failed += 1;
                }
                ConformanceExecution::TimedOut => {
                    println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                    let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                    errors.push(msg.clone());
                    junit_results.push((rel_path, false, msg, timeout_ms));
                    failed += 1;
                }
            }
        }
    }

    let total_duration_ms = suite_start.elapsed().as_millis() as u64;

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
        write_junit_xml(path, &junit_results);
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
) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
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
        execute(source, Some(path)),
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

async fn run_determinism_case(path: &Path, timeout_ms: u64) -> Result<(), String> {
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
    )
    .await?;
    let second_output = execute_determinism_run(
        &source,
        path,
        timeout_ms,
        &second_mode,
        &replay_dir,
        &replay_transcript,
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

pub(crate) async fn run_determinism_tests(path_str: &str, filter: Option<&str>, timeout_ms: u64) {
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

        match run_determinism_case(&path, timeout_ms).await {
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
        match run_determinism_case(&path, timeout_ms).await {
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

    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
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
                let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
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
        collect_harn_files_sorted, logical_path, parse_xfail_marker, resolve_conformance_selection,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTestDir {
        path: PathBuf,
    }

    static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    impl TempTestDir {
        fn new() -> Self {
            let unique = format!(
                "harn-cli-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, "// test").unwrap();
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
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
}
