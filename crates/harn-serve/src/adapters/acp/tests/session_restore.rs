//! Session restore across a process boundary.
//!
//! `session/list` answers what exists from the canonical session store, so
//! `session/load` has to answer restorability from the same store. These tests
//! pin both directions: a store-only session loads, and an id no store holds
//! still fails loudly.

use super::event_log_barrier::ResetActiveEventLog;
use super::*;

#[test]
fn a_live_cwd_filter_refuses_two_unresolvable_paths() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().to_path_buf();
    let id = "failed-cwd-filter";
    let mut server = AcpServer::new(AcpServerConfig::new(None).with_launcher_environment(
        harn_vm::security::LauncherEnvironment::from_snapshot(Default::default()),
    ));
    server
        .insert_session(id.into(), cwd.clone(), SessionInfo::default())
        .unwrap();
    let params = serde_json::json!({"cwd":cwd});
    let session = server.sessions.get(id).unwrap();
    assert!(
        server.session_matches_list_filters(id, session, &params),
        "known existing cwd must match"
    );
    directory.close().unwrap();
    assert!(
        !server.session_matches_list_filters(id, session, &params),
        "two failed path resolutions are not an equal scope"
    );
}

/// A session this server never saw, present only in the project's canonical
/// store, must load — the store is the same oracle `session/list` answers from,
/// so anything listable is loadable.
///
/// The failure this pins down: restorability used to be decided by replaying
/// the observability event log, which holds nothing for a session recorded by a
/// previous process, so every listed session answered `unknown session`.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_restores_a_session_only_the_canonical_store_holds() {
    assert_store_only_session_restores(false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_restores_the_selected_nested_workspace() {
    assert_store_only_session_restores(true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_shared_physical_store_keeps_selected_workspaces_isolated() {
    assert_store_only_session_restores(true, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn nested_workspace_cold_load_replays_the_actual_journal_admitted_reply() {
    let _reset = ResetActiveEventLog;
    tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().expect("parent workspace");
        std::fs::write(dir.path().join("harn.toml"), "").unwrap();
        let project = dir.path().join("child");
        std::fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let pipeline = project.join("reply.harn");
        std::fs::write(&pipeline, r#"import { agent_loop } from "std/agent/loop"
pipeline default(harness: Harness) {
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({text: "private progress draft", tool_calls: [{
    id: "saved-noop", name: "noop", arguments: {},
  }]})
  harness.llm.mock_enqueue({text: "admitted stored reply"})
  let tools = tool_registry()
  tools = tool_define(tools, "noop", "Read a deterministic fixture", {
    handler: { args -> "actual tool result" }, parameters: {},
    returns: {type: "string"}, annotations: {kind: "read"},
  })
  agent_loop(harness, prompt, nil, {provider: "mock", tools: tools, tool_format: "native"})
  harness.runtime.store_set("selected-execution-cwd", cwd)
  assert(len(harness.llm.mock_calls()) == 2)
}
"#).unwrap();
        let launcher = harn_vm::security::LauncherEnvironment::from_snapshot(Default::default());
        let config = AcpServerConfig::for_pipeline(pipeline.display().to_string()).with_launcher_environment(launcher.clone());
        let (tx, mut rx, server, session_id) = start_acp_code_session_with_config(config, serde_json::json!(project)).await;
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{
            "sessionId":session_id, "prompt":[{"type":"text", "text":"Run the tool then reply"}],
        }})).unwrap();
        loop {
            let message = recv_json(&mut rx).await;
            if message["method"] == "host/capabilities" {
                tx.send(serde_json::json!({"jsonrpc":"2.0", "id":message["id"], "result":{}})).unwrap();
            } else if message["id"] == 3 {
                assert!(message.get("error").is_none(), "actual prompt: {message}");
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
        }
        let moved = dir.path().join("moved-child");
        std::fs::create_dir(&moved).unwrap();
        let moved = moved.canonicalize().unwrap();
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":4, "method":"harn.session_reanchor", "params":{
            "sessionId":session_id, "path":moved,
        }})).unwrap();
        loop { let message = recv_json(&mut rx).await; if message["id"] == 4 {
            assert!(message.get("error").is_none(), "reanchor: {message}");
            assert_eq!(message["result"]["changed"], true); break;
        }}
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":5, "method":"session/prompt", "params":{
            "sessionId":session_id, "prompt":[{"type":"text", "text":"Run again after reanchor"}],
        }})).unwrap();
        loop { let message = recv_json(&mut rx).await;
            if message["method"] == "host/capabilities" { tx.send(serde_json::json!({"jsonrpc":"2.0", "id":message["id"], "result":{}})).unwrap(); }
            else if message["id"] == 5 { assert!(message.get("error").is_none(), "reanchored prompt: {message}");
                assert_eq!(message["result"]["stopReason"], "end_turn"); break;
            }
        }
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":6, "method":"session/list", "params":{"cwd":moved}})).unwrap();
        loop { let message = recv_json(&mut rx).await; if message["id"] == 6 {
            assert!(message["result"]["sessions"].as_array().unwrap().iter().any(|row| row["sessionId"] == session_id), "live list must follow execution cwd: {message}"); break;
        }}
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":70,
            "method":harn_vm::agent_sessions::CANONICAL_HISTORY_BOUNDARIES_METHOD,
            "params":{"sessionId":session_id}})).unwrap();
        loop { let message = recv_json(&mut rx).await; if message["id"] == 70 {
            assert!(message.get("error").is_none(), "reanchored history boundaries: {message}");
            assert!(!message["result"]["positions"].as_array().unwrap().is_empty(), "actual journal history must be reached: {message}"); break;
        }}
        let fork_id = format!("{session_id}-fork");
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":7, "method":"session/fork", "params":{"sessionId":session_id, "id":fork_id}})).unwrap();
        loop { let message = recv_json(&mut rx).await; if message["id"] == 7 {
            assert!(message.get("error").is_none(), "reanchored fork: {message}");
            assert_eq!(message["result"]["sessionId"], fork_id); break;
        }}
        drop(tx);
        server.await.unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(project.join(".harn/store.json")).unwrap()).unwrap();
        assert_eq!(saved["selected-execution-cwd"], moved.display().to_string(), "registered builtin must keep fixed storage while execution cwd changes");
        assert!(!moved.join(".harn/session-store.sqlite").exists());
        assert!(!moved.join(".harn/store.json").exists());
        assert!(!dir.path().join(".harn/session-store.sqlite").exists(), "execution manifest must not redirect the journal to its empty parent");
        let (tx, requests) = mpsc::unbounded_channel();
        let (responses, mut rx) = mpsc::unbounded_channel();
        let server = tokio::task::spawn_local(super::run_acp_channel_server(AcpServerConfig::new(None).with_launcher_environment(launcher), requests, responses));
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"session/load", "params":{
            "sessionId":session_id, "cwd":project, "environmentPolicy":{"kind":"isolated"},
        }})).unwrap();
        let mut replay = String::new();
        loop {
            let message = recv_json(&mut rx).await;
            if message["id"] == 1 {
                assert!(message.get("error").is_none(), "cold actual journal load: {message}");
                assert_eq!(message["result"]["sessionId"], session_id);
                break;
            }
            replay.push_str(&message.to_string());
        }
        assert!(replay.contains("admitted stored reply"), "admitted reply was not restored: {replay}");
        assert!(replay.contains("actual tool result"), "actual tool was not reached and persisted: {replay}");
        assert!(!replay.contains("private progress draft"), "unadmitted prose must stay private");
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/load", "params":{
            "sessionId":fork_id, "cwd":project, "environmentPolicy":{"kind":"isolated"},
        }})).unwrap();
        loop { let message = recv_json(&mut rx).await; if message["id"] == 2 {
            assert!(message.get("error").is_none(), "reanchored fork cold load: {message}");
            assert_eq!(message["result"]["sessionId"], fork_id); break;
        }}
        drop(tx);
        server.await.unwrap();
    }).await;
}

