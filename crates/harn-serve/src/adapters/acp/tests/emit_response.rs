//! Typed `harness.runtime.emit_response` must share ACP's presentation stream.
//!
//! Burin observed that `harn fix --capability-migrations-only` rewrote ACP
//! fixtures from the ambient `emit_response` builtin to
//! `harness.runtime.emit_response`, which then routed through `host/call` and
//! produced no `agent_message_chunk` (harn#6374 / downstream env passthrough).

use super::*;

async fn collect_typed_emit_prompt(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: i64,
    prompt_text: &str,
) -> (String, bool) {
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
    let mut saw_host_call_emit = false;
    let mut saw_completed = false;
    for _ in 0..64 {
        let message = recv_json(response_rx).await;
        match message.get("method").and_then(|value| value.as_str()) {
            Some("host/capabilities") => {
                // Advertise the host-owned op so a regression that falls through
                // to `host/call` is observable instead of silently failing.
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": message["id"].clone(),
                        "result": {"runtime": ["emit_response"]},
                    }))
                    .expect("send host/capabilities response");
            }
            Some("host/call") => {
                let name = message["params"]["name"].as_str().unwrap_or_default();
                if name == "runtime.emit_response" {
                    saw_host_call_emit = true;
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": null,
                        }))
                        .expect("answer host/call emit_response");
                }
            }
            Some("session/update")
                if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" =>
            {
                if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                    output.push_str(text);
                }
            }
            _ if message["id"] == id => {
                assert!(
                    message.get("error").is_none() || message["error"].is_null(),
                    "typed emit_response prompt failed: {message}"
                );
                assert_eq!(message["result"]["stopReason"], "end_turn");
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_completed, "prompt {id} should complete successfully");
    (output, saw_host_call_emit)
}

#[tokio::test(flavor = "current_thread")]
async fn typed_runtime_emit_response_streams_agent_message_chunk() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session().await;

            let (output, saw_host_call) = collect_typed_emit_prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                2,
                "harness.runtime.emit_response({text: \"typed-emit-probe\"})",
            )
            .await;

            assert!(
                output.contains("typed-emit-probe"),
                "typed Runtime.emit_response must project through AcpBridge::send_update; got {output:?}"
            );
            assert!(
                !saw_host_call,
                "ACP must override harness.runtime.emit_response instead of falling through to host/call"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn typed_runtime_emit_response_survives_architect_read_only_ceiling() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, _server, session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/set_mode",
                    "params": {"sessionId": session_id, "modeId": "architect"},
                }))
                .expect("send session/set_mode");
            let ack = recv_json(&mut response_rx).await;
            assert_eq!(ack["id"], 2);
            for expected in ["current_mode_update", "config_option_update"] {
                let notification = recv_json(&mut response_rx).await;
                assert_eq!(notification["method"], "session/update");
                assert_eq!(notification["params"]["update"]["sessionUpdate"], expected);
            }

            let (output, saw_host_call) = collect_typed_emit_prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                "harness.runtime.emit_response({text: \"architect-emit-probe\"})",
            )
            .await;

            assert!(
                output.contains("architect-emit-probe"),
                "presentation-classified emit_response must pass architect read_only ceilings; got {output:?}"
            );
            assert!(
                !saw_host_call,
                "architect mode must still use the ACP presentation override"
            );
        })
        .await;
}
