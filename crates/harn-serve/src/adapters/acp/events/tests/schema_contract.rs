use std::collections::BTreeSet;

use super::{
    agent_event_ext_fixture_events, AcpAgentEventSink, AcpOutput, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD,
};
use crate::adapters::acp::events::agent_event_ext_params;
use harn_vm::agent_events::AgentEvent;
use harn_vm::agent_events::AgentEventSink;

pub(in crate::adapters::acp::events) async fn collect_notifications(
    events: Vec<AgentEvent>,
) -> Vec<serde_json::Value> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (sink, expected_len) = (AcpAgentEventSink::new(AcpOutput::Channel(tx)), events.len());
    for event in events {
        sink.handle_event(&event);
    }

    let mut notifications = Vec::with_capacity(expected_len);
    for _ in 0..expected_len {
        let line = rx.recv().await.expect("ACP event notification");
        notifications.push(serde_json::from_str(&line).expect("json"));
    }
    notifications
}

/// The canonical content location is unchanged by Harn's history identity.
#[tokio::test(flavor = "current_thread")]
async fn agent_message_chunk_visible_text_lives_under_content_meta_harn() {
    let actual = collect_notifications(vec![AgentEvent::AgentMessageChunk {
        session_id: "session-1".into(),
        content: "hello".into(),
        history_source_event_id: Some("canonical-answer".into()),
    }])
    .await;
    let payload = &actual[0];
    let content = &payload["params"]["update"]["content"];
    assert_eq!(
        payload["params"]["update"]["historySourceEventId"],
        "canonical-answer"
    );
    assert_eq!(content["type"], "text");
    assert_eq!(content["text"], "hello");
    assert_eq!(content["_meta"]["harn"]["visible_text"], "hello");
    assert_eq!(content["_meta"]["harn"]["visible_delta"], "hello");
    assert!(content.get("visible_text").is_none());
    assert!(content.get("visible_delta").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn message_history_identity_is_typed_in_the_emitted_wire_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/protocols/schemas/acp-session-update.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let messages = collect_notifications(vec![
        AgentEvent::AgentMessageChunk {
            session_id: "session-1".into(),
            content: "Answer".into(),
            history_source_event_id: Some("published-answer".into()),
        },
        AgentEvent::UserMessage {
            session_id: "session-1".into(),
            message_id: "client-id".into(),
            content: vec![serde_json::json!({"type":"text", "text":"Question"})],
            history_source_event_id: Some("saved-user".into()),
        },
    ])
    .await;
    assert_eq!(messages.len(), 2);
    for message in messages {
        assert!(
            validator.is_valid(&message),
            "real emitted message must validate"
        );
        for identity in [serde_json::json!(42), serde_json::json!("")] {
            let mut invalid = message.clone();
            invalid["params"]["update"]["historySourceEventId"] = identity;
            assert!(
                !validator.is_valid(&invalid),
                "invalid history identity must be rejected"
            );
        }
    }
}

/// Append the `purpose_label` fixture, which exists so the advertised-kind
/// contract below stays complete.
///
/// It is appended rather than written into the literal because the literal is
/// spliced at a fixed index: an entry added there shifts the splice point and
/// rotates six unrelated fixtures out of position.
pub(super) fn with_purpose_label(mut events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    events.push(AgentEvent::PurposeLabel {
        session_id: "session-1".to_string(),
        label: "Searching for GitHub issues".to_string(),
        source: harn_vm::agent_events::PurposeLabelSource::Declared,
        iteration: Some(0),
        tool_call_ids: Vec::new(),
        tool_call_count: Some(3),
    });
    events
}

#[test]
fn agent_event_envelope_rejects_payloads_with_reserved_keys() {
    for key in ["kind", "sessionId"] {
        let payload = serde_json::Value::Object(serde_json::Map::from_iter([(
            key.to_string(),
            serde_json::json!("payload-owned"),
        )]));
        let error = agent_event_ext_params("passthrough_fixture", "session-1", payload)
            .expect_err("a payload cannot claim an envelope key");
        assert!(error.contains(key), "missing colliding key in {error}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_terminal_verify_preserves_its_payload_discriminator() {
    let actual = collect_notifications(vec![AgentEvent::ReservedTerminalVerify {
        session_id: "session-1".to_string(),
        payload: serde_json::json!({
            "reserveKind": "zero_write_terminal_verify",
            "phase": "verify_started",
            "iteration": 3,
        }),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "reserved_terminal_verify");
    assert_eq!(params["reserveKind"], "zero_write_terminal_verify");
}

/// Checks the advertised vocabulary for Harn events that ride on
/// `_harn/agentEvent` because ACP has no canonical slot.
#[tokio::test(flavor = "current_thread")]
async fn agent_event_ext_notifications_use_advertised_wire_contract() {
    let actual = collect_notifications(agent_event_ext_fixture_events()).await;

    let judge = actual
        .iter()
        .find(|notification| notification["params"]["kind"] == "judge_decision")
        .expect("judge_decision fixture");
    assert_eq!(
        judge["params"]["source"],
        serde_json::json!("deterministic")
    );
    assert_eq!(judge["params"]["escalationRecommended"], true);
    assert_eq!(judge["params"]["escalationTarget"], "frontier");
    // The verdict's audit basis rides `reasoning` and `nextStep`. The retired
    // evidence arrays must not reappear on the wire under any name.
    assert!(judge["params"]["specificGaps"].is_null());
    assert!(judge["params"]["acceptedEvidence"].is_null());

    for notification in actual {
        assert_eq!(
            notification["method"].as_str().expect("method"),
            HARN_AGENT_EVENT_METHOD,
            "every Harn agent-event extension notification must use the \
                 advertised _harn/agentEvent method"
        );
        assert!(
            notification["params"]["sessionId"].is_string(),
            "sessionId must be a top-level string on every agent event"
        );
        let kind = notification["params"]["kind"]
            .as_str()
            .expect("kind discriminator");
        assert!(
            HARN_AGENT_EVENT_KINDS.contains(&kind),
            "{kind} is not advertised in HARN_AGENT_EVENT_KINDS — clients \
                 cannot subscribe to undocumented kinds"
        );
    }
}

#[test]
fn conformance_schema_accepts_every_advertised_agent_event_kind() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/protocols/schemas/acp-session-update.schema.json");
    let source = std::fs::read_to_string(&path).expect("read ACP conformance schema");
    let schema: serde_json::Value = serde_json::from_str(&source).expect("parse ACP schema");
    let values = schema["$defs"]["HarnAgentEventNotification"]["properties"]["params"]
        ["properties"]["kind"]["enum"]
        .as_array()
        .expect("agent event kind enum");
    let schema_kinds: BTreeSet<&str> = values
        .iter()
        .map(|value| value.as_str().expect("agent event kind string"))
        .collect();
    let advertised_kinds: BTreeSet<&str> = HARN_AGENT_EVENT_KINDS.iter().copied().collect();

    assert_eq!(
        schema_kinds, advertised_kinds,
        "ACP conformance schema and advertised event kinds must change together"
    );
}
