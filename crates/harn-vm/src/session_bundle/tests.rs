use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::orchestration::{
    LlmUsageRecord, RunChildRecord, RunObservabilityRecord, RunStageRecord,
    RunTranscriptPointerRecord, RunVerificationOutcomeRecord, RunWorkerLineageRecord,
};

fn repo_fixture_path(name: &str) -> String {
    format!(
        "{}/../../spec/session-bundles/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn fixture_run() -> RunRecord {
    let synthetic_api_key = format!("{}{}", "sk-test_", "1234567890abcdefghijklmnop");
    let synthetic_bearer = format!("{} {}", "Bearer", "abcDEFghi12345");
    RunRecord {
        type_name: "run_record".to_string(),
        id: "run_123".to_string(),
        workflow_id: "wf_123".to_string(),
        workflow_name: Some("Review".to_string()),
        task: "Review the failing deployment".to_string(),
        status: "completed".to_string(),
        started_at: "2026-05-01T00:00:00Z".to_string(),
        finished_at: Some("2026-05-01T00:01:00Z".to_string()),
        transcript: Some(json!({
            "_type": "transcript",
            "messages": [
                {"role": "user", "content": format!("token {synthetic_api_key}")}
            ],
            "events": [
                {"kind": "permission_denied", "reason": "policy", "arguments": {"api_key": "abc"}},
                {"type": "tool_schemas", "schemas": [{"name": "read_file"}]}
            ],
            "assets": [],
            "summary": "done",
            "metadata": {"system_prompt": "be careful"}
        })),
        usage: Some(LlmUsageRecord {
            input_tokens: 10,
            output_tokens: 4,
            call_count: 1,
            total_duration_ms: 100,
            total_cost: 0.01,
            models: vec!["mock/model".to_string()],
        }),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "answer".to_string(),
            transcript: Some(json!({
                "messages": [{"role": "assistant", "content": "done"}],
                "events": [],
                "assets": []
            })),
            ..RunStageRecord::default()
        }],
        tool_recordings: vec![ToolCallRecord {
            tool_name: "read_file".to_string(),
            tool_use_id: "toolu_1".to_string(),
            args_hash: "abc123".to_string(),
            result: synthetic_bearer,
            is_rejected: false,
            duration_ms: 7,
            iteration: 1,
            timestamp: "2026-05-01T00:00:30Z".to_string(),
        }],
        hitl_questions: vec![RunHitlQuestionRecord {
            request_id: "hitl_1".to_string(),
            prompt: "Approve?".to_string(),
            agent: "agent".to_string(),
            trace_id: None,
            asked_at: "2026-05-01T00:00:10Z".to_string(),
        }],
        ..RunRecord::default()
    }
}

fn fixture_observability() -> RunObservabilityRecord {
    RunObservabilityRecord {
        schema_version: 4,
        verification_outcomes: vec![RunVerificationOutcomeRecord {
            stage_id: "stage_1".to_string(),
            node_id: "answer".to_string(),
            status: "completed".to_string(),
            passed: Some(true),
            summary: Some("verification passed with private details".to_string()),
        }],
        transcript_pointers: vec![RunTranscriptPointerRecord {
            id: "transcript_1".to_string(),
            label: "LLM transcript".to_string(),
            kind: "llm_jsonl".to_string(),
            location: "run-llm/llm_transcript.jsonl".to_string(),
            path: Some("/private/harn/run_123/run-llm/llm_transcript.jsonl".to_string()),
            available: true,
        }],
        ..RunObservabilityRecord::default()
    }
}

fn fixture_replay_fixture(run: &RunRecord) -> ReplayFixture {
    ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: "fixture_run_123".to_string(),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        created_at: "2026-05-01T00:00:30Z".to_string(),
        eval_kind: Some("replay".to_string()),
        expected_status: run.status.clone(),
        ..ReplayFixture::default()
    }
}

