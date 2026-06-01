use super::builtins::normalize_host_capability_manifest;
use super::*;
use super::{
    acp_agent_capabilities, configured_llm_route_for_capabilities, sanitize_visible_assistant_text,
    AcpBridge, AcpOutput, AcpServer, AcpServerConfig, SessionCancellation, ACP_AUTH_REQUIRED_CODE,
    ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_PROVIDER_CATALOG_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use crate::{ApiKeyAuthConfig, AuthMethodConfig, AuthPolicy};
use harn_vm::visible_text::VisibleTextState;
use harn_vm::VmValue;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc;

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
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for ACP response")
        .expect("ACP response channel closed");
    serde_json::from_str(&line).expect("ACP JSON line")
}

async fn start_acp_channel_session() -> (
    mpsc::UnboundedSender<serde_json::Value>,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
    String,
) {
    start_acp_channel_session_with_config(AcpServerConfig::new(None), serde_json::json!(".")).await
}

async fn start_acp_channel_session_with_config(
    config: AcpServerConfig,
    cwd: serde_json::Value,
) -> (
    mpsc::UnboundedSender<serde_json::Value>,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
    String,
) {
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
            "id": 1,
            "method": "session/new",
            "params": {"cwd": cwd},
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
) -> (
    mpsc::UnboundedSender<serde_json::Value>,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
    String,
) {
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

fn attach_test_host_bridge(
    server: &mut AcpServer,
    session_id: &str,
) -> Arc<harn_vm::bridge::HostBridge> {
    let inject_state = server
        .sessions
        .get(session_id)
        .expect("session")
        .inject_state
        .clone();
    let host_bridge = Arc::new(
        harn_vm::bridge::HostBridge::from_parts_with_writer_cancel_notify_and_injection_state(
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(|_| Ok(())),
            1,
            Some(inject_state),
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
            "params": {"cwd": "."},
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
            "params": {"cwd": "."},
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
            "method": "session/pending_injections",
            "params": {"sessionId": session_id},
        }))
        .await;
    let pending = recv_json(&mut rx).await;
    assert_eq!(pending["result"]["pendingCount"], 1);
    assert_eq!(pending["result"]["injections"][0]["kind"], "reminder");
    assert_eq!(pending["result"]["injections"][0]["reminderId"], "rem-acp");
    assert_eq!(pending["result"]["injections"][0]["mode"], "finish_step");
    assert_eq!(pending["result"]["injections"][0]["body"], "Host reminder");

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
            "method": "session/pending_injections",
            "params": {"sessionId": session_id},
        }))
        .await;
    let empty = recv_json(&mut rx).await;
    assert_eq!(empty["result"]["pendingCount"], 0);

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
            "params": {"cwd": "."},
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
async fn session_cancel_tool_call_targets_registered_call() {
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

    // Pretend a tool call is in flight by registering one directly.
    let (_handle, _guard) =
        harn_vm::tool_call_cancellations::register(session_id.clone(), "call_42", "git_push")
            .expect("registered");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "toolCallId": "call_42",
                "reason": "user clicked stop",
                "injectReminder": false,
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    assert_eq!(response["result"]["status"], "cancelled");
    assert_eq!(response["result"]["callId"], "call_42");
    assert_eq!(response["result"]["tool"], "git_push");

    // A second cancel must report `already_cancelled` so the host can
    // suppress redundant retries.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/cancel_tool_call",
            "params": {
                "sessionId": session_id,
                "toolCallId": "call_42",
                "reason": "still stopping",
                "injectReminder": false,
            },
        }))
        .await;
    let second = recv_json(&mut rx).await;
    assert_eq!(second["result"]["status"], "already_cancelled");
    assert_eq!(second["result"]["tool"], "git_push");
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
async fn session_inject_accepts_with_message_id_and_delivers_same_id() {
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
    let bridge = attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": [{"type": "text", "text": "queued follow-up"}],
            },
        }))
        .await;
    let response = recv_json(&mut rx).await;
    let message_id = response["result"]["messageId"]
        .as_str()
        .expect("messageId")
        .to_string();
    assert!(message_id.starts_with("msg_inj_"));

    let delivered = bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::EndOfInteraction)
        .await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message_id, message_id);
    assert_eq!(delivered[0].content, "queued follow-up");
    assert_eq!(
        delivered[0].transcript_content,
        serde_json::json!([{"type": "text", "text": "queued follow-up"}])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_requires_active_prompt_bridge() {
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
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": "not running",
            },
        }))
        .await;
    let rejected = recv_json(&mut rx).await;
    assert_eq!(rejected["error"]["code"], -32004);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no active prompt"));
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
            "params": {"cwd": "."},
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

