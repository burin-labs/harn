//! Session restore across a process boundary.
//!
//! `session/list` answers what exists from the canonical session store, so
//! `session/load` has to answer restorability from the same store. These tests
//! pin both directions: a store-only session loads, and an id no store holds
//! still fails loudly.

use super::event_log_barrier::ResetActiveEventLog;
use super::*;

/// A session this server never saw, present only in the project's canonical
/// store, must load — the store is the same oracle `session/list` answers from,
/// so anything listable is loadable.
///
/// The failure this pins down: restorability used to be decided by replaying
/// the observability event log, which holds nothing for a session recorded by a
/// previous process, so every listed session answered `unknown session`.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_restores_a_session_only_the_canonical_store_holds() {
    let _reset = ResetActiveEventLog;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let project = dir.path().join("project");
            std::fs::create_dir_all(project.join(".harn")).expect("project state dir");
            let session_id = "01a003d0-1513-7271-90aa-4542d6059498";

            // Seed the canonical store exactly as a prior process would have
            // left it: a session row plus its durable transcript. Nothing here
            // touches the event log.
            {
                use harn_session_store::{
                    AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
                };
                let store = SqliteSessionStore::open(project.join(".harn/session-store.sqlite"))
                    .expect("open canonical store");
                store
                    .create(CreateSession {
                        id: Some(session_id.to_string()),
                        ..CreateSession::default()
                    })
                    .await
                    .expect("create stored session");
                store
                    .append(
                        session_id,
                        AppendEvent::new(
                            SessionEventKind::Message,
                            serde_json::json!({
                                "transcript_event": {
                                    "kind": "message",
                                    "role": "assistant",
                                    "visibility": "public",
                                    "text": "the earlier conversation",
                                }
                            }),
                        ),
                    )
                    .await
                    .expect("append stored transcript");
            }

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::new(None),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/load",
                    "params": {
                        "sessionId": session_id,
                        "cwd": project.display().to_string(),
                    },
                }))
                .expect("send session/load");

            let mut replayed_text = String::new();
            let response = loop {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 1 {
                    break message;
                }
                replayed_text.push_str(&message.to_string());
            };

            assert!(
                response.get("error").is_none(),
                "a session the canonical store holds must load, got {response}"
            );
            assert!(
                replayed_text.contains("the earlier conversation"),
                "session/load must replay the stored transcript, got {replayed_text}"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// The one case that should still fail loudly: an id no store holds. Without
/// this, "load everything" would turn a typo into a silent empty session.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_still_rejects_an_id_no_store_holds() {
    let _reset = ResetActiveEventLog;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let project = dir.path().join("project");
            std::fs::create_dir_all(&project).expect("project dir");

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::new(None),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/load",
                    "params": {
                        "sessionId": "never-existed",
                        "cwd": project.display().to_string(),
                    },
                }))
                .expect("send session/load");

            let response = loop {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 1 {
                    break message;
                }
            };
            assert_eq!(
                response["error"]["code"], -32602,
                "an id no store holds stays a loud failure, got {response}"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}