fn fixture_run_with_worker_snapshot(root: &Path) -> (RunRecord, PathBuf) {
    let mut run = fixture_run();
    let snapshot_path = root.join("workers").join("worker_1.json");
    fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
    fs::write(
        &snapshot_path,
        serde_json::to_string_pretty(&json!({
            "_type": "worker_snapshot",
            "id": "worker_1",
            "name": "sub-agent",
            "task": "continue portable work",
            "status": "suspended",
            "snapshot_path": snapshot_path.to_string_lossy(),
            "config": {
                "mode": "sub_agent",
                "spec": {
                    "name": "sub-agent",
                    "task": "continue portable work",
                    "session_id": "session-child",
                    "parent_session_id": "session-parent"
                }
            },
            "suspension": {
                "reason": "operator",
                "initiator": "operator",
                "suspended_at": "2026-05-01T00:00:20Z",
                "snapshot_ref": snapshot_path.to_string_lossy()
            }
        }))
        .unwrap(),
    )
    .unwrap();
    run.child_runs = vec![RunChildRecord {
        worker_id: "worker_1".to_string(),
        worker_name: "sub-agent".to_string(),
        task: "continue portable work".to_string(),
        status: "suspended".to_string(),
        started_at: "2026-05-01T00:00:00Z".to_string(),
        session_id: Some("session-child".to_string()),
        parent_session_id: Some("session-parent".to_string()),
        snapshot_path: Some(snapshot_path.to_string_lossy().into_owned()),
        ..RunChildRecord::default()
    }];
    run.observability = Some(RunObservabilityRecord {
        worker_lineage: vec![RunWorkerLineageRecord {
            worker_id: "worker_1".to_string(),
            worker_name: "sub-agent".to_string(),
            task: "continue portable work".to_string(),
            status: "suspended".to_string(),
            session_id: Some("session-child".to_string()),
            parent_session_id: Some("session-parent".to_string()),
            snapshot_path: Some(snapshot_path.to_string_lossy().into_owned()),
            ..RunWorkerLineageRecord::default()
        }],
        ..RunObservabilityRecord::default()
    });
    (run, snapshot_path)
}

#[test]
fn sanitized_export_redacts_secrets_and_records_manifest() {
    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    let rendered = serde_json::to_string(&bundle).unwrap();
    assert!(!rendered.contains("1234567890abcdefghijklmnop"));
    assert!(!rendered.contains("abcDEFghi12345"));
    assert!(rendered.contains(REDACTED_PLACEHOLDER));
    assert!(bundle
        .redaction
        .entries
        .iter()
        .any(|entry| entry.class == "secret_pattern_or_url"));
    assert_eq!(bundle.transcript.sections.len(), 2);
    assert_eq!(bundle.tools.schemas.len(), 1);
    assert_eq!(bundle.permissions.len(), 2);
}

#[test]
fn replay_only_export_withholds_prompt_and_tool_payloads() {
    let options = SessionBundleExportOptions {
        mode: SessionBundleExportMode::ReplayOnly,
        ..SessionBundleExportOptions::default()
    };
    let bundle = export_run_record_bundle(&fixture_run(), &options).unwrap();
    let rendered = serde_json::to_string(&bundle).unwrap();
    assert!(!rendered.contains("Review the failing deployment"));
    assert!(!rendered.contains("done"));
    assert!(rendered.contains(REPLAY_ONLY_PLACEHOLDER));
    assert!(bundle
        .redaction
        .entries
        .iter()
        .any(|entry| entry.action == "withheld"));
}

#[test]
fn observability_exports_in_replay_envelope_and_imports_without_embedded_run_record() {
    let mut run = fixture_run();
    run.replay_fixture = Some(fixture_replay_fixture(&run));
    run.observability = Some(fixture_observability());

    let mut bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();

    let observability = bundle
        .replay
        .observability
        .as_ref()
        .expect("bundle replay observability");
    assert_eq!(observability.schema_version, 4);
    assert_eq!(observability.verification_outcomes.len(), 1);
    assert_eq!(
        observability.verification_outcomes[0].node_id.as_str(),
        "answer"
    );
    assert_eq!(bundle.replay.verification_outcomes.len(), 1);
    assert_eq!(
        bundle.replay.verification_outcomes[0].node_id.as_str(),
        "answer"
    );
    assert_eq!(observability.transcript_pointers.len(), 1);

    bundle.replay.run_record = None;
    let imported = import_run_record_value(&bundle).unwrap();
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["passed"],
        json!(true)
    );
    assert_eq!(
        imported["observability"]["transcript_pointers"][0]["path"],
        json!("/private/harn/run_123/run-llm/llm_transcript.jsonl")
    );
}

