use std::collections::BTreeSet;

use super::{
    agent_event_ext_fixture_events, collect_notifications, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD,
};
use crate::adapters::acp::events::agent_event_ext_params;
use harn_vm::agent_events::AgentEvent;

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
