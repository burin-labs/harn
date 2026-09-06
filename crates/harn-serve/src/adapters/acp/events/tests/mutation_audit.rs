//! Split out of the parent test file, which sits on the source-length
//! ratchet's legacy list. These four cases are one contract: whether the
//! ambient mutation session reaches the ACP wire as `audit`, and what the
//! typed `mutationStatus` reads when it does not. The child reaches the
//! parent's fixtures through its own `use super::*`.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn tool_call_includes_audit_when_mutation_session_is_active() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let policy = ToolApprovalPolicy {
        require_approval: vec!["edit_*".into()],
        write_path_allowlist: vec!["src/**".into()],
        ..Default::default()
    };
    let audit = MutationSessionRecord {
        session_id: "session_42".into(),
        parent_session_id: Some("session_root".into()),
        run_id: Some("run_42".into()),
        worker_id: Some("worker_3".into()),
        execution_kind: Some("worker".into()),
        mutation_scope: "apply_workspace".into(),
        approval_policy: Some(policy),
    };
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "edit_file".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "src/main.rs"}),
        parsing: None,
        audit: Some(audit),
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let audit_value = &update_harn_meta(&payload)["audit"];
    assert_eq!(audit_value["session_id"], "session_42");
    assert_eq!(audit_value["parent_session_id"], "session_root");
    assert_eq!(audit_value["run_id"], "run_42");
    assert_eq!(audit_value["worker_id"], "worker_3");
    assert_eq!(audit_value["execution_kind"], "worker");
    assert_eq!(audit_value["mutation_scope"], "apply_workspace");
    assert_eq!(
        audit_value["approval_policy"]["require_approval"][0],
        "edit_*"
    );
    assert_eq!(
        audit_value["approval_policy"]["write_path_allowlist"][0],
        "src/**"
    );
    assert!(payload["params"]["update"].get("audit").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_omits_audit_when_no_mutation_session() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        kind: Some(ToolKind::Read),
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "README.md"}),
        parsing: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(
        payload["params"]["update"].get("_meta").is_none(),
        "got: {payload}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_includes_audit_when_mutation_session_is_active() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let audit = MutationSessionRecord {
        session_id: "session_42".into(),
        run_id: Some("run_42".into()),
        mutation_scope: "apply_workspace".into(),
        execution_kind: Some("workflow".into()),
        ..Default::default()
    };
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "edit_file".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"text": "ok"})),
        error: None,
        duration_ms: Some(11),
        execution_duration_ms: Some(7),
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        executor: Some(ToolExecutor::HostBridge),
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: Some(audit),
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let update = &payload["params"]["update"];
    assert_eq!(update["sessionUpdate"], "tool_call_update");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["audit"]["session_id"], "session_42");
    assert_eq!(harn_meta["audit"]["run_id"], "run_42");
    assert_eq!(harn_meta["audit"]["mutation_scope"], "apply_workspace");
    assert_eq!(harn_meta["audit"]["execution_kind"], "workflow");
    assert_eq!(harn_meta["executor"], "host_bridge");
    assert_eq!(harn_meta["durationMs"], 11);
    assert_eq!(harn_meta["executionDurationMs"], 7);
    assert!(update.get("audit").is_none());
    assert!(update.get("executor").is_none());
    assert!(update.get("durationMs").is_none());
    assert!(update.get("executionDurationMs").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_omits_audit_but_keeps_typed_mutation_status() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let update = &payload["params"]["update"];
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["mutationStatus"], "unknown");
    assert!(update.get("audit").is_none());
    assert!(update.get("mutationStatus").is_none());
}
