use super::*;

#[tokio::test(flavor = "current_thread")]
async fn unsupported_session_inputs_are_refused_before_admission_or_restore() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    let workspace = tempfile::tempdir().unwrap();
    for method in ["session/new", "session/load", "session/resume"] {
        for option in [
            serde_json::json!({"mcpServers": [{"name": "ignored", "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": []}]}),
            serde_json::json!({"mcpServers": "not an array"}),
            serde_json::json!({"systemPrompt": {"mode": "append", "content": "must be applied"}}),
        ] {
            let mut params = serde_json::json!({"cwd": workspace.path(), "sessionId": "unadmitted", "environmentPolicy": {"kind": "isolated"}});
            params
                .as_object_mut()
                .unwrap()
                .extend(option.as_object().unwrap().clone());
            server.handle_incoming_message(serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})).await;
            let response = recv_response_with_id(&mut rx, 7).await;
            assert_eq!(response["error"]["code"], -32602, "{response}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("does not support")),
                "{response}"
            );
            assert!(
                server.sessions.is_empty(),
                "refusal must precede session admission"
            );
        }
    }
    server.handle_incoming_message(serde_json::json!({
        "jsonrpc": "2.0", "id": 8, "method": "session/new",
        "params": {"cwd": workspace.path(), "mcpServers": [], "environmentPolicy": {"kind": "isolated"}},
    })).await;
    let response = recv_response_with_id(&mut rx, 8).await;
    assert!(response["result"]["sessionId"].is_string(), "{response}");
    assert_eq!(
        server.sessions.len(),
        1,
        "empty-server control must admit a real session"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn negotiated_terminal_support_controls_the_real_prompt_dispatch() {
    harn_clock::test_support::with_local_tasks(async {
        let mut admitted = 0;
        for (capabilities, terminal_supported) in [
            (serde_json::json!({}), false),
            (serde_json::json!({"terminal": false}), false),
            (serde_json::json!({"terminal": "true"}), false),
            (serde_json::json!({"terminal": true}), true),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(run_acp_channel_server(
                AcpServerConfig::new(None), request_rx, response_tx,
            ));
            request_tx.send(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": 1, "clientCapabilities": capabilities},
            })).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 1).await.get("result").is_some());
            request_tx.send(serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "session/new",
                "params": {"cwd": workspace.path(), "environmentPolicy": {"kind": "isolated"}},
            })).unwrap();
            let created = recv_response_with_id(&mut response_rx, 2).await;
            let session_id = created["result"]["sessionId"].as_str().unwrap();
            request_tx.send(serde_json::json!({
                "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
                "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "run_command(\"printf capability-probe\")"}]},
            })).unwrap();

            let mut terminals = Vec::new();
            let mut response = None;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 3 && message.get("method").is_none() {
                    response = Some(message);
                    break;
                }
                let Some(method) = message.get("method").and_then(serde_json::Value::as_str) else { continue; };
                if message.get("id").is_none() { continue; }
                let result = match method {
                    "terminal/create" => {
                        assert!(terminal_supported, "unadvertised terminal request: {message}");
                        serde_json::json!({"terminalId": "negotiated-terminal"})
                    }
                    "terminal/wait_for_exit" => serde_json::json!({"exitCode": 0}),
                    "terminal/output" => serde_json::json!({"output": "capability-probe", "exitStatus": {"exitCode": 0}}),
                    _ => serde_json::json!({}),
                };
                if method.starts_with("terminal/") { terminals.push(method.to_owned()); }
                request_tx.send(serde_json::json!({"jsonrpc": "2.0", "id": message["id"], "result": result})).unwrap();
            }
            let response = response.expect("the prompt must settle");
            if terminal_supported {
                assert_eq!(terminals, ["terminal/create", "terminal/wait_for_exit", "terminal/output", "terminal/release"]);
                assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
                admitted += 1;
            } else {
                assert!(terminals.is_empty(), "{terminals:?}");
                assert!(response["error"]["message"].as_str().is_some_and(|message| message.contains("did not advertise")), "{response}");
            }
            drop(request_tx);
            server.await.unwrap();
        }
        assert_eq!(admitted, 1, "the supported control must actually dispatch");
    }).await;
}
