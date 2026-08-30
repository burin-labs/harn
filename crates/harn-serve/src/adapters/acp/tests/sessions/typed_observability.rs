use super::*;

pub(super) fn timeline_evidence(
    session_id: &str,
) -> harn_vm::orchestration::ExecutionEvidenceRecord {
    let metadata = || BTreeMap::from([("session_id".to_string(), serde_json::json!(session_id))]);
    harn_vm::orchestration::ExecutionEvidenceRecord {
        trace_spans: vec![
            RunTraceSpanRecord {
                trace_id: "trace-acp".to_string(),
                span_id: 1,
                kind: "pipeline".to_string(),
                name: "root".to_string(),
                start_ms: 1,
                duration_ms: 2,
                metadata: metadata(),
                ..RunTraceSpanRecord::default()
            },
            RunTraceSpanRecord {
                trace_id: "trace-acp".to_string(),
                span_id: 2,
                parent_id: Some(1),
                kind: "tool_call".to_string(),
                name: "child".to_string(),
                start_ms: 2,
                duration_ms: 3,
                metadata: metadata(),
                ..RunTraceSpanRecord::default()
            },
        ],
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn typed_observability_logs_never_become_assistant_messages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 19,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{
                            "type": "text",
                            "text": concat!(
                                "harness.obs.log_info(\"private info\", {phase: \"setup\"})\n",
                                "harness.obs.log_warn(\"private warning\")\n",
                                "harness.stdio.println(\"public reply\")",
                            ),
                        }],
                    },
                }))
                .expect("send session/prompt");

            let mut assistant_text = String::new();
            let mut log_messages = Vec::new();
            let mut completed = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host/capabilities response");
                    continue;
                }
                if message["method"] == "session/update" {
                    let update = &message["params"]["update"];
                    if update["sessionUpdate"] == "agent_message_chunk" {
                        assistant_text.push_str(
                            update["content"]["text"]
                                .as_str()
                                .expect("assistant chunk text"),
                        );
                    } else if update["sessionUpdate"] == "log" {
                        log_messages.push(update["_meta"]["harn"].clone());
                    }
                }
                if message["id"] == 19 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    completed = true;
                    break;
                }
            }

            assert!(completed, "prompt should complete");
            assert_eq!(assistant_text, "public reply\n");
            assert_eq!(log_messages.len(), 2);
            assert_eq!(log_messages[0]["level"], "info");
            assert_eq!(log_messages[0]["message"], "private info");
            assert_eq!(log_messages[0]["fields"]["phase"], "setup");
            assert_eq!(log_messages[1]["level"], "warn");
            assert_eq!(log_messages[1]["message"], "private warning");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}
