use super::super::event_log_barrier::ResetActiveEventLog;
use super::*;
use harn_vm::event_log::EventLog as _;
use harn_vm::orchestration::{save_run_record, RunRecord, RunTraceSpanRecord};
use harn_vm::session_timeline::SESSION_TIMELINE_SCHEMA_VERSION;

fn timeline_evidence(session_id: &str) -> harn_vm::orchestration::ExecutionEvidenceRecord {
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
async fn acp_session_timeline_query_and_subscribe_use_event_log() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _reset = ResetActiveEventLog;
            let log = harn_vm::event_log::install_memory_for_current_thread(16);
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;
            let topic = harn_vm::session_timeline::agent_events_topic(&session_id);

            log.append(
                &topic,
                harn_vm::event_log::LogEvent::new(
                    "tool_call",
                    serde_json::json!({
                        "session_id": session_id.clone(),
                        "event": {
                            "type": "tool_call",
                            "session_id": session_id.clone(),
                            "tool_call_id": "tool-1",
                            "tool_name": "read",
                            "status": "pending",
                            "raw_input": {"authorization": "should-redact"}
                        }
                    }),
                )
                .with_headers(BTreeMap::from([(
                    "session_id".to_string(),
                    session_id.clone(),
                )])),
            )
            .await
            .unwrap();

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": harn_vm::session_timeline::SESSION_TIMELINE_QUERY_METHOD,
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send timeline query");
            let snapshot = recv_json(&mut response_rx).await;
            assert_eq!(snapshot["id"], 20);
            assert_eq!(
                snapshot["result"]["schemaVersion"],
                SESSION_TIMELINE_SCHEMA_VERSION
            );
            let expected_coverage =
                serde_json::json!({"returned": 1, "available": 1, "truncated": false});
            assert_eq!(snapshot["result"]["coverage"], expected_coverage);
            assert_eq!(snapshot["result"]["nodes"][0]["category"], "agent_event");
            assert_eq!(
                snapshot["result"]["nodes"][0]["attributes"]["event"]["raw_input"]["authorization"],
                serde_json::json!(harn_vm::redact::REDACTED_PLACEHOLDER)
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 24,
                    "method": harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD,
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send session view query");
            let session_view = recv_json(&mut response_rx).await;
            assert_eq!(session_view["id"], 24);
            assert_eq!(session_view["result"]["schema"], "harn.session_view.v1");
            assert_eq!(session_view["result"]["session"]["session_id"], session_id);
            assert_eq!(session_view["result"]["session"]["last_event_id"], 1);
            assert!(session_view["result"]["projection"]["projection_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));

            let temp = tempfile::tempdir().unwrap();
            let run_path = temp.path().join("timeline-run.json");
            save_run_record(
                &RunRecord {
                    id: "timeline-run".to_string(),
                    evidence: timeline_evidence(&session_id),
                    ..RunRecord::default()
                },
                Some(run_path.to_str().unwrap()),
            )
            .unwrap();
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 23,
                    "method": harn_vm::session_timeline::SESSION_TIMELINE_QUERY_METHOD,
                    "params": {
                        "sessionId": session_id.clone(),
                        "runId": "timeline-run",
                        "runPath": run_path.display().to_string(),
                    },
                }))
                .expect("send timeline run query");
            let run_snapshot = recv_json(&mut response_rx).await;
            assert_eq!(run_snapshot["id"], 23);
            assert!(run_snapshot.get("error").is_none(), "{run_snapshot}");
            let root = run_snapshot["result"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == "span:trace-acp:1")
                .expect("timeline root span");
            assert_eq!(root["children"][0], "span:trace-acp:2");

            let from_cursor = serde_json::json!({
                "topics": BTreeMap::from([(topic.as_str().to_string(), 1_u64)]),
            });
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": harn_vm::session_timeline::SESSION_TIMELINE_SUBSCRIBE_METHOD,
                    "params": {
                        "sessionId": session_id.clone(),
                        "subscriptionId": "timeline-test",
                        "fromCursor": from_cursor,
                    },
                }))
                .expect("send timeline subscribe");
            let subscribed = recv_json(&mut response_rx).await;
            assert_eq!(subscribed["id"], 21);
            assert_eq!(subscribed["result"]["subscriptionId"], "timeline-test");

            log.append(
                &topic,
                harn_vm::event_log::LogEvent::new(
                    "agent_message_chunk",
                    serde_json::json!({
                        "session_id": session_id.clone(),
                        "event": {
                            "type": "agent_message_chunk",
                            "session_id": session_id.clone(),
                            "content": "hello"
                        }
                    }),
                )
                .with_headers(BTreeMap::from([(
                    "session_id".to_string(),
                    session_id.clone(),
                )])),
            )
            .await
            .unwrap();
            let update = recv_json(&mut response_rx).await;
            assert_eq!(
                update["method"],
                harn_vm::session_timeline::SESSION_TIMELINE_UPDATE_METHOD
            );
            assert_eq!(update["params"]["subscriptionId"], "timeline-test");
            assert_eq!(
                update["params"]["update"]["node"]["name"],
                "agent_message_chunk"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 22,
                    "method": harn_vm::session_timeline::SESSION_TIMELINE_UNSUBSCRIBE_METHOD,
                    "params": {"subscriptionId": "timeline-test"},
                }))
                .expect("send timeline unsubscribe");
            let unsubscribed = recv_json(&mut response_rx).await;
            assert_eq!(unsubscribed["id"], 22);
            assert_eq!(unsubscribed["result"]["removed"], true);

            drop(request_tx);
            server.await.unwrap();
        })
        .await;
}