#[test]
fn import_backfills_observability_from_first_class_verification_outcomes() {
    let mut run = fixture_run();
    run.observability = Some(fixture_observability());

    let mut bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();
    bundle.replay.observability = None;
    bundle
        .replay
        .run_record
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("observability");

    let imported = import_run_record_value(&bundle).unwrap();
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["summary"],
        json!("verification passed with private details")
    );
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["passed"],
        json!(true)
    );
}

#[test]
fn export_derives_first_class_verification_outcomes_when_observability_is_missing() {
    let mut run = fixture_run();
    run.stages[0].kind = "verify".to_string();
    run.stages[0].status = "completed".to_string();
    run.stages[0].outcome = "success".to_string();
    run.stages[0].verification = Some(json!({"pass": true, "summary": "tests passed"}));

    let mut bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();

    assert!(bundle.replay.observability.is_none());
    assert_eq!(bundle.replay.verification_outcomes.len(), 1);
    assert_eq!(bundle.replay.verification_outcomes[0].passed, Some(true));

    bundle.replay.run_record = None;
    bundle.replay.replay_fixture = Some(fixture_replay_fixture(&run));
    let imported = import_run_record_value(&bundle).unwrap();
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["summary"],
        json!("{\"pass\":true,\"summary\":\"tests passed\"}")
    );
}

#[test]
fn replay_fixture_import_preserves_first_class_verification_outcomes() {
    let mut run = fixture_run();
    run.replay_fixture = Some(fixture_replay_fixture(&run));
    run.observability = Some(fixture_observability());

    let mut bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();
    bundle.replay.run_record = None;
    bundle.replay.observability = None;

    let imported = import_run_record_value(&bundle).unwrap();
    assert_eq!(imported["observability"]["schema_version"], json!(4));
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["node_id"],
        json!("answer")
    );
}

#[test]
fn import_backfills_observability_from_replay_envelope_when_run_record_lacks_it() {
    let mut run = fixture_run();
    run.observability = Some(fixture_observability());

    let mut bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();
    bundle.replay.run_record.as_mut().unwrap()["observability"] = JsonValue::Null;

    let imported = import_run_record_value(&bundle).unwrap();
    assert_eq!(
        imported["observability"]["verification_outcomes"][0]["summary"],
        json!("verification passed with private details")
    );
}

#[test]
fn sanitized_export_redacts_replay_observability_pointer_paths() {
    let mut run = fixture_run();
    run.observability = Some(fixture_observability());

    let bundle = export_run_record_bundle(&run, &SessionBundleExportOptions::default()).unwrap();
    let observability = bundle
        .replay
        .observability
        .as_ref()
        .expect("bundle replay observability");
    assert_eq!(
        observability.transcript_pointers[0].path.as_deref(),
        Some(REDACTED_PLACEHOLDER)
    );
    assert!(bundle.redaction.entries.iter().any(|entry| {
        entry.path == "$.replay.observability.transcript_pointers[0].path"
            && entry.class == "local_pointer_path"
    }));
}

#[test]
fn replay_only_export_withholds_observability_verification_summaries() {
    let mut run = fixture_run();
    run.observability = Some(fixture_observability());
    let options = SessionBundleExportOptions {
        mode: SessionBundleExportMode::ReplayOnly,
        ..SessionBundleExportOptions::default()
    };

    let bundle = export_run_record_bundle(&run, &options).unwrap();
    let rendered = serde_json::to_string(&bundle).unwrap();
    assert!(!rendered.contains("private details"));
    assert_eq!(
        bundle
            .replay
            .observability
            .as_ref()
            .unwrap()
            .verification_outcomes[0]
            .summary
            .as_deref(),
        Some(REPLAY_ONLY_PLACEHOLDER)
    );
    assert_eq!(
        bundle.replay.verification_outcomes[0].summary.as_deref(),
        Some(REPLAY_ONLY_PLACEHOLDER)
    );
}

