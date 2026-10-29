use super::builtins::normalize_host_capability_manifest;
use super::*;
use super::{
    acp_agent_capabilities, configured_llm_route_for_capabilities, AcpBridge, AcpOutput, AcpServer,
    AcpServerConfig, SessionCancellation, ACP_AUTH_REQUIRED_CODE, ACP_SCHEMA_COMPATIBILITY,
    HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS, HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use crate::{ApiKeyAuthConfig, AuthMethodConfig, AuthPolicy};
use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use harn_vm::VmValue;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc;
mod client_capabilities;
mod provider_default_tests;
fn acp_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvSnapshot {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
    fn capture(names: &[&'static str]) -> Self {
        Self {
            saved: names
                .iter()
                .map(|name| (*name, std::env::var(name).ok()))
                .collect(),
        }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

async fn recv_json(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
    let line = harn_clock::test_support::within("ACP message", rx.recv())
        .await
        .expect("ACP response channel closed");
    serde_json::from_str(&line).expect("ACP JSON line")
}

/// Answer a server-to-client request (`host/capabilities`, `terminal/*`)
/// with an empty JSON-RPC `result`.
fn reply_empty_result(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    message: &serde_json::Value,
) {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"].clone(),
            "result": {},
        }))
        .expect("send empty client response");
}

/// Skip notifications until the JSON-RPC response with `id` arrives.
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

/// Only the hostlib-gated rollback/redo test builds transcript
/// messages by hand; gate the helper identically so a featureless
/// `clippy --all-targets -D warnings` run stays clean.
#[cfg(feature = "hostlib")]
fn make_acp_test_message(role: &str, content: &str) -> VmValue {
    VmValue::dict(BTreeMap::from([
        (
            "role".to_string(),
            VmValue::String(arcstr::ArcStr::from(role)),
        ),
        (
            "content".to_string(),
            VmValue::String(arcstr::ArcStr::from(content)),
        ),
    ]))
}

type AcpTestSession = (
    mpsc::UnboundedSender<serde_json::Value>,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
    String,
);

async fn start_acp_channel_session() -> AcpTestSession {
    start_acp_channel_session_with_config(AcpServerConfig::new(None), serde_json::json!(".")).await
}

async fn start_acp_channel_session_with_config(
    config: AcpServerConfig,
    cwd: serde_json::Value,
) -> AcpTestSession {
    start_acp_channel_session_with_params(
        config,
        serde_json::json!({"cwd": cwd, "environmentPolicy": {"kind": "isolated", "grants": []}}),
    )
    .await
}

async fn start_acp_channel_session_with_params(
    config: AcpServerConfig,
    params: serde_json::Value,
) -> AcpTestSession {
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel();
    let server = tokio::task::spawn_local(super::run_acp_channel_server(
        config,
        request_rx,
        response_tx,
    ));

    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {"terminal": true}},
        }))
        .expect("advertise test terminal support");
    assert!(recv_response_with_id(&mut response_rx, 0)
        .await
        .get("result")
        .is_some());

    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": params,
        }))
        .expect("send session/new");
    let created = recv_json(&mut response_rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    (request_tx, response_rx, server, session_id)
}

async fn start_acp_code_session_with_config(
    config: AcpServerConfig,
    cwd: serde_json::Value,
) -> AcpTestSession {
    let (request_tx, mut response_rx, server, session_id) =
        start_acp_channel_session_with_config(config, cwd).await;
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id.clone(), "modeId": "code"},
        }))
        .expect("send session/set_mode");
    let _ack = recv_json(&mut response_rx).await;
    let _mode_notification = recv_json(&mut response_rx).await;
    let _config_notification = recv_json(&mut response_rx).await;
    (request_tx, response_rx, server, session_id)
}

async fn start_acp_code_session() -> AcpTestSession {
    start_acp_code_session_with_config(AcpServerConfig::new(None), serde_json::json!(".")).await
}

