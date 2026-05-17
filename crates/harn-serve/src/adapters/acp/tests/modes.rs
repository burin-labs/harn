use super::*;
#[tokio::test(flavor = "current_thread")]
async fn acp_authenticate_uses_shared_auth_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let config = AcpServerConfig::new(None).with_auth_policy(AuthPolicy {
                methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                    keys: BTreeSet::from(["secret".to_string()]),
                })],
            });
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                config,
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 0,
                    "method": "initialize",
                }))
                .expect("send initialize");
            let initialize = recv_json(&mut response_rx).await;
            assert_eq!(
                initialize["result"]["authMethods"][0]["_meta"]["harn"]["scheme"],
                "api_key"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/new",
                    "params": {"cwd": "."},
                }))
                .expect("send unauthenticated session/new");
            let blocked = recv_json(&mut response_rx).await;
            assert_eq!(blocked["id"], 1);
            assert_eq!(blocked["error"]["code"], ACP_AUTH_REQUIRED_CODE);
            assert_eq!(blocked["error"]["message"], "auth_required");
            assert_eq!(blocked["error"]["data"]["authMethods"][0]["id"], "apiKey");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "authenticate",
                    "params": {
                        "methodId": "apiKey",
                        "_meta": {"harn": {"apiKey": "secret"}}
                    },
                }))
                .expect("send authenticate");
            let authenticated = recv_json(&mut response_rx).await;
            assert_eq!(authenticated["id"], 2);
            assert_eq!(
                authenticated["result"]["_meta"]["harn"]["principal"]["scheme"],
                "api_key"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/new",
                    "params": {"cwd": "."},
                }))
                .expect("send authenticated session/new");
            let created = recv_json(&mut response_rx).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session id")
                .to_string();

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "println(\"allowed\")"}],
                    },
                }))
                .expect("send authenticated prompt");
            let mut saw_allowed = false;
            let mut saw_response = false;
            for _ in 0..16 {
                let message = recv_json(&mut response_rx).await;
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
                    Some("session/update") => {
                        saw_allowed |= message["params"]["update"]["content"]["text"]
                            .as_str()
                            .is_some_and(|text| text == "allowed\n");
                    }
                    _ if message["id"] == 4 => {
                        saw_response = true;
                        assert_eq!(message["result"]["stopReason"], "end_turn");
                        break;
                    }
                    _ => {}
                }
            }
            assert!(saw_allowed, "authenticated prompt should execute");
            assert!(saw_response, "authenticated prompt should complete");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// `session/new` returns the legacy `SessionModeState` and the
/// preferred `configOptions` selector from the same catalog.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_new_advertises_session_mode_state_and_config_options() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;

    let modes = &created["result"]["modes"];
    assert_eq!(modes["currentModeId"], "ask");
    let available = modes["availableModes"]
        .as_array()
        .expect("availableModes array");
    let ids: Vec<&str> = available
        .iter()
        .map(|mode| mode["id"].as_str().expect("mode id"))
        .collect();
    assert_eq!(ids, vec!["ask", "architect", "code", "shadow"]);
    // Each entry must carry a human-readable name; description is
    // optional per spec but Harn always populates it.
    for mode in available {
        assert!(
            mode["name"].as_str().is_some_and(|name| !name.is_empty()),
            "mode {mode} missing name"
        );
    }

    let config_options = created["result"]["configOptions"]
        .as_array()
        .expect("configOptions array");
    let mode_option = config_options
        .iter()
        .find(|entry| entry["id"] == "mode")
        .expect("mode config option");
    assert_eq!(mode_option["category"], "mode");
    assert_eq!(mode_option["type"], "select");
    assert_eq!(mode_option["currentValue"], "ask");
    let option_ids: Vec<&str> = mode_option["options"]
        .as_array()
        .expect("mode options")
        .iter()
        .map(|mode| mode["value"].as_str().expect("mode value"))
        .collect();
    assert_eq!(option_ids, vec!["ask", "architect", "code", "shadow"]);

    let model_option = config_options
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model config option");
    assert_eq!(model_option["category"], "model");
    assert_eq!(model_option["type"], "select");
    // No model is pinned for a freshly created session — the dropdown
    // should advertise the "inherit ambient default" sentinel as the
    // current value so clients render an "unpinned" state.
    assert_eq!(model_option["currentValue"], "@inherit");
}