#[test]
fn local_export_embeds_worker_snapshots_and_import_materializes_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (run, source_snapshot_path) = fixture_run_with_worker_snapshot(tmp.path());

    let bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(bundle.replay.worker_snapshots.len(), 1);
    let snapshot = &bundle.replay.worker_snapshots[0];
    assert_eq!(snapshot.worker_id, "worker_1");
    assert_eq!(snapshot.status, "suspended");
    assert_eq!(
        snapshot.source_path.as_deref(),
        Some(source_snapshot_path.to_string_lossy().as_ref())
    );
    assert_eq!(snapshot.value["config"]["mode"], json!("sub_agent"));

    let imported_dir = tmp.path().join("imported-worker-snapshots");
    let materialized = materialize_worker_snapshots(&bundle, &imported_dir).unwrap();
    assert_eq!(materialized.len(), 1);
    assert_eq!(materialized[0].worker_id, "worker_1");

    let materialized_snapshot: JsonValue =
        serde_json::from_str(&fs::read_to_string(&materialized[0].path).unwrap()).unwrap();
    assert_eq!(
        materialized_snapshot["snapshot_path"],
        json!(materialized[0].path)
    );
    assert_eq!(
        materialized_snapshot["suspension"]["snapshot_ref"],
        json!(materialized[0].path)
    );
    assert_eq!(
        materialized_snapshot["config"]["spec"]["session_id"],
        json!("session-child")
    );

    let imported =
        import_run_record_value_with_materialized_worker_snapshots(&bundle, &materialized).unwrap();
    assert_eq!(
        imported["child_runs"][0]["snapshot_path"],
        json!(materialized[0].path)
    );
    assert_eq!(
        imported["observability"]["worker_lineage"][0]["snapshot_path"],
        json!(materialized[0].path)
    );
    assert_ne!(
        imported["child_runs"][0]["snapshot_path"],
        json!(source_snapshot_path.to_string_lossy())
    );
}

#[test]
fn worker_snapshot_checkpoint_builds_resumable_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let (_run, source_snapshot_path) = fixture_run_with_worker_snapshot(tmp.path());

    let run = run_record_from_worker_snapshot(&source_snapshot_path).unwrap();
    assert_eq!(run.status, "suspended");
    assert_eq!(run.workflow_id, "worker_snapshot_checkpoint");
    assert_eq!(run.checkpoints[0].persisted_at, "2026-05-01T00:00:20Z");
    assert_eq!(
        run.replay_fixture.as_ref().unwrap().created_at,
        "2026-05-01T00:00:20Z"
    );
    assert_eq!(
        run.replay_fixture.as_ref().unwrap().eval_kind.as_deref(),
        Some("worker_snapshot_checkpoint")
    );
    assert_eq!(run.checkpoints.len(), 1);
    assert_eq!(run.child_runs.len(), 1);
    assert_eq!(
        run.child_runs[0].snapshot_path.as_deref(),
        Some(source_snapshot_path.to_string_lossy().as_ref())
    );
    assert_eq!(run.observability.as_ref().unwrap().schema_version, 4);
    assert_eq!(
        run.observability.as_ref().unwrap().worker_lineage[0]
            .snapshot_path
            .as_deref(),
        Some(source_snapshot_path.to_string_lossy().as_ref())
    );

    let bundle = export_worker_snapshot_bundle(
        &source_snapshot_path,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(bundle.source.status, "suspended");
    assert!(bundle.replay.replay_fixture.is_some());
    assert_eq!(bundle.replay.checkpoints.len(), 1);
    assert!(bundle
        .replay
        .deterministic_events
        .iter()
        .any(|event| event.source == "run.checkpoints"));
    assert_eq!(bundle.replay.worker_snapshots.len(), 1);
    assert_eq!(bundle.replay.worker_snapshots[0].worker_id, "worker_1");
    assert_eq!(
        bundle.replay.worker_snapshots[0].source_path.as_deref(),
        Some(source_snapshot_path.to_string_lossy().as_ref())
    );

    let imported_dir = tmp.path().join("checkpoint-imported-worker-snapshots");
    let materialized = materialize_worker_snapshots(&bundle, &imported_dir).unwrap();
    let imported =
        import_run_record_value_with_materialized_worker_snapshots(&bundle, &materialized).unwrap();
    assert_eq!(
        imported["metadata"]["worker_snapshot_path"],
        json!(materialized[0].path)
    );
    assert_ne!(
        imported["metadata"]["worker_snapshot_path"],
        json!(source_snapshot_path.to_string_lossy())
    );
}

