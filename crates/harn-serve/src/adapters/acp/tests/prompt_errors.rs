use super::*;

async fn new_session(server: &mut AcpServer, rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    recv_json(rx).await["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string()
}

async fn prompt_response(
    server: &mut AcpServer,
    rx: &mut mpsc::UnboundedReceiver<String>,
    id: i64,
    params: serde_json::Value,
) -> serde_json::Value {
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": params,
        }))
        .await;
    for _ in 0..8 {
        let message = recv_json(rx).await;
        if message["id"] == id {
            return message;
        }
    }
    panic!("session/prompt {id} did not produce a response")
}

fn assert_prompt_error_data(response: &serde_json::Value, code: i64, terminal_class: &str) {
    assert_eq!(response["error"]["code"], code);
    assert_eq!(
        response["error"]["data"],
        serde_json::json!({
            "schema": ACP_PROMPT_ERROR_DATA_SCHEMA,
            "terminalClass": terminal_class,
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compile_error_clears_active_inject_bridge_and_has_typed_data() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    let session_id = new_session(&mut server, &mut rx).await;

    let response = prompt_response(
        &mut server,
        &mut rx,
        2,
        serde_json::json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "let x ="}],
        }),
    )
    .await;
    assert_prompt_error_data(&response, -32000, "generic_throw");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": "must not target the failed prompt",
            },
        }))
        .await;
    let rejected = recv_json(&mut rx).await;
    assert_eq!(rejected["error"]["code"], -32004);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no active prompt"));
}

#[tokio::test(flavor = "current_thread")]
async fn execution_error_preserves_structured_class_over_misleading_prose() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{
                            "type": "text",
                            "text": "throw_error(\"provider rate limit 429 in /tmp/run-429/result\", \"tool_rejected\")",
                        }],
                    },
                }))
                .expect("send session/prompt");

            let mut prompt_response = None;
            for _ in 0..64 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["id"] == 2 {
                    prompt_response = Some(message);
                    break;
                }
            }

            let response = prompt_response.expect("session/prompt response");
            assert_prompt_error_data(&response, -32000, "tool_policy_rejected");
            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn execution_error_projects_resource_contention_as_typed_data() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{
                            "type": "text",
                            "text": "throw_error(\"session_store: database is locked\", \"resource_busy\")",
                        }],
                    },
                }))
                .expect("send session/prompt");

            let mut prompt_response = None;
            for _ in 0..64 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["id"] == 2 {
                    prompt_response = Some(message);
                    break;
                }
            }

            let response = prompt_response.expect("session/prompt response");
            assert_prompt_error_data(&response, -32000, "resource_busy");
            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_errors_are_typed_protocol_failures() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    for (id, params) in [
        (1, serde_json::json!({"prompt": []})),
        (
            2,
            serde_json::json!({
                "sessionId": "unknown-session",
                "prompt": [{"type": "text", "text": "unknown session"}],
            }),
        ),
        (
            3,
            serde_json::json!({
                "sessionId": "unknown-session",
                "prompt": [{"type": "future_content"}],
            }),
        ),
    ] {
        let response = prompt_response(&mut server, &mut rx, id, params).await;
        assert_prompt_error_data(&response, -32602, "agent_loop_protocol_failure");
    }
}
