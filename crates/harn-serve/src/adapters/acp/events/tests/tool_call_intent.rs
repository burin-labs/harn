//! The tool-call start event's `intent` reaches the ACP `tool_call` update as
//! Harn metadata, and its absence leaves no key behind.

use super::*;

fn tool_call(intent: Option<&str>) -> AgentEvent {
    AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read_file".to_string(),
        kind: Some(ToolKind::Read),
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "README.md"}),
        parsing: None,
        audit: None,
        intent: intent.map(str::to_string),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_projects_intent_into_harn_meta() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    sink.handle_event(&tool_call(Some("Looking for PR 456 artifacts")));
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["params"]["update"]["sessionUpdate"], "tool_call");
    assert_eq!(
        update_harn_meta(&payload)["intent"],
        "Looking for PR 456 artifacts"
    );
    assert!(payload["params"]["update"].get("intent").is_none());

    sink.handle_event(&tool_call(None));
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(
        payload["params"]["update"].get("_meta").is_none(),
        "no intent, no Harn metadata: {payload}"
    );
}