#[test]
fn worker_snapshot_checkpoint_rejects_non_suspended_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot_path = tmp.path().join("worker-completed.json");
    fs::write(
        &snapshot_path,
        serde_json::to_string_pretty(&json!({
            "_type": "worker_snapshot",
            "id": "worker_done",
            "name": "done",
            "task": "already finished",
            "status": "completed",
            "snapshot_path": snapshot_path.to_string_lossy()
        }))
        .unwrap(),
    )
    .unwrap();

    let err = run_record_from_worker_snapshot(&snapshot_path).unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::UnsupportedCheckpointState {
            status: "completed".to_string()
        }
    );
}

#[test]
fn worker_snapshot_checkpoint_rejects_non_worker_snapshot_json() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot_path = tmp.path().join("not-worker.json");
    fs::write(
        &snapshot_path,
        serde_json::to_string_pretty(&json!({
            "_type": "run_record",
            "id": "looks_suspended",
            "status": "suspended",
            "config": {},
            "suspension": {}
        }))
        .unwrap(),
    )
    .unwrap();

    let err = run_record_from_worker_snapshot(&snapshot_path).unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::InvalidType {
            path: "$.worker_snapshot._type".to_string(),
            expected: "\"worker_snapshot\"".to_string()
        }
    );
}

#[test]
fn worker_snapshot_checkpoint_requires_resume_critical_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot_path = tmp.path().join("worker-missing-suspension.json");
    fs::write(
        &snapshot_path,
        serde_json::to_string_pretty(&json!({
            "_type": "worker_snapshot",
            "id": "worker_missing_suspension",
            "status": "suspended",
            "config": {}
        }))
        .unwrap(),
    )
    .unwrap();

    let err = run_record_from_worker_snapshot(&snapshot_path).unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::MissingRequired("$.worker_snapshot.suspension".to_string())
    );
}

#[test]
fn sanitized_export_redacts_worker_snapshot_local_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (run, source_snapshot_path) = fixture_run_with_worker_snapshot(tmp.path());

    let bundle = export_run_record_bundle(&run, &SessionBundleExportOptions::default()).unwrap();
    let snapshot = &bundle.replay.worker_snapshots[0];

    assert_eq!(snapshot.source_path.as_deref(), Some(REDACTED_PLACEHOLDER));
    assert_eq!(snapshot.snapshot_ref, REDACTED_PLACEHOLDER);
    assert_eq!(snapshot.value["snapshot_path"], json!(REDACTED_PLACEHOLDER));
    assert_eq!(
        snapshot.value["suspension"]["snapshot_ref"],
        json!(REDACTED_PLACEHOLDER)
    );
    assert!(!serde_json::to_string(&bundle)
        .unwrap()
        .contains(source_snapshot_path.to_string_lossy().as_ref()));
}

#[test]
fn validation_rejects_missing_required_fields() {
    let err = validate_session_bundle_value(&json!({}), &SessionBundleValidationOptions::default())
        .unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::MissingRequired("$._type".to_string())
    );

    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    let mut value = serde_json::to_value(bundle).unwrap();
    value.as_object_mut().unwrap().remove("producer");
    let err = validate_session_bundle_value(&value, &SessionBundleValidationOptions::default())
        .unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::MissingRequired("$.producer".to_string())
    );

    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    let mut value = serde_json::to_value(bundle).unwrap();
    value["source"]
        .as_object_mut()
        .unwrap()
        .remove("run_record_id");
    let err = validate_session_bundle_value(&value, &SessionBundleValidationOptions::default())
        .unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::MissingRequired("$.source.run_record_id".to_string())
    );
}

#[test]
fn validation_rejects_future_versions() {
    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    let mut value = serde_json::to_value(bundle).unwrap();
    value["schema_version"] = json!(999);
    let err = validate_session_bundle_value(&value, &SessionBundleValidationOptions::default())
        .unwrap_err();
    assert_eq!(
        err,
        SessionBundleError::UnsupportedSchemaVersion {
            found: 999,
            supported: SESSION_BUNDLE_SCHEMA_VERSION,
        }
    );
}

