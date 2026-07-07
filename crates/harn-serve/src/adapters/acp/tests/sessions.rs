use super::*;
use harn_vm::event_log::EventLog as _;
use harn_vm::orchestration::{save_run_record, RunRecord, RunTraceSpanRecord};

struct ResetActiveEventLog;

impl Drop for ResetActiveEventLog {
    fn drop(&mut self) {
        harn_vm::event_log::reset_active_event_log();
    }
}

async fn run_prompt_with_project_capability(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: i64,
    prompt_text: &str,
    project_read_capability: bool,
) -> String {
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

    let host_capabilities = if project_read_capability {
        serde_json::json!({"project": ["read_file"]})
    } else {
        serde_json::json!({})
    };
    let mut output = String::new();
    let mut saw_completed = false;
    for _ in 0..64 {
        let message = recv_json(response_rx).await;
        match message.get("method").and_then(|value| value.as_str()) {
            Some("host/capabilities") => {
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
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_completed, "prompt {id} should complete successfully");
    output
}

async fn run_json_prompt(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: i64,
    prompt_text: &str,
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
                return serde_json::from_str(output.trim()).expect("prompt JSON output");
            }
            _ => {}
        }
    }
    panic!("prompt {id} did not complete")
}

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
            assert_eq!(snapshot["result"]["schemaVersion"], 1);
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
                    trace_spans: vec![
                        RunTraceSpanRecord {
                            trace_id: "trace-acp".to_string(),
                            span_id: 1,
                            kind: "pipeline".to_string(),
                            name: "root".to_string(),
                            start_ms: 1,
                            duration_ms: 2,
                            metadata: BTreeMap::from([(
                                "session_id".to_string(),
                                serde_json::json!(session_id.clone()),
                            )]),
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
                            metadata: BTreeMap::from([(
                                "session_id".to_string(),
                                serde_json::json!(session_id.clone()),
                            )]),
                            ..RunTraceSpanRecord::default()
                        },
                    ],
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

