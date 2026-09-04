use super::*;

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