#[test]
fn validation_rejects_unredacted_secret_markers() {
    let options = SessionBundleExportOptions {
        mode: SessionBundleExportMode::Local,
        ..SessionBundleExportOptions::default()
    };
    let bundle = export_run_record_bundle(&fixture_run(), &options).unwrap();
    let value = serde_json::to_value(bundle).unwrap();
    let err = validate_session_bundle_value(&value, &SessionBundleValidationOptions::default())
        .unwrap_err();
    assert!(matches!(err, SessionBundleError::UnsafeSecretMarker { .. }));
}

#[test]
fn imported_run_record_prefers_embedded_run_record() {
    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    let run_record = import_run_record_value(&bundle).unwrap();
    assert_eq!(run_record["_type"], "run_record");
    assert_eq!(run_record["id"], "run_123");
}

#[test]
fn schema_is_self_describing() {
    let schema = session_bundle_schema();
    assert_eq!(schema["properties"]["_type"]["const"], SESSION_BUNDLE_TYPE);
    assert_eq!(
        schema["properties"]["schema_version"]["maximum"],
        SESSION_BUNDLE_SCHEMA_VERSION
    );
}

#[test]
fn checked_session_bundle_fixtures_validate_and_import() {
    for fixture in [
        "sample-local.bundle.json",
        "sample-sanitized.bundle.json",
        "sample-replay-only.bundle.json",
    ] {
        let path = repo_fixture_path(fixture);
        let content = fs::read_to_string(&path).unwrap();
        let bundle =
            validate_session_bundle_str(&content, &SessionBundleValidationOptions::default())
                .unwrap();
        assert_eq!(bundle.source.run_record_id, "run_fixture_session_bundle");

        let imported = import_run_record_value(&bundle).unwrap();
        assert_eq!(imported["_type"], "run_record");
        assert_eq!(imported["id"], "run_fixture_session_bundle");
    }
}

#[test]
fn workspace_anchor_in_transcript_metadata_round_trips_to_bundle_workspace() {
    use crate::workspace_anchor::{MountMode, MountedRoot, WorkspaceAnchor};
    use std::path::PathBuf;

    let mut run = fixture_run();
    let anchor = WorkspaceAnchor {
        primary: PathBuf::from("/workspace/example"),
        additional_roots: vec![MountedRoot {
            path: PathBuf::from("/workspace/lib"),
            mount_mode: MountMode::ReadOnly,
            mounted_at: "2026-05-01T00:00:00Z".to_string(),
        }],
        anchored_at: "2026-05-01T00:00:00Z".to_string(),
    };
    let transcript = run.transcript.as_mut().unwrap();
    transcript["metadata"]["workspace_anchor"] = anchor.to_json();

    let bundle = export_run_record_bundle(
        &run,
        &SessionBundleExportOptions {
            mode: SessionBundleExportMode::Local,
            ..SessionBundleExportOptions::default()
        },
    )
    .unwrap();
    let workspace = bundle
        .workspace
        .expect("workspace section should be present");
    assert_eq!(workspace.primary.as_deref(), Some("/workspace/example"));
    assert_eq!(workspace.additional_roots.len(), 1);
    assert_eq!(workspace.additional_roots[0].mount_mode, "read_only");
    assert_eq!(
        workspace.anchored_at.as_deref(),
        Some("2026-05-01T00:00:00Z")
    );
}

#[test]
fn workspace_section_absent_when_no_anchor_metadata() {
    let bundle =
        export_run_record_bundle(&fixture_run(), &SessionBundleExportOptions::default()).unwrap();
    assert!(bundle.workspace.is_none());
}