/// `session/load` echoes the active mode state back to a
/// reconnecting client so the UI can re-render the selected mode
/// without an extra round-trip.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_includes_current_mode_state() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "architect"},
        }))
        .await;
    // Drain success ack plus mode/config notifications.
    let _ack = recv_json(&mut rx).await;
    let _mode_notification = recv_json(&mut rx).await;
    let _config_notification = recv_json(&mut rx).await;

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/load",
            "params": {"sessionId": session_id},
        }))
        .await;
    let loaded = recv_json(&mut rx).await;
    assert_eq!(loaded["result"]["modes"]["currentModeId"], "architect");
    assert_eq!(
        loaded["result"]["configOptions"][0]["currentValue"],
        "architect"
    );
    assert!(loaded["result"]["modes"]["availableModes"]
        .as_array()
        .expect("available modes")
        .iter()
        .any(|m| m["id"] == "architect"));
}

/// `session/resume` restores the same session mode/config state as
/// `session/load`, but it must not replay persisted `session/update`
/// notifications before responding.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_resume_includes_current_mode_state_without_replay() {
    harn_vm::event_log::reset_active_event_log();
    let _log = harn_vm::event_log::install_memory_for_current_thread(64);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "architect"},
        }))
        .await;
    let _ack = recv_json(&mut rx).await;
    let _mode_notification = recv_json(&mut rx).await;
    let _config_notification = recv_json(&mut rx).await;

    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::AgentMessageChunk {
        session_id: session_id.clone(),
        content: "do not replay me".to_string(),
    });
    tokio::task::yield_now().await;

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/resume",
            "params": {"sessionId": session_id},
        }))
        .await;
    let resumed = recv_json(&mut rx).await;
    assert_eq!(resumed["id"], 3);
    assert!(resumed.get("method").is_none(), "resume must respond first");
    assert_eq!(resumed["result"]["modes"]["currentModeId"], "architect");
    assert_eq!(
        resumed["result"]["configOptions"][0]["currentValue"],
        "architect"
    );
    assert!(
        resumed["result"].get("replayed").is_none(),
        "session/resume must not include replay metadata"
    );
    assert!(
        rx.try_recv().is_err(),
        "session/resume must not emit replay notifications"
    );

    harn_vm::agent_events::clear_session_sinks(created["result"]["sessionId"].as_str().unwrap());
    harn_vm::event_log::reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_restore_methods_reject_unknown_sessions() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    for (id, method) in [(1, "session/load"), (2, "session/resume")] {
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": {"sessionId": "missing-session"},
            }))
            .await;
        let response = recv_json(&mut rx).await;
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown session")),
            "unexpected error for {method}: {response}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_load_replays_persisted_agent_events() {
    harn_vm::event_log::reset_active_event_log();
    let _log = harn_vm::event_log::install_memory_for_current_thread(64);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::AgentMessageChunk {
        session_id: session_id.clone(),
        content: "replay me".to_string(),
    });
    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::Plan {
        session_id: session_id.clone(),
        plan: serde_json::json!([{"content": "do the thing", "status": "pending"}]),
    });
    tokio::task::yield_now().await;

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/load",
            "params": {"sessionId": session_id},
        }))
        .await;

    let message_replay = recv_json(&mut rx).await;
    assert_eq!(message_replay["method"], "session/update");
    assert_eq!(
        message_replay["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    assert_eq!(
        message_replay["params"]["update"]["content"]["text"],
        "replay me"
    );
    assert_eq!(message_replay["_harn"]["replayed"], true);
    assert_eq!(
        message_replay["params"]["update"]["_meta"]["harn"]["replayed"],
        true
    );

    let plan_replay = recv_json(&mut rx).await;
    assert_eq!(plan_replay["method"], "session/update");
    assert_eq!(plan_replay["params"]["update"]["sessionUpdate"], "plan");
    assert_eq!(plan_replay["_harn"]["replayed"], true);
    assert_eq!(
        plan_replay["params"]["update"]["_meta"]["harn"]["replayed"],
        true
    );

    let loaded = recv_json(&mut rx).await;
    assert_eq!(loaded["id"], 2);
    assert_eq!(loaded["result"]["replayed"].as_array().unwrap().len(), 2);
    assert_eq!(
        loaded["result"]["replayed"][0]["type"],
        "agent_message_chunk"
    );
    assert_eq!(loaded["result"]["replayed"][1]["type"], "plan");

    harn_vm::agent_events::clear_session_sinks(created["result"]["sessionId"].as_str().unwrap());
    harn_vm::event_log::reset_active_event_log();
}

