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