/// Verifies the checked-in bundle fixtures match what `export_run_record_bundle`
/// produces today, and (when `HARN_REGENERATE_FIXTURES=1`) rewrites them in
/// place.
///
/// Pins `bundle_id`, `created_at`, and the version strings to deterministic
/// fixture values, and sorts the redaction-entry list by path so the
/// rendered output is stable across runs.
///
/// To regenerate after a schema change:
/// `HARN_REGENERATE_FIXTURES=1 cargo test -p harn-vm checked_session_bundle_fixtures_match_export`
#[test]
fn checked_session_bundle_fixtures_match_export() {
    let raw = fs::read_to_string(repo_fixture_path("sample-run-record.json"))
        .expect("read sample-run-record.json");
    let run: RunRecord =
        serde_json::from_str(&raw).expect("decode sample-run-record.json as RunRecord");
    let regenerate = std::env::var_os("HARN_REGENERATE_FIXTURES").is_some();

    for (name, mode) in [
        ("sample-local.bundle.json", SessionBundleExportMode::Local),
        (
            "sample-sanitized.bundle.json",
            SessionBundleExportMode::Sanitized,
        ),
        (
            "sample-replay-only.bundle.json",
            SessionBundleExportMode::ReplayOnly,
        ),
    ] {
        let options = SessionBundleExportOptions {
            mode,
            include_attachments: true,
            ..SessionBundleExportOptions::default()
        };
        let mut bundle =
            export_run_record_bundle(&run, &options).expect("export bundle for fixture");
        pin_fixture_bundle_fields(&mut bundle);
        let mut value = serde_json::to_value(&bundle).expect("encode bundle");
        sort_redaction_entries(&mut value);
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("pretty encode")
        );
        let path = repo_fixture_path(name);
        if regenerate {
            fs::write(&path, &rendered).unwrap_or_else(|err| panic!("write {name}: {err}"));
            continue;
        }
        let on_disk = fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {name}: {err}"));
        // Defense-in-depth against Windows autocrlf: `.gitattributes` pins
        // these fixtures to LF, but local clones with `core.autocrlf=true`
        // could still surface CRLF here without the pin.
        assert_eq!(
            on_disk.replace("\r\n", "\n"),
            rendered,
            "{name} is stale; re-run with HARN_REGENERATE_FIXTURES=1 to refresh"
        );
    }
}

fn pin_fixture_bundle_fields(bundle: &mut SessionBundle) {
    bundle.bundle_id = "bundle_fixture_session_bundle".to_string();
    bundle.created_at = "2026-05-01T00:00:30Z".to_string();
    bundle.producer.version = "fixture".to_string();
    bundle.runtime.harn_version = "fixture".to_string();
}

fn sort_redaction_entries(value: &mut JsonValue) {
    let Some(JsonValue::Array(entries)) = value
        .get_mut("redaction")
        .and_then(|r| r.get_mut("entries"))
    else {
        return;
    };
    entries.sort_by(|a, b| {
        a.get("path")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .cmp(b.get("path").and_then(JsonValue::as_str).unwrap_or(""))
    });
}

// --- Agent-session liveness (#3682 portable suspend/resume keystone) ---

fn replay_event(event_id: u64, occurred_at_ms: i64, event: AgentEvent) -> AgentSessionReplayEvent {
    let kind = match &event {
        AgentEvent::UserMessage { .. } => "user_message",
        AgentEvent::AgentMessageChunk { .. } => "agent_message_chunk",
        AgentEvent::SessionClosed { .. } => "session_closed",
        _ => "event",
    }
    .to_string();
    AgentSessionReplayEvent {
        event_id,
        kind,
        occurred_at_ms,
        event,
    }
}

fn user_turn(session_id: &str, event_id: u64, occurred_at_ms: i64) -> AgentSessionReplayEvent {
    replay_event(
        event_id,
        occurred_at_ms,
        AgentEvent::UserMessage {
            session_id: session_id.to_string(),
            message_id: format!("m{event_id}"),
            content: vec![json!({ "type": "text", "text": "do the thing" })],
        },
    )
}

fn assistant_turn(session_id: &str, event_id: u64, occurred_at_ms: i64) -> AgentSessionReplayEvent {
    replay_event(
        event_id,
        occurred_at_ms,
        AgentEvent::AgentMessageChunk {
            session_id: session_id.to_string(),
            content: "working on it".to_string(),
        },
    )
}

fn session_closed(
    session_id: &str,
    event_id: u64,
    occurred_at_ms: i64,
    status: &str,
) -> AgentSessionReplayEvent {
    replay_event(
        event_id,
        occurred_at_ms,
        AgentEvent::SessionClosed {
            session_id: session_id.to_string(),
            reason: "done".to_string(),
            status: status.to_string(),
            metadata: json!({}),
        },
    )
}