/// Setting a valid mode ack's with an empty result and emits a
/// `current_mode_update` notification carrying the new mode id.
/// Locks the canonical session-modes wire shape so clients depend
/// on it directly.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_mode_emits_current_mode_update_notification() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "architect"},
        }))
        .await;
    let ack = recv_json(&mut rx).await;
    assert_eq!(ack["id"], 2);
    assert!(ack["result"].is_object());
    assert!(ack["error"].is_null());

    let notification = recv_json(&mut rx).await;
    assert_eq!(notification["method"], "session/update");
    assert_eq!(notification["params"]["sessionId"], session_id);
    assert_eq!(
        notification["params"]["update"]["sessionUpdate"],
        "current_mode_update"
    );
    assert_eq!(notification["params"]["update"]["modeId"], "architect");

    let config_notification = recv_json(&mut rx).await;
    assert_eq!(config_notification["method"], "session/update");
    assert_eq!(
        config_notification["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    assert_eq!(
        config_notification["params"]["update"]["configOptions"][0]["currentValue"],
        "architect"
    );
}

/// Re-setting the same mode is a no-op: the agent ack's the request but
/// does not emit redundant mode/config notifications.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_mode_is_idempotent_when_mode_unchanged() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    // Active mode after session/new is "ask"; re-set to it.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "ask"},
        }))
        .await;
    let ack = recv_json(&mut rx).await;
    assert_eq!(ack["id"], 2);
    assert!(ack["result"].is_object());

    // Follow up with a real transition so the channel produces a
    // notification we can recognize. If a stray idempotent
    // notification had been sent, it would arrive *before* this
    // one and the assert below would fail.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "architect"},
        }))
        .await;
    let _ack2 = recv_json(&mut rx).await;
    let notification = recv_json(&mut rx).await;
    assert_eq!(notification["method"], "session/update");
    assert_eq!(notification["params"]["update"]["modeId"], "architect");
}

/// `configOptions` is ACP's preferred mode selector. Harn keeps it
/// synchronized with the legacy `modes` surface while the protocol
/// transitions away from dedicated mode methods.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_config_option_updates_mode() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "mode",
                "value": "shadow",
            },
        }))
        .await;
    let ack = recv_json(&mut rx).await;
    assert_eq!(ack["id"], 2);
    assert_eq!(ack["result"]["configOptions"][0]["currentValue"], "shadow");

    let mode_notification = recv_json(&mut rx).await;
    assert_eq!(
        mode_notification["params"]["update"]["sessionUpdate"],
        "current_mode_update"
    );
    assert_eq!(mode_notification["params"]["update"]["modeId"], "shadow");

    let config_notification = recv_json(&mut rx).await;
    assert_eq!(
        config_notification["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    assert_eq!(
        config_notification["params"]["update"]["configOptions"][0]["currentValue"],
        "shadow"
    );
}

