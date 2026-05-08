//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as an agent runtime accessible from any host application
//! (IDEs, CLI tools, web apps, etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.

mod builtins;
mod commands;
mod events;
mod execute;
mod io;
mod modes;

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harn_vm::agent_events::{clear_session_sinks, register_sink, AgentEventSink};
use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

use crate::{
    AdapterDescriptor, AuthMethodConfig, AuthPolicy, AuthRequest, AuthenticatedPrincipal,
    AuthorizationDecision,
};
use commands::{
    discover_commands, parse_slash_invocation, render_available_commands, DiscoveredCommand,
};
use events::AcpAgentEventSink;
use io::send_json_response;

pub(super) const ACP_SCHEMA_COMPATIBILITY: &str =
    "agentclientprotocol/agent-client-protocol schema v0.12.2";

pub(super) const HARN_SESSION_UPDATE_EXTENSIONS: &[&str] = &[
    "fs_watch",
    "handoff",
    "hitl_request",
    "hitl_resolved",
    "log",
    "progress",
    "skill_activated",
    "skill_deactivated",
    "skill_scope_tools",
    "tool_search_query",
    "tool_search_result",
    "transcript_compacted",
    "worker_update",
];

/// JSON-RPC method name for the ACP `ExtNotification` envelope that
/// carries Harn pipeline-loop milestones. The leading `_` puts it in
/// the ACP-reserved extension namespace, so strict clients that don't
/// know the method MUST ignore it gracefully (per the ACP
/// extensibility spec). Callers should never hardcode the literal —
/// reference this constant so a future rename ripples through the
/// adapter, fixtures, tests, and capability advertisement together.
pub(super) const HARN_AGENT_EVENT_METHOD: &str = "_harn/agentEvent";

/// Pipeline-loop milestone kinds the adapter currently emits via
/// `_harn/agentEvent`. The list is stable wire vocabulary — adding a
/// new kind is additive and SHOULD be treated by clients as
/// "unknown kind, ignore." Keep it sorted for diff-friendliness and
/// keep it in lockstep with the match arm in `events.rs`.
pub(super) const HARN_AGENT_EVENT_KINDS: &[&str] = &[
    "budget_exhausted",
    "daemon_watchdog_tripped",
    "feedback_injected",
    "judge_decision",
    "loop_stuck",
    "turn_end",
    "turn_start",
];

pub(super) const HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS: &[&str] = &[
    "audit",
    "durationMs",
    "error",
    "errorCategory",
    "executionDurationMs",
    "executor",
    "parsing",
    "rawInputPartial",
];

pub(super) const HARN_CONTENT_EXTENSION_FIELDS: &[&str] = &["visible_delta", "visible_text"];
const ACP_AUTH_REQUIRED_CODE: i64 = -32000;

fn harn_acp_extension_meta() -> serde_json::Value {
    serde_json::json!({
        "harn": {
            "schemaCompatibility": ACP_SCHEMA_COMPATIBILITY,
            "sessionUpdateExtensions": HARN_SESSION_UPDATE_EXTENSIONS,
            "toolLifecycleExtensionFields": HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
            "contentExtensionFields": HARN_CONTENT_EXTENSION_FIELDS,
            // ACP `ExtNotification` methods this server emits beyond the
            // canonical `session/update` stream. Clients that recognize
            // the method consume the payload; clients that don't MUST
            // ignore it (per ACP extensibility spec). Keys are method
            // names; values are static descriptors so a client can
            // version-check before subscribing.
            "extensionMethods": {
                HARN_AGENT_EVENT_METHOD: {
                    "description": "Pipeline-loop milestones (turn boundaries, \
                                    feedback injections, budget exhaustion, \
                                    loop-stuck, daemon watchdog) that have no \
                                    canonical ACP session/update mapping.",
                    "kinds": HARN_AGENT_EVENT_KINDS,
                    "schema": "https://harnlang.com/spec/harn-extensions/agent-event/v1",
                },
            },
            "hostCapabilityOperations": {
                "process": [
                    "exec",
                    "list_shells",
                    "get_default_shell",
                    "set_default_shell",
                    "shell_invocation"
                ]
            },
            "extensionContract": "https://harnlang.com/spec/harn-extensions/v1",
        }
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn configured_llm_route_for_capabilities() -> (String, String) {
    let provider = non_empty_env("HARN_LLM_PROVIDER")
        .filter(|provider| !provider.eq_ignore_ascii_case("auto"))
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
                && (non_empty_env("HARN_LLM_MODEL").is_some()
                    || non_empty_env("LOCAL_LLM_MODEL").is_some())
            {
                Some("local".to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            non_empty_env("HARN_LLM_MODEL").map(|model| {
                let resolved = harn_vm::llm_config::resolve_model_info(&model);
                resolved.provider
            })
        })
        .unwrap_or_else(harn_vm::llm_config::default_provider);

    let raw_model = non_empty_env("HARN_LLM_MODEL").or_else(|| {
        if provider == "local" {
            non_empty_env("LOCAL_LLM_MODEL")
        } else {
            None
        }
    });
    let model = raw_model
        .map(|model| harn_vm::llm_config::resolve_model(&model).0)
        .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider(&provider));

    (provider, model)
}

fn acp_prompt_capabilities() -> serde_json::Value {
    let (provider, model) = configured_llm_route_for_capabilities();
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &model);
    serde_json::json!({
        "image": capabilities.vision || capabilities.vision_supported,
        "audio": capabilities.audio,
        "embeddedContext": capabilities.pdf || capabilities.files_api_supported,
    })
}

fn acp_agent_capabilities() -> serde_json::Value {
    serde_json::json!({
        "_meta": harn_acp_extension_meta(),
        "loadSession": true,
        "promptCapabilities": acp_prompt_capabilities(),
        "mcpCapabilities": {
            "http": true,
            "sse": true,
        },
        "sessionCapabilities": {
            "list": {},
        },
    })
}

fn verbose_bridge_logs_enabled() -> bool {
    matches!(
        std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    ) || matches!(
        std::env::var("HARN_ACP_TRACE_CALLS").ok().as_deref(),
        Some("1")
    )
}

fn host_call_timeout(method: &str) -> std::time::Duration {
    let configured = std::env::var("HARN_HOST_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    if let Some(seconds) = configured {
        return std::time::Duration::from_secs(seconds);
    }
    if method == "host/call" {
        return std::time::Duration::from_secs(300);
    }
    std::time::Duration::from_secs(60)
}

fn suppress_default_info_log(message: &str) -> bool {
    if verbose_bridge_logs_enabled() {
        return false;
    }
    [
        "ACP_BOOT:",
        "span_end ",
        "WORKFLOW_POLICY:",
        "HINTS:",
        "AGENT_CONTEXT:",
        "SIBLING_OUTLINES:",
        "PROVIDERS: count=",
        "AUTO: base context start",
        "AUTO: base context done",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

#[derive(Clone, Default)]
struct SessionInfo {
    title: Option<String>,
    meta: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone)]
pub(super) struct SessionCancellation {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) notify: Arc<Notify>,
    /// Set by the transport reader after it resets cancellation for a
    /// prompt, so the prompt handler does not erase a cancel notification
    /// that arrived while the prompt was queued.
    prepared_prompt: Arc<AtomicBool>,
}

impl Default for SessionCancellation {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            prepared_prompt: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SessionCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }

    fn prepare_prompt(&self) {
        self.reset();
        self.prepared_prompt.store(true, Ordering::SeqCst);
    }

    fn begin_prompt(&self) {
        if !self.prepared_prompt.swap(false, Ordering::SeqCst) {
            self.reset();
        }
    }
}

struct Session {
    cwd: PathBuf,
    /// If a cancel was requested for the current prompt execution.
    cancellation: SessionCancellation,
    /// Active host bridge for queued input / daemon resume while a prompt runs.
    host_bridge: Option<Rc<harn_vm::bridge::HostBridge>>,
    info: SessionInfo,
    /// Snapshot of slash-commands most recently advertised over
    /// `available_commands_update` for this session, used to skip re-emits
    /// when the underlying pipeline source hasn't changed.
    advertised_commands: Vec<DiscoveredCommand>,
    /// Active session mode id (one of [`modes::MODE_CATALOG`]). Drives
    /// the capability ceiling pushed for the next `session/prompt`.
    current_mode_id: String,
}

