use super::*;

async fn recv_response_with_id(
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    id: u64,
) -> serde_json::Value {
    for _ in 0..32 {
        let message = recv_json(response_rx).await;
        if message["id"].as_u64() == Some(id) {
            return message;
        }
    }
    panic!("timed out waiting for response {id}");
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_recap_query_uses_the_canonical_store_contract() {
    use harn_session_store::{
        AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let project = tempfile::tempdir().expect("project root");
            std::fs::create_dir_all(project.path().join(".harn")).expect("session store dir");
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::new(None),
                    serde_json::json!(project.path()),
                )
                .await;
            let store = SqliteSessionStore::open(project.path().join(".harn/session-store.sqlite"))
                .expect("canonical recap store");
            store
                .create(CreateSession {
                    id: Some(session_id.clone()),
                    ..CreateSession::default()
                })
                .await
                .expect("create recap session");
            let mut event = AppendEvent::new(
                SessionEventKind::Message,
                serde_json::json!({
                    "transcript_event": {
                        "kind": "message",
                        "role": "user",
                        "visibility": "public",
                        "text": "Summarize the incident",
                        "metadata": {}
                    }
                }),
            );
            event
                .headers
                .insert("run_id".to_string(), "run-acp".to_string());
            event
                .headers
                .insert("turn_id".to_string(), "turn-acp".to_string());
            store
                .append(&session_id, event)
                .await
                .expect("append recap source event");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 19,
                    "method": harn_vm::session_recap::SESSION_RECAP_QUERY_METHOD,
                    "params": {"sessionId": session_id, "limit": 10},
                }))
                .expect("send recap query");
            let response = recv_response_with_id(&mut response_rx, 19).await;
            assert_eq!(response["result"]["state"], "available");
            assert_eq!(response["result"]["snapshot"]["coverage"]["scanned"], 1);
            assert_eq!(response["result"]["snapshot"]["coverage"]["matched"], 1);
            assert_eq!(
                response["result"]["snapshot"]["turns"][0]["prompts"][0]["text"],
                "Summarize the incident"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": harn_vm::session_recap::SESSION_RECAP_QUERY_METHOD,
                    "params": {"sessionId": "missing-session"},
                }))
                .expect("send missing recap query");
            let missing = recv_response_with_id(&mut response_rx, 21).await;
            assert_eq!(
                missing["result"],
                serde_json::json!({"state": "unavailable", "reason": "session_missing"})
            );

            drop(request_tx);
            server.await.unwrap();
        })
        .await;
}
