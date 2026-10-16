use super::*;
use serde_json::json;

fn source() -> TrainingProjectionSource {
    TrainingProjectionSource {
        run_id: "run-1".into(),
        execution_id: Some("execution-1".into()),
        stage_ids: vec!["stage-1".into()],
        recorded_session_id: Some("session-1".into()),
        descriptor_schema_version: "descriptor-v1".into(),
        transcript_path: PathBuf::from("run-1/transcript.jsonl"),
        transcript_sha256: "sha256:verified-by-adapter".into(),
        transcript_byte_len: 1024,
        terminal_status: "completed".into(),
    }
}

fn transcript() -> String {
    [
        json!({"type": "system_prompt", "content": "Follow the request."}),
        json!({"type": "tool_schemas", "hash": "catalog-1", "schemas": []}),
        json!({"type": "message", "session_id": "session-1", "message_index": 0, "role": "user", "content": "Reply."}),
        json!({"type": "provider_call_request", "call_id": "call-1", "provider": "fixture", "model": "fixture", "tool_format": "native"}),
        json!({"type": "provider_call_response", "call_id": "call-1", "provider": "fixture", "model": "fixture", "text": "Done.", "tool_calls": [], "parsed_tool_calls": []}),
        json!({"type": "message", "session_id": "session-1", "message_index": 1, "role": "assistant", "content": "Done."}),
    ].iter().map(|row| format!("{row}\n")).collect()
}

#[test]
fn projects_verified_source_identity_and_exact_turns() {
    let source = source();
    let example = project_training_transcript(&source, &transcript(), Some("session-1")).unwrap();
    assert_eq!(
        example
            .messages
            .iter()
            .map(|row| row.role.as_str())
            .collect::<Vec<_>>(),
        ["system", "user", "assistant"]
    );
    assert_eq!(example.messages[2].content, "Done.");
    assert_eq!(example.provenance.run_id, source.run_id);
    assert_eq!(example.provenance.execution_id, source.execution_id);
    assert_eq!(example.provenance.stage_id.as_deref(), Some("stage-1"));
    assert_eq!(
        example.provenance.source.transcript_sha256,
        source.transcript_sha256
    );
    assert_eq!(
        example.provenance.source.transcript_byte_len,
        source.transcript_byte_len
    );
    assert_eq!(example.provenance.source.event_count, 6);
}

#[test]
fn multiple_stages_do_not_invent_stage_provenance() {
    let mut source = source();
    source.stage_ids.push("stage-2".into());
    let example = project_training_transcript(&source, &transcript(), None).unwrap();
    assert_eq!(example.provenance.stage_id, None);
}

#[test]
fn rejects_requested_or_recorded_session_mismatches() {
    let mut source = source();
    let text = transcript();
    assert_eq!(
        project_training_transcript(&source, &text, Some("another-session"))
            .unwrap_err()
            .kind,
        "session_id_mismatch"
    );
    source.recorded_session_id = Some("another-session".into());
    assert_eq!(
        project_training_transcript(&source, &text, None)
            .unwrap_err()
            .kind,
        "session_id_mismatch"
    );
}

#[test]
fn rejects_malformed_jsonl_instead_of_skipping_the_row() {
    let mut body = "{\"type\":\"diagnostic\"}\n".repeat(3);
    body.push_str("{not json}\n");
    let error = source::parse_events(&body, "sidecar").unwrap_err();
    assert_eq!(error.kind, "malformed_jsonl");
    assert_eq!(error.event_index, Some(4));
}

#[test]
fn rejects_a_row_with_no_type_discriminator() {
    let error = source::parse_events("{\"content\":\"x\"}\n", "sidecar").unwrap_err();
    assert_eq!(error.kind, "malformed_jsonl");
    assert!(error.message.contains("`type`"), "{error}");
}