#[async_trait(?Send)]
pub trait AcpRuntimeConfigurator: Send + Sync {
    async fn configure(
        &self,
        _vm: &mut harn_vm::Vm,
        _source_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NoopAcpRuntimeConfigurator;

#[async_trait(?Send)]
impl AcpRuntimeConfigurator for NoopAcpRuntimeConfigurator {}

#[derive(Clone)]
pub struct AcpServerConfig {
    pub pipeline: Option<String>,
    pub auth_policy: AuthPolicy,
    pub runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    pub llm_config_overrides: Option<harn_vm::llm_config::ProvidersConfig>,
    pub llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
}

impl AcpServerConfig {
    pub fn new(pipeline: Option<String>) -> Self {
        Self {
            pipeline,
            auth_policy: AuthPolicy::allow_all(),
            runtime_configurator: Arc::new(NoopAcpRuntimeConfigurator),
            llm_config_overrides: None,
            llm_capability_overrides: None,
        }
    }

    pub fn for_pipeline(path: impl Into<String>) -> Self {
        Self::new(Some(path.into()))
    }

    pub fn with_runtime_configurator(
        mut self,
        runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    ) -> Self {
        self.runtime_configurator = runtime_configurator;
        self
    }

    pub fn with_auth_policy(mut self, auth_policy: AuthPolicy) -> Self {
        self.auth_policy = auth_policy;
        self
    }

    pub fn with_llm_overrides(
        mut self,
        llm_config: Option<harn_vm::llm_config::ProvidersConfig>,
        llm_capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    ) -> Self {
        self.llm_config_overrides = llm_config;
        self.llm_capability_overrides = llm_capabilities;
        self
    }
}

#[derive(Clone)]
pub(super) enum AcpOutput {
    Stdout(Arc<std::sync::Mutex<()>>),
    Channel(mpsc::UnboundedSender<String>),
}

impl AcpOutput {
    fn stdout() -> Self {
        Self::Stdout(Arc::new(std::sync::Mutex::new(())))
    }

    pub(super) fn write_line(&self, line: &str) {
        match self {
            Self::Stdout(lock) => {
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.write_all(b"\n");
                let _ = stdout.flush();
            }
            Self::Channel(tx) => {
                let _ = tx.send(line.to_string());
            }
        }
    }
}

fn mark_cancelled_session(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    params: &serde_json::Value,
) -> bool {
    let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
        return false;
    };
    let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) else {
        return false;
    };
    cancellation.cancel();
    true
}

fn lookup_session_cancellation(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    session_id: &str,
) -> Option<SessionCancellation> {
    cancellations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned()
}

fn preempt_session_cancel(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    msg: &serde_json::Value,
) -> bool {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/cancel") {
        return false;
    }
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    mark_cancelled_session(cancellations, params);
    msg.get("id").is_none()
}

fn prepare_session_prompt(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    msg: &serde_json::Value,
) {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/prompt") {
        return;
    }
    let Some(session_id) = msg
        .get("params")
        .and_then(|params| params.get("sessionId"))
        .and_then(|value| value.as_str())
    else {
        return;
    };
    if let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) {
        cancellation.prepare_prompt();
    }
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedAcpPrompt {
    text: String,
    content: Vec<serde_json::Value>,
    messages: Vec<serde_json::Value>,
}

fn normalize_acp_prompt(params: &serde_json::Value) -> Result<NormalizedAcpPrompt, String> {
    let Some(prompt) = params.get("prompt") else {
        return Ok(NormalizedAcpPrompt {
            text: String::new(),
            content: Vec::new(),
            messages: prompt_messages_for_content(&[]),
        });
    };
    let blocks = prompt.as_array().ok_or_else(|| {
        "session/prompt: prompt must be an array of ACP content blocks".to_string()
    })?;

    let mut content = Vec::new();
    for block in blocks {
        content.push(normalize_acp_prompt_block(block)?);
    }

    let text = prompt_text_from_content(&content);
    let messages = prompt_messages_for_content(&content);
    Ok(NormalizedAcpPrompt {
        text,
        content,
        messages,
    })
}

fn normalize_acp_prompt_block(block: &serde_json::Value) -> Result<serde_json::Value, String> {
    match block.get("type").and_then(|value| value.as_str()) {
        Some("text") => Ok(serde_json::json!({
            "type": "text",
            "text": required_string(block, "text", "text prompt block")?,
        })),
        Some("image") => normalize_binary_prompt_block(block, "image"),
        Some("audio") => normalize_binary_prompt_block(block, "audio"),
        Some("resource") => normalize_embedded_resource_block(block),
        Some("resource_link") => normalize_resource_link_block(block),
        Some(other) => Err(format!(
            "session/prompt: unsupported content block type `{other}`"
        )),
        None => Err("session/prompt: content block is missing required `type`".to_string()),
    }
}

