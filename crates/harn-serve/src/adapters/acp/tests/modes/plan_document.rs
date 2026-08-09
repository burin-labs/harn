use super::*;

#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_replays_persisted_agent_events() {
    harn_vm::event_log::reset_active_event_log();
    let log = harn_vm::event_log::install_memory_for_current_thread(64);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    install_test_agent_event_log_sink(&log, &session_id);

    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::AgentMessageChunk {
        session_id: session_id.clone(),
        content: "replay me".to_string(),
    });
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({"plan": [{"content": "do the thing", "status": "pending"}]}),
    );
    let event = harn_vm::llm::plan::create_plan_document_event(
        plan,
        "test-agent",
        "test",
        "2026-01-01T00:00:00Z",
        "plan-event-test",
    )
    .expect("plan document");
    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::PlanDocumentUpdated {
        session_id: session_id.clone(),
        event: Box::new(event),
    });

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/load",
            "params": {"sessionId": session_id},
        }))
        .await;

    let message_replay = recv_json(&mut rx).await;
    assert_eq!(message_replay["method"], "session/update");
    assert_eq!(
        message_replay["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    assert_eq!(
        message_replay["params"]["update"]["content"]["text"],
        "replay me"
    );
    assert_eq!(message_replay["_harn"]["replayed"], true);
    assert_eq!(
        message_replay["params"]["update"]["_meta"]["harn"]["replayed"],
        true
    );

    let plan_replay = recv_json(&mut rx).await;
    assert_eq!(plan_replay["method"], "session/update");
    assert_eq!(plan_replay["params"]["update"]["sessionUpdate"], "plan");
    assert_eq!(
        plan_replay["params"]["update"]["harnPlanDocument"]["schema_version"],
        "harn.plan_document.v1"
    );
    assert_eq!(plan_replay["_harn"]["replayed"], true);
    assert_eq!(
        plan_replay["params"]["update"]["_meta"]["harn"]["replayed"],
        true
    );

    let loaded = recv_json(&mut rx).await;
    assert_eq!(loaded["id"], 2);
    assert_eq!(loaded["result"]["replayed"].as_array().unwrap().len(), 2);
    assert_eq!(
        loaded["result"]["replayed"][0]["type"],
        "agent_message_chunk"
    );
    assert_eq!(
        loaded["result"]["replayed"][1]["type"],
        "plan_document_updated"
    );

    harn_vm::agent_events::clear_session_sinks(created["result"]["sessionId"].as_str().unwrap());
    harn_vm::event_log::reset_active_event_log();
}
