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
        VmValue::String(Rc::from(session_id.as_str())),
    );
    args.insert(
        "path".to_string(),
        VmValue::String(Rc::from(file.to_string_lossy().as_ref())),
    );
    args.insert("content".to_string(), VmValue::String(Rc::from("draft")));
    (registry
        .find("hostlib_tools_write_file")
        .expect("write_file builtin")
        .handler)(&[VmValue::Dict(Rc::new(args))])
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
            "resume": {},
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
    let initial = "pipeline main() { println(\"first\") }\n";
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
    let named_source = "@command(name: \"alpha\") pipeline alpha() { println(\"alpha\") }\n\
                       pipeline main() { println(\"main\") }\n";
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
    let source = "pipeline main() { println(\"inline\") }\n";
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
    let source = "pipeline main() { println(\"baseline\") }\n";
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

mod commands;
mod modes;
mod sessions;
