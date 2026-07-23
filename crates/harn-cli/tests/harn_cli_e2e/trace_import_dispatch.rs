//! `harn trace import` dispatch contract tests.
//!
//! The JSONL→fixture conversion lives in the self-hosted
//! `cli/trace_import.harn` script; the Rust side is only the env-var
//! dispatch shim. These tests drive the real subprocess path so the
//! shipping conversion logic is what gets asserted.

use std::process::Command;

struct SubprocessOutcome {
    stderr: String,
    exit_code: i32,
}

fn run_trace_import(trace: &str, trace_id: Option<&str>) -> (SubprocessOutcome, Option<String>) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let trace_path = dir.path().join("trace.jsonl");
    let output_path = dir.path().join("fixture.jsonl");
    std::fs::write(&trace_path, trace).expect("write trace file");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    // Run inside the temp dir with relative paths: the embedded script's
    // filesystem access is sandbox-scoped to workspace roots derived from
    // the process cwd.
    cmd.current_dir(dir.path())
        .arg("trace")
        .arg("import")
        .arg("--trace-file")
        .arg("trace.jsonl")
        .arg("--output")
        .arg("fixture.jsonl");
    if let Some(id) = trace_id {
        cmd.arg("--trace-id").arg(id);
    }
    let output = cmd.output().expect("spawn harn trace import");
    let outcome = SubprocessOutcome {
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    };
    let fixture = std::fs::read_to_string(&output_path).ok();
    (outcome, fixture)
}

#[test]
fn converts_generic_trace_jsonl_to_cli_fixture() {
    let trace = concat!(
        "{\"trace_id\":\"trace-1\",\"prompt\":\"Question\",\"response\":{\"text\":\"Answer\",\"model\":\"gpt-test\"},\"tool_calls\":[{\"name\":\"read_file\",\"arguments\":{\"path\":\"README.md\"}}]}\n",
        "{\"trace_id\":\"trace-2\",\"prompt\":\"Ignored\",\"response\":\"Nope\"}\n"
    );

    let (outcome, fixture) = run_trace_import(trace, Some("trace-1"));
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);

    let fixture = fixture.expect("fixture file written");
    let fixtures: Vec<&str> = fixture.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(fixtures.len(), 1, "fixture body: {fixture}");
    assert!(
        fixtures[0].contains("\"text\":\"Answer\""),
        "{}",
        fixtures[0]
    );
    assert!(
        fixtures[0].contains("\"model\":\"gpt-test\""),
        "{}",
        fixtures[0]
    );
    assert!(
        fixtures[0].contains("\"name\":\"read_file\""),
        "{}",
        fixtures[0]
    );
}

#[test]
fn rejects_empty_trace_filter_result() {
    let (outcome, _) = run_trace_import(
        "{\"trace_id\":\"trace-1\",\"prompt\":\"Question\",\"response\":\"Answer\"}\n",
        Some("trace-missing"),
    );

    assert_ne!(outcome.exit_code, 0, "expected a non-zero exit");
    assert!(
        outcome.stderr.contains("matched no records"),
        "stderr: {}",
        outcome.stderr
    );
}
