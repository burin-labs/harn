//! End-to-end coverage for `harn runs view --json`.

use std::fs;
use std::path::Path;

use crate::test_util::cli::{run_harn, stdout_json};
use crate::test_util::process::harn_e2e_command;
use tempfile::TempDir;

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
fn runs_inspect_bounds_absolute_and_relative_transcript_descriptors() {
    use harn_vm::orchestration::{
        describe_llm_transcript_sidecar, RunObservabilityRecord, RunRecord,
        RunTranscriptPointerRecord,
    };
    let root = TempDir::new().unwrap();
    let bundle = root.path().join("bundle");
    let inside = bundle.join("inside/llm_transcript.jsonl");
    let outside = root.path().join("outside/llm_transcript.jsonl");
    let body =
        b"{\"type\":\"agent_session_finalized\",\"terminal\":true,\"final_status\":\"done\"}\n";
    for path in [&inside, &outside] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    let cases = vec![
        ("absolute_inside", inside.clone(), None, true, false),
        (
            "relative_inside",
            "inside/llm_transcript.jsonl".into(),
            None,
            true,
            false,
        ),
        (
            "relative_field_inside",
            outside.clone(),
            Some("inside/llm_transcript.jsonl"),
            true,
            false,
        ),
        ("absolute_outside", outside.clone(), None, false, false),
        (
            "relative_parent",
            "../outside/llm_transcript.jsonl".into(),
            None,
            false,
            false,
        ),
        (
            "relative_field_parent",
            inside.clone(),
            Some("../outside/llm_transcript.jsonl"),
            false,
            false,
        ),
        (
            "absolute_relative_field",
            inside.clone(),
            Some(outside.to_str().unwrap()),
            false,
            false,
        ),
        (
            "absolute_parent",
            bundle.join("../outside/llm_transcript.jsonl"),
            None,
            false,
            false,
        ),
        (
            "missing_absolute_parent",
            bundle.join("../outside/missing.jsonl"),
            None,
            false,
            false,
        ),
        (
            "missing_inside",
            bundle.join("missing.jsonl"),
            None,
            false,
            false,
        ),
        ("relative_run_path", inside.clone(), None, true, true),
    ];
    #[cfg(unix)]
    let cases = {
        let mut cases = cases;
        let inside_link = bundle.join("inside-link.jsonl");
        let outside_link = bundle.join("outside-link.jsonl");
        std::os::unix::fs::symlink(&inside, &inside_link).unwrap();
        std::os::unix::fs::symlink(&outside, &outside_link).unwrap();
        cases.push(("inside_symlink", inside_link, None, true, false));
        cases.push(("outside_symlink", outside_link, None, false, false));
        cases
    };
    assert_eq!(cases.len(), if cfg!(unix) { 13 } else { 11 });
    for (name, supplied, relative, expected, relative_run_path) in cases {
        let run_path = bundle.join(format!("{name}.json"));
        let mut run = RunRecord {
            id: "descriptor-path-probe".to_string(),
            status: "completed".to_string(),
            ..Default::default()
        };
        let mut descriptor = describe_llm_transcript_sidecar(&run, &run_path, &inside).unwrap();
        descriptor.path = supplied.to_string_lossy().into_owned();
        descriptor.relative_path = relative.map(str::to_string);
        run.observability = Some(RunObservabilityRecord {
            transcript_pointers: vec![RunTranscriptPointerRecord {
                kind: "llm_jsonl".to_string(),
                available: true,
                verification_status: "verified".to_string(),
                descriptor: Some(descriptor),
                ..Default::default()
            }],
            ..Default::default()
        });
        fs::write(&run_path, serde_json::to_vec(&run).unwrap()).unwrap();
        let argument = if relative_run_path {
            run_path.strip_prefix(root.path()).unwrap()
        } else {
            &run_path
        };
        let output = harn_e2e_command()
            .current_dir(root.path())
            .args(["runs", "inspect"])
            .arg(argument)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Status: completed"), "{name}: {stdout}");
        let prefix = "- transcript LLM transcript sidecar [llm_jsonl] available=";
        let rows: Vec<_> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .collect();
        assert_eq!(rows.len(), 1, "{name}: {stdout}");
        let actual = rows[0].split_once(' ').unwrap().0;
        assert_eq!(
            actual,
            if expected { "true" } else { "false" },
            "{name}: {stdout}"
        );
    }
}

#[test]
fn run_record_failures_use_the_cli_diagnostic_sink() {
    let temp = TempDir::new().unwrap();
    let invalid = temp.path().join("invalid.json");
    fs::write(&invalid, "not a run record").unwrap();

    for path in [&invalid, &temp.path().join("missing.json")] {
        let output = run_harn(&["runs", "inspect", path.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("[harn] error:"), "{stderr}");
        assert!(stderr.contains("failed to load run record"), "{stderr}");
    }
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
    assert_eq!(value["schema"], "harn.run_report.v2");
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