/// `session/set_mode` rejects unknown sessions and unknown mode
/// ids with structured JSON-RPC errors instead of silently
/// accepting them.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_mode_validates_inputs() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    // Unknown session.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/set_mode",
            "params": {"sessionId": "ghost", "modeId": "architect"},
        }))
        .await;
    let unknown_session = recv_json(&mut rx).await;
    assert_eq!(unknown_session["id"], 1);
    assert_eq!(unknown_session["error"]["code"], -32602);
    assert!(unknown_session["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("Unknown session"));

    // Unknown mode id.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "not-a-mode"},
        }))
        .await;
    let unknown_mode = recv_json(&mut rx).await;
    assert_eq!(unknown_mode["id"], 3);
    assert_eq!(unknown_mode["error"]["code"], -32602);
    assert!(unknown_mode["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("Unknown mode"));
}

/// `architect` mode pushes a read-only capability ceiling for the
/// duration of `session/prompt`, so a script that calls
/// `write_file()` in plan mode is rejected by the VM policy gate
/// instead of mutating the workspace. Doubles as the conformance
/// case for "client switches mode mid-session, agent's behavior
/// changes" (#897 acceptance).
#[tokio::test(flavor = "current_thread")]
async fn acp_architect_mode_blocks_destructive_writes_in_prompt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("forbidden.txt");
            let target_str = target
                .to_str()
                .expect("temp path is utf-8")
                .replace('\\', "\\\\");

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
                    "params": {"cwd": dir.path().display().to_string()},
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
                    "id": 2,
                    "method": "session/set_mode",
                    "params": {"sessionId": session_id, "modeId": "architect"},
                }))
                .expect("send session/set_mode");
            // Drain ack + current_mode_update notification.
            let _ack = recv_json(&mut response_rx).await;
            let mode_notification = recv_json(&mut response_rx).await;
            assert_eq!(
                mode_notification["params"]["update"]["sessionUpdate"],
                "current_mode_update"
            );
            assert_eq!(mode_notification["params"]["update"]["modeId"], "architect");

            let prompt_source = format!("write_file(\"{target_str}\", \"should not be written\")");
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt_source}],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_error = false;
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
                if message["id"] == 3 {
                    let error = &message["error"];
                    assert!(
                        !error.is_null(),
                        "architect mode should reject write_file but got result {:?}",
                        message["result"]
                    );
                    let message_text = error["message"].as_str().unwrap_or_default();
                    assert!(
                        message_text.contains("workspace write ceiling"),
                        "unexpected error message: {message_text}"
                    );
                    saw_error = true;
                    break;
                }
            }
            assert!(saw_error, "prompt should produce a JSON-RPC error response");
            assert!(
                !target.exists(),
                "architect mode must not allow write_file to mutate the workspace"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// `code` mode is full-access mode: writes succeed because Harn leaves
/// host and runtime capability resolution authoritative instead of
/// installing an ACP mode ceiling. Pairs with the architect-mode test to
/// confirm mode policy is the only behavior difference between prompts.
#[tokio::test(flavor = "current_thread")]
async fn acp_code_mode_allows_writes_in_prompt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("allowed.txt");
            let target_str = target
                .to_str()
                .expect("temp path is utf-8")
                .replace('\\', "\\\\");

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
                    "params": {"cwd": dir.path().display().to_string()},
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
                    "id": 2,
                    "method": "session/set_mode",
                    "params": {"sessionId": session_id, "modeId": "code"},
                }))
                .expect("send session/set_mode");
            let _ack = recv_json(&mut response_rx).await;
            let _notification = recv_json(&mut response_rx).await;

            let prompt_source = format!("write_file(\"{target_str}\", \"hello from code mode\")");
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt_source}],
                    },
                }))
                .expect("send session/prompt");

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
                if message["id"] == 3 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    saw_completed = true;
                    break;
                }
            }
            assert!(saw_completed, "code-mode prompt should complete");
            assert!(
                target.exists(),
                "code mode should allow write_file to mutate the workspace"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// `session/fork` carries the parent's active mode over to the
/// branched session and surfaces it on the fork response, so
/// clients can render the correct mode badge on the new branch
/// without an extra round-trip.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_fork_inherits_parent_current_mode() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let parent_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": parent_id, "modeId": "architect"},
        }))
        .await;
    let _ack = recv_json(&mut rx).await;
    let _notification = recv_json(&mut rx).await;

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/fork",
            "params": {"session_id": parent_id},
        }))
        .await;
    // The fork emits a session_info_update notification *before*
    // the response; drain notifications until the response with
    // matching id arrives so the test is order-independent.
    let mut fork_response = None;
    for _ in 0..6 {
        let msg = recv_json(&mut rx).await;
        if msg["id"] == 3 {
            fork_response = Some(msg);
            break;
        }
    }
    let fork_response = fork_response.expect("fork response");
    assert_eq!(fork_response["result"]["state"], "forked");
    assert_eq!(
        fork_response["result"]["modes"]["currentModeId"],
        "architect"
    );
}

