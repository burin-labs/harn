use super::*;

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_tool_call_targets_registered_call() {
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

    // Pretend a bridge-routed tool call is in flight in this session's
    // explicit cancellation address space.
    let host_bridge = attach_test_host_bridge(&mut server, &session_id);
    let registry = host_bridge.tool_call_cancellation_registry();
    let (_handle, _guard) = registry.register(session_id.clone(), "call_42", "git_push");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "toolCallId": "call_42",
                "reason": "user clicked stop",
                "injectReminder": false,
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["result"]["status"], "cancelled");
    assert_eq!(response["result"]["callId"], "call_42");
    assert_eq!(response["result"]["tool"], "git_push");

    // A second cancel must report `already_cancelled` so the host can
    // suppress redundant retries.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "toolCallId": "call_42",
                "reason": "still stopping",
                "injectReminder": false,
            },
        }))
        .await;
    let second = recv_json(&mut rx).await;
    assert_eq!(second["result"]["status"], "already_cancelled");
    assert_eq!(second["result"]["tool"], "git_push");
}
