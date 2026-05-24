//! Integration tests for `harn run --json` (issue #1755).
//!
//! Spawns the real binary so we exercise the clap parser, the
//! NDJSON sink installation, and the VM stdout/result plumbing
//! end-to-end.

use std::process::Command;

use harn_cli::commands::run::{json_events::RUN_JSON_SCHEMA_VERSION, RUN_SUMMARY_SCHEMA_VERSION};
use harn_cli::tests::common::json_envelope::assert_envelope;
use serde_json::Value;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    path
}

fn parse_ndjson(stdout: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("ndjson line not valid JSON ({error}): {line}"))
        })
        .collect()
}

fn last_json_line(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("expected at least one stderr line, got: {text:?}"));
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("last stderr line is not valid JSON ({error}): {line}\nfull stderr:\n{text}")
    })
}

fn write_llm_summary_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    write_script(
        dir,
        name,
        r#"
pipeline main(_) {
    llm_mock_clear()
    llm_mock({
      text: "pong",
      input_tokens: 7,
      output_tokens: 5,
      model: "mock",
    })
    let response = llm_call("ping", nil, {provider: "mock"})
    __io_println(response.text)
}
"#,
    )
}

#[test]
fn run_json_emits_monotonic_seq_with_typed_events() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "hello.harn",
        r#"
pipeline main(_) {
    __io_println("hello")
    __io_println("world")
    __io_println("!")
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg(&script)
        // Pin the event-log backend to memory so the run does not
        // touch ~/.harn/event-log; keeps test hermetic.
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = parse_ndjson(&output.stdout);
    assert!(
        lines.len() >= 4,
        "expected ≥4 events (3 stdout + 1 result), got {}:\n{}",
        lines.len(),
        String::from_utf8_lossy(&output.stdout)
    );

    // Every line is a versioned envelope.
    for line in &lines {
        let _ = assert_envelope(line, RUN_JSON_SCHEMA_VERSION);
    }

    // seq is monotonic, contiguous, and starts at 1.
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| {
            line["data"]["seq"]
                .as_u64()
                .unwrap_or_else(|| panic!("data.seq missing on {line}"))
        })
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "seq must be 1..=N contiguous"
    );

    let types: Vec<&str> = lines
        .iter()
        .map(|line| line["data"]["event_type"].as_str().expect("event_type"))
        .collect();

    // First three are stdout events with the println payloads.
    assert_eq!(types[0], "stdout", "first event type: {types:?}");
    assert_eq!(types[1], "stdout");
    assert_eq!(types[2], "stdout");
    assert_eq!(
        lines[0]["data"]["payload"].as_str(),
        Some("hello\n"),
        "{lines:?}"
    );
    assert_eq!(lines[1]["data"]["payload"].as_str(), Some("world\n"));
    assert_eq!(lines[2]["data"]["payload"].as_str(), Some("!\n"));

    // Terminal event is the result.
    let last = lines.last().expect("at least one event");
    assert_eq!(last["data"]["event_type"], "result");
    assert_eq!(last["data"]["exit_code"], 0);
}

#[test]
fn run_emit_summary_json_defaults_to_terminal_stderr_and_reports_llm_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_llm_summary_script(tmp.path(), "summary_llm.harn");

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-summary-json")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --emit-summary-json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "pong\n");

    let summary = last_json_line(&output.stderr);
    assert_eq!(
        summary["schema_version"].as_u64(),
        Some(u64::from(RUN_SUMMARY_SCHEMA_VERSION))
    );
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["exit_code"].as_i64(), Some(0));
    assert!(
        summary["wall_time_ms"].as_u64().is_some(),
        "summary: {summary}"
    );
    assert_eq!(summary["llm"]["call_count"].as_i64(), Some(1));
    assert_eq!(summary["llm"]["input_tokens"].as_i64(), Some(7));
    assert_eq!(summary["llm"]["output_tokens"].as_i64(), Some(5));
    assert!(summary["llm"]["time_ms"].as_i64().is_some());
    assert!(summary["llm"]["cost_usd"].as_f64().is_some());
}

#[test]
fn run_emit_summary_json_keeps_llm_metrics_when_trace_is_rendered() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_llm_summary_script(tmp.path(), "summary_llm_trace.harn");

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--trace")
        .arg("--emit-summary-json")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn traced harn run --emit-summary-json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "pong\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LLM trace"),
        "expected human trace before summary: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary = last_json_line(&output.stderr);
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["llm"]["call_count"].as_i64(), Some(1));
    assert_eq!(summary["llm"]["input_tokens"].as_i64(), Some(7));
    assert_eq!(summary["llm"]["output_tokens"].as_i64(), Some(5));
}

#[test]
fn run_emit_summary_json_file_sink_does_not_change_run_json_stdout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "summary_file.harn",
        r#"
pipeline main(_) {
    __io_println("hello")
}
"#,
    );
    let summary_path = tmp.path().join("run-summary.jsonl");

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg("--emit-summary-json")
        .arg("--summary-file")
        .arg(&summary_path)
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --json --emit-summary-json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "summary-file should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = parse_ndjson(&output.stdout);
    assert_eq!(
        lines.last().unwrap()["data"]["event_type"],
        "result",
        "stdout NDJSON must remain the run event stream: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["data"]["event_type"].as_str() != Some("run_summary")),
        "summary must not be mixed into --json stdout: {lines:?}"
    );

    let summary_text = std::fs::read_to_string(&summary_path).expect("read summary file");
    let summary: Value = serde_json::from_str(summary_text.trim()).expect("summary json");
    assert_eq!(
        summary["schema_version"].as_u64(),
        Some(u64::from(RUN_SUMMARY_SCHEMA_VERSION))
    );
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["exit_code"].as_i64(), Some(0));
}

