use super::*;

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_producer_data_under_harn_meta() {
    let payload = collect_notifications(vec![AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-8".to_string(),
        tool_name: "edit".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: ToolMutationStatus::Applied,
        changed_paths: Some(vec!["src/lib.rs".to_string(), "tests/lib.rs".to_string()]),
        data: Some(serde_json::json!({
            "command_status": "succeeded",
            "run_outcome": {"exit_code": 0}
        })),
        executor: Some(ToolExecutor::HostBridge),
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    }])
    .await
    .pop()
    .expect("ACP update");

    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["mutationStatus"], "applied");
    assert_eq!(
        harn_meta["changedPaths"],
        serde_json::json!(["src/lib.rs", "tests/lib.rs"])
    );
    assert_eq!(
        harn_meta["data"],
        serde_json::json!({
            "command_status": "succeeded",
            "run_outcome": {"exit_code": 0}
        })
    );
    assert!(payload["params"]["update"].get("changedPaths").is_none());
    assert!(payload["params"]["update"].get("data").is_none());
}