#[tokio::test]
async fn public_acp_output_callback_receives_server_lines() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = lines.clone();
    let mut server = AcpServer::new_with_output(
        AcpServerConfig::new(None),
        AcpOutput::callback(move |line| {
            captured
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(line.to_string());
        }),
    );

    server
        .handle_incoming_message(
            AcpJsonRpcRequest::initialize(1)
                .into_json_value()
                .expect("initialize request serializes"),
        )
        .await;

    let lines = lines.lock().unwrap_or_else(|error| error.into_inner());
    let response: serde_json::Value =
        serde_json::from_str(lines.first().expect("one ACP response line"))
            .expect("response is JSON");
    assert_eq!(response["id"], serde_json::json!(1));
    assert_eq!(response["result"]["agentInfo"]["name"], "harn");
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_host_event_round_trips_typed_tool_result() {
    harn_vm::reset_thread_local_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
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
            "method": "session/inject_host_event",
            "params": {
                "sessionId": session_id,
                "event": {
                    "kind": "host_tool_result",
                    "delivery": "immediate",
                    "payload": {
                        "tool_name": "host.search",
                        "raw_input": {"query": "typed boundary"},
                        "raw_output": {"matches": 3}
                    },
                    "provenance": {
                        "initiator": "user",
                        "source": "acp-test",
                        "host": "test-host",
                        "ts_ms": 42
                    }
                }
            }
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["result"]["status"], "injected");
    assert_eq!(response["result"]["delivery"], "immediate");

    let messages = harn_vm::agent_sessions::messages_json(&session_id);
    let message = messages.last().expect("injected message");
    assert_eq!(message["role"], "user");
    assert_eq!(
        message["metadata"]["host_injection"]["tool_name"],
        "host.search"
    );
    assert_eq!(
        message["metadata"]["host_injection"]["provenance"]["source"],
        "acp-test"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_host_event_rejects_unknown_wire_fields() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/inject_host_event",
            "params": {
                "sessionId": "session",
                "event": {
                    "kind": "host_attachment",
                    "payload": {},
                    "provenance": {
                        "initiator": "user",
                        "source": "test",
                        "ts_ms": 42
                    },
                    "rendered": "host-authored prompt material"
                }
            }
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["error"]["code"], -32602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown field `rendered`"));
}

fn attach_test_host_bridge(
    server: &mut AcpServer,
    session_id: &str,
) -> Arc<harn_vm::bridge::HostBridge> {
    let session = server.sessions.get(session_id).expect("session");
    let inject_state = session.inject_state.clone();
    let tool_call_cancellations = session.concurrent_control.tool_call_cancellations.clone();
    let host_bridge = Arc::new(
        harn_vm::bridge::HostBridge::from_parts_with_writer_and_control(
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            std::sync::Arc::new(|_| Ok(())),
            1,
            harn_vm::bridge::HostBridgeControlState::new(
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::sync::Arc::new(tokio::sync::Notify::new()),
                inject_state,
                tool_call_cancellations,
            ),
        ),
    );
    server
        .sessions
        .get_mut(session_id)
        .expect("session")
        .host_bridge = Some(host_bridge.clone());
    host_bridge
}