#[tokio::test(flavor = "current_thread")]
async fn session_prompt_compile_error_clears_active_inject_bridge() {
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
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "let x ="}],
            },
        }))
        .await;
    let mut saw_prompt_error = false;
    for _ in 0..4 {
        let message = recv_json(&mut rx).await;
        if message["id"] == 2 {
            assert_eq!(message["error"]["code"], -32000);
            saw_prompt_error = true;
            break;
        }
    }
    assert!(
        saw_prompt_error,
        "compile failure should answer session/prompt"
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": "must not target the failed prompt",
            },
        }))
        .await;
    let rejected = recv_json(&mut rx).await;
    assert_eq!(rejected["error"]["code"], -32004);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no active prompt"));
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_revoke_and_replace_pending_messages() {
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
    let bridge = attach_test_host_bridge(&mut server, &session_id);

    for (id, text) in [(2, "first"), (3, "second")] {
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "session/inject",
                "params": {
                    "sessionId": session_id,
                    "mode": "steer",
                    "content": [{"type": "text", "text": text}],
                },
            }))
            .await;
    }
    let first = recv_json(&mut rx).await;
    let first_id = first["result"]["messageId"].as_str().unwrap().to_string();
    let second = recv_json(&mut rx).await;
    let second_id = second["result"]["messageId"].as_str().unwrap().to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/replace_inject",
            "params": {
                "sessionId": session_id,
                "messageId": first_id,
                "content": [{"type": "text", "text": "first edited"}],
            },
        }))
        .await;
    let replace = recv_json(&mut rx).await;
    assert_eq!(replace["result"]["messageId"], first_id);
    assert_eq!(replace["result"]["status"], "replaced");

    for id in [5, 6] {
        server
            .handle_incoming_message(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "session/revoke_inject",
                    "params": {
                        "sessionId": session_id,
                        "messageId": second_id,
                    },
            }))
            .await;
        let revoke = recv_json(&mut rx).await;
        assert_eq!(revoke["result"]["messageId"], second_id);
        assert_eq!(
            revoke["result"]["status"],
            if id == 5 {
                "revoked"
            } else {
                "already_revoked"
            }
        );
    }

    let delivered = bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::AfterCurrentOperation)
        .await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message_id, first_id);
    assert_eq!(delivered[0].content, "first edited");
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_state_survives_prompt_bridge_replacement() {
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
    let first_bridge = attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": [{"type": "text", "text": "before replacement"}],
            },
        }))
        .await;
    let accepted = recv_json(&mut rx).await;
    let message_id = accepted["result"]["messageId"]
        .as_str()
        .expect("message id")
        .to_string();

    server
        .sessions
        .get_mut(&session_id)
        .expect("session")
        .host_bridge = None;

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/replace_inject",
            "params": {
                "sessionId": session_id,
                "messageId": message_id,
                "content": [{"type": "text", "text": "after replacement"}],
            },
        }))
        .await;
    let replace = recv_json(&mut rx).await;
    assert_eq!(replace["result"]["messageId"], message_id);
    assert_eq!(replace["result"]["status"], "replaced");

    let replacement_bridge = Arc::new(
        harn_vm::bridge::HostBridge::from_parts_with_writer_cancel_notify_and_injection_state(
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(|_| Ok(())),
            10_000,
            Some(first_bridge.injection_state()),
        ),
    );
    server
        .sessions
        .get_mut(&session_id)
        .expect("session")
        .host_bridge = Some(replacement_bridge.clone());

    let delivered = replacement_bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::EndOfInteraction)
        .await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message_id, message_id);
    assert_eq!(delivered[0].content, "after replacement");
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_reports_unknown_and_already_delivered_ids() {
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
    let bridge = attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/revoke_inject",
            "params": {"sessionId": session_id, "messageId": "missing"},
        }))
        .await;
    let unknown = recv_json(&mut rx).await;
    assert_eq!(unknown["error"]["data"]["reason"], "unknown_message_id");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": [{"type": "text", "text": "deliver me"}],
            },
        }))
        .await;
    let accepted = recv_json(&mut rx).await;
    let message_id = accepted["result"]["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    let delivered = bridge
        .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::EndOfInteraction)
        .await;
    assert_eq!(delivered.len(), 1);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/replace_inject",
            "params": {
                "sessionId": session_id,
                "messageId": message_id,
                "content": [{"type": "text", "text": "too late"}],
            },
        }))
        .await;
    let already_delivered = recv_json(&mut rx).await;
    assert_eq!(
        already_delivered["error"]["data"]["reason"],
        "already_delivered"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_inject_rejects_cross_actor_mutation() {
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
    attach_test_host_bridge(&mut server, &session_id);

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/inject",
            "params": {
                "sessionId": session_id,
                "mode": "queue",
                "content": "owned pending message",
                "_harn": {
                    "actor": {
                        "clientId": "controller-a",
                        "role": "controller",
                        "source": "ide"
                    }
                }
            },
        }))
        .await;
    let accepted = recv_json(&mut rx).await;
    let message_id = accepted["result"]["messageId"].as_str().unwrap();
    assert_eq!(accepted["result"]["status"], "accepted");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/replace_inject",
            "params": {
                "sessionId": session_id,
                "messageId": message_id,
                "content": "stolen edit",
                "_harn": {
                    "actor": {
                        "clientId": "controller-b",
                        "role": "controller",
                        "source": "ide"
                    }
                }
            },
        }))
        .await;
    let rejected = recv_json(&mut rx).await;
    assert_eq!(
        rejected["error"]["data"]["reason"],
        "not_owner_or_not_authorized"
    );
    assert_eq!(
        rejected["error"]["data"]["owner"]["clientId"],
        "controller-a"
    );
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_fs_mode_and_commit_staged_apply_deferred_hostlib_writes() {
    use harn_hostlib::{
        tools::{permissions, ToolsCapability},
        BuiltinRegistry, HostlibCapability,
    };

    permissions::reset();
    permissions::enable_for_test();

    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("draft.txt");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": dir.path().to_string_lossy()},
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
            "method": "session/fs_mode",
            "params": {"sessionId": session_id.clone(), "mode": "staged"},
        }))
        .await;
    let mode_response = recv_json(&mut rx).await;
    assert_eq!(mode_response["result"]["previousMode"], "immediate");
    assert_eq!(mode_response["result"]["mode"], "staged");
    let mode_update = recv_json(&mut rx).await;
    assert_eq!(
        mode_update["params"]["update"]["_meta"]["harn"]["kind"],
        "staged_writes_pending"
    );
    assert_eq!(
        mode_update["params"]["update"]["_meta"]["harn"]["pendingCount"],
        0
    );

    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let mut args = BTreeMap::new();
    args.insert(
        "session_id".to_string(),
        VmValue::String(Arc::from(session_id.as_str())),
    );
    args.insert(
        "path".to_string(),
        VmValue::String(Arc::from(file.to_string_lossy().as_ref())),
    );
    args.insert("content".to_string(), VmValue::String(Arc::from("draft")));
    (registry
        .find("hostlib_tools_write_file")
        .expect("write_file builtin")
        .handler)(&[VmValue::Dict(Arc::new(args))])
    .expect("stage write");
    assert!(!file.exists(), "ACP staged mode should defer disk writes");

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/fs_commit_staged",
            "params": {"sessionId": session_id.clone()},
        }))
        .await;
    let commit_response = recv_json(&mut rx).await;
    assert_eq!(
        commit_response["result"]["committedPaths"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "draft");

    let commit_update = recv_json(&mut rx).await;
    assert_eq!(
        commit_update["params"]["update"]["_meta"]["harn"]["kind"],
        "staged_writes_pending"
    );
    assert_eq!(
        commit_update["params"]["update"]["_meta"]["harn"]["pendingCount"],
        0
    );
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_session_restore_tool_call_restores_pre_image_and_emits_update() {
    use harn_hostlib::tools::permissions;

    permissions::enable_for_test();

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
            "params": {"cwd": dir.path().to_string_lossy()},
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
        VmValue::List(Arc::new(vec![VmValue::String(Arc::from(
            "scope_test_command",
        ))])),
    );

    let normalized = normalize_host_capability_manifest(VmValue::Dict(Arc::new(root)));
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
        VmValue::Dict(Arc::new(BTreeMap::new())),
    );
    let mut process = BTreeMap::new();
    process.insert(
        "operations".to_string(),
        VmValue::Dict(Arc::new(operations)),
    );
    let mut root = BTreeMap::new();
    root.insert("process".to_string(), VmValue::Dict(Arc::new(process)));

    let normalized = normalize_host_capability_manifest(VmValue::Dict(Arc::new(root)));
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
fn sanitize_visible_assistant_text_strips_internal_markers() {
    let raw = "hello\n##DONE##\nDONE\n[result of read]\nsecret\n[end of read result]\nworld";
    assert_eq!(
        sanitize_visible_assistant_text(raw, false),
        "hello\n\nworld"
    );
}