async fn assert_store_only_session_restores(parent_manifest: bool, shared_store: bool) {
    let _reset = ResetActiveEventLog;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let project = dir.path().join("project");
            std::fs::create_dir_all(project.join(".harn")).expect("project state dir");
            let project = project.canonicalize().expect("canonical selected workspace");
            if parent_manifest {
                std::fs::write(dir.path().join("harn.toml"), "").expect("parent manifest");
            }
            let session_id = "01a003d0-1513-7271-90aa-4542d6059498";
            let foreign_id = "01a003d0-1513-7271-90aa-4542d6059499";
            let state_dir = if shared_store { dir.path().join("shared-state") } else { project.join(".harn") };
            let launcher = harn_vm::security::LauncherEnvironment::from_snapshot(if shared_store {
                std::collections::BTreeMap::from([("HARN_STATE_DIR".into(), state_dir.display().to_string())])
            } else { Default::default() });

            // Seed the canonical store exactly as a prior process would have
            // left it: a session row plus its durable transcript. Nothing here
            // touches the event log.
            {
                use harn_session_store::{
                    AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
                };
                let store = SqliteSessionStore::open(state_dir.join("session-store.sqlite"))
                    .expect("open canonical store");
                store
                    .create(CreateSession {
                        id: Some(session_id.to_string()),
                        cwd: Some(project.display().to_string()),
                        project_scope: Some(project.display().to_string()),
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
                store
                    .append(
                        session_id,
                        AppendEvent::new(
                            SessionEventKind::Receipt,
                            serde_json::json!({"audit": true}),
                        ),
                    )
                    .await
                    .expect("append nonvisible durable tail");
                let foreign = dir.path().join("sibling");
                std::fs::create_dir(&foreign).unwrap();
                let foreign = foreign.canonicalize().unwrap();
                store.create(CreateSession { id: Some(foreign_id.into()), cwd: Some(foreign.display().to_string()),
                    project_scope: Some(foreign.display().to_string()), ..CreateSession::default() }).await.unwrap();
                store.append(foreign_id, AppendEvent::new(SessionEventKind::Message,
                    serde_json::json!({"transcript_event":{"kind":"message", "role":"assistant", "visibility":"public", "text":"foreign workspace prose"}}))).await.unwrap();
            }

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::new(None).with_launcher_environment(launcher),
                request_rx,
                response_tx,
            ));

            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":0, "method":"session/list", "params":{"cwd":project}})).unwrap();
            let listed = loop { let message = recv_json(&mut response_rx).await; if message["id"] == 0 { break message; } };
            assert!(listed.get("error").is_none(), "selected store list: {listed}");
            assert!(listed.to_string().contains(session_id), "known row must be listed: {listed}");
            assert!(!listed.to_string().contains(foreign_id), "physical alias must not list foreign rows: {listed}");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/load",
                    "params": {
                        "sessionId": session_id,
                        "cwd": project.display().to_string(),
                        "environmentPolicy": {"kind": "isolated"},
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
            assert_eq!(response["result"]["sessionId"], session_id);
            assert!(
                replayed_text.contains("the earlier conversation"),
                "session/load must replay the stored transcript, got {replayed_text}"
            );
            assert_eq!(response["result"]["session"]["lastEventId"], 2);
            assert_eq!(
                response["result"]["session"]["_meta"]["harn"]["lastEventId"],
                2
            );
            assert_eq!(response["result"]["replayed"][0]["eventId"], 1);

            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/load", "params":{
                "sessionId":foreign_id, "cwd":project, "environmentPolicy":{"kind":"isolated"},
            }})).unwrap();
            loop {
                let message = recv_json(&mut response_rx).await;
                assert!(!message.to_string().contains("foreign workspace prose"), "foreign history leaked: {message}");
                if message["id"] == 2 { assert_eq!(message["error"]["code"], -32602); break; }
            }

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
                        "environmentPolicy": {"kind": "isolated"},
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