/// Pinning a model via `session/set_config_option(configId="model")`
/// updates the harn-vm session pin, surfaces the new value back in the
/// response, and emits a `config_option_update` notification so other
/// connected clients (e.g. additional editor panes) re-render the
/// selector. Validates the v1 model-swap contract from #1721.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_config_option_pins_model_and_emits_update() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "model",
                "value": "claude-sonnet-4-6",
            },
        }))
        .await;
    let ack = recv_json(&mut rx).await;
    assert_eq!(ack["id"], 2);
    let pinned_value = ack["result"]["configOptions"]
        .as_array()
        .expect("configOptions array")
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model config option")
        .get("currentValue")
        .and_then(|v| v.as_str())
        .expect("currentValue string");
    assert_eq!(pinned_value, "claude-sonnet-4-6");

    let notification = recv_json(&mut rx).await;
    assert_eq!(notification["method"], "session/update");
    assert_eq!(
        notification["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    let pin_in_notification = notification["params"]["update"]["configOptions"]
        .as_array()
        .expect("configOptions in notification")
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model entry in notification")
        .get("currentValue")
        .and_then(|v| v.as_str())
        .expect("currentValue string");
    assert_eq!(pin_in_notification, "claude-sonnet-4-6");

    assert_eq!(
        harn_vm::agent_sessions::pinned_model(&session_id).as_deref(),
        Some("claude-sonnet-4-6"),
        "vm session state must reflect the pin"
    );

    // Clear the pin by sending the `@inherit` sentinel — empty strings
    // would violate the spec's `currentValue` minLength constraint, so
    // the wire surface uses a stable string instead.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "model",
                "value": "@inherit",
            },
        }))
        .await;
    let clear_ack = recv_json(&mut rx).await;
    assert_eq!(clear_ack["id"], 3);
    let cleared_value = clear_ack["result"]["configOptions"]
        .as_array()
        .expect("configOptions array")
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model config option")
        .get("currentValue")
        .and_then(|v| v.as_str())
        .expect("currentValue string");
    assert_eq!(cleared_value, "@inherit");
    let _clear_notification = recv_json(&mut rx).await;
    assert!(
        harn_vm::agent_sessions::pinned_model(&session_id).is_none(),
        "clearing the pin should remove the vm-side selector"
    );
}

/// `session/set_config_option(configId="model")` rejects selectors that
/// don't resolve to a registered provider — the wire surface stays
/// distinct from the more permissive `provider/model` form a Harn
/// script can pass to `llm_call`.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_config_option_rejects_unknown_model_provider() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "model",
                "value": "nosuchprovider:nosuchmodel",
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["id"], 2);
    assert_eq!(response["error"]["code"], -32602);
    let message = response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("invalid_model"),
        "error message should be tagged invalid_model: {message}"
    );
    assert!(
        harn_vm::agent_sessions::pinned_model(&session_id).is_none(),
        "rejected pin must not mutate vm session state"
    );
}

/// Unknown `configId` values surface a structured error listing the
/// supported ids, so clients can distinguish "spec drift" from
/// "validation failure" without parsing the message.
#[tokio::test(flavor = "current_thread")]
async fn acp_set_config_option_rejects_unknown_config_id() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "temperature",
                "value": "0.4",
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["id"], 2);
    assert_eq!(response["error"]["code"], -32602);
    let message = response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("Unknown config option") && message.contains("model"),
        "error message should advertise the registry's supported ids: {message}"
    );
}
