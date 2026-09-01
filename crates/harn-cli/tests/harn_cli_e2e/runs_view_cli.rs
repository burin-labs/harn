//! End-to-end coverage for `harn runs view --json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn run_harn(args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(args)
        .output()
        .expect("spawn harn")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

fn write_run(path: &Path, id: &str, session_id: &str) {
    fs::write(
        path,
        format!(
            r#"{{
  "_type": "run_record",
  "id": "{id}",
  "workflow_id": "wf",
  "workflow_name": "Workflow",
  "task": "do work",
  "status": "completed",
  "started_at": "2026-01-01T00:00:00Z",
  "finished_at": "2026-01-01T00:00:01Z",
  "stages": [
    {{
      "id": "stage_{id}",
      "node_id": "plan",
      "kind": "llm",
      "status": "completed",
      "outcome": "ok",
      "started_at": "2026-01-01T00:00:00Z",
      "finished_at": "2026-01-01T00:00:01Z",
      "visible_text": "done",
      "metadata": {{"session_id": "{session_id}"}}
    }}
  ],
  "usage": {{
    "input_tokens": 10,
    "output_tokens": 5,
    "total_duration_ms": 1000,
    "call_count": 1,
    "total_cost": 0.01,
    "models": ["model-a"]
  }}
}}"#
        ),
    )
    .unwrap();
}

#[test]
fn runs_view_prints_run_view_json() {
    let temp = TempDir::new().unwrap();
    let run_path = temp.path().join("run.json");
    write_run(&run_path, "run_1", "session_1");

    let output = run_harn(&["runs", "view", run_path.to_str().unwrap(), "--json"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = stdout_json(&output);
    assert_eq!(value["schema"], "harn.run_view.v1");
    assert_eq!(value["run"]["run_id"], "run_1");
    assert_eq!(value["run"]["session_id"], "session_1");
    assert_eq!(value["visible_text"], "done");
    assert!(value["projection"]["projection_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
}

#[test]
fn runs_view_rejects_unowned_execution_evidence_on_the_real_cli_path() {
    let temp = TempDir::new().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(
        &run_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "_type": "run_record",
            "id": "host-run-id",
            "workflow_id": "wf",
            "task": "do work",
            "status": "completed",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:00:01Z",
            "evidence": {
                "schema_version": 1,
                "execution_id": "host-run-id",
                "trace_spans": [{
                    "kind": "llm_call",
                    "name": "untrusted span",
                    "metadata": {"harn.execution.id": "host-run-id"}
                }],
                "flight_recording": {
                    "schema_version": 1,
                    "execution_id": "host-run-id",
                    "format": "harn.flight.v1+json",
                    "path": "/private/untrusted.json",
                    "content_hash": format!("blake3:{}", "a".repeat(64)),
                    "byte_length": 10,
                    "retained_events": 1,
                    "dropped_events": 0,
                    "value_policy": "omitted"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_harn(&["runs", "view", run_path.to_str().unwrap(), "--json"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence = &stdout_json(&output)["evidence"];
    assert!(evidence["execution_id"].is_null());
    assert!(evidence["flight_recording"].is_null());
    assert!(evidence["trace_spans"][0]["metadata"]
        .get("harn.execution.id")
        .is_none());
    assert!(evidence["gaps"].as_array().unwrap().iter().any(|gap| {
        gap["component"] == "execution_identity" && gap["code"] == "projection_invalid"
    }));
}

#[test]
fn runs_view_prints_session_view_json_for_directory() {
    let temp = TempDir::new().unwrap();
    write_run(&temp.path().join("one.json"), "run_1", "session_1");
    write_run(&temp.path().join("two.json"), "run_2", "session_1");

    let output = run_harn(&["runs", "view", temp.path().to_str().unwrap(), "--json"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = stdout_json(&output);
    assert_eq!(value["schema"], "harn.session_view.v1");
    assert_eq!(value["session"]["session_id"], "session_1");
    assert_eq!(value["session"]["run_count"], 2);
    assert_eq!(value["history"].as_array().unwrap().len(), 2);
}

#[test]
fn runs_report_correlates_root_and_child_on_the_real_cli_path() {
    let temp = TempDir::new().unwrap();
    let child_path = temp.path().join("child.json");
    let root_path = temp.path().join("root.json");
    fs::write(
        &child_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "_type": "run_record",
            "id": "child",
            "workflow_id": "child-workflow",
            "task": "child task",
            "status": "completed",
            "started_at": "2026-01-01T00:00:01Z",
            "finished_at": "2026-01-01T00:00:02Z",
            "parent_run_id": "root",
            "root_run_id": "root"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        &root_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "_type": "run_record",
            "id": "root",
            "workflow_id": "root-workflow",
            "task": "root task",
            "status": "completed",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:00:03Z",
            "root_run_id": "root",
            "child_runs": [{
                "worker_id": "worker-1",
                "worker_name": "child",
                "task": "child task",
                "status": "completed",
                "started_at": "2026-01-01T00:00:01Z",
                "finished_at": "2026-01-01T00:00:02Z",
                "run_id": "child",
                "run_path": child_path
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_harn(&["runs", "report", root_path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = stdout_json(&output);
    assert_eq!(value["schema"], "harn.run_report.v1");
    assert_eq!(value["root_run_id"], "root");
    assert_eq!(value["agents"].as_array().unwrap().len(), 2);
    assert_eq!(value["delegations"][0]["forward_pointer"], true);
    assert_eq!(value["delegations"][0]["back_pointer"], true);
}

#[test]
fn runs_report_rejects_malformed_root_record() {
    let temp = TempDir::new().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(&run_path, "not json").unwrap();

    let output = run_harn(&["runs", "report", run_path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to build run report"));
}
