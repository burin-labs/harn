use super::*;

async fn receive_plan_mutation(
    rx: &mut mpsc::UnboundedReceiver<String>,
    response_id: u64,
) -> (serde_json::Value, serde_json::Value) {
    let mut notification = None;
    let mut response = None;
    let mut received = Vec::new();
    for _ in 0..4 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for plan mutation {response_id}; received {received:?}")
            })
            .expect("ACP response channel closed");
        let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON line");
        received.push(message.clone());
        if message["method"] == "session/update"
            && message["params"]["update"]["sessionUpdate"] == "plan"
        {
            notification = Some(message);
        } else if message["id"] == response_id {
            assert!(
                message.get("error").is_none(),
                "plan mutation {response_id} failed: {message}"
            );
            response = Some(message);
        }
        if notification.is_some() && response.is_some() {
            break;
        }
    }
    (
        notification.expect("plan mutation notification"),
        response.expect("plan mutation response"),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn acp_plan_mutations_conflict_receipt_reopen_and_replay() {
    harn_vm::reset_thread_local_state();
    harn_vm::event_log::reset_active_event_log();
    let _log = harn_vm::event_log::install_memory_for_current_thread(64);
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
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({"plan": [{"content": "Ship the workspace", "status": "pending"}]}),
    );
    let seed = harn_vm::llm::plan::create_plan_document_event(
        plan,
        "agent",
        "update_plan",
        "2026-01-01T00:00:00Z",
        "plan-event-seed",
    )
    .expect("plan document");
    harn_vm::llm::plan::persist_plan_document_event(&session_id, &seed)
        .expect("persist seed document");
    let document_id = seed.document().document_id.clone();
    let initial_revision = seed.document().current_revision.revision_id.clone();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": initial_revision,
                "mutation": {"kind": "edit", "markdown": "# Edited plan\n\nShip the workspace safely."}
            },
        }))
        .await;
    let (edit_update, edit_response) = receive_plan_mutation(&mut rx, 2).await;
    let edited_revision = edit_response["result"]["planDocument"]["current_revision"]
        ["revision_id"]
        .as_str()
        .expect("edited revision")
        .to_string();
    assert_eq!(
        edit_update["params"]["update"]["harnPlanDocument"]["current_revision"]["revision_id"],
        edited_revision
    );
    assert_eq!(
        edit_response["result"]["planDocument"]["current_revision"]["parent_revision_id"],
        initial_revision
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": initial_revision,
                "mutation": {"kind": "edit", "markdown": "# Stale overwrite"}
            },
        }))
        .await;
    let stale = recv_json(&mut rx).await;
    assert_eq!(stale["error"]["code"], ACP_PLAN_REVISION_CONFLICT_CODE);
    assert_eq!(
        stale["error"]["data"]["schemaVersion"],
        ACP_PLAN_REVISION_CONFLICT_SCHEMA
    );
    assert_eq!(stale["error"]["data"]["currentRevisionId"], edited_revision);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": edited_revision,
                "mutation": {
                    "kind": "add_comment",
                    "commentId": "review-1",
                    "anchor": {
                        "step_id": "step-1",
                        "quoted_text": "Ship the workspace",
                        "range": {"start": 0, "end": 13}
                    },
                    "body": "Prove the replay path."
                }
            },
        }))
        .await;
    let (_, comment_response) = receive_plan_mutation(&mut rx, 4).await;
    let commented_revision = comment_response["result"]["planDocument"]["current_revision"]
        ["revision_id"]
        .as_str()
        .expect("comment revision")
        .to_string();
    assert_eq!(
        comment_response["result"]["planDocument"]["comments"][0]["state"],
        "open"
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": commented_revision,
                "mutation": {
                    "kind": "change_comment_state",
                    "commentId": "review-1",
                    "state": "resolved",
                    "agentRunId": "agent-run-1",
                    "explanation": "Replay assertion added."
                }
            },
        }))
        .await;
    let (_, resolved_response) = receive_plan_mutation(&mut rx, 5).await;
    let resolved_revision = resolved_response["result"]["planDocument"]["current_revision"]
        ["revision_id"]
        .as_str()
        .expect("resolved revision")
        .to_string();
    let receipt = &resolved_response["result"]["planDocument"]["resolution_receipts"][0];
    assert_eq!(receipt["agent_run_id"], "agent-run-1");
    assert_eq!(receipt["output_revision_id"], resolved_revision);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": resolved_revision,
                "mutation": {
                    "kind": "change_comment_state",
                    "commentId": "review-1",
                    "state": "reopened",
                    "explanation": "Needs a product-path assertion."
                }
            },
        }))
        .await;
    let (_, reopened_response) = receive_plan_mutation(&mut rx, 6).await;
    let reopened_revision = reopened_response["result"]["planDocument"]["current_revision"]
        ["revision_id"]
        .as_str()
        .expect("reopened revision")
        .to_string();
    assert_eq!(
        reopened_response["result"]["planDocument"]["comments"][0]["state"],
        "reopened"
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session_id,
                "documentId": document_id,
                "expectedRevisionId": reopened_revision,
                "mutation": {
                    "kind": "approve",
                    "reviewer": "terminal-user",
                    "reason": "Apply this exact revision."
                }
            },
        }))
        .await;
    let (_, approved_response) = receive_plan_mutation(&mut rx, 7).await;
    let approved_revision = approved_response["result"]["planDocument"]["current_revision"]
        ["revision_id"]
        .as_str()
        .expect("approved revision")
        .to_string();
    assert_eq!(
        approved_response["result"]["planDocument"]["current_revision"]["parent_revision_id"],
        reopened_revision
    );
    assert_eq!(
        approved_response["result"]["planDocument"]["current_revision"]["plan"]["approval"]
            ["state"],
        "approved"
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "session/load",
            "params": {"sessionId": session_id},
        }))
        .await;
    let mut replayed_document = None;
    loop {
        let message = recv_json(&mut rx).await;
        if message["method"] == "session/update"
            && message["params"]["update"]["sessionUpdate"] == "plan"
        {
            replayed_document = Some(message["params"]["update"]["harnPlanDocument"].clone());
        }
        if message["id"] == 8 {
            break;
        }
    }
    let replayed_document = replayed_document.expect("replayed plan document");
    assert_eq!(
        replayed_document["current_revision"]["revision_id"],
        approved_revision
    );
    assert_eq!(replayed_document["comments"][0]["state"], "reopened");
    assert_eq!(
        replayed_document["resolution_receipts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    harn_vm::agent_events::clear_session_sinks(&session_id);
    harn_vm::event_log::reset_active_event_log();
}

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
