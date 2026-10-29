use super::*;

#[tokio::test(flavor = "current_thread")]
async fn config_discriminants_cannot_change_a_string_setting_through_boolean_values() {
    let workspace = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_incoming_message(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/new",
        "params": {"cwd": workspace.path(), "mcpServers": [], "environmentPolicy": {"kind": "isolated"}},
    })).await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let session_id = created["result"]["sessionId"].as_str().unwrap();
    for (index, (input, expected_mode)) in [
        (serde_json::json!({"value": "code"}), Some("code")),
        (serde_json::json!({"type": "boolean", "value": "ask"}), None),
        (serde_json::json!({"type": "boolean", "value": true}), None),
        (serde_json::json!({"type": "boolean", "value": false}), None),
        (serde_json::json!({"type": 1, "value": "ask"}), None),
        (serde_json::json!({"value": true}), None),
        (
            serde_json::json!({"type": "future", "value": "ask"}),
            Some("ask"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = u64::try_from(index).unwrap() + 2;
        let mut params = input;
        params["sessionId"] = serde_json::json!(session_id);
        params["configId"] = serde_json::json!("mode");
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "session/set_config_option", "params": params,
            }))
            .await;
        let response = recv_response_with_id(&mut rx, id).await;
        if let Some(expected) = expected_mode {
            assert_eq!(
                response["result"]["configOptions"][0]["currentValue"], expected,
                "{response}"
            );
        } else {
            assert_eq!(response["error"]["code"], -32602, "{response}");
            assert_eq!(
                server.sessions[session_id].current_mode_id, "code",
                "refusal must not mutate mode"
            );
        }
    }
}
