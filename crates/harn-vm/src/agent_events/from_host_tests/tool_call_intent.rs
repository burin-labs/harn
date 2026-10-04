//! The tool-call start event's `intent` is normalized at the host boundary by
//! the same owner the approval request uses.

use super::*;

fn tool_call_intent_from(payload: serde_json::Value) -> Option<String> {
    match accepted_host_event("intent", "tool_call", &payload) {
        AgentEvent::ToolCall { intent, .. } => intent,
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn tool_call_intent_is_collapsed_and_kept() {
    let intent = tool_call_intent_from(json!({
        "tool_call_id": "t1",
        "tool_name": "read_file",
        "intent": "  Looking for\n PR 456 artifacts ",
    }));
    assert_eq!(intent.as_deref(), Some("Looking for PR 456 artifacts"));
}

#[test]
fn tool_call_without_intent_or_with_a_blank_one_has_none() {
    let absent = tool_call_intent_from(json!({"tool_call_id": "t1", "tool_name": "read_file"}));
    assert_eq!(absent, None);
    let blank = tool_call_intent_from(json!({
        "tool_call_id": "t1",
        "tool_name": "read_file",
        "intent": " \n ",
    }));
    assert_eq!(blank, None);
    let event = accepted_host_event(
        "intent",
        "tool_call",
        &json!({"tool_call_id": "t1", "tool_name": "read_file", "intent": ""}),
    );
    let wire = serde_json::to_value(&event).expect("serialize");
    assert!(wire.get("intent").is_none(), "no intent, no key: {wire}");
}

#[test]
fn tool_call_intent_is_bounded() {
    let intent = tool_call_intent_from(json!({
        "tool_call_id": "t1",
        "tool_name": "read_file",
        "intent": "y".repeat(crate::llm::tool_call_intent::MAX_CHARS * 2),
    }))
    .expect("intent");
    assert_eq!(
        intent.chars().count(),
        crate::llm::tool_call_intent::MAX_CHARS
    );
    assert!(intent.ends_with('\u{2026}'));
}
