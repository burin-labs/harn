use super::*;
use harn_vm::agent_events::SubagentTerminalStatus;

#[tokio::test(flavor = "current_thread")]
async fn emits_advertised_lineage_extension() {
    let notifications = collect_notifications(vec![AgentEvent::SubagentStop {
        session_id: "parent-session".to_string(),
        parent_run_id: "parent-run".to_string(),
        child_run_id: "child-run".to_string(),
        terminal_status: SubagentTerminalStatus::Timeout,
        terminal_class: "timeout".to_string(),
        reason: "deadline elapsed".to_string(),
        result_ref: Some("agent-session:child-run".to_string()),
        receipt_ref: Some("agent-session:child-run#sub_agent_result".to_string()),
        cancellation: None,
        timeout: Some(serde_json::json!({"source": "agent_loop"})),
        completed_at_ms: 1234,
    }])
    .await;

    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    assert_eq!(notification["params"]["kind"], "subagent_stop");
    assert_eq!(notification["params"]["sessionId"], "parent-session");
    assert_eq!(notification["params"]["parentRunId"], "parent-run");
    assert_eq!(notification["params"]["childRunId"], "child-run");
    assert_eq!(notification["params"]["terminalStatus"], "timeout");
    assert_eq!(notification["params"]["completedAtMs"], 1234);
    assert!(HARN_AGENT_EVENT_KINDS.contains(&"subagent_stop"));
}
