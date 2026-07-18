use super::*;

struct ResetActiveEventLog;

impl Drop for ResetActiveEventLog {
    fn drop(&mut self) {
        harn_vm::event_log::reset_active_event_log();
    }
}

#[test]
fn cancelled_prompt_retains_persistence_failure_metadata() {
    let error =
        harn_vm::agent_events::AgentEventSinkError::new("event_log", "injected append failure");

    let result = super::super::prompt::cancelled_prompt_result(Some(&error));

    assert_eq!(result["stopReason"], "cancelled");
    assert!(result["_meta"]["harn"]["persistenceError"]
        .as_str()
        .expect("persistence error metadata")
        .contains("injected append failure"));
}

#[tokio::test(flavor = "current_thread")]
async fn completed_prompt_is_durable_before_immediate_session_load() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _reset = ResetActiveEventLog;
            let _log = harn_vm::event_log::install_memory_for_current_thread(32);
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;
            let prompt = format!(
                "__host_agent_emit_event(\"{session_id}\", \"progress_reported\", \
                 {{message: \"durable before prompt response\"}})"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt}],
                    },
                }))
                .expect("send session/prompt");

            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                } else if message["id"] == 2 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    break;
                }
            }

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/load",
                    "params": {"sessionId": session_id},
                }))
                .expect("send immediate session/load");

            let mut replay_notification = None;
            let loaded = loop {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 3 {
                    break message;
                }
                if message["method"] == "session/update" && message["_harn"]["replayed"] == true {
                    replay_notification = Some(message);
                }
            };

            let replay_notification =
                replay_notification.expect("session/load must replay the prompt event");
            assert_eq!(
                replay_notification["params"]["update"]["_meta"]["harn"]["message"],
                "durable before prompt response"
            );
            let replayed = loaded["result"]["replayed"]
                .as_array()
                .expect("session/load replay list");
            assert_eq!(
                replayed
                    .iter()
                    .filter(|event| event["type"] == "progress_reported")
                    .count(),
                1,
                "the completed prompt event must be persisted exactly once"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}