#[cfg(unix)]
#[test]
fn run_emit_summary_json_fd_sink_writes_inherited_descriptor() {
    use std::os::fd::AsRawFd;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "summary_fd.harn",
        r#"
pipeline main(_) {
    __io_println("fd")
}
"#,
    );
    let summary_path = tmp.path().join("summary-fd.jsonl");
    let summary_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&summary_path)
        .expect("open summary fd");
    let fd = summary_file.as_raw_fd();

    // Rust opens files close-on-exec. Clear the bit around this spawn so the
    // child can exercise the real --summary-fd path without changing stdio.
    // SAFETY: `fd` comes from a live `File`, and `fcntl` does not take ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFD): {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `fd` is still owned by `summary_file`; this only updates its
    // close-on-exec flag until we restore the original flag set below.
    let clear_result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    assert!(
        clear_result >= 0,
        "fcntl(F_SETFD clear CLOEXEC): {}",
        std::io::Error::last_os_error()
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-summary-json")
        .arg("--summary-fd")
        .arg(fd.to_string())
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --summary-fd");

    // SAFETY: `fd` is still open, and restoring the saved flag set preserves
    // the descriptor's ownership and offset.
    let restore_result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
    assert!(
        restore_result >= 0,
        "fcntl(F_SETFD restore): {}",
        std::io::Error::last_os_error()
    );
    drop(summary_file);

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "fd\n");
    assert!(
        output.stderr.is_empty(),
        "--summary-fd should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary_text = std::fs::read_to_string(&summary_path).expect("read summary fd file");
    let summary: Value = serde_json::from_str(summary_text.trim()).expect("summary json");
    assert_eq!(
        summary["schema_version"].as_u64(),
        Some(u64::from(RUN_SUMMARY_SCHEMA_VERSION))
    );
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["exit_code"].as_i64(), Some(0));
}

#[test]
fn run_emit_summary_json_reports_runtime_failure_exit_code() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "summary_failure.harn",
        r#"
pipeline main(_) {
    throw "boom"
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-summary-json")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn failing harn run --emit-summary-json");

    assert_eq!(output.status.code(), Some(1));
    let summary = last_json_line(&output.stderr);
    assert_eq!(
        summary["schema_version"].as_u64(),
        Some(u64::from(RUN_SUMMARY_SCHEMA_VERSION))
    );
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["exit_code"].as_i64(), Some(1));
}

#[test]
fn run_emit_summary_json_reports_compile_failure_exit_code() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "summary_compile_failure.harn",
        r#"
pipeline main(_) {
    let =
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-summary-json")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn compile-failing harn run --emit-summary-json");

    assert_eq!(output.status.code(), Some(1));
    let summary = last_json_line(&output.stderr);
    assert_eq!(
        summary["schema_version"].as_u64(),
        Some(u64::from(RUN_SUMMARY_SCHEMA_VERSION))
    );
    assert_eq!(summary["event"], "run_summary");
    assert_eq!(summary["exit_code"].as_i64(), Some(1));
}

#[test]
fn run_json_quiet_suppresses_stdout_events() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "quiet.harn",
        r#"
pipeline main(_) {
    __io_println("noisy")
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg("--quiet")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --json --quiet");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = parse_ndjson(&output.stdout);
    let types: Vec<&str> = lines
        .iter()
        .map(|line| line["data"]["event_type"].as_str().expect("event_type"))
        .collect();
    assert!(
        !types.contains(&"stdout"),
        "--quiet must drop stdout events: {types:?}"
    );
    // We still expect a terminal `result` event.
    assert_eq!(types.last(), Some(&"result"), "{types:?}");
    // seq stays tight even after filtering.
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["data"]["seq"].as_u64().expect("seq"))
        .collect();
    for window in seqs.windows(2) {
        assert!(
            window[0] < window[1],
            "seq not strictly monotonic: {seqs:?}"
        );
    }
}

#[test]
fn run_json_emits_terminal_error_on_runtime_failure() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Throwing an unhandled error exits with code 1 and surfaces an
    // `error` event on the wire stream.
    let script = write_script(
        tmp.path(),
        "boom.harn",
        r#"
pipeline main(_) {
    throw "boom"
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --json (failing)");

    assert!(
        !output.status.success(),
        "expected non-zero exit, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1));

    let lines = parse_ndjson(&output.stdout);
    let last = lines.last().expect("at least one event on failure");
    assert_eq!(last["data"]["event_type"], "error", "lines: {lines:?}");
    assert_eq!(last["data"]["error"]["code"], "runtime");
    assert!(
        last["data"]["error"]["message"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "error.message must be non-empty: {last}"
    );
}

#[test]
fn json_schemas_includes_run_command() {
    // E2.2 exit criterion: the `run` schema must register in the
    // catalog so agents can discover it.
    let output = Command::new(binary_path())
        .args(["--json-schemas"])
        .output()
        .expect("spawn harn --json-schemas");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("valid JSON catalog");
    let entries = value["data"].as_array().expect("data array");
    let run_entry = entries
        .iter()
        .find(|entry| entry["command"] == "run")
        .unwrap_or_else(|| panic!("`run` not in catalog: {value}"));
    assert_eq!(
        run_entry["schemaVersion"].as_u64(),
        Some(u64::from(RUN_JSON_SCHEMA_VERSION))
    );
}