#[tokio::test(flavor = "current_thread")]
async fn session_remind_accepts_typed_reminder_payload() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/remind",
            "params": {
                "sessionId": session_id,
                "body": "Host reminder",
                "tags": ["host"],
                "dedupe_key": "host-reminder",
                "ttl_turns": 2,
                "mode": "finish_step",
                "_meta": {"harn": {"source": "test"}},
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert!(response["result"]["reminderId"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn session_reminder_pending_list_and_revoke_controls() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    let bridge = attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/remind",
            "params": {
                "sessionId": session_id,
                "id": "rem-acp",
                "body": "Host reminder",
                "tags": ["host"],
                "dedupe_key": "host-reminder",
                "ttl_turns": 2,
                "mode": "finish_step",
            },
        }))
        .await;
    let reminded = recv_json(&mut rx).await;
    assert_eq!(reminded["result"]["reminderId"], "rem-acp");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/pending_prompts",
            "params": {"sessionId": session_id},
        }))
        .await;
    let pending = recv_json(&mut rx).await;
    assert_eq!(pending["result"]["prompts"], serde_json::json!([]));
    assert_eq!(pending["result"]["reminders"][0]["reminderId"], "rem-acp");
    assert_eq!(pending["result"]["reminders"][0]["mode"], "finish_step");
    assert_eq!(pending["result"]["reminders"][0]["body"], "Host reminder");

    for id in [4, 5] {
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "session/revoke_reminder",
                "params": {
                    "sessionId": session_id,
                    "reminderId": "rem-acp",
                },
            }))
            .await;
        let revoke = recv_json(&mut rx).await;
        assert_eq!(revoke["result"]["reminderId"], "rem-acp");
        assert_eq!(
            revoke["result"]["status"],
            if id == 4 {
                "revoked"
            } else {
                "already_revoked"
            }
        );
    }

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "session/pending_prompts",
            "params": {"sessionId": session_id},
        }))
        .await;
    let empty = recv_json(&mut rx).await;
    assert_eq!(empty["result"]["reminders"], serde_json::json!([]));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/remind",
            "params": {
                "sessionId": session_id,
                "id": "rem-delivered",
                "body": "Delivered reminder",
                "mode": "finish_step",
            },
        }))
        .await;
    let reminded = recv_json(&mut rx).await;
    assert_eq!(reminded["result"]["reminderId"], "rem-delivered");

    let delivered = bridge
        .take_queued_transcript_injections_for(
            harn_vm::bridge::DeliveryCheckpoint::AfterCurrentOperation,
        )
        .await;
    assert_eq!(delivered.len(), 1);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "session/revoke_reminder",
            "params": {
                "sessionId": session_id,
                "reminderId": "rem-delivered",
            },
        }))
        .await;
    let delivered_revoke = recv_json(&mut rx).await;
    assert_eq!(delivered_revoke["error"]["code"], -32602);
    assert_eq!(
        delivered_revoke["error"]["data"]["reason"],
        "already_delivered"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_remind_rejects_user_message_payload() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/remind",
            "params": {
                "sessionId": session_id,
                "content": "This is user input, not a reminder.",
                "mode": "finish_step",
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["error"]["code"], -32602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("HARN-RMD-002"));
}

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_tool_call_returns_not_found_when_no_call_in_flight() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
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
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "toolCallId": "call_unknown",
                "reason": "user clicked stop",
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["result"]["status"], "not_found");
    assert_eq!(response["result"]["callId"], "call_unknown");
    assert!(response["result"]["tool"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_tool_call_rejects_missing_tool_call_id() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
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
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "reason": "no id supplied",
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["error"]["code"], -32602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("toolCallId"));
}

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_is_idempotent_and_actor_attributed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": ".", "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    for (id, expected) in [(2, "cancelled"), (3, "already_cancelled")] {
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "session/cancel",
                "params": {
                    "sessionId": session_id,
                    "_harn": {
                        "actor": {
                            "clientId": "controller-a",
                            "connectionId": "conn-a",
                            "role": "controller",
                            "source": "ide"
                        }
                    }
                },
            }))
            .await;
        let response = recv_json(&mut rx).await;
        assert_eq!(response["result"]["status"], expected);
        assert_eq!(
            response["result"]["_meta"]["harn"]["actor"]["clientId"],
            "controller-a"
        );
    }
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_session_rollback_and_redo_move_transcript_and_filesystem_together() {
    harn_vm::reset_thread_local_state();

    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "before").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": dir.path().to_string_lossy(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    harn_hostlib::fs::configure_session_root(&session_id, dir.path());

    harn_vm::agent_sessions::inject_message(
        &session_id,
        make_acp_test_message("user", "before turn"),
    )
    .expect("seed transcript");
    let before_transcript = harn_vm::agent_sessions::transcript(&session_id).expect("transcript");
    harn_hostlib::fs_snapshot::snapshot(
        &session_id,
        "turn-file",
        &[file.to_string_lossy().into_owned()],
        Some(dir.path()),
    )
    .expect("snapshot");
    std::fs::write(&file, "after").unwrap();
    harn_vm::agent_sessions::inject_message(
        &session_id,
        make_acp_test_message("assistant", "after turn"),
    )
    .expect("append transcript");
    harn_vm::agent_sessions::record_completed_turn_checkpoint(
        &session_id,
        before_transcript,
        vec!["turn-file".to_string()],
    )
    .expect("record checkpoint")
    .expect("checkpoint changed");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/rollback",
            "params": {"sessionId": session_id.clone()},
        }))
        .await;
    let rollback_update = recv_json(&mut rx).await;
    assert_eq!(
        rollback_update["params"]["update"]["sessionUpdate"],
        "session_rollback"
    );
    let rollback = recv_json(&mut rx).await;
    assert_eq!(rollback["result"]["status"], "rolled_back");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "before");
    assert_eq!(harn_vm::agent_sessions::messages_json(&session_id).len(), 1);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/redo",
            "params": {"sessionId": session_id.clone()},
        }))
        .await;
    let redo_update = recv_json(&mut rx).await;
    assert_eq!(
        redo_update["params"]["update"]["sessionUpdate"],
        "session_redo"
    );
    let redo = recv_json(&mut rx).await;
    assert_eq!(redo["result"]["status"], "redone");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "after");
    assert_eq!(harn_vm::agent_sessions::messages_json(&session_id).len(), 2);
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_session_restore_tool_call_restores_pre_image_and_emits_update() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("subject.txt");
    std::fs::write(&file, b"pre").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": dir.path().to_string_lossy(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    // session/new returns a fresh UUID per call so this test's snapshot
    // bundle cannot collide with another test running in the same
    // process. Synthesize a unique tool-call id to match.
    let tool_call_id = format!(
        "tc-acp-restore-{}-{}",
        std::process::id(),
        ACP_RESTORE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );

    let snapshot = harn_hostlib::fs_snapshot::snapshot(
        &session_id,
        &tool_call_id,
        &[file.to_string_lossy().into_owned()],
        Some(dir.path()),
    )
    .expect("snapshot");
    assert_eq!(snapshot.captured_paths.len(), 1);

    // Clobber the on-disk file, then ask ACP to restore the tool call.
    std::fs::write(&file, b"clobbered").unwrap();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/restore_tool_call",
            "params": {
                "sessionId": session_id.clone(),
                "toolCallId": tool_call_id.clone(),
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["result"]["toolCallId"], tool_call_id);
    let restored = response["result"]["restoredPaths"].as_array().unwrap();
    assert_eq!(restored.len(), 1);

    let update = recv_json(&mut rx).await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["sessionUpdate"],
        "tool_call_update"
    );
    assert_eq!(update["params"]["update"]["status"], "restored");
    assert_eq!(update["params"]["update"]["toolCallId"], tool_call_id);
    assert_eq!(
        update["params"]["update"]["_meta"]["harn"]["kind"],
        "tool_call_restored"
    );

    assert_eq!(std::fs::read(&file).unwrap(), b"pre");

    harn_hostlib::fs_snapshot::drop_session_snapshots(&session_id);
}

