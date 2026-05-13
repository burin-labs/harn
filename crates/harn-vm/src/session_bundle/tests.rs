use std::fs;

use serde_json::json;

use super::*;
use crate::orchestration::{LlmUsageRecord, RunStageRecord};

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