#[tokio::test(flavor = "current_thread")]
async fn acp_server_handles_session_flow_and_prompt_updates() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
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
                    "method": "initialize",
                }))
                .expect("send initialize");
            let initialize = recv_json(&mut response_rx).await;
            assert_eq!(initialize["id"], 1);
            assert_eq!(initialize["result"]["agentInfo"]["name"], "harn");
            // A file-less attach server (no configured auth methods)
            // still advertises the spec-conformant local "none" method so
            // `initialize` passes the ACP registry auth gate.
            assert_eq!(
                initialize["result"]["authMethods"],
                serde_json::json!([{
                    "id": "none",
                    "type": "agent",
                    "name": "Local (no authentication)",
                    "description": "Connect without credentials. The agent runs locally and accepts the session as an anonymous principal.",
                    "_meta": {
                        "harn": {
                            "scheme": "none",
                            "challenge": { "type": "none" }
                        }
                    }
                }])
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["loadSession"],
                true
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["sessionCapabilities"],
                serde_json::json!({
                    "close": {},
                    "list": {},
                    "resume": {},
                    "rollback": {},
                    "redo": {},
                    "restoreToolCall": {},
                    "cancelToolCall": {},
                })
            );
            assert!(
                initialize["result"]["agentCapabilities"]["sessionCapabilities"]
                    .get("fork")
                    .is_none(),
                "initialize must not advertise Harn-only session/fork as an ACP SessionCapability"
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["mcpCapabilities"],
                serde_json::json!({
                    "http": true,
                    "sse": true,
                })
            );
            assert!(
                initialize["result"]["agentCapabilities"]["promptCapabilities"]["image"]
                    .is_boolean()
            );
            assert!(
                initialize["result"]["agentCapabilities"]["promptCapabilities"]["audio"]
                    .is_boolean()
            );
            assert!(
                initialize["result"]["agentCapabilities"]["promptCapabilities"]["embeddedContext"]
                    .is_boolean()
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["_meta"]["harn"]["schemaCompatibility"],
                ACP_SCHEMA_COMPATIBILITY
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["_meta"]["harn"]["extensionContract"],
                "https://harnlang.com/spec/harn-extensions/v1"
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["_meta"]["harn"]
                    ["sessionUpdateExtensions"],
                serde_json::json!(HARN_SESSION_UPDATE_EXTENSIONS)
            );
            let agent_event_method = &initialize["result"]["agentCapabilities"]["_meta"]["harn"]
                ["extensionMethods"][HARN_AGENT_EVENT_METHOD];
            assert!(
                agent_event_method.is_object(),
                "agent capabilities must advertise the {HARN_AGENT_EVENT_METHOD} \
                     ExtNotification method for clients that support it; got: {agent_event_method}"
            );
            assert_eq!(
                agent_event_method["kinds"],
                serde_json::json!(HARN_AGENT_EVENT_KINDS),
                "advertised kinds must match the canonical HARN_AGENT_EVENT_KINDS list"
            );
            assert_eq!(
                initialize["result"]["agentCapabilities"]["_meta"]["harn"]
                    ["toolLifecycleExtensionFields"],
                serde_json::json!(HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS)
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {"cwd": "."},
                }))
                .expect("send session/new");
            let created = recv_json(&mut response_rx).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session id")
                .to_string();

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/load",
                    "params": {"sessionId": session_id},
                }))
                .expect("send session/load");
            let loaded = recv_json(&mut response_rx).await;
            assert_eq!(loaded["result"]["session"]["sessionId"], session_id);

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "__io_println(\"hello from acp\")"}],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_update = false;
            let mut saw_completed = false;
            for _ in 0..16 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                {
                    assert_eq!(
                        message["params"]["update"]["content"]["_meta"]["harn"]["visible_delta"],
                        "hello from acp"
                    );
                    assert!(message["params"]["update"]["content"]
                        .get("visible_delta")
                        .is_none());
                    saw_update = true;
                }
                if message["id"] == 4 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    saw_completed = true;
                    break;
                }
            }
            assert!(saw_update, "prompt should emit session/update text");
            assert!(saw_completed, "prompt should finish successfully");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_list_filters_by_workspace_anchor_and_cwd() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let primary = dir.path().join("project");
            let sibling = dir.path().join("project-tools");
            std::fs::create_dir_all(&primary).expect("primary dir");
            std::fs::create_dir_all(&sibling).expect("sibling dir");

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
                    "method": "session/new",
                    "params": {"cwd": primary.display().to_string()},
                }))
                .expect("send first session/new");
            let first = recv_json(&mut response_rx).await;
            let first_id = first["result"]["sessionId"]
                .as_str()
                .expect("first session id")
                .to_string();
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {"cwd": sibling.display().to_string()},
                }))
                .expect("send second session/new");
            let second = recv_json(&mut response_rx).await;
            let second_id = second["result"]["sessionId"]
                .as_str()
                .expect("second session id")
                .to_string();

            harn_vm::agent_sessions::set_workspace_anchor(
                &first_id,
                Some(harn_vm::workspace_anchor::WorkspaceAnchor {
                    primary: primary.clone(),
                    additional_roots: Vec::new(),
                    anchored_at: "2026-05-25T00:00:00Z".to_string(),
                }),
            )
            .expect("set first anchor");
            harn_vm::agent_sessions::set_workspace_anchor(
                &second_id,
                Some(harn_vm::workspace_anchor::WorkspaceAnchor {
                    primary: sibling.clone(),
                    additional_roots: Vec::new(),
                    anchored_at: "2026-05-25T00:00:00Z".to_string(),
                }),
            )
            .expect("set second anchor");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/list",
                    "params": {
                        "workspaceAnchor": {"primary": primary.display().to_string()},
                    },
                }))
                .expect("send anchored session/list");
            let anchored = recv_json(&mut response_rx).await;
            assert_eq!(
                anchored["result"]["sessions"]
                    .as_array()
                    .expect("anchored sessions")
                    .len(),
                1
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["sessionId"],
                serde_json::json!(first_id)
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["workspaceAnchor"]["primary"],
                serde_json::json!(primary.display().to_string())
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["_meta"]["harn"]["liveState"],
                serde_json::json!("live")
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "session/list",
                    "params": {"cwd": sibling.display().to_string()},
                }))
                .expect("send cwd session/list");
            let by_cwd = recv_json(&mut response_rx).await;
            assert_eq!(
                by_cwd["result"]["sessions"][0]["sessionId"],
                serde_json::json!(second_id)
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "session/load",
                    "params": {"sessionId": first_id},
                }))
                .expect("send session/load");
            let loaded = recv_json(&mut response_rx).await;
            assert_eq!(
                loaded["result"]["session"]["_meta"]["harn"]["workspaceAnchor"]["primary"],
                serde_json::json!(primary.display().to_string())
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_harn_workspace_anchor_methods_mutate_live_session() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            harn_vm::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let primary = dir.path().join("project");
            let sibling = dir.path().join("project-tools");
            let target = dir.path().join("target");
            std::fs::create_dir_all(&primary).expect("primary dir");
            std::fs::create_dir_all(&sibling).expect("sibling dir");
            std::fs::create_dir_all(&target).expect("target dir");
            let canonical_sibling = sibling.canonicalize().expect("canonical sibling");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::new(None),
                    serde_json::json!(primary.display().to_string()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "harn.session_workspace_roots",
                    "params": {"sessionId": session_id},
                }))
                .expect("send roots request");
            let roots = recv_response_with_id(&mut response_rx, 2).await;
            assert_eq!(
                roots["result"]["workspaceAnchor"]["primary"],
                serde_json::json!(primary.display().to_string())
            );
            assert_eq!(
                harn_vm::agent_sessions::workspace_anchor(&session_id)
                    .expect("live anchor")
                    .primary,
                primary
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "harn.session_add_root",
                    "params": {
                        "sessionId": session_id,
                        "path": sibling.display().to_string(),
                        "mountMode": "extend",
                    },
                }))
                .expect("send add-root request");
            let added = recv_response_with_id(&mut response_rx, 3).await;
            let additional = added["result"]["workspaceAnchor"]["additional_roots"]
                .as_array()
                .expect("additional roots");
            assert_eq!(additional.len(), 1);
            assert_eq!(
                additional[0]["path"],
                serde_json::json!(canonical_sibling.display().to_string())
            );
            assert_eq!(additional[0]["mount_mode"], serde_json::json!("extend"));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "harn.session_reanchor",
                    "params": {
                        "sessionId": session_id,
                        "path": target.display().to_string(),
                        "reason": "test reanchor",
                    },
                }))
                .expect("send reanchor request");
            let reanchored = recv_response_with_id(&mut response_rx, 4).await;
            assert_eq!(reanchored["result"]["changed"], serde_json::json!(true));
            assert_eq!(
                reanchored["result"]["previousWorkspaceAnchor"]["primary"],
                serde_json::json!(primary.display().to_string())
            );
            assert_eq!(
                reanchored["result"]["workspaceAnchor"]["primary"],
                serde_json::json!(target.display().to_string())
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "session/list",
                    "params": {
                        "workspaceAnchor": {"primary": target.display().to_string()},
                    },
                }))
                .expect("send filtered session/list");
            let listed = recv_response_with_id(&mut response_rx, 5).await;
            assert_eq!(
                listed["result"]["sessions"][0]["sessionId"],
                serde_json::json!(session_id)
            );
            assert_eq!(
                listed["result"]["sessions"][0]["workspaceAnchor"]["primary"],
                serde_json::json!(target.display().to_string())
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_harn_session_reanchor_seeds_missing_anchor_from_live_cwd() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            harn_vm::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let primary = dir.path().join("project");
            let target = dir.path().join("target");
            std::fs::create_dir_all(&primary).expect("primary dir");
            std::fs::create_dir_all(&target).expect("target dir");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::new(None),
                    serde_json::json!(primary.display().to_string()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "harn.session_reanchor",
                    "params": {
                        "sessionId": session_id,
                        "path": target.display().to_string(),
                    },
                }))
                .expect("send reanchor request");
            let reanchored = recv_response_with_id(&mut response_rx, 2).await;
            assert_eq!(
                reanchored["result"]["previousWorkspaceAnchor"]["primary"],
                serde_json::json!(primary.display().to_string())
            );
            assert_eq!(
                reanchored["result"]["workspaceAnchor"]["primary"],
                serde_json::json!(target.display().to_string())
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_truncate_mutates_current_session_and_notifies_client() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            harn_vm::reset_thread_local_state();
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            let first = run_json_prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                2,
                r#"
const sid = agent_session_current_id()
guard sid != nil else { throw "missing session id" }
agent_session_inject(sid, {role: "user", content: "alpha"})
const snap = agent_session_snapshot(sid)
__io_println(json_stringify({len: len(snap["messages"]), messages: snap["messages"]}))
"#,
            )
            .await;
            assert_eq!(first["len"], 1);
            let second = run_json_prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                r#"