#[cfg(feature = "hostlib")]
static ACP_RESTORE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[test]
fn normalize_host_capabilities_wraps_array_entries_in_ops_dicts() {
    let mut root = BTreeMap::new();
    root.insert(
        "project".to_string(),
        VmValue::List(Arc::new(vec![VmValue::String(arcstr::ArcStr::from(
            "scope_test_command",
        ))])),
    );

    let normalized = normalize_host_capability_manifest(VmValue::dict(root));
    let manifest = normalized.as_dict().expect("dict manifest");
    let project = manifest
        .get("project")
        .and_then(|value| value.as_dict())
        .expect("project capability dict");
    let ops = project
        .get("ops")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .expect("ops list");

    assert!(ops
        .iter()
        .any(|value| value.display() == "scope_test_command"));
}

#[test]
fn normalize_host_capabilities_derives_ops_from_operation_metadata() {
    let mut operations = BTreeMap::new();
    operations.insert(
        "get_default_shell".to_string(),
        VmValue::dict_map(Default::default()),
    );
    let mut process = BTreeMap::new();
    process.insert("operations".to_string(), VmValue::dict(operations));
    let mut root = BTreeMap::new();
    root.insert("process".to_string(), VmValue::dict(process));

    let normalized = normalize_host_capability_manifest(VmValue::dict(root));
    let manifest = normalized.as_dict().expect("dict manifest");
    let process = manifest
        .get("process")
        .and_then(|value| value.as_dict())
        .expect("process capability dict");
    let ops = process
        .get("ops")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .expect("ops list");

    assert!(ops
        .iter()
        .any(|value| value.display() == "get_default_shell"));
}

