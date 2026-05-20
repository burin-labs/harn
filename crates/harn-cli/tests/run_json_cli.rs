//! Integration tests for `harn run --json` (issue #1755).
//!
//! Spawns the real binary so we exercise the clap parser, the
//! NDJSON sink installation, and the VM stdout/result plumbing
//! end-to-end.

use std::process::Command;

use harn_cli::commands::run::json_events::RUN_JSON_SCHEMA_VERSION;
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