fn normalize_binary_prompt_block(
    block: &serde_json::Value,
    block_type: &str,
) -> Result<serde_json::Value, String> {
    let media_type = required_media_type(block, block_type)?;
    let data = block
        .get("data")
        .or_else(|| block.get("base64"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let uri = block
        .get("uri")
        .or_else(|| block.get("url"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());

    let mut normalized = serde_json::json!({
        "type": block_type,
        "media_type": media_type,
    });
    if let Some(data) = data {
        normalized["base64"] = serde_json::json!(data);
        if let Some(uri) = uri {
            normalized["source_uri"] = serde_json::json!(uri);
        }
    } else if let Some(uri) = uri {
        normalized["url"] = serde_json::json!(uri);
    } else {
        return Err(format!(
            "session/prompt: {block_type} block requires `data` or `uri`"
        ));
    }
    if block_type == "image" {
        if let Some(detail) = block.get("detail").and_then(|value| value.as_str()) {
            normalized["detail"] = serde_json::json!(detail);
        }
    }
    Ok(normalized)
}

fn normalize_embedded_resource_block(
    block: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let resource = block
        .get("resource")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "session/prompt: resource block requires `resource` object".to_string())?;
    let uri = resource
        .get("uri")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "session/prompt: embedded resource requires `uri`".to_string())?;
    let media_type = resource
        .get("mimeType")
        .or_else(|| resource.get("media_type"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());

    if let Some(text) = resource.get("text").and_then(|value| value.as_str()) {
        return Ok(serde_json::json!({
            "type": "text",
            "text": render_embedded_text_resource(uri, media_type, text),
            "uri": uri,
            "media_type": media_type,
        }));
    }

    let blob = resource
        .get("blob")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "session/prompt: embedded resource requires `text` or `blob`".to_string())?;
    let Some(media_type) = media_type else {
        return Ok(serde_json::json!({
            "type": "text",
            "text": format!("Embedded binary resource: {uri}\nMIME type: unknown"),
            "uri": uri,
        }));
    };
    if media_type.starts_with("image/") {
        Ok(serde_json::json!({
            "type": "image",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else if media_type.starts_with("audio/") {
        Ok(serde_json::json!({
            "type": "audio",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else if media_type == "application/pdf" {
        Ok(serde_json::json!({
            "type": "pdf",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else {
        Ok(serde_json::json!({
            "type": "text",
            "text": format!("Embedded binary resource: {uri}\nMIME type: {media_type}"),
            "uri": uri,
            "media_type": media_type,
        }))
    }
}

fn normalize_resource_link_block(block: &serde_json::Value) -> Result<serde_json::Value, String> {
    let uri = required_string(block, "uri", "resource_link prompt block")?;
    let mut lines = vec![format!("Resource link: {uri}")];
    for key in ["name", "title", "description", "mimeType", "media_type"] {
        if let Some(value) = block.get(key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                lines.push(format!("{key}: {value}"));
            }
        }
    }
    if let Some(size) = block.get("size").and_then(|value| value.as_u64()) {
        lines.push(format!("size: {size}"));
    }
    Ok(serde_json::json!({
        "type": "text",
        "text": lines.join("\n"),
        "uri": uri,
    }))
}

fn render_embedded_text_resource(uri: &str, media_type: Option<&str>, text: &str) -> String {
    let mut rendered = format!("Embedded resource: {uri}");
    if let Some(media_type) = media_type {
        rendered.push_str(&format!("\nMIME type: {media_type}"));
    }
    rendered.push_str("\n\n");
    rendered.push_str(text);
    rendered
}

fn required_media_type(block: &serde_json::Value, block_type: &str) -> Result<String, String> {
    block
        .get("mimeType")
        .or_else(|| block.get("media_type"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session/prompt: {block_type} block requires `mimeType`"))
}

fn required_string(value: &serde_json::Value, key: &str, context: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session/prompt: {context} requires `{key}`"))
}

fn retarget_prompt_text(prompt: &mut NormalizedAcpPrompt, text: String) {
    if let Some(block) = prompt
        .content
        .iter_mut()
        .find(|block| block.get("type").and_then(|value| value.as_str()) == Some("text"))
    {
        block["text"] = serde_json::json!(text);
    } else {
        prompt.content.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": text,
            }),
        );
    }
    prompt.text = prompt_text_from_content(&prompt.content);
    prompt.messages = prompt_messages_for_content(&prompt.content);
}

fn prompt_text_from_content(content: &[serde_json::Value]) -> String {
    content
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                block.get("text").and_then(|value| value.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt_messages_for_content(content: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let message_content = if content.is_empty() {
        serde_json::Value::String(String::new())
    } else {
        serde_json::Value::Array(content.to_vec())
    };
    vec![serde_json::json!({
        "role": "user",
        "content": message_content,
    })]
}

fn harn_auth_meta(
    params: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    params
        .get("_meta")
        .and_then(|value| value.get("harn"))
        .and_then(|value| value.as_object())
}

fn harn_auth_string<'a>(
    meta: &'a serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| meta.get(*field).and_then(|value| value.as_str()))
        .or_else(|| {
            let credentials = meta.get("credentials")?.as_object()?;
            fields
                .iter()
                .find_map(|field| credentials.get(*field).and_then(|value| value.as_str()))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn harn_auth_headers(
    meta: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, String> {
    let Some(headers) = meta.get("headers").and_then(|value| value.as_object()) else {
        return BTreeMap::new();
    };
    headers
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect()
}

fn acp_auth_request_for_method(
    method: &AuthMethodConfig,
    params: &serde_json::Value,
) -> Result<AuthRequest, String> {
    let meta = harn_auth_meta(params).ok_or_else(|| {
        "authenticate requires `_meta.harn` credentials for Harn auth policies".to_string()
    })?;
    let mut request = AuthRequest {
        method: harn_auth_string(meta, &["method"])
            .unwrap_or("ACP")
            .to_string(),
        path: harn_auth_string(meta, &["path"])
            .unwrap_or("authenticate")
            .to_string(),
        body: harn_auth_string(meta, &["body"])
            .map(|value| value.as_bytes().to_vec())
            .unwrap_or_default(),
        headers: harn_auth_headers(meta),
        validated_oauth: None,
    };

    match method {
        AuthMethodConfig::ApiKey(_) => {
            if request.headers.is_empty() {
                let api_key =
                    harn_auth_string(meta, &["apiKey", "api_key", "token", "bearerToken"])
                        .ok_or_else(|| {
                            "authenticate requires an API key in `_meta.harn.apiKey`".to_string()
                        })?;
                request
                    .headers
                    .insert("x-api-key".to_string(), api_key.to_string());
            }
        }
        AuthMethodConfig::Hmac(_) => {
            if request.headers.is_empty() {
                return Err(
                    "authenticate requires HMAC headers in `_meta.harn.headers`".to_string()
                );
            }
        }
        AuthMethodConfig::OAuth21(_) => {
            return Err(
                "OAuth ACP authentication requires transport-validated bearer claims".to_string(),
            );
        }
    }

    Ok(request)
}

/// ACP server that reads JSON-RPC requests from a transport and writes
/// responses / notifications back to that same transport.
pub struct AcpServer {
    descriptor: AdapterDescriptor,
    /// Optional pipeline file to execute on each `session/prompt`.
    pipeline: Option<String>,
    /// Shared harn-serve auth policy for adapter entrypoints.
    auth_policy: AuthPolicy,
    /// Principal authenticated through ACP's connection-level `authenticate` method.
    authenticated_principal: Option<AuthenticatedPrincipal>,
    /// CLI/project hook used to install package-provided runtime extensions.
    runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    /// Active sessions keyed by session ID.
    sessions: HashMap<String, Session>,
    /// Monotonically increasing JSON-RPC request ID for outgoing requests.
    next_id: AtomicU64,
    /// Pending outgoing request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Cancel flags are shared with the transport reader so
    /// `session/cancel` can interrupt a blocked `session/prompt` turn.
    session_cancellations: Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    /// Transport output sink.
    output: AcpOutput,
}

impl AcpServer {
    pub fn new(config: AcpServerConfig) -> Self {
        Self::new_with_output(config, AcpOutput::stdout())
    }

    fn new_with_output(config: AcpServerConfig, output: AcpOutput) -> Self {
        harn_vm::llm_config::set_user_overrides(config.llm_config_overrides.clone());
        harn_vm::llm::capabilities::set_user_overrides(config.llm_capability_overrides.clone());

        Self {
            descriptor: AdapterDescriptor {
                id: "acp".to_string(),
                caller_shape: "agent-session".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            pipeline: config.pipeline,
            auth_policy: config.auth_policy,
            authenticated_principal: None,
            runtime_configurator: config.runtime_configurator,
            sessions: HashMap::new(),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            output,
        }
    }

    /// Write a complete JSON-RPC message to the current transport.
    fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC success response.
    fn send_response(&self, id: &serde_json::Value, result: serde_json::Value) {
        let response = harn_vm::jsonrpc::response(id.clone(), result);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC error response.
    fn send_error(&self, id: &serde_json::Value, code: i64, message: &str) {
        let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    fn send_error_with_data(
        &self,
        id: &serde_json::Value,
        code: i64,
        message: &str,
        data: serde_json::Value,
    ) {
        let response = harn_vm::jsonrpc::error_response_with_data(id.clone(), code, message, data);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    #[allow(dead_code)]
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` notification with an agent message chunk.
    #[allow(dead_code)]
    fn send_update(&self, session_id: &str, text: &str) {
        let visible_text = sanitize_visible_assistant_text(text, true);
        let mut content = serde_json::json!({
            "type": "text",
            "text": text,
        });
        let mut content_meta = serde_json::Map::new();
        content_meta.insert(
            "visible_text".to_string(),
            serde_json::Value::String(visible_text.clone()),
        );
        content_meta.insert(
            "visible_delta".to_string(),
            serde_json::Value::String(visible_text),
        );
        events::merge_harn_meta(&mut content, content_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": content,
                },
            }),
        );
    }

    fn send_prompt_error(&self, session_id: &str, id: &serde_json::Value, message: &str) {
        self.send_update(session_id, &format!("Error: {message}\n"));
        self.send_error(id, -32000, message);
        eprintln!("{message}");
    }

    /// Generate a unique session ID.
    fn next_session_id(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn register_session_cancellation(&mut self, session_id: &str) -> SessionCancellation {
        let cancellation = SessionCancellation::default();
        self.session_cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), cancellation.clone());
        cancellation
    }

    fn handle_initialize(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": acp_agent_capabilities(),
                "agentInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "authMethods": self.auth_policy.acp_auth_methods(),
            }),
        );
    }

    fn auth_required_data(&self) -> serde_json::Value {
        serde_json::json!({
            "authMethods": self.auth_policy.acp_auth_methods(),
        })
    }

    fn send_auth_required(&self, id: &serde_json::Value) {
        self.send_error_with_data(
            id,
            ACP_AUTH_REQUIRED_CODE,
            "auth_required",
            self.auth_required_data(),
        );
    }

    fn requires_authentication(&self) -> bool {
        !self.auth_policy.methods.is_empty() && self.authenticated_principal.is_none()
    }

    fn reject_unauthenticated(&self, id: &serde_json::Value) -> bool {
        if !self.requires_authentication() {
            return false;
        }
        if !id.is_null() {
            self.send_auth_required(id);
        }
        true
    }

    async fn handle_authenticate(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let method_id = match params.get("methodId").and_then(|value| value.as_str()) {
            Some(method_id) => method_id,
            None => {
                self.send_error(id, -32602, "authenticate requires methodId");
                return;
            }
        };
        let Some(method) = self.auth_policy.method_by_acp_id(method_id) else {
            self.send_error_with_data(
                id,
                -32602,
                "authenticate methodId was not advertised",
                self.auth_required_data(),
            );
            return;
        };
        let auth = match acp_auth_request_for_method(method, params) {
            Ok(auth) => auth,
            Err(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
                return;
            }
        };
        match self.auth_policy.authorize(&auth).await {
            AuthorizationDecision::Authorized(principal) => {
                self.authenticated_principal = Some(principal.clone());
                self.send_response(
                    id,
                    serde_json::json!({
                        "_meta": {
                            "harn": {
                                "authenticated": true,
                                "principal": {
                                    "subject": principal.subject,
                                    "scheme": principal.scheme,
                                }
                            }
                        }
                    }),
                );
            }
            AuthorizationDecision::Rejected(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
            }
        }
    }

    pub fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }

    fn insert_session(&mut self, session_id: String, cwd: PathBuf, info: SessionInfo) {
        let cancellation = self.register_session_cancellation(&session_id);
        self.sessions.insert(
            session_id.clone(),
            Session {
                cwd,
                cancellation,
                host_bridge: None,
                info,
                advertised_commands: Vec::new(),
                current_mode_id: modes::DEFAULT_MODE_ID.to_string(),
            },
        );
        harn_vm::agent_sessions::open_or_create(Some(session_id));
    }

    fn handle_session_new(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let session_id = self.next_session_id();
        self.insert_session(session_id.clone(), cwd, SessionInfo::default());

        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "modes": modes::session_mode_state(modes::DEFAULT_MODE_ID),
                "configOptions": modes::config_options_state(modes::DEFAULT_MODE_ID),
            }),
        );

        self.emit_available_commands(&session_id);
    }

    /// Read the configured pipeline source for `session_id`. Returns
    /// `None` for inline-prompt sessions (no `--pipeline`) and on read
    /// error — the regular prompt path will surface the error to the
    /// client at execution time.
    fn read_pipeline_source(&self, session_id: &str) -> Option<String> {
        let pipeline_path = self.pipeline.as_deref()?;
        let cwd = &self.sessions.get(session_id)?.cwd;
        let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
            PathBuf::from(pipeline_path)
        } else {
            cwd.join(pipeline_path)
        };
        std::fs::read_to_string(&full_path).ok()
    }

    /// Discover and emit `available_commands_update` if the command set
    /// has changed since the last emission for this session.
    fn emit_available_commands(&mut self, session_id: &str) {
        let Some(source) = self.read_pipeline_source(session_id) else {
            return;
        };
        self.refresh_advertised_commands(session_id, &source);
    }

    /// Hot-reload variant of [`Self::emit_available_commands`] that uses
    /// pre-loaded source instead of re-reading from disk. Driven from
    /// `handle_session_prompt` on every prompt so editor changes between
    /// prompts propagate to the client without a restart.
    fn refresh_advertised_commands(&mut self, session_id: &str, source: &str) {
        let commands = discover_commands(source);
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        if session.advertised_commands == commands {
            return;
        }
        session.advertised_commands = commands.clone();
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": render_available_commands(&commands),
                },
            }),
        );
    }

    fn emit_session_info_update(&self, session_id: &str, info: &SessionInfo) {
        let mut update = serde_json::json!({
            "sessionUpdate": "session_info_update",
        });
        if let Some(title) = &info.title {
            update["title"] = serde_json::json!(title);
        }
        if !info.meta.is_empty() {
            update["_meta"] = serde_json::Value::Object(info.meta.clone());
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }

    fn handle_session_fork(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let src_id = params
            .get("session_id")
            .or_else(|| params.get("sessionId"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let Some(src_id) = src_id else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        let Some(src_cwd) = self
            .sessions
            .get(&src_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {src_id}"));
            return;
        };

        if !harn_vm::agent_sessions::exists(&src_id) {
            harn_vm::agent_sessions::open_or_create(Some(src_id.clone()));
        }

        let keep_first = match params.get("keep_first").and_then(|value| value.as_i64()) {
            Some(value) if value < 0 => {
                self.send_error(id, -32602, "Invalid keep_first: must be >= 0");
                return;
            }
            Some(value) => Some(value as usize),
            None => None,
        };
        let dst_id = params
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if let Some(dst_id) = dst_id.as_deref() {
            if self.sessions.contains_key(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
            if harn_vm::agent_sessions::exists(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
        }
        let branch_name = params
            .get("branch_name")
            .and_then(|value| value.as_str())
            .map(str::to_string);

        let new_session_id = match keep_first {
            Some(keep_first) => harn_vm::agent_sessions::fork_at(&src_id, keep_first, dst_id),
            None => harn_vm::agent_sessions::fork(&src_id, dst_id),
        };
        let Some(new_session_id) = new_session_id else {
            self.send_error(id, -32000, &format!("Failed to fork session: {src_id}"));
            return;
        };

        let snapshot = harn_vm::agent_sessions::snapshot(&new_session_id)
            .and_then(|value| serde_json::to_value(harn_vm::llm::vm_value_to_json(&value)).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let branched_at = snapshot
            .get("branched_at_event_index")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut meta = serde_json::Map::new();
        meta.insert("state".to_string(), serde_json::json!("forked"));
        meta.insert("parent_id".to_string(), serde_json::json!(src_id.clone()));
        meta.insert("branched_at".to_string(), branched_at.clone());
        if let Some(branch_name) = &branch_name {
            meta.insert("branch_name".to_string(), serde_json::json!(branch_name));
        }
        let info = SessionInfo {
            title: branch_name,
            meta,
        };

        let parent_mode_id = self
            .sessions
            .get(&src_id)
            .map(|session| session.current_mode_id.clone())
            .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
        let cancellation = self.register_session_cancellation(&new_session_id);
        self.sessions.insert(
            new_session_id.clone(),
            Session {
                cwd: src_cwd,
                cancellation,
                host_bridge: None,
                info: info.clone(),
                advertised_commands: Vec::new(),
                current_mode_id: parent_mode_id.clone(),
            },
        );
        self.emit_session_info_update(&new_session_id, &info);
        self.emit_available_commands(&new_session_id);
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": new_session_id,
                "state": "forked",
                "parent_id": src_id,
                "branched_at": branched_at,
                "modes": modes::session_mode_state(&parent_mode_id),
                "configOptions": modes::config_options_state(&parent_mode_id),
            }),
        );
    }

    async fn handle_session_prompt(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                self.send_error(id, -32602, "Missing sessionId");
                return;
            }
        };

        let prompt = match normalize_acp_prompt(params) {
            Ok(prompt) => prompt,
            Err(message) => {
                self.send_prompt_error(&session_id, id, &message);
                return;
            }
        };
        let prompt_text = prompt.text.clone();

        let (cwd, cancellation, current_mode_id) = match self.sessions.get_mut(&session_id) {
            Some(s) => {
                s.cancellation.begin_prompt();
                s.host_bridge = None;
                (
                    s.cwd.clone(),
                    s.cancellation.clone(),
                    s.current_mode_id.clone(),
                )
            }
            None => {
                self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
                return;
            }
        };
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());

        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
                PathBuf::from(pipeline_path)
            } else {
                cwd.join(pipeline_path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => (src, Some(full_path)),
                Err(e) => {
                    let message = format!("Failed to read pipeline {}: {e}", full_path.display());
                    self.send_prompt_error(&session_id, id, &message);
                    return;
                }
            }
        } else {
            // Inline-prompt mode has no persistent pipeline source to host
            // `@command`-tagged decls, so a leading slash invocation can
            // only be the user expecting to invoke an advertised command
            // that doesn't exist. Surface a friendly error instead of
            // wrapping `/foo args` into `pipeline main() { /foo args }`,
            // which would fail with a generic "Compilation error" later.
            if parse_slash_invocation(&prompt_text).is_some() {
                self.send_prompt_error(
                    &session_id,
                    id,
                    "Slash commands require `--pipeline <file>`; the agent is running in inline mode.",
                );
                return;
            }
            // Wrap inline prompt source in a pipeline so the compiler has
            // an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        // Hot-reload: re-discover slash-commands from the just-loaded
        // source and emit `available_commands_update` if the set changed
        // since the last advertise. Only meaningful when a pipeline file
        // is configured; inline prompts have no persistent surface to
        // attach commands to.
        if source_path.is_some() {
            self.refresh_advertised_commands(&session_id, &source);
        }

        // Slash-command dispatch: if the prompt begins with `/<name>` and
        // `<name>` matches an advertised command, route to the named
        // pipeline with the post-name text as the new `prompt`. Unknown
        // slashes fall through unmodified — the default pipeline can
        // choose to treat them as text or surface its own diagnostic.
        let (effective_prompt, target_pipeline) = match parse_slash_invocation(&prompt_text) {
            Some((cmd_name, args)) => {
                let pipeline_name = self.sessions.get(&session_id).and_then(|session| {
                    session
                        .advertised_commands
                        .iter()
                        .find(|c| c.name == cmd_name)
                        .map(|c| c.pipeline_name.clone())
                });
                match pipeline_name {
                    Some(name) => (args.to_string(), Some(name)),
                    None => (prompt_text.clone(), None),
                }
            }
            None => (prompt_text.clone(), None),
        };
        let prompt_text = effective_prompt;
        let mut prompt = prompt;
        if prompt_text != prompt.text {
            retarget_prompt_text(&mut prompt, prompt_text.clone());
        }

        let output = self.output.clone();
        let pending = self.pending.clone();
        let next_id = &self.next_id;
        let sid = session_id.clone();

        // Translate AgentEvents into ACP session/update notifications so
        // the client observes tool lifecycle on the wire. The event-log
        // sink is reinstalled here because prompt teardown clears all
        // per-session transport sinks after each turn.
        clear_session_sinks(&session_id);
        harn_vm::agent_sessions::register_event_log_sink(&session_id);
        register_sink(
            session_id.clone(),
            Arc::new(AcpAgentEventSink::new(output.clone())),
        );

        let bridge = Rc::new(AcpBridge {
            session_id: sid.clone(),
            output: output.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancellation: cancellation.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let bridge_output = output.clone();
        let host_bridge = Rc::new(harn_vm::bridge::HostBridge::from_parts_with_writer(
            bridge.pending.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                bridge_output.write_line(line);
                Ok(())
            }),
            bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
        ));
        host_bridge.set_session_id(&bridge.session_id);

        let compile_started = Instant::now();
        let chunk = match target_pipeline.as_deref() {
            Some(name) => harn_vm::compile_source_named(&source, name),
            None => harn_vm::compile_source(&source),
        };
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                self.send_prompt_error(&session_id, id, &format!("Compilation error: {e}"));
                return;
            }
        };
        let compile_ms = compile_started.elapsed().as_millis() as u64;
        bridge.send_log(
            "info",
            &format!("ACP_BOOT: compile_ms={compile_ms}"),
            Some(serde_json::json!({
                "compile_ms": compile_ms,
                "pipeline": source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<inline>".to_string()),
            })),
        );
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = Some(host_bridge.clone());
        }

        let id_owned = id.clone();
        let send_output = self.output.clone();
        let _mode_guard = modes::ModePolicyGuard::enter(&current_mode_id);
        let host_bridge_for_response = host_bridge.clone();
        let result = execute::execute_chunk(
            chunk,
            bridge.clone(),
            host_bridge,
            execute::PromptGlobals {
                text: &prompt_text,
                content: &prompt.content,
                messages: &prompt.messages,
            },
            source_path.as_deref(),
            &cwd,
            self.runtime_configurator.clone(),
        )
        .await;
        drop(_mode_guard);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = None;
        }

        // Unregister so a session reusing this id can't receive stale
        // events routed to a dropped stdout lock.
        clear_session_sinks(&session_id);

        match result {
            Ok(output) => {
                if !output.is_empty() {
                    bridge.send_update(&output);
                }
                let stop_reason = if cancellation.cancelled.load(Ordering::SeqCst) {
                    "cancelled".to_string()
                } else {
                    host_bridge_for_response
                        .take_prompt_stop_reason()
                        .unwrap_or_else(|| "end_turn".to_string())
                };
                send_json_response(
                    &send_output,
                    &id_owned,
                    serde_json::json!({"stopReason": stop_reason}),
                );
            }
            Err(e) => {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_output,
                        &id_owned,
                        serde_json::json!({"stopReason": "cancelled"}),
                    );
                } else {
                    self.send_prompt_error(&sid, &id_owned, &e);
                }
            }
        }
    }

    fn handle_session_cancel(&mut self, params: &serde_json::Value) {
        mark_cancelled_session(&self.session_cancellations, params);
    }

    async fn handle_session_input(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        let Some(content) = params.get("content").and_then(|v| v.as_str()) else {
            return;
        };
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("wait_for_completion");
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge
                .push_queued_user_message(content.to_string(), mode)
                .await;
        }
    }

    fn handle_agent_resume(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge.signal_resume();
        }
    }

    fn handle_session_list(&self, id: &serde_json::Value) {
        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .map(|(sid, session)| {
                let mut item = serde_json::json!({
                    "sessionId": sid,
                    "cwd": session.cwd,
                });
                if let Some(title) = &session.info.title {
                    item["title"] = serde_json::json!(title);
                }
                if !session.info.meta.is_empty() {
                    item["_meta"] = serde_json::Value::Object(session.info.meta.clone());
                }
                item
            })
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }

    async fn handle_hitl_respond(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_cwd = params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| session.cwd.as_path());
        let fallback_cwd = self
            .sessions
            .values()
            .next()
            .map(|session| session.cwd.as_path());
        let cwd = session_cwd.or(fallback_cwd);
        let response: harn_vm::HitlHostResponse = match serde_json::from_value(params.clone()) {
            Ok(response) => response,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid harn.hitl.respond params: {error}"),
                );
                return;
            }
        };
        match harn_vm::append_hitl_response(cwd, response).await {
            Ok(_) => self.send_response(id, serde_json::json!({"ok": true})),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn workflow_base_dir_for<'a>(&'a self, params: &'a serde_json::Value) -> Option<&'a PathBuf> {
        params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| &session.cwd)
            .or_else(|| self.sessions.values().next().map(|session| &session.cwd))
    }

    async fn handle_workflow_signal(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/signal: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match harn_vm::workflow_signal_for_base(base_dir, workflow_id, name, payload) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_query(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/query: no session cwd available");
            return;
        };
        match harn_vm::workflow_query_for_base(base_dir, workflow_id, name) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    async fn handle_workflow_update(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/update: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let timeout_ms = params
            .get("timeoutMs")
            .and_then(|value| value.as_u64())
            .unwrap_or(30_000);
        match harn_vm::workflow_update_for_base(
            base_dir,
            workflow_id,
            name,
            payload,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
        {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_pause(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/pause: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/pause: no session cwd available");
            return;
        };
        match harn_vm::workflow_pause_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/resume: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/resume: no session cwd available");
            return;
        };
        match harn_vm::workflow_resume_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    async fn handle_session_load(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/load requires sessionId");
            return;
        };

        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return;
        };

        let mut session_value = serde_json::json!({
            "sessionId": session_id,
            "cwd": session.cwd.display().to_string(),
        });
        if let Some(title) = session.info.title.as_ref() {
            session_value["title"] = serde_json::json!(title);
        }
        if !session.info.meta.is_empty() {
            session_value["_meta"] = serde_json::Value::Object(session.info.meta.clone());
        }

        let replay_events =
            match harn_vm::orchestration::load_agent_session_replay_events(session_id).await {
                Ok(events) => events,
                Err(error) => {
                    self.send_error(
                        id,
                        -32000,
                        &format!("Failed to replay session {session_id}: {error}"),
                    );
                    return;
                }
            };
        let replay_sink = AcpAgentEventSink::for_replay(self.output.clone());
        for replay_event in &replay_events {
            replay_sink.handle_event(&replay_event.event);
        }
        let replayed: Vec<serde_json::Value> = replay_events
            .iter()
            .map(|event| {
                serde_json::json!({
                    "eventId": event.event_id,
                    "type": serde_json::to_value(&event.event)
                        .ok()
                        .and_then(|value| value.get("type").cloned())
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();

        self.send_response(
            id,
            serde_json::json!({
                "session": session_value,
                "modes": modes::session_mode_state(&session.current_mode_id),
                "configOptions": modes::config_options_state(&session.current_mode_id),
                "replayed": replayed,
            }),
        );
    }

    fn set_session_mode(&mut self, session_id: &str, mode_id: &str) -> Result<bool, String> {
        if !modes::is_known(mode_id) {
            return Err(format!(
                "Unknown mode '{mode_id}'. Available: {}",
                modes::known_mode_ids().join(", ")
            ));
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.current_mode_id == mode_id {
            return Ok(false);
        }
        session.current_mode_id = mode_id.to_string();
        Ok(true)
    }

    fn emit_current_mode_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "modeId": mode_id,
                },
            }),
        );
    }

    fn emit_config_option_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": modes::config_options_state(mode_id),
                },
            }),
        );
    }

    fn handle_session_set_mode(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires sessionId");
            return;
        };
        let Some(mode_id) = params
            .get("modeId")
            .or_else(|| params.get("mode_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires modeId");
            return;
        };

        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(id, serde_json::json!({}));
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    fn handle_session_set_config_option(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires sessionId");
            return;
        };
        let Some(config_id) = params.get("configId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires configId");
            return;
        };
        if config_id != "mode" {
            self.send_error(
                id,
                -32602,
                &format!("Unknown config option '{config_id}'. Available: mode"),
            );
            return;
        }
        let Some(mode_id) = params.get("value").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires value");
            return;
        };

        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": modes::config_options_state(mode_id),
                    }),
                );
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    async fn handle_incoming_message(&mut self, msg: serde_json::Value) {
        if msg.get("method").is_none() && msg.get("id").is_some() {
            if let Some(id) = msg["id"].as_u64() {
                let mut pending = self.pending.lock().await;
                if let Some(sender) = pending.remove(&id) {
                    let _ = sender.send(msg);
                }
            }
            return;
        }

        let method = match msg.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return,
        };
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method.as_str() {
            "initialize" => {
                self.handle_initialize(&id);
            }
            "authenticate" => {
                self.handle_authenticate(&id, &params).await;
            }
            "session/new" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_new(&id, &params);
            }
            "session/load" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_load(&id, &params).await;
            }
            "session/fork" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fork(&id, &params);
            }
            "session/set_mode" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_mode(&id, &params);
            }
            "session/set_config_option" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_config_option(&id, &params);
            }
            "session/prompt" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_prompt(&id, &params).await;
            }
            "session/cancel" => {
                self.handle_session_cancel(&params);
            }
            "session/input" | "user_message" | "agent/user_message" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_input(&params).await;
            }
            "agent/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_agent_resume(&params);
            }
            "harn.hitl.respond" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_hitl_respond(&id, &params).await;
            }
            "workflow/signal" | "harn.workflow.signal" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_signal(&id, &params).await;
            }
            "workflow/query" | "harn.workflow.query" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_query(&id, &params);
            }
            "workflow/update" | "harn.workflow.update" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_update(&id, &params).await;
            }
            "workflow/pause" | "harn.workflow.pause" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_pause(&id, &params);
            }
            "workflow/resume" | "harn.workflow.resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_resume(&id, &params);
            }
            "session/list" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_list(&id);
            }
            _ => {
                if !id.is_null() {
                    self.send_error(&id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}

pub async fn run_acp_channel_server(
    config: AcpServerConfig,
    mut request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(response_tx));
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (routed_tx, mut routed_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            tokio::task::spawn_local(async move {
                while let Some(msg) = request_rx.recv().await {
                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_cancel(&cancellations, &msg) {
                        continue;
                    }

                    let _ = routed_tx.send(msg);
                }

                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = routed_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
}

/// Shared state that bridge-style builtins use to communicate with the
/// ACP client (editor) over JSON-RPC.
pub(super) struct AcpBridge {
    pub(super) session_id: String,
    pub(super) output: AcpOutput,
    pub(super) pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    pub(super) next_id_counter: AtomicU64,
    pub(super) cancellation: SessionCancellation,
    /// Name of the currently executing Harn script (without .harn suffix).
    pub(super) script_name: std::sync::Mutex<String>,
    pub(super) assistant_state: std::sync::Mutex<VisibleTextState>,
}

impl AcpBridge {
    /// Write a complete JSON-RPC line to stdout.
    fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC notification.
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` with agent_message_chunk.
    pub(super) fn send_update(&self, text: &str) {
        let (visible_text, visible_delta) = self
            .assistant_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text, true);
        let mut content = serde_json::json!({
            "type": "text",
            "text": text,
        });
        let mut content_meta = serde_json::Map::new();
        content_meta.insert(
            "visible_text".to_string(),
            serde_json::Value::String(visible_text),
        );
        content_meta.insert(
            "visible_delta".to_string(),
            serde_json::Value::String(visible_delta),
        );
        events::merge_harn_meta(&mut content, content_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": content,
                },
            }),
        );
    }

    /// Send a structured `session/update` with progress phase, message,
    /// and data. `progress` is a harn vendor-extension session-update
    /// variant; canonical ACP has no progress-phase concept, so all
    /// vendor fields ride under `update._meta.harn`.
    pub(super) fn send_progress(
        &self,
        phase: &str,
        message: &str,
        progress: Option<i64>,
        total: Option<i64>,
        data: Option<serde_json::Value>,
    ) {
        let mut update = serde_json::json!({
            "sessionUpdate": "progress",
        });
        let mut harn_meta = serde_json::Map::new();
        harn_meta.insert(
            "phase".to_string(),
            serde_json::Value::String(phase.to_string()),
        );
        harn_meta.insert(
            "message".to_string(),
            serde_json::Value::String(message.to_string()),
        );
        if let Some(p) = progress {
            harn_meta.insert("progress".to_string(), serde_json::Value::from(p));
        }
        if let Some(t) = total {
            harn_meta.insert("total".to_string(), serde_json::Value::from(t));
        }
        if let Some(d) = data {
            harn_meta.insert("data".to_string(), d);
        }
        events::merge_harn_meta(&mut update, harn_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Send a structured `session/update` with log level, message, and
    /// fields. `log` is a harn vendor-extension; canonical ACP has no
    /// log channel on the session-update stream, so all vendor fields
    /// ride under `update._meta.harn`.
    pub(super) fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
        if level == "info" && suppress_default_info_log(message) {
            return;
        }
        let mut update = serde_json::json!({
            "sessionUpdate": "log",
        });
        let mut harn_meta = serde_json::Map::new();
        harn_meta.insert(
            "level".to_string(),
            serde_json::Value::String(level.to_string()),
        );
        harn_meta.insert(
            "message".to_string(),
            serde_json::Value::String(message.to_string()),
        );
        if let Some(f) = fields {
            harn_meta.insert("fields".to_string(), f);
        }
        events::merge_harn_meta(&mut update, harn_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Set the currently executing script name (without .harn suffix).
    fn set_script_name(&self, name: &str) {
        *self.script_name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Get the current script name.
    #[allow(dead_code)]
    fn get_script_name(&self) -> String {
        self.script_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Send a JSON-RPC request to the client and await the response.
    pub(super) async fn call_client(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        self.call_client_inner(method, params, true).await
    }

    pub(super) async fn call_client_for_cleanup(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        self.call_client_inner(method, params, false).await
    }

    async fn call_client_inner(
        &self,
        method: &str,
        params: serde_json::Value,
        abort_on_cancel: bool,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        if abort_on_cancel && self.cancellation.cancelled.load(Ordering::SeqCst) {
            return Err(harn_vm::VmError::Runtime("Cancelled".into()));
        }

        let id = self.next_id_counter.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        if let Ok(line) = serde_json::to_string(&request) {
            self.write_line(&line);
        }

        let timeout = host_call_timeout(method);
        let cancellation = self.cancellation.clone();
        let wait_cancelled = async move {
            loop {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    return;
                }
                cancellation.notify.notified().await;
            }
        };
        tokio::pin!(wait_cancelled);

        tokio::select! {
            result = rx => {
                let msg = result
                    .map_err(|_| harn_vm::VmError::Runtime("Client closed connection".into()))?;
                if let Some(error) = msg.get("error") {
                    let message = error["message"].as_str().unwrap_or("Unknown client error");
                    Err(harn_vm::VmError::Runtime(format!(
                        "Client error: {message}"
                    )))
                } else {
                    Ok(msg["result"].clone())
                }
            }
            _ = &mut wait_cancelled, if abort_on_cancel => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(harn_vm::VmError::Runtime("Cancelled".into()))
            }
            _ = tokio::time::sleep(timeout) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(harn_vm::VmError::Runtime(format!(
                    "Client did not respond to '{method}' within {timeout:?}"
                )))
            }
        }
    }
}

/// Start the ACP server. Reads JSON-RPC from stdin, writes to stdout.
pub async fn run_acp_server(config: AcpServerConfig) {
    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let mut server = AcpServer::new(config);

            // stdin dispatcher: routes responses to pending waiters, and
            // requests/notifications onto the request channel.
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (request_tx, mut request_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            eprintln!("[harn] ACP workflow server ready on stdio");

            tokio::task::spawn_local(async move {
                let stdin = tokio::io::stdin();
                let reader = tokio::io::BufReader::new(stdin);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    let msg: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_cancel(&cancellations, &msg) {
                        continue;
                    }

                    let _ = request_tx.send(msg);
                }

                // stdin closed — clean up pending.
                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = request_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::builtins::normalize_host_capability_manifest;
    use super::{
        acp_agent_capabilities, configured_llm_route_for_capabilities,
        sanitize_visible_assistant_text, AcpBridge, AcpOutput, AcpServer, AcpServerConfig,
        SessionCancellation, ACP_AUTH_REQUIRED_CODE, ACP_SCHEMA_COMPATIBILITY,
        HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
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
        start_acp_channel_session_with_config(AcpServerConfig::new(None), serde_json::json!("."))
            .await
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
                assert_eq!(initialize["result"]["authMethods"], serde_json::json!([]));
                assert_eq!(
                    initialize["result"]["agentCapabilities"]["loadSession"],
                    true
                );
                assert_eq!(
                    initialize["result"]["agentCapabilities"]["sessionCapabilities"],
                    serde_json::json!({
                        "list": {},
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
                    initialize["result"]["agentCapabilities"]["promptCapabilities"]
                        ["image"]
                        .is_boolean()
                );
                assert!(
                    initialize["result"]["agentCapabilities"]["promptCapabilities"]
                        ["audio"]
                        .is_boolean()
                );
                assert!(
                    initialize["result"]["agentCapabilities"]["promptCapabilities"]
                        ["embeddedContext"]
                        .is_boolean()
                );
                assert_eq!(
                    initialize["result"]["agentCapabilities"]["_meta"]["harn"]
                        ["schemaCompatibility"],
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
                let agent_event_method = &initialize["result"]["agentCapabilities"]["_meta"]
                    ["harn"]["extensionMethods"][HARN_AGENT_EVENT_METHOD];
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
                            "prompt": [{"type": "text", "text": "println(\"hello from acp\")"}],
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
                            message["params"]["update"]["content"]["_meta"]["harn"]
                                ["visible_delta"],
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
  let blocks = llm_mock_calls()[0].messages[0].content
  println(blocks[0].text == "Please inspect this context.")
  println(blocks[1].type == "image")
  println(blocks[1].base64 == "iVBORw0KGgo=")
  println(blocks[1].media_type == "image/png")
  println(blocks[2].type == "audio")
  println(blocks[2].base64 == "UklGRiQ=")
  println(blocks[2].media_type == "audio/wav")
  println(contains(blocks[3].text, "file:///tmp/example.txt"))
  println(contains(blocks[3].text, "hello from embedded context"))
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
                        if let Some(text) = message["params"]["update"]["content"]["text"].as_str()
                        {
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
        let bridge = Rc::new(AcpBridge {
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
                "toolCallId": "tool-1",
                "toolName": "edit",
                "options": [
                    {"id": "approve", "label": "Approve"},
                    {"id": "deny", "label": "Deny"}
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
            "result": {"outcome": "approved"},
        });
        crate::protocol_fixture_tests::assert_fixture_documents_match(
            "conformance/protocols/fixtures/acp/session_request_permission.valid.json",
            vec![outgoing, response.clone()],
        );

        let mut server = server;
        server.handle_incoming_message(response).await;
        let result = call.await.expect("permission response");
        assert_eq!(result["outcome"], "approved");
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
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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
        assert_eq!(config_options.len(), 1);
        assert_eq!(config_options[0]["id"], "mode");
        assert_eq!(config_options[0]["category"], "mode");
        assert_eq!(config_options[0]["type"], "select");
        assert_eq!(config_options[0]["currentValue"], "ask");
        let option_ids: Vec<&str> = config_options[0]["options"]
            .as_array()
            .expect("mode options")
            .iter()
            .map(|mode| mode["value"].as_str().expect("mode value"))
            .collect();
        assert_eq!(option_ids, vec!["ask", "architect", "code", "shadow"]);
    }

    /// `session/load` echoes the active mode state back to a
    /// reconnecting client so the UI can re-render the selected mode
    /// without an extra round-trip.
    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_load_includes_current_mode_state() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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

    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_load_replays_persisted_agent_events() {
        harn_vm::event_log::reset_active_event_log();
        let _log = harn_vm::event_log::install_memory_for_current_thread(64);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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

        harn_vm::agent_events::clear_session_sinks(
            created["result"]["sessionId"].as_str().unwrap(),
        );
        harn_vm::event_log::reset_active_event_log();
    }

    /// Setting a valid mode ack's with an empty result and emits a
    /// `current_mode_update` notification carrying the new mode id.
    /// Locks the canonical session-modes wire shape so clients depend
    /// on it directly.
    #[tokio::test(flavor = "current_thread")]
    async fn acp_set_mode_emits_current_mode_update_notification() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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

                let prompt_source =
                    format!("write_file(\"{target_str}\", \"should not be written\")");
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

                let prompt_source =
                    format!("write_file(\"{target_str}\", \"hello from code mode\")");
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
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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

    /// End-to-end ACP slash-command flow: a Zed-style client receives the
    /// `available_commands_update` notification immediately after
    /// `session/new`, then invokes one of the advertised commands and
    /// observes a successful round-trip with the named pipeline executed.
    /// Locks the wire shape required by the ACP spec
    /// (<https://agentclientprotocol.com/protocol/slash-commands>).
    #[tokio::test(flavor = "current_thread")]
    async fn acp_advertises_and_dispatches_slash_commands() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let pipeline_path = dir.path().join("commands.harn");
                std::fs::write(
                    &pipeline_path,
                    "@command(name: \"review\", description: \"Review the diff\", \
                     hint: \"focus area\")\n\
                     pipeline review_branch(task) {\n  \
                       println(\"REVIEW:\" + prompt)\n}\n\n\
                     pipeline default(task) {\n  \
                       println(\"DEFAULT:\" + prompt)\n}\n",
                )
                .expect("write pipeline");

                let (request_tx, request_rx) = mpsc::unbounded_channel();
                let (response_tx, mut response_rx) = mpsc::unbounded_channel();
                let server = tokio::task::spawn_local(super::run_acp_channel_server(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                    request_rx,
                    response_tx,
                ));

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "session/new",
                        "params": {"cwd": dir.path()},
                    }))
                    .expect("send session/new");
                let created = recv_json(&mut response_rx).await;
                let session_id = created["result"]["sessionId"]
                    .as_str()
                    .expect("session id")
                    .to_string();

                let advertised = recv_json(&mut response_rx).await;
                assert_eq!(advertised["method"], "session/update");
                assert_eq!(advertised["params"]["sessionId"], session_id);
                assert_eq!(
                    advertised["params"]["update"]["sessionUpdate"],
                    "available_commands_update"
                );
                let commands = advertised["params"]["update"]["availableCommands"]
                    .as_array()
                    .expect("availableCommands array");
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0]["name"], "review");
                assert_eq!(commands[0]["description"], "Review the diff");
                assert_eq!(commands[0]["input"]["hint"], "focus area");

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "session/prompt",
                        "params": {
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": "/review src/lib.rs"}],
                        },
                    }))
                    .expect("send session/prompt");

                let mut saw_review_chunk = false;
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
                        let text = message["params"]["update"]["content"]["text"]
                            .as_str()
                            .unwrap_or_default();
                        if text.contains("REVIEW:src/lib.rs") {
                            saw_review_chunk = true;
                        }
                        assert!(
                            !text.contains("DEFAULT:"),
                            "default pipeline must not run when slash command dispatches"
                        );
                    }
                    if message["id"] == 2 {
                        assert_eq!(message["result"]["stopReason"], "end_turn");
                        saw_completed = true;
                        break;
                    }
                }
                assert!(saw_review_chunk, "named pipeline should run for /review");
                assert!(saw_completed, "prompt should finish successfully");

                drop(request_tx);
                server.await.expect("ACP channel server task");
            })
            .await;
    }

    /// Unknown slash invocations (i.e. `/typo args` when `typo` isn't
    /// advertised) must not be re-routed — the original prompt text
    /// flows through to the default pipeline so it can decide how to
    /// handle the literal slash.
    #[tokio::test(flavor = "current_thread")]
    async fn acp_unknown_slash_invocation_falls_through_to_default_pipeline() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let pipeline_path = dir.path().join("fallthrough.harn");
                std::fs::write(
                    &pipeline_path,
                    "@command(name: \"known\", description: \"known\")\n\
                     pipeline known(task) { println(\"KNOWN\") }\n\n\
                     pipeline default(task) { println(\"DEFAULT:\" + prompt) }\n",
                )
                .expect("write pipeline");

                let (request_tx, request_rx) = mpsc::unbounded_channel();
                let (response_tx, mut response_rx) = mpsc::unbounded_channel();
                let server = tokio::task::spawn_local(super::run_acp_channel_server(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                    request_rx,
                    response_tx,
                ));

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "session/new",
                        "params": {"cwd": dir.path()},
                    }))
                    .expect("send session/new");
                let created = recv_json(&mut response_rx).await;
                let session_id = created["result"]["sessionId"]
                    .as_str()
                    .expect("session id")
                    .to_string();
                let _advertised = recv_json(&mut response_rx).await;

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "session/prompt",
                        "params": {
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": "/typo and friends"}],
                        },
                    }))
                    .expect("send session/prompt");

                let mut saw_default_with_full_text = false;
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
                        let text = message["params"]["update"]["content"]["text"]
                            .as_str()
                            .unwrap_or_default();
                        if text.contains("DEFAULT:/typo and friends") {
                            saw_default_with_full_text = true;
                        }
                    }
                    if message["id"] == 2 {
                        assert_eq!(message["result"]["stopReason"], "end_turn");
                        break;
                    }
                }
                assert!(
                    saw_default_with_full_text,
                    "default pipeline should receive the full original prompt text"
                );

                drop(request_tx);
                server.await.expect("ACP channel server task");
            })
            .await;
    }

    /// Inline-prompt mode (no `--pipeline`) has no surface for
    /// `@command`-tagged pipelines. A leading slash is unambiguously a
    /// user error there; surface a clear diagnostic instead of letting
    /// the compile-time `pipeline main() { /foo args }` error leak out.
    #[tokio::test(flavor = "current_thread")]
    async fn acp_inline_mode_rejects_slash_invocations_with_friendly_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

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
                    "prompt": [{"type": "text", "text": "/foo args"}],
                },
            }))
            .await;

        let update = recv_json(&mut rx).await;
        assert_eq!(update["method"], "session/update");
        assert!(
            update["params"]["update"]["content"]["_meta"]["harn"]["visible_delta"]
                .as_str()
                .unwrap_or_default()
                .contains("Slash commands require `--pipeline"),
            "expected friendly inline-mode diagnostic, got: {update}"
        );
        let error = recv_json(&mut rx).await;
        assert_eq!(error["id"], 2);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("Slash commands require `--pipeline"),
            "expected friendly inline-mode error message, got: {error}"
        );
    }

    /// Hot-reload: when the pipeline source changes between prompts, the
    /// next prompt re-emits `available_commands_update` with the fresh
    /// command set. When the source is unchanged, no duplicate update is
    /// emitted (idempotent advertise).
    #[tokio::test(flavor = "current_thread")]
    async fn acp_reemits_available_commands_on_pipeline_hot_reload() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let pipeline_path = dir.path().join("hot.harn");
                std::fs::write(
                    &pipeline_path,
                    "@command(name: \"alpha\", description: \"first\")\n\
                     pipeline alpha(task) { println(\"alpha\") }\n",
                )
                .expect("write initial pipeline");

                let (request_tx, request_rx) = mpsc::unbounded_channel();
                let (response_tx, mut response_rx) = mpsc::unbounded_channel();
                let server = tokio::task::spawn_local(super::run_acp_channel_server(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                    request_rx,
                    response_tx,
                ));

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "session/new",
                        "params": {"cwd": dir.path()},
                    }))
                    .expect("send session/new");
                let created = recv_json(&mut response_rx).await;
                let session_id = created["result"]["sessionId"]
                    .as_str()
                    .expect("session id")
                    .to_string();
                let initial = recv_json(&mut response_rx).await;
                let initial_commands = initial["params"]["update"]["availableCommands"]
                    .as_array()
                    .expect("availableCommands array");
                assert_eq!(initial_commands.len(), 1);
                assert_eq!(initial_commands[0]["name"], "alpha");

                std::fs::write(
                    &pipeline_path,
                    "@command(name: \"alpha\", description: \"first\")\n\
                     pipeline alpha(task) { println(\"alpha\") }\n\n\
                     @command(name: \"beta\", description: \"second\")\n\
                     pipeline beta(task) { println(\"beta\") }\n",
                )
                .expect("rewrite pipeline");

                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "session/prompt",
                        "params": {
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": "/beta now"}],
                        },
                    }))
                    .expect("send session/prompt");

                let mut saw_refreshed_advertise = false;
                let mut saw_beta_chunk = false;
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
                        && message["params"]["update"]["sessionUpdate"]
                            == "available_commands_update"
                    {
                        let names: Vec<String> = message["params"]["update"]["availableCommands"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|c| c["name"].as_str().unwrap().to_string())
                            .collect();
                        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
                        saw_refreshed_advertise = true;
                    }
                    if message["method"] == "session/update"
                        && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                        && message["params"]["update"]["content"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("beta")
                    {
                        saw_beta_chunk = true;
                    }
                    if message["id"] == 2 {
                        assert_eq!(message["result"]["stopReason"], "end_turn");
                        break;
                    }
                }
                assert!(
                    saw_refreshed_advertise,
                    "expected fresh available_commands_update after source change"
                );
                assert!(
                    saw_beta_chunk,
                    "the newly added /beta command should dispatch"
                );

                drop(request_tx);
                server.await.expect("ACP channel server task");
            })
            .await;
    }

    /// `session/prompt` returns the canonical ACP `stopReason` rather
    /// than Harn's internal "completed" / "cancelled" pair. This drives
    /// each branch of the mapping in `agent_session_host::canonical_acp_stop_reason`
    /// through a real ACP roundtrip with `provider: "mock"` so the
    /// adapter and the agent loop's finalize stay aligned with the
    /// canonical enum at <https://agentclientprotocol.com/protocol/prompt-turn>.
    async fn run_acp_agent_loop_prompt(prompt_body: &str) -> serde_json::Value {
        let (request_tx, mut response_rx, server, session_id) = start_acp_channel_session().await;

        // `agent_loop` requires the LLM/network capability ceiling.
        // The default `ask` mode clamps to read-only; switch to `code`
        // (`ActAuto` autonomy tier) so the test can exercise the loop.
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

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": prompt_body}],
                },
            }))
            .expect("send session/prompt");

        let mut stop_reason = serde_json::Value::Null;
        for _ in 0..64 {
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
                stop_reason = message["result"]["stopReason"].clone();
                break;
            }
        }
        drop(request_tx);
        server.await.expect("ACP channel server task");
        stop_reason
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_prompt_reports_end_turn_when_loop_finishes_naturally() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"all done\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "end_turn");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_prompt_reports_max_tokens_from_provider_signal() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"truncated\", stop_reason: \"max_tokens\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "max_tokens");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_prompt_reports_refusal_from_provider_signal() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"I cannot assist with that.\", stop_reason: \"refusal\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "refusal");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_session_prompt_reports_max_turn_requests_when_iteration_cap_hit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // `loop_until_done: true` keeps the loop iterating on a
                // text-only mock turn, and `max_iterations: 1` forces
                // the cap to fire on iteration 1 → ACP `max_turn_requests`.
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"still working\"})\n\
                            llm_mock({text: \"still working\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\", loop_until_done: true, max_iterations: 1})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "max_turn_requests");
            })
            .await;
    }
}
