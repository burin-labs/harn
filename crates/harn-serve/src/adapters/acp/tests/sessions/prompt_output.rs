use super::*;

pub(super) async fn run_prompt_with_project_capability(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: i64,
    prompt_text: &str,
    project_read_capability: bool,
) -> String {
    let message_id = format!("qualification-prompt-{id}");
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "messageId": message_id,
                "prompt": [{"type": "text", "text": prompt_text}],
            },
        }))
        .expect("send session/prompt");

    let host_capabilities = if project_read_capability {
        serde_json::json!({"project": ["read_file"]})
    } else {
        serde_json::json!({})
    };
    let mut output = String::new();
    let mut saw_completed = false;
    let mut capability_frames = 0;
    let mut visible_frames = 0;
    for _ in 0..64 {
        let message = recv_json(response_rx).await;
        match message.get("method").and_then(|value| value.as_str()) {
            Some("host/capabilities") => {
                assert_eq!(message["params"]["sessionId"], session_id);
                assert_eq!(
                    message["params"]["promptCorrelation"]["messageId"],
                    message_id
                );
                capability_frames += 1;
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": message["id"].clone(),
                        "result": host_capabilities.clone(),
                    }))
                    .expect("send host/capabilities response");
            }
            Some("session/update")
                if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" =>
            {
                assert_eq!(message["params"]["sessionId"], session_id);
                assert_eq!(
                    message["params"]["promptCorrelation"]["messageId"], message_id,
                    "the canonical prompt must install its scoped output for each turn"
                );
                visible_frames += 1;
                let content = &message["params"]["update"]["content"];
                let text = content["text"].as_str().expect("chunk text");
                let visible_delta = content["_meta"]["harn"]["visible_delta"]
                    .as_str()
                    .expect("visible_delta");
                assert!(
                    !visible_delta.contains(if prompt_text == "one" { "two" } else { "one" }),
                    "each prompt turn gets a fresh bridge visible-text state"
                );
                output.push_str(text);
            }
            _ if message["id"] == id => {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                assert!(
                    message.get("params").is_none(),
                    "responses keep their original shape"
                );
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_completed, "prompt {id} should complete successfully");
    assert!(
        capability_frames > 0,
        "prompt {id} must reach its host bridge"
    );
    assert!(
        visible_frames > 0,
        "prompt {id} must emit correlated visible output"
    );
    output
}

pub(super) async fn run_json_prompt(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: i64,
    prompt_text: &str,
    expected_live_assistant: Option<&str>,
) -> serde_json::Value {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt_text}],
            },
        }))
        .expect("send session/prompt");

    let mut output = String::new();
    for _ in 0..64 {
        let message = recv_json(response_rx).await;
        match message.get("method").and_then(|value| value.as_str()) {
            Some("host/capabilities") => {
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": message["id"].clone(),
                        "result": {},
                    }))
                    .expect("send host/capabilities response");
            }
            Some("session/update")
                if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" =>
            {
                if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                    output.push_str(text);
                }
            }
            _ if message["id"] == id => {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                let json_output = if let Some(expected) = expected_live_assistant {
                    output.strip_prefix(expected).unwrap_or_else(|| {
                        panic!("missing live assistant prefix {expected:?}: {output:?}")
                    })
                } else {
                    &output
                };
                return serde_json::from_str(json_output.trim()).unwrap_or_else(|error| {
                    panic!("prompt JSON output after live projection: {error}; output={output:?}")
                });
            }
            _ => {}
        }
    }
    panic!("prompt {id} did not complete")
}