#[test]
fn sanitize_visible_assistant_text_keeps_normal_code_fences() {
    let raw = "```ts\nconst x = 1\n```";
    assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
}

#[test]
fn sanitize_visible_assistant_text_drops_internal_json_fences() {
    let raw = "```json\n{\"plan\":[{\"tool_name\":\"read\"}]}\n```\n\nVisible";
    assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_drops_inline_planner_json() {
    let raw = "{\"mode\":\"ask_user\",\"direction\":\"Need one decision\",\"targets\":[\"src\"],\"tasks\":[\"Clarify scope\"],\"unknowns\":[\"Which one?\"]}\n\nVisible";
    assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_drops_partial_inline_planner_json() {
    let raw = "Visible\n{\"mode\":\"plan_then_execute\",\"direction\":\"Patch the file\"";
    assert_eq!(sanitize_visible_assistant_text(raw, true), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_keeps_normal_json() {
    let raw = "{\"status\":\"ok\",\"message\":\"Visible\"}";
    assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
}

#[test]
fn acp_agent_capabilities_use_canonical_initialize_shape() {
    let _guard = acp_env_lock().lock().unwrap();
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
    let (provider, model) = configured_llm_route_for_capabilities();
    assert_eq!(provider, "openai");
    assert!(
        harn_vm::llm_config::model_catalog_entry(&model).is_some(),
        "openai fallback must point at a registered catalog model (got {model})"
    );
    assert_eq!(capabilities["loadSession"], true);
    assert_eq!(
        capabilities["session"]["inject"],
        serde_json::json!({
            "modes": ["queue", "steer"],
            "pending": {"replace": true},
        })
    );
    assert_eq!(
        capabilities["session"]["remind"],
        serde_json::json!({
            "modes": ["interrupt_immediate", "finish_step", "audit_only"],
            "pending": {"list": true, "revoke": true},
        })
    );
    assert_eq!(
        capabilities["promptCapabilities"],
        serde_json::json!({
            "image": true,
            "audio": true,
            "embeddedContext": false,
        })
    );
    assert_eq!(
        capabilities["mcpCapabilities"],
        serde_json::json!({
            "http": true,
            "sse": true,
        })
    );
    assert_eq!(
        capabilities["sessionCapabilities"],
        serde_json::json!({
            "close": {},
            "list": {},
            "resume": {},
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

#[tokio::test(flavor = "current_thread")]
async fn acp_provider_catalog_method_matches_export_artifact_with_overrides() {
    let _reset = crate::test_support::LlmOverrideReset;
    let overlay = crate::test_support::fixture_provider_overlay();
    let capability_overlay = crate::test_support::fixture_capability_overlay();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(
        AcpServerConfig::new(None)
            .with_llm_overrides(Some(overlay.clone()), Some(capability_overlay.clone())),
        AcpOutput::Channel(tx),
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": HARN_PROVIDER_CATALOG_METHOD,
            "params": {},
        }))
        .await;
    let response = recv_json(&mut rx).await;
    let expected = serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
        Some(&overlay),
        Some(&capability_overlay),
    ))
    .expect("expected catalog json");
    assert_eq!(response["result"], expected);
    assert!(response["result"]["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .any(|provider| provider["id"] == "fixture_runtime"));
}

#[test]
fn acp_prompt_capabilities_follow_configured_model_aliases() {
    let _guard = acp_env_lock().lock().unwrap();
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
    let (provider, model) = configured_llm_route_for_capabilities();
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
        })
    );
}

/// Compile cache: re-issuing a `session/prompt` on the same pipeline file
/// must serve the bytecode from cache. Touching the file (advancing mtime)
/// or switching the target pipeline name must invalidate the slot. This
/// drives the helper directly rather than spinning a full ACP server so the
/// assertion stays focused on the cache mechanics — the end-to-end path is
/// exercised by the existing hot-reload test in the `commands` submodule.
#[test]
fn compile_pipeline_cached_serves_cached_chunk_until_mtime_advances() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pipeline_path = dir.path().join("p.harn");
    let initial = "pipeline main() { __io_println(\"first\") }\n";
    std::fs::write(&pipeline_path, initial).expect("write initial");

    let mut server = AcpServer::new(AcpServerConfig::new(Some(
        pipeline_path.to_string_lossy().to_string(),
    )));

    let (_chunk, hit1) = server
        .compile_pipeline_cached(initial, Some(pipeline_path.as_path()), None)
        .expect("first compile");
    assert!(!hit1, "first compile must miss the cache");

    let (_chunk, hit2) = server
        .compile_pipeline_cached(initial, Some(pipeline_path.as_path()), None)
        .expect("second compile");
    assert!(hit2, "second compile of unchanged source must hit");

    // Switching `target_pipeline` invalidates the slot — a named compile
    // produces a different chunk than the default-entry compile.
    let named_source = "@command(name: \"alpha\") pipeline alpha() { __io_println(\"alpha\") }\n\
                       pipeline main() { __io_println(\"main\") }\n";
    std::fs::write(&pipeline_path, named_source).expect("write named");
    // Force mtime advance with a deterministic far-future literal so the
    // test doesn't read the wall clock (banned by `make lint-test-patterns`).
    // 2_000_000_000 = 2033-05-18, comfortably after any plausible CI clock
    // and well past the whole-second rounding some filesystems apply to
    // fresh writes.
    let bumped = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(&pipeline_path, bumped).expect("bump mtime");
    let (_chunk, hit3) = server
        .compile_pipeline_cached(named_source, Some(pipeline_path.as_path()), Some("alpha"))
        .expect("named compile");
    assert!(
        !hit3,
        "different mtime + target_pipeline must miss the previous slot"
    );

    let (_chunk, hit4) = server
        .compile_pipeline_cached(named_source, Some(pipeline_path.as_path()), Some("alpha"))
        .expect("named compile second");
    assert!(hit4, "repeated named compile must hit");
}

/// Inline-mode prompts (no `source_path`) are not cached — they're one-off
/// by construction and caching them would just bloat memory.
#[test]
fn compile_pipeline_cached_does_not_cache_inline_prompts() {
    let mut server = AcpServer::new(AcpServerConfig::new(None));
    let source = "pipeline main() { __io_println(\"inline\") }\n";
    let (_chunk, hit1) = server
        .compile_pipeline_cached(source, None, None)
        .expect("first inline compile");
    assert!(!hit1);
    let (_chunk, hit2) = server
        .compile_pipeline_cached(source, None, None)
        .expect("second inline compile");
    assert!(
        !hit2,
        "inline-mode compiles must not be cached (per-turn source is dynamic)"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn vm_baseline_cached_serves_file_backed_context_until_key_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pipeline_path = dir.path().join("baseline.harn");
    let source = "pipeline main() { __io_println(\"baseline\") }\n";
    std::fs::write(&pipeline_path, source).expect("write pipeline");

    let mut server = AcpServer::new(AcpServerConfig::new(Some(
        pipeline_path.to_string_lossy().to_string(),
    )));
    let (_baseline, hit1, _ms1) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            None,
            dir.path(),
            "code",
        )
        .await
        .expect("first prepare");
    assert_eq!(hit1, Some(false), "first prepare must fill the cache");

    let (_baseline, hit2, _ms2) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            None,
            dir.path(),
            "code",
        )
        .await
        .expect("second prepare");
    assert_eq!(hit2, Some(true), "unchanged file-backed context must hit");

    let (_baseline, hit3, _ms3) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            Some("review"),
            dir.path(),
            "code",
        )
        .await
        .expect("target prepare");
    assert_eq!(
        hit3,
        Some(false),
        "target pipeline is part of baseline invalidation"
    );

    let (_baseline, hit4, _ms4) = server
        .prepare_vm_baseline_cached(
            source,
            Some(pipeline_path.as_path()),
            Some("review"),
            dir.path(),
            "plan",
        )
        .await
        .expect("mode prepare");
    assert_eq!(
        hit4,
        Some(false),
        "ACP mode is part of baseline invalidation"
    );

    let (baseline, hit5, ms5) = server
        .prepare_vm_baseline_cached(source, None, None, dir.path(), "code")
        .await
        .expect("inline prepare");
    assert!(baseline.is_none());
    assert_eq!(hit5, None);
    assert_eq!(ms5, 0);
}

#[test]
fn parse_oauth_redirect_url_extracts_code_state_issuer() {
    let (state, code, issuer) = parse_oauth_redirect_url(
        "burin://oauth/callback?code=auth-code&state=xyz&iss=https://auth.example",
    )
    .expect("parse");
    assert_eq!(state, "xyz");
    assert_eq!(code, "auth-code");
    assert_eq!(issuer.as_deref(), Some("https://auth.example"));
}

#[test]
fn parse_oauth_redirect_url_propagates_provider_error() {
    let error = parse_oauth_redirect_url(
        "http://127.0.0.1/cb?error=access_denied&error_description=nope&state=xyz",
    )
    .expect_err("error param");
    assert!(error.contains("access_denied"), "{error}");
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn parse_oauth_redirect_url_requires_code() {
    let error =
        parse_oauth_redirect_url("http://127.0.0.1/cb?state=xyz").expect_err("missing code");
    assert!(error.contains("code"), "{error}");
}

mod commands;
mod modes;
mod sessions;
