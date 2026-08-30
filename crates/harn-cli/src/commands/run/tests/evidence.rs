use std::collections::HashSet;

use super::super::{
    execute_run_with_options, CliLlmMockMode, FlightRecorderOptions, ProjectRuntimeMode,
    RunExecutionOptions, RunProfileOptions,
};

#[tokio::test]
async fn canonical_run_persists_one_identity_across_record_spans_and_flight_artifact() {
    let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
    harn_vm::reset_thread_local_state();
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("main.harn");
    let flight_path = temp.path().join("flight.json");
    std::fs::write(
        &script,
        r#"pipeline main(harness: Harness) {
  const secret = "do-not-record-me"
  if secret == "do-not-record-me" { return 0 }
  return 9
}"#,
    )
    .unwrap();

    let outcome = execute_run_with_options(
        script.to_str().unwrap(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunExecutionOptions {
            project_runtime: ProjectRuntimeMode::Standalone,
            flight_recorder: FlightRecorderOptions {
                enabled: true,
                out: Some(flight_path.clone()),
                max_events: 512,
                retain_files: 2,
            },
            ..RunExecutionOptions::default()
        },
    )
    .await;
    assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
    assert!(flight_path.is_file());

    let run_root = harn_vm::runtime_paths::run_root(temp.path());
    let run_paths = std::fs::read_dir(&run_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    assert_eq!(run_paths.len(), 1, "run paths: {run_paths:?}");
    let run = harn_vm::orchestration::load_run_record(&run_paths[0]).unwrap();
    assert!(run.id.starts_with("hxe-"));
    assert_eq!(run.evidence.execution_id.as_deref(), Some(run.id.as_str()));
    assert_eq!(run.status, "completed");
    assert!(run.evidence.trace_spans.iter().any(|span| {
        span.kind == "pipeline"
            && span
                .metadata
                .get(harn_vm::tracing::meta::EXECUTION_ID)
                .and_then(serde_json::Value::as_str)
                == Some(run.id.as_str())
    }));
    assert!(run.evidence.trace_spans.iter().all(|span| {
        span.metadata
            .get(harn_vm::tracing::meta::EXECUTION_ID)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|execution_id| execution_id == run.id)
    }));
    let artifact = run.evidence.flight_recording.unwrap();
    assert_eq!(artifact.execution_id, run.id);
    assert_eq!(artifact.path, flight_path.to_string_lossy());
    assert!(artifact.retained_events > 0);
    let flight_bytes = std::fs::read(&flight_path).unwrap();
    assert_eq!(artifact.byte_length, flight_bytes.len() as u64);
    assert_eq!(
        artifact.content_hash,
        format!("blake3:{}", blake3::hash(&flight_bytes).to_hex())
    );
    assert!(!String::from_utf8(flight_bytes)
        .unwrap()
        .contains("do-not-record-me"));

    let disabled = execute_run_with_options(
        script.to_str().unwrap(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunExecutionOptions {
            project_runtime: ProjectRuntimeMode::Standalone,
            ..RunExecutionOptions::default()
        },
    )
    .await;
    assert_eq!(disabled.exit_code, 0, "{}", disabled.stderr);
    let records = std::fs::read_dir(&run_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| harn_vm::orchestration::load_run_record(&entry.path()).ok())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .filter(|run| run.evidence.flight_recording.is_some())
            .count(),
        1
    );
    assert_ne!(records[0].id, records[1].id);
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn cli_agent_workflow_and_automatic_record_share_one_execution_identity() {
    let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
    harn_vm::reset_thread_local_state();
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("agent_workflow.harn");
    let run_root = harn_vm::runtime_paths::run_root(temp.path());
    let workflow_path = run_root.join("workflow.json");
    let quoted_workflow_path = serde_json::to_string(&workflow_path.to_string_lossy()).unwrap();
    std::fs::write(
        &script,
        format!(
            r#"import {{ workflow_execute }} from "std/workflow/execute"

pipeline main(harness: Harness) {{
  const flow = workflow_graph({{
    name: "evidence-agent",
    entry: "act",
    nodes: {{
      act: {{kind: "stage", mode: "llm", model_policy: {{provider: "mock"}}}},
    }},
    edges: [],
  }})
  const result = workflow_execute(
    harness,
    "Prove execution evidence correlation",
    flow,
    [],
    {{max_steps: 2, persist_path: {quoted_workflow_path}}},
  )
  harness.stdio.println(result?.status)
}}"#
        ),
    )
    .unwrap();

    let outcome = execute_run_with_options(
        script.to_str().unwrap(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunExecutionOptions {
            project_runtime: ProjectRuntimeMode::Standalone,
            ..RunExecutionOptions::default()
        },
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.lines().any(|line| line == "completed"));
    let workflow = harn_vm::orchestration::load_run_record(&workflow_path).unwrap();
    assert!(workflow.id.starts_with("run_"));
    assert_eq!(workflow.status, "completed");

    let automatic = std::fs::read_dir(&run_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("hxe-") && name.ends_with(".json"))
        })
        .map(|entry| harn_vm::orchestration::load_run_record(&entry.path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(automatic.len(), 1, "automatic records: {automatic:?}");
    let automatic = &automatic[0];
    assert_eq!(automatic.status, "completed");
    assert_eq!(
        workflow.evidence.execution_id,
        automatic.evidence.execution_id
    );
    assert_eq!(
        workflow.evidence.execution_id.as_deref(),
        Some(automatic.id.as_str())
    );
    assert!(
        !workflow.evidence.trace_spans.is_empty(),
        "workflow record did not capture any trace spans"
    );
    assert!(workflow.evidence.trace_spans.iter().all(|span| {
        span.metadata
            .get(harn_vm::tracing::meta::EXECUTION_ID)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|execution_id| execution_id == automatic.id)
    }));
    harn_vm::reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn flight_persist_failure_leaves_a_durable_evidence_gap() {
    let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
    harn_vm::reset_thread_local_state();
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("main.harn");
    let directory_instead_of_file = temp.path().join("not-a-flight-file");
    std::fs::create_dir(&directory_instead_of_file).unwrap();
    std::fs::write(&script, "fn main(harness: Harness) { return 0 }").unwrap();

    let outcome = execute_run_with_options(
        script.to_str().unwrap(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunExecutionOptions {
            project_runtime: ProjectRuntimeMode::Standalone,
            flight_recorder: FlightRecorderOptions {
                enabled: true,
                out: Some(directory_instead_of_file),
                max_events: 32,
                retain_files: 1,
            },
            ..RunExecutionOptions::default()
        },
    )
    .await;

    assert_ne!(outcome.exit_code, 0);
    assert!(outcome
        .stderr
        .contains("failed to persist flight recording"));
    let run_root = harn_vm::runtime_paths::run_root(temp.path());
    let records = std::fs::read_dir(run_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| harn_vm::orchestration::load_run_record(&entry.path()).ok())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let run = &records[0];
    assert_eq!(run.status, "failed");
    assert!(run.evidence.flight_recording.is_none());
    assert_eq!(run.evidence.gaps.len(), 1);
    assert_eq!(run.evidence.gaps[0].component, "flight_recording");
    assert_eq!(run.evidence.gaps[0].code, "persist_failed");
    harn_vm::reset_thread_local_state();
}
