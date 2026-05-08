use super::builtins::normalize_host_capability_manifest;
use super::*;
use super::{
    acp_agent_capabilities, configured_llm_route_for_capabilities, sanitize_visible_assistant_text,
    AcpBridge, AcpOutput, AcpServer, AcpServerConfig, SessionCancellation, ACP_AUTH_REQUIRED_CODE,
    ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS, HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use crate::{ApiKeyAuthConfig, AuthMethodConfig, AuthPolicy};
use harn_vm::visible_text::VisibleTextState;
use harn_vm::VmValue;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    let _notification = recv_json(&mut response_rx).await;
    (request_tx, response_rx, server, session_id)
}

#[test]
fn normalize_host_capabilities_wraps_array_entries_in_ops_dicts() {
    let mut root = BTreeMap::new();
    root.insert(
        "project".to_string(),
        VmValue::List(Rc::new(vec![VmValue::String(Rc::from(
            "scope_test_command",
        ))])),
    );

    let normalized = normalize_host_capability_manifest(VmValue::Dict(Rc::new(root)));
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
        VmValue::Dict(Rc::new(BTreeMap::new())),
    );
    let mut process = BTreeMap::new();
    process.insert("operations".to_string(), VmValue::Dict(Rc::new(operations)));
    let mut root = BTreeMap::new();
    root.insert("process".to_string(), VmValue::Dict(Rc::new(process)));

    let normalized = normalize_host_capability_manifest(VmValue::Dict(Rc::new(root)));
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

    assert_eq!(
        configured_llm_route_for_capabilities(),
        ("openai".into(), "gpt-4o".into())
    );
    assert_eq!(capabilities["loadSession"], true);
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
            "list": {},
        })
    );
    assert!(
        capabilities["sessionCapabilities"].get("fork").is_none(),
        "Harn-only session/fork must not be advertised as an ACP SessionCapability"
    );
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

    assert_eq!(
        configured_llm_route_for_capabilities(),
        ("anthropic".into(), "claude-sonnet-4-20250514".into())
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

mod commands;
mod modes;
mod sessions;