const sid = agent_session_current_id()
guard sid != nil else { throw "missing session id" }
agent_session_inject(sid, {role: "user", content: "beta"})
const snap = agent_session_snapshot(sid)
__io_println(json_stringify({len: len(snap["messages"]), messages: snap["messages"]}))
"#,
            )
            .await;
            assert_eq!(second["len"], 2);

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "session/truncate",
                    "params": {
                        "sessionId": session_id.clone(),
                        "keepFirst": 1,
                        "reason": "user_edit",
                    },
                }))
                .expect("send session/truncate");

            let mut response = None;
            let mut notification = None;
            for _ in 0..4 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 4 {
                    response = Some(message);
                } else if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "session_truncated"
                {
                    notification = Some(message);
                }
                if response.is_some() && notification.is_some() {
                    break;
                }
            }
            let response = response.expect("truncate response");
            assert_eq!(response["result"]["sessionId"], session_id);
            assert_eq!(response["result"]["keptTurnCount"], 1);
            assert_eq!(response["result"]["removedTurnCount"], 1);
            assert!(response["result"]["newTipTurnId"].is_string());

            let notification = notification.expect("session_truncated notification");
            assert_eq!(notification["params"]["sessionId"], session_id);
            assert_eq!(notification["params"]["update"]["keptTurnCount"], 1);
            assert_eq!(notification["params"]["update"]["removedTurnCount"], 1);
            assert_eq!(notification["params"]["update"]["reason"], "user_edit");

            let snapshot = run_json_prompt(
                &request_tx,
                &mut response_rx,
                &session_id,
                5,
                r#"
const sid = agent_session_current_id()
guard sid != nil else { throw "missing session id" }
const snap = agent_session_snapshot(sid)
__io_println(json_stringify({len: len(snap["messages"]), messages: snap["messages"]}))
"#,
            )
            .await;
            assert_eq!(snapshot["len"], 1);
            assert_eq!(snapshot["messages"][0]["content"], "alpha");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_mcp_catalog_projects_allowlist_over_advertised_items() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            harn_vm::reset_thread_local_state();
            let (request_tx, mut response_rx, server, _session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "mcp/catalog",
                    "params": {
                        "allowlist": {
                            "schemaVersion": 1,
                            "defaultEnabled": true,
                            "items": [
                                {"server": "github", "kind": "tool", "name": "create_issue", "enabled": false}
                            ]
                        },
                        "advertised": {
                            "github": [
                                {"kind": "tool", "name": "create_issue"},
                                {"kind": "tool", "name": "list_issues"}
                            ]
                        }
                    },
                }))
                .expect("send mcp/catalog");

            let mut response = None;
            for _ in 0..6 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 7 {
                    response = Some(message);
                    break;
                }
            }
            let response = response.expect("mcp/catalog response");
            let result = &response["result"];
            assert_eq!(result["schemaVersion"], 1);
            assert_eq!(result["defaultEnabled"], true);
            let github = &result["servers"][0];
            assert_eq!(github["name"], "github");
            // Items sorted by (kind, name): create_issue first, disabled by allowlist.
            assert_eq!(github["items"][0]["name"], "create_issue");
            assert_eq!(github["items"][0]["enabled"], false);
            assert_eq!(github["items"][1]["name"], "list_issues");
            assert_eq!(github["items"][1]["enabled"], true);

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_mcp_authorize_requires_url() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, _session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "mcp/authorize",
                    "params": {},
                }))
                .expect("send mcp/authorize");

            let mut response = None;
            for _ in 0..6 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 11 {
                    response = Some(message);
                    break;
                }
            }
            let response = response.expect("mcp/authorize response");
            assert_eq!(response["error"]["code"], -32602);
            assert!(response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("url"));

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_mcp_oauth_callback_validates_and_rejects_unknown_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, _session_id) =
                start_acp_channel_session().await;

            // Missing state/code → invalid params.
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "mcp/oauth_callback",
                    "params": {"code": "abc"},
                }))
                .expect("send mcp/oauth_callback");
            // Well-formed but no pending flow matches the state → -32000.
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 13,
                    "method": "mcp/oauth_callback",
                    "params": {"state": "no-such", "code": "abc"},
                }))
                .expect("send mcp/oauth_callback");

            let mut invalid = None;
            let mut unknown = None;
            for _ in 0..8 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 12 {
                    invalid = Some(message);
                } else if message["id"] == 13 {
                    unknown = Some(message);
                }
                if invalid.is_some() && unknown.is_some() {
                    break;
                }
            }
            assert_eq!(invalid.expect("invalid response")["error"]["code"], -32602);
            let unknown = unknown.expect("unknown-state response");
            assert_eq!(unknown["error"]["code"], -32000);
            assert!(unknown["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no pending MCP authorization"));

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_truncate_validates_inputs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            for (id, params, expected) in [
                (
                    2,
                    serde_json::json!({"sessionId": session_id.clone()}),
                    "Missing keepFirst",
                ),
                (
                    3,
                    serde_json::json!({"sessionId": session_id.clone(), "keepFirst": -1}),
                    "Invalid keepFirst: must be >= 0",
                ),
                (
                    4,
                    serde_json::json!({"sessionId": "missing-session", "keepFirst": 0}),
                    "Unknown session: missing-session",
                ),
            ] {
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "session/truncate",
                        "params": params,
                    }))
                    .expect("send session/truncate");
                let response = recv_json(&mut response_rx).await;
                assert_eq!(response["id"], id);
                assert_eq!(response["error"]["code"], -32602);
                assert_eq!(response["error"]["message"], expected);
            }

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_profile_json_appends_one_line_per_prompt_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let profile_path = dir.path().join("profile.ndjson");
            let config = AcpServerConfig::new(None).with_profile(AcpProfileConfig {
                text: false,
                json_path: Some(profile_path.clone()),
            });
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(config, serde_json::json!(dir.path())).await;

            for id in 2..=3 {
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "session/prompt",
                        "params": {
                            "sessionId": session_id.clone(),
                            "prompt": [{"type": "text", "text": "__io_println(\"profiled\")"}],
                        },
                    }))
                    .expect("send session/prompt");

                let mut saw_completed = false;
                for _ in 0..16 {
                    let message = recv_json(&mut response_rx).await;
                    if message["method"] == "host/capabilities" {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send host capabilities response");
                    }
                    if message["id"] == id {
                        assert_eq!(message["result"]["stopReason"], "end_turn");
                        saw_completed = true;
                        break;
                    }
                }
                assert!(saw_completed, "prompt should finish successfully");
            }

            let lines = std::fs::read_to_string(&profile_path).expect("read profile ndjson");
            let entries = lines
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 2, "profile output:\n{lines}");
            assert_eq!(entries[0]["session_id"], session_id);
            assert_eq!(entries[0]["turn"], 1);
            assert_eq!(entries[1]["turn"], 2);
            assert!(entries[0]["rollup"]["by_kind"].is_array());

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_close_and_stop_alias_free_active_session() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::new(None),
                request_rx,
                response_tx,
            ));

            for (index, method) in ["session/close", "session/stop"].into_iter().enumerate() {
                let request_base = 10 + (index as i64 * 10);
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_base,
                        "method": "session/new",
                        "params": {"cwd": "."},
                    }))
                    .expect("send session/new");
                let created = recv_json(&mut response_rx).await;
                let session_id = created["result"]["sessionId"]
                    .as_str()
                    .expect("session id")
                    .to_string();
                assert!(harn_vm::agent_sessions::exists(&session_id));

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_base + 1,
                        "method": method,
                        "params": {"sessionId": session_id},
                    }))
                    .expect("send session close request");
                let closed = recv_json(&mut response_rx).await;
                assert_eq!(closed["id"], request_base + 1);
                assert_eq!(closed["result"], serde_json::json!({}));
                assert!(
                    !harn_vm::agent_sessions::exists(&session_id),
                    "{method} should free VM session state"
                );

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_base + 2,
                        "method": "session/list",
                        "params": {},
                    }))
                    .expect("send session/list");
                let listed = recv_json(&mut response_rx).await;
                let sessions = listed["result"]["sessions"].as_array().unwrap();
                assert!(
                    sessions
                        .iter()
                        .all(|entry| entry["sessionId"].as_str() != Some(session_id.as_str())),
                    "{method} should remove the active ACP session"
                );

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_base + 3,
                        "method": "session/prompt",
                        "params": {
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": "__io_println(\"closed\")"}],
                        },
                    }))
                    .expect("send session/prompt");
                let rejected = recv_json(&mut response_rx).await;
                assert_eq!(rejected["id"], request_base + 3);
                assert_eq!(rejected["error"]["code"], -32602);
            }

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_close_cancels_pending_host_bridge_call() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "__io_println(\"after host capabilities\")"}],
                    },
                }))
                .expect("send session/prompt");

            let host_capabilities = recv_json(&mut response_rx).await;
            assert_eq!(host_capabilities["method"], "host/capabilities");
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/close",
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send session/close");

            let mut saw_cancelled_response = false;
            let mut saw_close_response = false;
            for _ in 0..8 {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 2 {
                    assert_eq!(message["result"]["stopReason"], "cancelled");
                    saw_cancelled_response = true;
                } else if message["id"] == 3 {
                    assert_eq!(message["result"], serde_json::json!({}));
                    assert!(!harn_vm::agent_sessions::exists(&session_id));
                    saw_close_response = true;
                    break;
                }
            }

            assert!(
                saw_cancelled_response,
                "prompt should observe close as cancellation"
            );
            assert!(saw_close_response, "session/close should free the session");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_file_backed_pipeline_installs_harness_global() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("harness.harn");
            std::fs::write(
                &pipeline_path,
                r#"
import { env_int } from "std/config"

pipeline default(task) {
  __io_println(env_int("HARN_ACP_HARNESS_REGRESSION_UNSET", 7))
  harness.stdio.println("via-harness")
}"#,
            )
            .expect("write pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            let output = run_prompt_with_project_capability(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                "hello",
                false,
            )
            .await;
            assert_eq!(output, "7\nvia-harness\n");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_file_backed_vm_baseline_keeps_prompt_turns_isolated() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("isolation.harn");
            let profile_path = dir.path().join("profile.ndjson");
            std::fs::write(
                &pipeline_path,
                r#"
pipeline default(task) {
  const cell = shared_cell({scope: "task_group", key: "turn", initial: prompt})
  __io_println(prompt)
  __io_println(shared_get(cell))
  shared_set(cell, "dirty")
  const held = sync_gate_acquire("runner", 1)
  const blocked = sync_gate_acquire("runner", 1, 0)
  __io_println(blocked == nil)
  sync_release(held)
  const metrics = sync_metrics("gate", "runner")
  __io_println(metrics.acquisition_count)
  __io_println(host_has("project", "read_file"))
}"#,
            )
            .expect("write pipeline");
            let config =
                AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string())
                    .with_profile(AcpProfileConfig {
                        text: false,
                        json_path: Some(profile_path.clone()),
                    });
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(config, serde_json::json!(dir.path())).await;

            let first = run_prompt_with_project_capability(
                &request_tx,
                &mut response_rx,
                &session_id,
                3,
                "one",
                true,
            )
            .await;
            assert_eq!(first, "one\none\ntrue\n1\ntrue\n");

            let second = run_prompt_with_project_capability(
                &request_tx,
                &mut response_rx,
                &session_id,
                4,
                "two",
                false,
            )
            .await;
            assert_eq!(
                second, "two\ntwo\ntrue\n1\nfalse\n",
                "prompt globals, shared runtime state, sync metrics, and host capability cache must reset per turn"
            );

            let lines = std::fs::read_to_string(&profile_path).expect("read profile ndjson");
            let entries = lines
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 2, "profile output:\n{lines}");
            for entry in &entries {
                let buckets = entry["rollup"]["by_kind"]
                    .as_array()
                    .expect("profile kind buckets");
                assert!(
                    buckets
                        .iter()
                        .any(|bucket| bucket["kind"] == "vm_setup" && bucket["count"] == 1),
                    "ACP profile must expose vm_setup bucket: {entry}"
                );
            }

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_exposes_multimodal_prompt_messages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("multimodal.harn");
            std::fs::write(
                &pipeline_path,
                r#"pipeline default(task) {
  llm_mock_clear()
  llm_mock({text: "ok"})
  llm_call("", nil, {provider: "mock", messages: prompt_messages})
  const blocks = llm_mock_calls()[0].messages[0].content
  __io_println(blocks[0].text == "Please inspect this context.")
  __io_println(blocks[1].type == "image")
  __io_println(blocks[1].base64 == "iVBORw0KGgo=")
  __io_println(blocks[1].media_type == "image/png")
  __io_println(blocks[2].type == "audio")
  __io_println(blocks[2].base64 == "UklGRiQ=")
  __io_println(blocks[2].media_type == "audio/wav")
  __io_println(contains(blocks[3].text, "file:///tmp/example.txt"))
  __io_println(contains(blocks[3].text, "hello from embedded context"))
}"#,
            )
            .expect("write pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [
                            {"type": "text", "text": "Please inspect this context."},
                            {
                                "type": "image",
                                "mimeType": "image/png",
                                "data": "iVBORw0KGgo=",
                                "uri": "file:///tmp/pixel.png"
                            },
                            {
                                "type": "audio",
                                "mimeType": "audio/wav",
                                "data": "UklGRiQ="
                            },
                            {
                                "type": "resource",
                                "resource": {
                                    "uri": "file:///tmp/example.txt",
                                    "mimeType": "text/plain",
                                    "text": "hello from embedded context"
                                }
                            }
                        ],
                    },
                }))
                .expect("send session/prompt");

            let mut output = String::new();
            let mut saw_completed = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                {
                    if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                        output.push_str(text);
                    }
                }
                if message["id"] == 3 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    saw_completed = true;
                    break;
                }
            }
            assert!(saw_completed, "prompt should complete successfully");
            assert!(
                !output.contains("false"),
                "multimodal prompt assertions failed; output was:\n{output}"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_surfaces_multimodal_capability_errors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("unsupported_vision.harn");
            std::fs::write(
                &pipeline_path,
                r#"pipeline default(task) {
  llm_call("", nil, {provider: "mock", model: "gpt-3.5-turbo", messages: prompt_messages})
}"#,
            )
            .expect("write pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [
                            {"type": "text", "text": "caption"},
                            {
                                "type": "image",
                                "mimeType": "image/png",
                                "data": "iVBORw0KGgo="
                            }
                        ],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_error = false;
            for _ in 0..24 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["id"] == 3 {
                    let error = message["error"]["message"]
                        .as_str()
                        .expect("prompt error message");
                    assert!(
                        error.contains("option `vision` is not supported"),
                        "unexpected error: {error}"
                    );
                    saw_error = true;
                    break;
                }
            }
            assert!(saw_error, "prompt should return a capability error");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_bridge_routes_session_request_permission_response() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let server =
        AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx.clone()));
    let bridge = Arc::new(AcpBridge {
        session_id: "session-1".to_string(),
        output: AcpOutput::Channel(tx),
        pending: server.pending.clone(),
        next_id_counter: AtomicU64::new(77),
        cancellation: SessionCancellation::default(),
        script_name: Mutex::new(String::new()),
        assistant_state: Mutex::new(VisibleTextState::default()),
    });

    let call = bridge.call_client(
        "session/request_permission",
        serde_json::json!({
            "sessionId": "session-1",
            "toolCall": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-1",
                "title": "edit",
                "kind": "other"
            },
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
            ]
        }),
    );
    tokio::pin!(call);

    let outgoing = tokio::select! {
        message = recv_json(&mut rx) => message,
        result = &mut call => panic!("permission call completed before host response: {result:?}"),
    };
    assert_eq!(outgoing["id"], 77);
    assert_eq!(outgoing["method"], "session/request_permission");

    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 77,
        "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
    });
    crate::protocol_fixture_tests::assert_fixture_documents_match(
        "conformance/protocols/fixtures/acp/session_request_permission.valid.json",
        vec![outgoing, response.clone()],
    );

    let mut server = server;
    server.handle_incoming_message(response).await;
    let result = call.await.expect("permission response");
    assert_eq!(result["outcome"]["outcome"], "selected");
    assert_eq!(result["outcome"]["optionId"], "allow");
}