#[test]
fn liveness_is_closed_when_session_closed_present() {
    let sid = "sess-closed";
    let events = vec![
        user_turn(sid, 1, 1_000),
        assistant_turn(sid, 2, 1_500),
        session_closed(sid, 3, 2_000, "succeeded"),
    ];
    assert_eq!(
        agent_session_liveness(&events),
        AgentSessionLiveness::Closed {
            status: "succeeded".to_string(),
            finished_at_ms: 2_000,
        }
    );
}

#[test]
fn liveness_falls_back_to_completed_for_empty_close_status() {
    let sid = "sess-empty-status";
    let events = vec![user_turn(sid, 1, 1_000), session_closed(sid, 2, 2_000, "")];
    let liveness = agent_session_liveness(&events);
    assert_eq!(liveness.status(), SESSION_BUNDLE_STATUS_COMPLETED);
    assert!(!liveness.is_suspended());
}

#[test]
fn liveness_is_suspended_when_no_session_closed() {
    // A live loop frozen mid-turn (or a time-traveled prefix) has no terminal
    // SessionClosed event. It must NOT be labeled completed.
    let sid = "sess-live";
    let events = vec![user_turn(sid, 1, 1_000), assistant_turn(sid, 2, 1_500)];
    let liveness = agent_session_liveness(&events);
    assert_eq!(liveness, AgentSessionLiveness::Suspended);
    assert!(liveness.is_suspended());
    assert_eq!(liveness.status(), SESSION_BUNDLE_STATUS_SUSPENDED);
}

#[test]
fn bundle_status_reflects_suspended_loop_not_completed() {
    let sid = "sess-suspend-bundle";
    let events = vec![user_turn(sid, 1, 1_000), assistant_turn(sid, 2, 1_500)];
    let bundle = session_bundle_from_agent_session_events(sid, &events).expect("bundle");
    assert_eq!(bundle.source.status, SESSION_BUNDLE_STATUS_SUSPENDED);
    assert_eq!(bundle.source.finished_at, None);
    assert_eq!(
        bundle.metadata.get(SESSION_BUNDLE_LIVENESS_KEY),
        Some(&json!("suspended"))
    );
    assert_eq!(
        bundle
            .replay
            .replay_fixture
            .as_ref()
            .unwrap()
            .expected_status,
        SESSION_BUNDLE_STATUS_SUSPENDED
    );
}

#[test]
fn bundle_status_reflects_closed_loop() {
    let sid = "sess-closed-bundle";
    let events = vec![
        user_turn(sid, 1, 1_000),
        assistant_turn(sid, 2, 1_500),
        session_closed(sid, 3, 2_000, "succeeded"),
    ];
    let bundle = session_bundle_from_agent_session_events(sid, &events).expect("bundle");
    assert_eq!(bundle.source.status, "succeeded");
    assert!(bundle.source.finished_at.is_some());
    assert_eq!(
        bundle.metadata.get(SESSION_BUNDLE_LIVENESS_KEY),
        Some(&json!("closed"))
    );
}

#[test]
fn time_travel_prefix_is_suspended_not_completed() {
    // Truncating a completed session to a mid-run prefix (the replay `--at`
    // path) drops the SessionClosed event; the prefix is suspended.
    let sid = "sess-timetravel";
    let full = vec![
        user_turn(sid, 1, 1_000),
        assistant_turn(sid, 2, 1_500),
        session_closed(sid, 3, 2_000, "succeeded"),
    ];
    assert!(!agent_session_liveness(&full).is_suspended());
    let prefix = &full[..2];
    assert_eq!(
        agent_session_liveness(prefix),
        AgentSessionLiveness::Suspended
    );
    let bundle = session_bundle_from_agent_session_events(sid, prefix).expect("bundle");
    assert_eq!(bundle.source.status, SESSION_BUNDLE_STATUS_SUSPENDED);
}

#[test]
fn suspended_bundle_export_is_deterministic_round_trip() {
    // Byte-faithful freeze: re-exporting the same frozen stream yields an
    // identical bundle (modulo nondeterministic producer/version fields), the
    // determinism the resume side relies on.
    let sid = "sess-determinism";
    let events = vec![user_turn(sid, 1, 1_000), assistant_turn(sid, 2, 1_500)];
    let first = session_bundle_from_agent_session_events(sid, &events).expect("first");
    let second = session_bundle_from_agent_session_events(sid, &events).expect("second");
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
}
