use super::*;
use harn_vm::agent_events::{emit_event, AgentEvent, AgentEventTransport};

#[tokio::test(flavor = "current_thread")]
async fn idle_plan_mutation_does_not_leave_a_transport_for_late_prompt_events() {
    harn_vm::event_log::reset_active_event_log();
    let _log = harn_vm::event_log::install_memory_for_current_thread(64);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let output = AcpOutput::Channel(tx);
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), output.clone());
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}}
        }))
        .await;
    let created = harn_clock::test_support::within("created session", rx.recv())
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    let session = created["result"]["sessionId"].as_str().unwrap();
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({"plan": [{"content": "Keep transport ownership", "status": "pending"}]}),
    );
    let seed = harn_vm::llm::plan::create_plan_document_event(
        plan,
        "agent",
        "update_plan",
        "2026-01-01T00:00:00Z",
        "transport-plan-seed",
    )
    .unwrap();
    harn_vm::llm::plan::persist_plan_document_event(session, &seed).unwrap();
    let correlation: AcpPromptCorrelation =
        serde_json::from_value(serde_json::json!({"messageId": "old-prompt"})).unwrap();
    let old = AgentEventTransport::new(Arc::new(events::AcpAgentEventSink::new(
        output.for_prompt(correlation),
    )));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": ACP_METHOD_SESSION_PLAN_DOCUMENT_MUTATE,
            "params": {
                "sessionId": session,
                "documentId": seed.document().document_id,
                "expectedRevisionId": seed.document().current_revision.revision_id,
                "mutation": {"kind": "edit", "markdown": "# Revised plan"}
            }
        }))
        .await;
    let update = harn_clock::test_support::within("reached plan mutation projection", rx.recv())
        .await
        .unwrap();
    let update: serde_json::Value = serde_json::from_str(&update).unwrap();
    assert_eq!(update["method"], "session/update");
    assert_eq!(update["params"]["update"]["sessionUpdate"], "plan");
    assert!(update["params"].get("promptCorrelation").is_none());
    let response = harn_clock::test_support::within("plan mutation result", rx.recv())
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 2);
    assert!(response.get("error").is_none());
    assert!(response["result"]["planDocument"].is_object());
    assert!(
        rx.try_recv().is_err(),
        "exactly one idle mutation projection"
    );

    old.with(|| {
        emit_event(&AgentEvent::IterationStart {
            session_id: session.to_owned(),
            iteration: 1,
            provider: String::new(),
            model: String::new(),
        });
    });
    let late = harn_clock::test_support::within(
        "nonempty old prompt delivery after idle mutation",
        rx.recv(),
    )
    .await
    .unwrap();
    let late: serde_json::Value = serde_json::from_str(&late).unwrap();
    assert_eq!(late["method"], "_harn/agentEvent");
    assert_eq!(
        late["params"]["promptCorrelation"]["messageId"],
        "old-prompt"
    );
    assert!(
        rx.try_recv().is_err(),
        "idle mutation cannot leave a dynamic uncorrelated duplicate"
    );
    harn_vm::agent_events::clear_session_sinks(session);
    harn_vm::event_log::reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn event_emitted_after_same_session_rebind_keeps_originating_wire_correlation() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let output = AcpOutput::Channel(tx);
    let transport = |id: &str| {
        let correlation: AcpPromptCorrelation =
            serde_json::from_value(serde_json::json!({"messageId": id})).unwrap();
        AgentEventTransport::new(Arc::new(events::AcpAgentEventSink::new(
            output.for_prompt(correlation),
        )))
    };
    let old = transport("old-prompt");
    let new = transport("new-prompt");
    let event = AgentEvent::IterationStart {
        session_id: "same-session".into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    };
    new.scope(async {
        emit_event(&event);
        old.scope(async {
            emit_event(&event);
        })
        .await;
        emit_event(&event);
    })
    .await;
    let mut identities = Vec::new();
    for _ in 0..3 {
        let line =
            harn_clock::test_support::within("nonempty correlated event delivery", rx.recv())
                .await
                .unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["method"], "_harn/agentEvent");
        let projected: AcpPromptCorrelation =
            serde_json::from_value(frame["params"]["promptCorrelation"].clone()).unwrap();
        identities.push(projected.message_id.unwrap());
    }
    assert_eq!(identities, ["new-prompt", "old-prompt", "new-prompt"]);
    assert!(
        rx.try_recv().is_err(),
        "transport delivery cannot duplicate registry delivery"
    );
}