#[test]
fn acp_agent_capabilities_use_canonical_initialize_shape() {
    let _guard = acp_env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _env = EnvSnapshot::capture(&[
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "LOCAL_LLM_BASE_URL",
        "LOCAL_LLM_MODEL",
        "MLX_MODEL_ID",
    ]);
    std::env::set_var("HARN_LLM_PROVIDER", "openai");
    std::env::remove_var("HARN_LLM_MODEL");
    std::env::remove_var("LOCAL_LLM_BASE_URL");
    std::env::remove_var("LOCAL_LLM_MODEL");
    std::env::remove_var("MLX_MODEL_ID");

    let capabilities = acp_agent_capabilities();

    // Pin only the provider routing invariant + that the resolved
    // model is a registered catalog entry. The specific OpenAI default
    // moves as the catalog tracks model deprecations.
    let (provider, model) = configured_llm_route_for_capabilities().expect("catalog default");
    assert_eq!(provider, "openai");
    assert!(
        harn_vm::llm_config::model_catalog_entry(&model).is_some(),
        "openai fallback must point at a registered catalog model (got {model})"
    );
    assert_eq!(capabilities["loadSession"], true);
    assert!(capabilities["session"].get("inject").is_none());
    assert_eq!(
        capabilities["session"]["pendingPrompts"],
        serde_json::json!({})
    );
    assert_eq!(
        capabilities["session"]["remind"],
        serde_json::json!({
            "modes": ["interrupt_immediate", "finish_step", "audit_only"],
            "pending": {"list": true, "revoke": true},
        })
    );
    assert_eq!(
        capabilities["session"]["injectHostEvent"],
        serde_json::json!({
            "kinds": ["host_tool_result", "host_attachment"],
            "delivery": ["turn_boundary", "immediate", "after_next_tool_call"],
        })
    );
    assert_eq!(
        capabilities["promptCapabilities"],
        serde_json::json!({
            "image": true,
            "audio": false,
            "embeddedContext": false,
            "delivery": {"steer": {}, "queue": {}},
        })
    );
    assert_eq!(
        capabilities["mcpCapabilities"],
        serde_json::json!({
            "http": false,
            "sse": false,
        })
    );
    assert_eq!(
        capabilities["sessionCapabilities"],
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
        capabilities["sessionCapabilities"].get("fork").is_none(),
        "Harn-only session/fork must not be advertised as an ACP SessionCapability"
    );
    assert_eq!(
        capabilities["_meta"]["harn"]["extensionMethods"][HARN_PROVIDER_CATALOG_METHOD]["schema"],
        harn_vm::provider_catalog::PROVIDER_CATALOG_SCHEMA_ID
    );
}

