use std::fs;

use serde_json::json;

use super::*;
use crate::orchestration::{
    LlmUsageRecord, RunObservabilityRecord, RunStageRecord, RunTranscriptPointerRecord,
    RunVerificationOutcomeRecord,
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
