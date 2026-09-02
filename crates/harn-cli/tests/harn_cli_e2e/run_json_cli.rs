//! Integration tests for `harn run --json` (issue #1755).
//!
//! Spawns the real binary so we exercise the clap parser, the
//! NDJSON sink installation, and the VM stdout/result plumbing
//! end-to-end.

use std::process::Command;

use harn_cli::commands::run::{
    json_events::RUN_JSON_SCHEMA_VERSION, RUN_PHASE_SCHEMA_VERSION, RUN_RUSAGE_SCHEMA_VERSION,
    RUN_SUMMARY_SCHEMA_VERSION,
};
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

fn read_json_file(path: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(text.trim()).unwrap_or_else(|error| {
        panic!(
            "file is not valid JSON ({error}): {}\n{text}",
            path.display()
        )
    })
}

fn write_llm_summary_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    write_script(
        dir,
        name,
        r#"
pipeline main(harness: Harness) {
    harness.llm.mock_clear()
    harness.llm.mock_enqueue({
      text: "pong",
      input_tokens: 7,
      output_tokens: 5,
      model: "mock",
    })
    const response = harness.llm.call("ping", nil, {provider: "mock"})
    harness.stdio.println(response.text)
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
pipeline main(harness: Harness) {
    harness.stdio.println("hello")
    harness.stdio.println("world")
    harness.stdio.println("!")
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
    assert_eq!(summary["llm"]["provider_call_count"].as_i64(), Some(1));
    assert_eq!(summary["llm"]["input_tokens"].as_i64(), Some(7));
    assert_eq!(summary["llm"]["output_tokens"].as_i64(), Some(5));
    assert!(summary["llm"]["time_ms"].as_i64().is_some());
    assert!(summary["llm"]["cost_usd"].as_f64().is_some());
    // Present unconditionally: without it a reader cannot tell a run that cost
    // nothing from one whose model the catalog prices no rate for.
    assert!(
        summary["llm"]["unpriced_calls"].as_i64().is_some(),
        "summary: {summary}"
    );
    assert!(summary["llm"].get("known_cost_usd").is_some());
    assert_eq!(summary["llm"]["usage_unknown_calls"].as_i64(), Some(0));
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
pipeline main(harness: Harness) {
    harness.stdio.println("hello")
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
pipeline main(harness: Harness) {
    harness.stdio.println("fd")
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
pipeline main(_task: unknown) {
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
        r"
pipeline main(_task: unknown) {
    let =
}
",
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
fn run_emit_phase_json_defaults_to_terminal_stderr_with_fixed_phase_shape() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_dir = tempfile::TempDir::new().expect("cache dir");
    let script = write_script(
        tmp.path(),
        "phase_stderr.harn",
        r#"
pipeline main(harness: Harness) {
    harness.stdio.println("phase")
}
"#,
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-phase-json")
        .arg(&script)
        .env("HARN_CACHE_DIR", cache_dir.path())
        .env("HARN_BYTECODE_CACHE", "1")
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --emit-phase-json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "phase\n");

    let phase = last_json_line(&output.stderr);
    assert_eq!(
        phase["schema_version"].as_u64(),
        Some(u64::from(RUN_PHASE_SCHEMA_VERSION))
    );
    assert_eq!(phase["event"], "run_phase");
    let phases = phase["phases"].as_array().expect("phase rows");
    assert_eq!(phases.len(), 7, "{phase}");
    let names: Vec<&str> = phases
        .iter()
        .map(|row| row["name"].as_str().expect("phase name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "parse",
            "typecheck",
            "bytecode_compile",
            "run_setup",
            "run_main",
            "module_compile",
            "module_load"
        ]
    );
    assert_eq!(phases[2]["cache"], "miss");
    assert!(phases[..5].iter().all(|phase| phase["kind"] == "top_level"));
    assert!(phases[5..]
        .iter()
        .all(|phase| phase["kind"] == "attribution"));
    assert!(phases[4]["events"].as_u64().is_some(), "{phase}");
}

#[test]
fn run_emit_phase_and_rusage_file_sinks_keep_run_json_stdout_clean() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_dir = tempfile::TempDir::new().expect("cache dir");
    let script = write_script(
        tmp.path(),
        "phase_file.harn",
        r#"
pipeline main(harness: Harness) {
    harness.stdio.println("hello")
}
"#,
    );
    let phase_path = tmp.path().join("run-phase.jsonl");
    let rusage_path = tmp.path().join("run-rusage.jsonl");

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg("--emit-phase-json")
        .arg("--phase-file")
        .arg(&phase_path)
        .arg("--emit-rusage-json")
        .arg("--rusage-file")
        .arg(&rusage_path)
        .arg(&script)
        .env("HARN_CACHE_DIR", cache_dir.path())
        .env("HARN_BYTECODE_CACHE", "1")
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --json --emit-phase-json --emit-rusage-json");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "file sinks should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = parse_ndjson(&output.stdout);
    assert_eq!(lines.last().unwrap()["data"]["event_type"], "result");
    assert!(
        lines.iter().all(|line| {
            !matches!(
                line["data"]["event_type"].as_str(),
                Some("run_phase" | "run_rusage")
            )
        }),
        "aux JSON must not be mixed into --json stdout: {lines:?}"
    );

    let phase = read_json_file(&phase_path);
    assert_eq!(phase["event"], "run_phase");
    assert_eq!(
        phase["schema_version"].as_u64(),
        Some(u64::from(RUN_PHASE_SCHEMA_VERSION))
    );
    assert_eq!(phase["phases"].as_array().expect("phases").len(), 7);

    let rusage = read_json_file(&rusage_path);
    assert_eq!(rusage["event"], "run_rusage");
    assert_eq!(
        rusage["schema_version"].as_u64(),
        Some(u64::from(RUN_RUSAGE_SCHEMA_VERSION))
    );
    assert!(rusage["cpu_ms"].as_u64().is_some(), "{rusage}");
}

#[cfg(unix)]
#[test]
fn run_emit_rusage_json_fd_sink_writes_inherited_descriptor() {
    use std::os::fd::AsRawFd;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "rusage_fd.harn",
        r#"
pipeline main(harness: Harness) {
    harness.stdio.println("fd")
}
"#,
    );
    let rusage_path = tmp.path().join("rusage-fd.jsonl");
    let rusage_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&rusage_path)
        .expect("open rusage fd");
    let fd = rusage_file.as_raw_fd();

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFD): {}",
        std::io::Error::last_os_error()
    );
    let clear_result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    assert!(
        clear_result >= 0,
        "fcntl(F_SETFD clear CLOEXEC): {}",
        std::io::Error::last_os_error()
    );

    let output = Command::new(binary_path())
        .arg("run")
        .arg("--emit-rusage-json")
        .arg("--rusage-fd")
        .arg(fd.to_string())
        .arg(&script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn run --rusage-fd");

    let restore_result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
    assert!(
        restore_result >= 0,
        "fcntl(F_SETFD restore): {}",
        std::io::Error::last_os_error()
    );
    drop(rusage_file);

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "fd\n");
    assert!(
        output.stderr.is_empty(),
        "--rusage-fd should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rusage = read_json_file(&rusage_path);
    assert_eq!(rusage["event"], "run_rusage");
    assert_eq!(
        rusage["schema_version"].as_u64(),
        Some(u64::from(RUN_RUSAGE_SCHEMA_VERSION))
    );
    assert!(rusage["cpu_ms"].as_u64().is_some(), "{rusage}");
}

#[test]
fn run_json_quiet_suppresses_stdout_events() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(
        tmp.path(),
        "quiet.harn",
        r#"
pipeline main(harness: Harness) {
    harness.stdio.println("noisy")
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
pipeline main(_task: unknown) {
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

fn run_json_launch_error(script: &std::path::Path) -> (i32, Value) {
    let output = Command::new(binary_path())
        .arg("run")
        .arg("--json")
        .arg(script)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn failing harn run --json");
    let exit_code = output.status.code().expect("run must exit normally");
    let lines = parse_ndjson(&output.stdout);
    let event = lines
        .into_iter()
        .find(|line| line["data"]["event_type"] == "error")
        .unwrap_or_else(|| panic!("missing terminal error event, stderr={:?}", output.stderr));
    (exit_code, event)
}

fn structured_import_class(event: &Value) -> &str {
    assert_eq!(event["data"]["error"]["code"], "compile_error");
    let details = &event["data"]["error"]["details"];
    assert_eq!(details["kind"], "import_failure");
    details["failure_class"]
        .as_str()
        .expect("typed failure_class")
}

fn assert_harn_build_identity(details: &Value) {
    assert_eq!(details["harn_version"], env!("CARGO_PKG_VERSION"));
    let revision = env!("HARN_BUILD_REVISION");
    if revision.is_empty() {
        assert!(details["harn_revision"].is_null());
    } else {
        assert_eq!(details["harn_revision"], revision);
    }
}

#[test]
fn run_json_projects_typed_import_failures_without_message_parsing() {
    let missing_symbol = tempfile::TempDir::new().expect("missing-symbol project");
    write_script(
        missing_symbol.path(),
        "lib.harn",
        "pub fn present() -> int { return 1 }\n",
    );
    let entry = write_script(
        missing_symbol.path(),
        "main.harn",
        "import { absent } from \"./lib\"\npipeline main() { return 0 }\n",
    );
    let (exit_code, event) = run_json_launch_error(&entry);
    assert_eq!(exit_code, 1);
    assert_eq!(structured_import_class(&event), "missing_imported_symbol");
    let details = &event["data"]["error"]["details"];
    assert_eq!(details["module"], "./lib");
    assert_eq!(details["symbol"], "absent");
    assert_eq!(details["source"], "lib.harn");
    assert_harn_build_identity(details);

    let broken_module = tempfile::TempDir::new().expect("broken-module project");
    write_script(
        broken_module.path(),
        "lib.harn",
        "pub fn broken( {\n  return 1\n}\n",
    );
    let entry = write_script(
        broken_module.path(),
        "main.harn",
        "import { broken } from \"./lib\"\npipeline main() { return 0 }\n",
    );
    let (exit_code, event) = run_json_launch_error(&entry);
    assert_eq!(exit_code, 1);
    assert_eq!(
        structured_import_class(&event),
        "imported_module_compile_failure"
    );
    let details = &event["data"]["error"]["details"];
    assert_eq!(details["module"], "./lib");
    assert!(details["symbol"].is_null());
    assert_eq!(details["source"], "lib.harn");
    assert_harn_build_identity(details);

    let unresolved_module = tempfile::TempDir::new().expect("unresolved-module project");
    let entry = write_script(
        unresolved_module.path(),
        "main.harn",
        "import { absent } from \"./missing\"\npipeline main() { return 0 }\n",
    );
    let (exit_code, event) = run_json_launch_error(&entry);
    assert_eq!(exit_code, 1);
    assert_eq!(structured_import_class(&event), "unresolved_module");
    let details = &event["data"]["error"]["details"];
    assert_eq!(details["module"], "./missing");
    assert_eq!(details["symbol"], "absent");
    assert_eq!(details["source"], "main.harn");
    assert_harn_build_identity(details);
}

#[test]
fn run_json_keeps_non_import_launch_failures_out_of_import_classes() {
    let entry_parse = tempfile::TempDir::new().expect("entry-parse project");
    let entry = write_script(
        entry_parse.path(),
        "main.harn",
        "pipeline main( {\n  return 0\n}\n",
    );
    let (exit_code, event) = run_json_launch_error(&entry);
    assert_eq!(exit_code, 1);
    assert_eq!(event["data"]["error"]["code"], "compile_error");
    assert!(event["data"]["error"]["details"].is_null());

    let package_failure = tempfile::TempDir::new().expect("package-failure project");
    let entry = write_script(
        package_failure.path(),
        "main.harn",
        "import { absent } from \"does_not_exist/value\"\npipeline main() { return 0 }\n",
    );
    let (exit_code, event) = run_json_launch_error(&entry);
    assert_eq!(exit_code, harn_cli::exit::RUN_SETUP_FAILURE);
    assert_eq!(event["data"]["error"]["code"], "package_materialization");
    assert!(event["data"]["error"]["details"].is_null());
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