#[test]
fn acp_prompt_capabilities_follow_configured_model_aliases() {
    let _guard = acp_env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _env = EnvSnapshot::capture(&[
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "LOCAL_LLM_BASE_URL",
        "LOCAL_LLM_MODEL",
        "MLX_MODEL_ID",
    ]);
    std::env::remove_var("HARN_LLM_PROVIDER");
    std::env::set_var("HARN_LLM_MODEL", "frontier");
    std::env::remove_var("LOCAL_LLM_BASE_URL");
    std::env::remove_var("LOCAL_LLM_MODEL");
    std::env::remove_var("MLX_MODEL_ID");

    let capabilities = acp_agent_capabilities();

    // The `frontier` alias resolves to whatever the embedded
    // providers.toml currently designates as the flagship Anthropic
    // model; pinning a specific id here would force a test churn every
    // time the catalog tracks an Anthropic refresh. Pin only the
    // routing invariant (provider) plus the model's catalog presence.
    let (provider, model) = configured_llm_route_for_capabilities().expect("explicit alias");
    assert_eq!(provider, "anthropic");
    assert!(
        harn_vm::llm_config::model_catalog_entry(&model)
            .is_some_and(|entry| entry.provider == "anthropic" && !entry.deprecated),
        "frontier route must point at a registered, non-deprecated anthropic model (got {model})"
    );
    assert_eq!(
        capabilities["promptCapabilities"],
        serde_json::json!({
            "image": true,
            "audio": true,
            "embeddedContext": true,
            "delivery": {"steer": {}, "queue": {}},
        })
    );
}

/// Drift guard: every `session/*` dispatch arm must gate on
/// `reject_unauthenticated`. `session/cancel` shipped without the guard once
/// (any unauthenticated peer could cancel a running session); this keeps the
/// next session method from repeating that.
#[test]
fn every_session_dispatch_arm_checks_authentication() {
    let src = include_str!("dispatch.rs");
    let lines: Vec<&str> = src.lines().collect();
    // Match-arm patterns in the dispatch match are indented exactly 12
    // spaces; arm bodies are indented deeper. Collect each session arm's
    // body by scanning to the next same-indent pattern.
    let is_pattern =
        |line: &str| line.starts_with("            \"") && !line.starts_with("             ");
    let mut checked = 0;
    for (i, line) in lines.iter().enumerate() {
        if !is_pattern(line) || !line.contains("\"session/") {
            continue;
        }
        let method = line.trim();
        let body: String = lines[i + 1..]
            .iter()
            .take_while(|l| !is_pattern(l))
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("reject_unauthenticated"),
            "dispatch arm {method} does not call reject_unauthenticated"
        );
        checked += 1;
    }
    assert!(
        checked >= 10,
        "expected to find the session/* dispatch arms in dispatch.rs (found {checked}); \
         if the match moved, update this test's pattern detection"
    );
}

mod batch_verification_stop;
mod caching;
mod canonical_fork;
mod commands;
mod config_values;
mod conservative_admission;
mod emit_response;
mod event_log_barrier;
mod host_call_turn_cache;
mod injection_admission;
mod model_routing;
mod modes;
mod oauth_redirect;
mod prompt_delivery;
mod prompt_errors;
mod reasoning_receipts;
mod runtime_overrides;
mod sanitize_visible_text;
mod served_agent_turn;
mod served_json_tool_fence;
mod session_environment;
mod session_inputs;
mod session_recap;
mod session_restore;
mod session_spend;
mod sessions;
#[cfg(feature = "hostlib")]
mod staged_writes;
mod stop_controls;
mod tool_call_cancellation;
#[cfg(feature = "hostlib")]
mod workspace_checkpoint;