#[test]
fn prepared_session_prompt_preserves_queued_cancel() {
    let cancellation = SessionCancellation::default();
    cancellation.cancel();
    cancellation.begin_prompt();
    assert!(
        !cancellation.cancelled.load(Ordering::SeqCst),
        "stale cancellation should not leak into a later prompt"
    );

    cancellation.prepare_prompt();
    cancellation.cancel();
    cancellation.begin_prompt();
    assert!(
        cancellation.cancelled.load(Ordering::SeqCst),
        "cancellation observed after a prompt was routed must not be reset at prompt start"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_cancel_kills_active_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        // `run_command` is rebound to the same ACP terminal path as
                        // `exec`, without exercising unrelated mode-policy checks.
                        "prompt": [{"type": "text", "text": "run_command(\"sleep 999\")"}],
                    },
                }))
                .expect("send session/prompt");

            let terminal_id = "term-cancel-demo";
            let mut saw_wait = false;
            let mut saw_kill = false;
            let mut saw_release = false;
            let mut saw_cancelled_response = false;
            for _ in 0..24 {
                let message = recv_json(&mut response_rx).await;
                match message.get("method").and_then(|value| value.as_str()) {
                    Some("host/capabilities") => {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send host capabilities response");
                    }
                    Some("terminal/create") => {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {"terminalId": terminal_id},
                            }))
                            .expect("send terminal/create response");
                    }
                    Some("terminal/wait_for_exit") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_wait = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/cancel",
                                "params": {"sessionId": session_id},
                            }))
                            .expect("send session/cancel");
                    }
                    Some("terminal/kill") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_kill = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/kill response");
                    }
                    Some("terminal/release") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_release = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/release response");
                    }
                    _ if message["id"] == 2 => {
                        assert_eq!(message["result"]["stopReason"], "cancelled");
                        saw_cancelled_response = true;
                        break;
                    }
                    _ => {}
                }
            }

            assert!(saw_wait, "prompt should block on terminal/wait_for_exit");
            assert!(saw_kill, "session/cancel should issue terminal/kill");
            assert!(
                saw_release,
                "cancelled terminal execution should still release the terminal"
            );
            assert!(
                saw_cancelled_response,
                "prompt should finish with stopReason=cancelled"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_close_cancels_active_terminal_before_freeing_session() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "run_command(\"sleep 999\")"}],
                    },
                }))
                .expect("send session/prompt");

            let terminal_id = "term-close-demo";
            let mut saw_wait = false;
            let mut saw_kill = false;
            let mut saw_release = false;
            let mut saw_cancelled_response = false;
            let mut saw_close_response = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                match message.get("method").and_then(|value| value.as_str()) {
                    Some("host/capabilities") => {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send host capabilities response");
                    }
                    Some("terminal/create") => {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {"terminalId": terminal_id},
                            }))
                            .expect("send terminal/create response");
                    }
                    Some("terminal/wait_for_exit") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_wait = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 3,
                                "method": "session/close",
                                "params": {"sessionId": session_id.clone()},
                            }))
                            .expect("send session/close");
                    }
                    Some("terminal/kill") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_kill = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/kill response");
                    }
                    Some("terminal/release") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_release = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/release response");
                    }
                    _ if message["id"] == 2 => {
                        assert_eq!(message["result"]["stopReason"], "cancelled");
                        saw_cancelled_response = true;
                    }
                    _ if message["id"] == 3 => {
                        assert_eq!(message["result"], serde_json::json!({}));
                        assert!(!harn_vm::agent_sessions::exists(&session_id));
                        saw_close_response = true;
                        break;
                    }
                    _ => {}
                }
            }

            assert!(saw_wait, "prompt should block on terminal/wait_for_exit");
            assert!(saw_kill, "session/close should issue terminal/kill");
            assert!(
                saw_release,
                "closed terminal execution should still release the terminal"
            );
            assert!(
                saw_cancelled_response,
                "prompt should finish with stopReason=cancelled"
            );
            assert!(
                saw_close_response,
                "session/close should respond after cleanup"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_cancel_kills_terminal_created_during_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session().await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "run_command(\"sleep 999\")"}],
                    },
                }))
                .expect("send session/prompt");

            let terminal_id = "term-created-during-cancel";
            let mut saw_create = false;
            let mut saw_wait = false;
            let mut saw_kill = false;
            let mut saw_release = false;
            let mut saw_cancelled_response = false;
            for _ in 0..24 {
                let message = recv_json(&mut response_rx).await;
                match message.get("method").and_then(|value| value.as_str()) {
                    Some("host/capabilities") => {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send host capabilities response");
                    }
                    Some("terminal/create") => {
                        saw_create = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/cancel",
                                "params": {"sessionId": session_id},
                            }))
                            .expect("send session/cancel");
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {"terminalId": terminal_id},
                            }))
                            .expect("send terminal/create response");
                    }
                    Some("terminal/wait_for_exit") => {
                        saw_wait = true;
                    }
                    Some("terminal/kill") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_kill = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/kill response");
                    }
                    Some("terminal/release") => {
                        assert_eq!(message["params"]["terminalId"], terminal_id);
                        saw_release = true;
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send terminal/release response");
                    }
                    _ if message["id"] == 2 => {
                        assert_eq!(message["result"]["stopReason"], "cancelled");
                        saw_cancelled_response = true;
                        break;
                    }
                    _ => {}
                }
            }

            assert!(saw_create, "prompt should request terminal/create");
            assert!(
                !saw_wait,
                "cancellation after terminal/create should not wait for process exit"
            );
            assert!(
                saw_kill,
                "created terminal should be killed when create races cancellation"
            );
            assert!(
                saw_release,
                "created terminal should be released when create races cancellation"
            );
            assert!(
                saw_cancelled_response,
                "prompt should finish with stopReason=cancelled"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}
