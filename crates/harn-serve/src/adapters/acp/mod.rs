//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as an agent runtime accessible from any host application
//! (IDEs, CLI tools, web apps, etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.
//!
//! Module map: `transport` owns stdio/channel routing, `sessions` owns session
//! state and cancellation, `schema` owns capability and prompt normalization,
//! `auth` owns ACP credential normalization, and `bridge` owns outbound host
//! calls and update/log/progress emission.

mod auth;
mod bridge;
mod builtins;
mod commands;
mod events;
mod execute;
mod io;
mod modes;
mod schema;
mod sessions;
#[cfg(test)]
mod tests;
mod transport;
mod types;

use auth::acp_auth_request_for_method;
use bridge::AcpBridge;
pub use bridge::AcpOutput;
#[cfg(test)]
use schema::configured_llm_route_for_capabilities;
use schema::{
    acp_agent_capabilities, normalize_acp_prompt, normalize_acp_prompt_block,
    prompt_text_from_content, retarget_prompt_text,
};
pub use schema::{
    ACP_SCHEMA_COMPATIBILITY, ACP_SESSION_UPDATE_VARIANTS, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS, HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use sessions::{
    apply_session_budget_rearm, lookup_session_cancellation, preempt_session_interruption,
    prepare_session_prompt, Session, SessionBudget, SessionCancellation, SessionInfo,
};
pub(crate) use transport::run_acp_channel_server_with_existing_handle;
pub use transport::{
    run_acp_channel_server, run_acp_channel_server_with_handle, run_acp_server,
    run_acp_websocket_server, AcpChannelHandle, AcpWebSocketServeOptions,
};
pub use types::{
    AcpContentBlock, AcpEmbeddedResource, AcpHarnMeta, AcpJsonRpcError, AcpJsonRpcErrorResponse,
    AcpJsonRpcId, AcpJsonRpcRequest, AcpJsonRpcResponse, AcpMeta, AcpSessionCancelToolCallParams,
    AcpSessionIdParams, AcpSessionInjectContent, AcpSessionInjectMode, AcpSessionInjectParams,
    AcpSessionMessageIdParams, AcpSessionNewParams, AcpSessionPromptParams, AcpSessionPromptResult,
    AcpSessionReplaceInjectParams, AcpSessionRestoreResult, ACP_METHOD_INITIALIZE,
    ACP_METHOD_SESSION_CANCEL, ACP_METHOD_SESSION_CANCEL_TOOL_CALL, ACP_METHOD_SESSION_CLOSE,
    ACP_METHOD_SESSION_INJECT, ACP_METHOD_SESSION_NEW, ACP_METHOD_SESSION_PENDING_INJECTIONS,
    ACP_METHOD_SESSION_PROMPT, ACP_METHOD_SESSION_REPLACE_INJECT, ACP_METHOD_SESSION_REVOKE_INJECT,
};

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use harn_vm::agent_events::{clear_session_sinks, register_sink, AgentEventSink};
use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

use crate::{
    AdapterDescriptor, AuthMethodConfig, AuthPolicy, AuthRequest, AuthenticatedPrincipal,
    AuthorizationDecision, BudgetSpec,
};
use commands::{
    discover_commands, parse_slash_invocation, render_available_commands, DiscoveredCommand,
};
use events::AcpAgentEventSink;
use io::send_json_response;

const ACP_AUTH_REQUIRED_CODE: i64 = -32000;

/// Default loopback redirect for `mcp/authorize` when the client does not
/// supply one. TUI/headless clients capture this with a loopback listener
/// (matching `harn mcp login`); GUI clients pass their own URL scheme instead.
const MCP_DEFAULT_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:9783/oauth/callback";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpImportTokenParams {
    #[serde(alias = "resource")]
    url: String,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    token_endpoint: Option<String>,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default, alias = "scopes")]
    scope: Option<String>,
}

/// Parse `state`, `code`, and the optional `iss` issuer out of a captured OAuth
/// redirect URL (e.g. `burin://oauth/callback?code=…&state=…&iss=…`). Returns
/// an error describing what the provider sent back when `code`/`state` are
/// absent (including a propagated `error`/`error_description` query).
fn parse_oauth_redirect_url(
    redirect_url: &str,
) -> Result<(String, String, Option<String>), String> {
    let parsed =
        url::Url::parse(redirect_url).map_err(|error| format!("invalid redirectUrl: {error}"))?;
    let query = |key: &str| {
        parsed
            .query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
    };
    if let Some(error) = query("error") {
        let description = query("error_description")
            .map(|description| format!(": {description}"))
            .unwrap_or_default();
        return Err(format!("authorization failed: {error}{description}"));
    }
    let state = query("state").ok_or_else(|| "redirectUrl is missing state".to_string())?;
    let code = query("code").ok_or_else(|| "redirectUrl is missing code".to_string())?;
    Ok((state, code, query("iss")))
}

#[derive(Clone)]
struct InjectControlRecord {
    owner: serde_json::Value,
    status: String,
}

fn session_id_param(params: &serde_json::Value) -> Option<String> {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn harn_meta(params: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    params
        .get("_harn")
        .and_then(serde_json::Value::as_object)
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|meta| meta.get("harn"))
                .and_then(serde_json::Value::as_object)
        })
}

fn string_meta_field(
    meta: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Option<String> {
    meta.get(camel)
        .or_else(|| meta.get(snake))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn control_actor_from_params(params: &serde_json::Value) -> serde_json::Value {
    let Some(meta) = harn_meta(params) else {
        return serde_json::json!({
            "clientId": "legacy-acp-client",
            "role": "host_owner",
            "source": "acp",
        });
    };
    if let Some(actor) = meta.get("actor").and_then(serde_json::Value::as_object) {
        let mut actor = actor.clone();
        actor
            .entry("source".to_string())
            .or_insert_with(|| serde_json::json!("acp"));
        actor
            .entry("role".to_string())
            .or_insert_with(|| serde_json::json!("host_owner"));
        actor
            .entry("clientId".to_string())
            .or_insert_with(|| serde_json::json!("legacy-acp-client"));
        return serde_json::Value::Object(actor);
    }

    let mut actor = serde_json::Map::new();
    actor.insert(
        "clientId".to_string(),
        serde_json::json!(string_meta_field(meta, "clientId", "client_id")
            .unwrap_or_else(|| "legacy-acp-client".to_string())),
    );
    if let Some(connection_id) = string_meta_field(meta, "connectionId", "connection_id") {
        actor.insert("connectionId".to_string(), serde_json::json!(connection_id));
    }
    if let Some(display_name) = string_meta_field(meta, "displayName", "display_name") {
        actor.insert("displayName".to_string(), serde_json::json!(display_name));
    }
    actor.insert(
        "role".to_string(),
        serde_json::json!(
            string_meta_field(meta, "role", "role").unwrap_or_else(|| "host_owner".to_string())
        ),
    );
    actor.insert(
        "source".to_string(),
        serde_json::json!(
            string_meta_field(meta, "source", "source").unwrap_or_else(|| { "acp".to_string() })
        ),
    );
    serde_json::Value::Object(actor)
}

fn actor_is_host_owner(actor: &serde_json::Value) -> bool {
    actor
        .get("role")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|role| role == "host_owner" || role == "hostOwner" || role == "owner")
}

fn control_id() -> String {
    format!("ctrl_{}", uuid::Uuid::now_v7().simple())
}

fn session_list_filter<'a>(
    params: &'a serde_json::Value,
    camel: &str,
    snake: &str,
) -> Option<&'a serde_json::Value> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .or_else(|| {
            params
                .get("filter")
                .and_then(|filter| filter.get(camel).or_else(|| filter.get(snake)))
        })
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|meta| meta.get("harn"))
                .and_then(|harn| harn.get(camel).or_else(|| harn.get(snake)))
        })
}

fn session_workspace_anchor_filter(params: &serde_json::Value) -> Option<&serde_json::Value> {
    session_list_filter(params, "workspaceAnchor", "workspace_anchor")
}

fn session_cwd_filter(params: &serde_json::Value) -> Option<&str> {
    session_list_filter(params, "cwd", "cwd").and_then(serde_json::Value::as_str)
}

fn session_live_state_filter(params: &serde_json::Value) -> Option<Vec<String>> {
    let value = session_list_filter(params, "liveState", "live_state")
        .or_else(|| session_list_filter(params, "state", "state"))?;
    if let Some(raw) = value.as_str() {
        return Some(vec![raw.to_string()]);
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    })
}

fn workspace_anchor_filter_matches(
    anchor: Option<&harn_vm::workspace_anchor::WorkspaceAnchor>,
    filter: Option<&serde_json::Value>,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(anchor) = anchor else {
        return false;
    };
    if let Some(raw) = filter.as_str() {
        return anchor.primary.to_string_lossy() == raw;
    }
    if let Some(primary) = filter.get("primary").and_then(serde_json::Value::as_str) {
        return anchor.primary.to_string_lossy() == primary;
    }
    harn_vm::workspace_anchor::WorkspaceAnchor::from_json(filter).is_ok_and(|expected| {
        expected.primary == anchor.primary && expected.additional_roots == anchor.additional_roots
    })
}

fn live_state_filter_matches(live_state: &str, filter: Option<&[String]>) -> bool {
    filter.is_none_or(|states| states.iter().any(|state| state == live_state))
}

fn bridge_mode_for_session_inject(params: &serde_json::Value) -> Result<&'static str, String> {
    match params.get("mode").and_then(|value| value.as_str()) {
        // ACP `session/inject.mode = "queue"` is the audit/trail variant —
        // the message ends up in the transcript on `loop_exit` but is
        // never rendered into a model prompt. The canonical bridge name
        // is `audit_only` (harn#2212).
        Some("queue") => Ok("audit_only"),
        Some("steer") => Ok("finish_step"),
        Some(other) => Err(format!(
            "session/inject: unsupported mode `{other}`; expected `queue` or `steer`"
        )),
        None => Err("session/inject requires mode".to_string()),
    }
}

fn normalize_session_inject_content(
    method: &str,
    params: &serde_json::Value,
) -> Result<(String, serde_json::Value), String> {
    let Some(content) = params.get("content") else {
        return Err(format!("{method} requires content"));
    };
    if let Some(text) = content.as_str() {
        if text.is_empty() {
            return Err(format!("{method}: content must not be empty"));
        }
        return Ok((
            text.to_string(),
            serde_json::Value::String(text.to_string()),
        ));
    }
    let Some(blocks) = content.as_array() else {
        return Err(format!(
            "{method}: content must be a string or an array of content blocks"
        ));
    };
    if blocks.is_empty() {
        return Err(format!("{method}: content must not be empty"));
    }
    let normalized = blocks
        .iter()
        .map(|block| {
            normalize_acp_prompt_block(block)
                .map_err(|message| message.replace("session/prompt", method))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let text = prompt_text_from_content(&normalized);
    Ok((text, serde_json::Value::Array(normalized)))
}

fn nonnegative_usize_param(
    params: &serde_json::Value,
    names: &[&str],
    label: &str,
) -> Result<Option<usize>, String> {
    for name in names {
        let Some(value) = params.get(*name) else {
            continue;
        };
        return match value.as_i64() {
            Some(value) if value >= 0 => Ok(Some(value as usize)),
            _ => Err(format!("Invalid {label}: must be >= 0")),
        };
    }
    Ok(None)
}

fn budget_config_value(spec: &BudgetSpec) -> String {
    let mut value = serde_json::Map::new();
    if let Some(cost) = spec.llm_cost_usd {
        value.insert("llm_cost_usd".to_string(), serde_json::json!(cost));
    }
    if let Some(tokens) = spec.llm_tokens {
        value.insert("llm_tokens".to_string(), serde_json::json!(tokens));
    }
    if let Some(calls) = spec.mcp_calls {
        value.insert("mcp_calls".to_string(), serde_json::json!(calls));
    }
    if let Some(queries) = spec.pg_queries {
        value.insert("pg_queries".to_string(), serde_json::json!(queries));
    }
    serde_json::to_string(&serde_json::Value::Object(value)).unwrap_or_else(|_| "{}".to_string())
}

fn normalize_budget_spec(mut spec: BudgetSpec) -> Option<BudgetSpec> {
    spec.llm_cost_usd = spec
        .llm_cost_usd
        .and_then(|value| value.is_finite().then_some(value.max(0.0)));
    (!spec.is_empty()).then_some(spec)
}

fn budget_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) -> Option<&'a serde_json::Value> {
    names.iter().find_map(|name| object.get(*name))
}

fn parse_budget_cost_field(
    object: &serde_json::Map<String, serde_json::Value>,
    names: &[&str],
    label: &str,
) -> Result<Option<f64>, String> {
    let Some(value) = budget_field(object, names) else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err(format!(
            "invalid_budget: {label} must be a non-negative number"
        ));
    };
    if !number.is_finite() || number < 0.0 {
        return Err(format!(
            "invalid_budget: {label} must be a non-negative finite number"
        ));
    }
    Ok(Some(number))
}

fn parse_budget_count_field(
    object: &serde_json::Map<String, serde_json::Value>,
    names: &[&str],
    label: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = budget_field(object, names) else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| format!("invalid_budget: {label} must be a non-negative integer"))
}

fn parse_budget_config_value(raw: &str) -> Result<SessionBudget, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == modes::BUDGET_INHERIT_VALUE {
        return Ok(SessionBudget::Inherit);
    }
    if matches!(trimmed, modes::BUDGET_OFF_VALUE | "none" | "unlimited") {
        return Ok(SessionBudget::Unlimited);
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
        format!(
            "invalid_budget: expected @inherit, off, or JSON object with llm_cost_usd/llm_tokens fields: {error}"
        )
    })?;
    if value.is_null() {
        return Ok(SessionBudget::Inherit);
    }
    let Some(object) = value.as_object() else {
        return Err(
            "invalid_budget: expected a JSON object with llm_cost_usd, llm_tokens, mcp_calls, or pg_queries".to_string(),
        );
    };
    let spec = BudgetSpec {
        llm_cost_usd: parse_budget_cost_field(
            object,
            &["llm_cost_usd", "llmCostUsd"],
            "llm_cost_usd",
        )?,
        llm_tokens: parse_budget_count_field(object, &["llm_tokens", "llmTokens"], "llm_tokens")?,
        mcp_calls: parse_budget_count_field(object, &["mcp_calls", "mcpCalls"], "mcp_calls")?,
        pg_queries: parse_budget_count_field(object, &["pg_queries", "pgQueries"], "pg_queries")?,
    };
    if spec.is_empty() {
        return Err("invalid_budget: budget object must include at least one limit".to_string());
    }
    Ok(SessionBudget::Custom(spec))
}

fn append_profile_json_line(
    path: &std::path::Path,
    session_id: &str,
    turn: u64,
    rollup: &harn_vm::profile::RunProfile,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create profile directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }
    let line = serde_json::to_string(&serde_json::json!({
        "turn": turn,
        "session_id": session_id,
        "rollup": rollup,
    }))
    .map_err(|error| format!("failed to serialize profile: {error}"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    writeln!(file, "{line}")
        .map_err(|error| format!("failed to append {}: {error}", path.display()))
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

#[derive(Clone, Default)]
pub struct AcpProfileConfig {
    pub text: bool,
    pub json_path: Option<PathBuf>,
}

impl AcpProfileConfig {
    pub fn is_enabled(&self) -> bool {
        self.text || self.json_path.is_some()
    }
}

#[derive(Clone)]
pub struct AcpServerConfig {
    pub pipeline: Option<String>,
    pub auth_policy: AuthPolicy,
    pub authenticated_principal: Option<AuthenticatedPrincipal>,
    pub runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    pub llm_config_overrides: Option<harn_vm::llm_config::ProvidersConfig>,
    pub llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    pub profile: AcpProfileConfig,
    pub budget: Option<BudgetSpec>,
    /// Read-only sandbox roots the embedder grants on top of the user's
    /// `workspace_roots`. Paths resolving under one of these are readable
    /// during a turn even though they sit outside the user workspace, but
    /// they are never writable or executable. Intended for an in-process
    /// host (e.g. burin's Rust TUI) that ships bundled assets — pipelines
    /// and their `@partials` — outside the user's project tree. Empty by
    /// default so embedders that do not set it keep the stock sandbox.
    pub read_only_roots: Vec<String>,
}

impl AcpServerConfig {
    pub fn new(pipeline: Option<String>) -> Self {
        Self {
            pipeline,
            auth_policy: AuthPolicy::allow_all(),
            authenticated_principal: None,
            runtime_configurator: Arc::new(NoopAcpRuntimeConfigurator),
            llm_config_overrides: None,
            llm_capability_overrides: None,
            profile: AcpProfileConfig::default(),
            budget: None,
            read_only_roots: Vec::new(),
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

    pub fn with_authenticated_principal(mut self, principal: AuthenticatedPrincipal) -> Self {
        self.authenticated_principal = Some(principal);
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

    pub fn with_profile(mut self, profile: AcpProfileConfig) -> Self {
        self.profile = profile;
        self
    }

    pub fn with_budget(mut self, budget: BudgetSpec) -> Self {
        self.budget = normalize_budget_spec(budget);
        self
    }

    pub fn with_llm_cost_budget(mut self, max_cost_usd: f64) -> Self {
        let mut budget = self.budget.unwrap_or_default();
        budget.llm_cost_usd = Some(max_cost_usd);
        self.budget = normalize_budget_spec(budget);
        self
    }

    pub fn with_llm_token_budget(mut self, max_tokens: u64) -> Self {
        let mut budget = self.budget.unwrap_or_default();
        budget.llm_tokens = Some(max_tokens);
        self.budget = normalize_budget_spec(budget);
        self
    }

    /// Register read-only sandbox roots that the embedder grants on top of
    /// the user's `workspace_roots`. Each path is canonicalized so the
    /// per-turn policy compares against the same normalized form the
    /// sandbox scope check uses; entries that cannot be canonicalized
    /// (e.g. they do not exist yet) fall back to their input string.
    /// Empty/blank entries are dropped.
    pub fn with_read_only_roots(mut self, roots: Vec<String>) -> Self {
        self.read_only_roots = canonicalize_read_only_roots(roots);
        self
    }
}

/// Canonicalize embedder-supplied read-only roots, dropping blank entries.
/// Falls back to the trimmed input when canonicalization fails so a root
/// that does not exist on disk yet is still carried verbatim.
fn canonicalize_read_only_roots(roots: Vec<String>) -> Vec<String> {
    roots
        .into_iter()
        .filter_map(|root| {
            let trimmed = root.trim();
            if trimmed.is_empty() {
                return None;
            }
            let canonical = std::fs::canonicalize(trimmed)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| trimmed.to_string());
            Some(canonical)
        })
        .collect()
}

/// Cached compiled pipeline. The ACP server re-uses the same pipeline file
/// across every `session/prompt`; recompiling on each turn was the dominant
/// per-turn overhead inside the VM (~80ms on auto.harn) before this cache.
/// Keyed by (path, mtime, target_pipeline_name) — when the file's mtime moves
/// forward we discard the cache and re-read/re-compile.
struct CompileCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    target_pipeline: Option<String>,
    source: String,
    chunk: harn_vm::Chunk,
}

/// Cached VM baseline for a file-backed ACP pipeline. This is intentionally
/// separate from bytecode caching: source can compile from cache while VM
/// setup is invalidated by cwd, project-root discovery, target pipeline, or
/// ACP mode changes.
struct VmBaselineCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    target_pipeline: Option<String>,
    source: String,
    cwd: PathBuf,
    project_root: Option<PathBuf>,
    mode_id: String,
    baseline: harn_vm::VmBaseline,
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
    /// ACP control-plane ownership for pending injected messages, keyed by
    /// session id then message id.
    inject_controls: HashMap<String, BTreeMap<String, InjectControlRecord>>,
    /// Monotonically increasing JSON-RPC request ID for outgoing requests.
    next_id: AtomicU64,
    /// Pending outgoing request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Cancel flags are shared with the transport reader so
    /// `session/cancel` can interrupt a blocked `session/prompt` turn.
    session_cancellations: Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    /// Transport output sink.
    output: AcpOutput,
    /// Compiled-pipeline cache. One slot — the ACP server runs a single
    /// pipeline file for its lifetime, so we only ever cache one chunk.
    compile_cache: Option<CompileCacheEntry>,
    /// Prepared VM baseline cache for the same file-backed pipeline.
    vm_baseline_cache: Option<VmBaselineCacheEntry>,
    /// Per-turn profile emission settings.
    profile: AcpProfileConfig,
    /// Provider/catalog overlays installed by the embedder for this server.
    llm_config_overrides: Option<harn_vm::llm_config::ProvidersConfig>,
    llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    /// Server-level budget inherited by sessions unless they override it.
    default_budget: Option<BudgetSpec>,
    /// Embedder-granted read-only sandbox roots, unioned into the per-turn
    /// capability policy so bundled assets outside the user workspace stay
    /// readable. Empty for embedders that do not opt in.
    read_only_roots: Vec<String>,
}

impl AcpServer {
    pub fn new(config: AcpServerConfig) -> Self {
        Self::new_with_output(config, AcpOutput::stdout())
    }

    /// Create an ACP server that writes responses and notifications to a
    /// caller-provided output sink.
    ///
    /// Prefer [`crate::EmbeddedAgent`] or [`run_acp_channel_server_with_handle`]
    /// unless the host already owns the compatible current-thread runtime and
    /// wants to drive incoming JSON-RPC messages directly.
    pub fn new_with_output(config: AcpServerConfig, output: AcpOutput) -> Self {
        harn_vm::llm_config::set_user_overrides(config.llm_config_overrides.clone());
        harn_vm::llm::capabilities::set_user_overrides(config.llm_capability_overrides.clone());
        let llm_config_overrides = config.llm_config_overrides.clone();
        let llm_capability_overrides = config.llm_capability_overrides.clone();

        Self {
            descriptor: AdapterDescriptor {
                id: "acp".to_string(),
                caller_shape: "agent-session".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            pipeline: config.pipeline,
            auth_policy: config.auth_policy,
            authenticated_principal: config.authenticated_principal,
            runtime_configurator: config.runtime_configurator,
            sessions: HashMap::new(),
            inject_controls: HashMap::new(),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            output,
            compile_cache: None,
            vm_baseline_cache: None,
            profile: config.profile,
            llm_config_overrides,
            llm_capability_overrides,
            default_budget: config.budget,
            read_only_roots: config.read_only_roots,
        }
    }

    /// Compile `source` for `target_pipeline` (or the default entry point
    /// when `target_pipeline` is None), reusing the cached chunk when the
    /// file at `source_path` has the same mtime as the last cache fill and
    /// the target hasn't changed.
    ///
    /// Returns `(chunk, hit)` so the caller can keep its existing compile-
    /// time telemetry meaningful (hits report ~0 ms).
    ///
    /// Inline-mode prompts pass `source_path: None` and never hit cache —
    /// the source is freshly generated per turn so there's nothing to reuse.
    fn compile_pipeline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
    ) -> Result<(harn_vm::Chunk, bool), String> {
        let target_owned = target_pipeline.map(|s| s.to_string());
        let cache_key = source_path.and_then(|path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .map(|mtime| (path.to_path_buf(), mtime))
        });
        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.compile_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                {
                    return Ok((entry.chunk.clone(), true));
                }
            }
        }
        let chunk = match target_pipeline {
            Some(name) => harn_vm::compile_source_named(source, name),
            None => harn_vm::compile_source(source),
        }
        .map_err(|e| format!("Compilation error: {e}"))?;
        if let Some((path, mtime)) = cache_key {
            self.compile_cache = Some(CompileCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                chunk: chunk.clone(),
            });
        }
        Ok((chunk, false))
    }

    async fn prepare_vm_baseline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
        cwd: &Path,
        mode_id: &str,
    ) -> Result<(Option<harn_vm::VmBaseline>, Option<bool>, u64), String> {
        let Some(source_path) = source_path else {
            return Ok((None, None, 0));
        };

        let prepare_started = Instant::now();
        let target_owned = target_pipeline.map(str::to_string);
        let cache_key = std::fs::metadata(source_path)
            .and_then(|m| m.modified())
            .ok()
            .map(|mtime| (source_path.to_path_buf(), mtime));
        let source_parent = source_path.parent().unwrap_or(cwd);
        let project_root = harn_vm::stdlib::process::find_project_root(source_parent)
            .or_else(|| harn_vm::stdlib::process::find_project_root(cwd));

        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.vm_baseline_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                    && entry.cwd == cwd
                    && entry.project_root == project_root
                    && entry.mode_id == mode_id
                {
                    return Ok((
                        Some(entry.baseline.clone()),
                        Some(true),
                        prepare_started.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        let baseline = execute::prepare_vm_baseline(
            source,
            source_path,
            cwd,
            self.runtime_configurator.clone(),
        )
        .await?;
        if let Some((path, mtime)) = cache_key {
            self.vm_baseline_cache = Some(VmBaselineCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                cwd: cwd.to_path_buf(),
                project_root,
                mode_id: mode_id.to_string(),
                baseline: baseline.clone(),
            });
        } else {
            self.vm_baseline_cache = None;
        }

        Ok((
            Some(baseline),
            Some(false),
            prepare_started.elapsed().as_millis() as u64,
        ))
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

    fn emit_control_outcome(
        &self,
        session_id: &str,
        method: &str,
        outcome: &str,
        status: &str,
        actor: serde_json::Value,
        target: serde_json::Value,
        reason: Option<&str>,
    ) {
        harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
            session_id: session_id.to_string(),
            control_id: control_id(),
            method: method.to_string(),
            outcome: outcome.to_string(),
            status: status.to_string(),
            actor,
            target,
            reason: reason.map(str::to_string),
            metadata: serde_json::Value::Null,
        });
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

    fn handle_provider_catalog(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
                self.llm_config_overrides.as_ref(),
                self.llm_capability_overrides.as_ref(),
            ))
            .expect("provider catalog serializes"),
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
        // A policy with no configured methods advertises the synthetic
        // local "none" flow (see `AuthPolicy::acp_auth_methods`). Honour
        // an explicit authenticate against it as a no-op success so the
        // advertised method is real: the caller is already an anonymous
        // principal.
        if self.auth_policy.methods.is_empty() && method_id == crate::ACP_LOCAL_NONE_METHOD_ID {
            let principal = AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
                granted_scopes: std::collections::BTreeSet::new(),
                tenant_id: None,
            };
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
            return;
        }
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
            // ACP authentication doesn't bind a route, so this branch is
            // unreachable unless a future caller threads per-route scopes
            // through this code path. Forward as auth-required so the
            // client knows to retry with a richer credential.
            AuthorizationDecision::MissingScope { required, granted } => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &crate::forbidden_message(&required, &granted),
                    self.auth_required_data(),
                );
            }
            // `authorize_mcp` is the only producer of this variant and
            // belongs to the harn-vm `harness.mcp.*` dispatch path, not
            // ACP authentication. Surfacing it here would mean policy
            // wiring leaked; forward as auth-required with the
            // policy's reason so the operator can debug.
            AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &reason,
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
                inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
                info,
                advertised_commands: Vec::new(),
                current_mode_id: modes::DEFAULT_MODE_ID.to_string(),
                budget: SessionBudget::Inherit,
                profile_turn: 0,
            },
        );
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        #[cfg(feature = "hostlib")]
        if let Some(session) = self.sessions.get(&session_id) {
            harn_hostlib::fs::configure_session_root(&session_id, &session.cwd);
        }
    }

    fn handle_session_new(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let session_id = self.next_session_id();
        self.insert_session(session_id.clone(), cwd, SessionInfo::default());
        let session = self
            .session_item_json(&session_id, "live", None)
            .unwrap_or_else(|| serde_json::json!({"sessionId": session_id}));

        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "session": session,
                "modes": modes::session_mode_state(modes::DEFAULT_MODE_ID),
                "configOptions": self.config_options_for_session(&session_id, modes::DEFAULT_MODE_ID),
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

    fn begin_profile_turn(&mut self, session_id: &str) -> u64 {
        if !self.profile.is_enabled() {
            return 0;
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return 0;
        };
        session.profile_turn += 1;
        harn_vm::tracing::set_tracing_enabled(true);
        session.profile_turn
    }

    fn finish_profile_turn(&self, session_id: &str, turn: u64) {
        if turn == 0 || !self.profile.is_enabled() {
            return;
        }
        let spans = harn_vm::tracing::take_spans();
        let rollup = harn_vm::profile::build(&spans);
        if self.profile.text {
            eprintln!("[harn] ACP profile session={session_id} turn={turn}");
            eprint!("{}", harn_vm::profile::render(&rollup));
        }
        if let Some(path) = self.profile.json_path.as_ref() {
            if let Err(error) = append_profile_json_line(path, session_id, turn, &rollup) {
                eprintln!("warning: failed to write ACP profile: {error}");
            }
        }
    }

    fn handle_session_fork(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let src_id = session_id_param(params);
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

        let keep_first =
            match nonnegative_usize_param(params, &["keep_first", "keepFirst"], "keep_first") {
                Ok(value) => value,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
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
        meta.insert("parent_id".to_string(), serde_json::json!(src_id));
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
        let parent_budget = self
            .sessions
            .get(&src_id)
            .map(|session| session.budget.clone())
            .unwrap_or_default();
        let cancellation = self.register_session_cancellation(&new_session_id);
        self.sessions.insert(
            new_session_id.clone(),
            Session {
                cwd: src_cwd,
                cancellation,
                host_bridge: None,
                inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
                info: info.clone(),
                advertised_commands: Vec::new(),
                current_mode_id: parent_mode_id.clone(),
                budget: parent_budget,
                profile_turn: 0,
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
                "configOptions": self.config_options_for_session(&new_session_id, &parent_mode_id),
            }),
        );
    }

    fn handle_session_truncate(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing sessionId");
            return;
        };
        let keep_first =
            match nonnegative_usize_param(params, &["keepFirst", "keep_first"], "keepFirst") {
                Ok(Some(value)) => value,
                Ok(None) => {
                    self.send_error(id, -32602, "Missing keepFirst");
                    return;
                }
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
            };
        let Some(cancellation) = self
            .sessions
            .get(&session_id)
            .map(|session| session.cancellation.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        };

        cancellation.cancel();
        if !harn_vm::agent_sessions::exists(&session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        }
        let Some(result) = harn_vm::agent_sessions::truncate(&session_id, keep_first) else {
            self.send_error(
                id,
                -32000,
                &format!("Failed to truncate session: {session_id}"),
            );
            return;
        };

        let mut update = serde_json::json!({
            "sessionUpdate": "session_truncated",
            "keptTurnCount": result.kept_turn_count,
            "removedTurnCount": result.removed_turn_count,
            "newTipTurnId": result.new_tip_turn_id,
        });
        if let Some(reason) = params.get("reason").and_then(|value| value.as_str()) {
            update["reason"] = serde_json::json!(reason);
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "keptTurnCount": result.kept_turn_count,
                "removedTurnCount": result.removed_turn_count,
                "newTipTurnId": result.new_tip_turn_id,
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

        let (cwd, cancellation, current_mode_id, inject_state, session_budget) =
            match self.sessions.get_mut(&session_id) {
                Some(s) => {
                    s.cancellation.begin_prompt();
                    s.host_bridge = None;
                    (
                        s.cwd.clone(),
                        s.cancellation.clone(),
                        s.current_mode_id.clone(),
                        s.inject_state.clone(),
                        s.budget.clone(),
                    )
                }
                None => {
                    self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
                    return;
                }
            };
        let prompt_budget = match session_budget {
            SessionBudget::Inherit => self.default_budget.clone(),
            SessionBudget::Unlimited => None,
            SessionBudget::Custom(spec) => Some(spec),
        };
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
        #[cfg(feature = "hostlib")]
        harn_hostlib::fs::configure_session_root(&session_id, &cwd);

        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if Path::new(pipeline_path).is_absolute() {
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

        let bridge = Arc::new(AcpBridge {
            session_id: sid.clone(),
            output: output.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancellation: cancellation.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let bridge_output = output.clone();
        let host_bridge = Arc::new(
            harn_vm::bridge::HostBridge::from_parts_with_writer_cancel_notify_and_injection_state(
                bridge.pending.clone(),
                cancellation.cancelled.clone(),
                cancellation.notify.clone(),
                Arc::new(move |line| {
                    bridge_output.write_line(line);
                    Ok(())
                }),
                bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
                Some(inject_state),
            ),
        );
        host_bridge.set_session_id(&bridge.session_id);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = Some(host_bridge.clone());
        }

        let compile_started = Instant::now();
        let (chunk, cache_hit) = match self.compile_pipeline_cached(
            &source,
            source_path.as_deref(),
            target_pipeline.as_deref(),
        ) {
            Ok(value) => value,
            Err(message) => {
                // Drop the error's "Compilation error: " prefix added inside
                // the helper — the caller used to format it identically.
                let formatted = message
                    .strip_prefix("Compilation error: ")
                    .map(|rest| format!("Compilation error: {rest}"))
                    .unwrap_or(message);
                self.clear_active_prompt_transport(&session_id);
                self.send_prompt_error(&session_id, id, &formatted);
                return;
            }
        };
        let compile_ms = compile_started.elapsed().as_millis() as u64;
        bridge.send_log(
            "info",
            &format!(
                "ACP_BOOT: compile_ms={compile_ms} cache={}",
                if cache_hit { "hit" } else { "miss" }
            ),
            Some(serde_json::json!({
                "compile_ms": compile_ms,
                "compile_cache": if cache_hit { "hit" } else { "miss" },
                "pipeline": source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<inline>".to_string()),
            })),
        );
        let profile_turn = self.begin_profile_turn(&session_id);
        let _mode_guard = modes::ModePolicyGuard::enter(&current_mode_id, &self.read_only_roots);
        let (vm_baseline, vm_baseline_cache_hit, vm_baseline_prepare_ms) = match self
            .prepare_vm_baseline_cached(
                &source,
                source_path.as_deref(),
                target_pipeline.as_deref(),
                &cwd,
                &current_mode_id,
            )
            .await
        {
            Ok(value) => value,
            Err(message) => {
                self.finish_profile_turn(&session_id, profile_turn);
                self.clear_active_prompt_transport(&session_id);
                self.send_prompt_error(&session_id, id, &message);
                return;
            }
        };
        let id_owned = id.clone();
        let send_output = self.output.clone();
        let host_bridge_for_response = host_bridge.clone();
        let _budget_guard = prompt_budget.as_ref().and_then(BudgetSpec::install);
        let result = execute::execute_chunk(
            chunk,
            bridge.clone(),
            host_bridge,
            execute::PromptGlobals {
                text: &prompt_text,
                content: &prompt.content,
                messages: &prompt.messages,
            },
            execute::VmSetup {
                source: &source,
                baseline: vm_baseline.as_ref(),
                baseline_cache_hit: vm_baseline_cache_hit,
                baseline_prepare_ms: vm_baseline_prepare_ms,
                source_path: source_path.as_deref(),
                cwd: &cwd,
                runtime_configurator: self.runtime_configurator.clone(),
            },
        )
        .await;
        self.finish_profile_turn(&session_id, profile_turn);
        drop(_mode_guard);
        self.clear_active_prompt_transport(&session_id);

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

    fn handle_session_cancel(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = session_id_param(params) else {
            if !id.is_null() {
                self.send_error(id, -32602, "session/cancel requires sessionId");
            }
            return;
        };
        let Some(cancellation) =
            lookup_session_cancellation(&self.session_cancellations, &session_id)
        else {
            if !id.is_null() {
                self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            }
            return;
        };

        let actor = control_actor_from_params(params);
        let newly_cancelled = if cancellation.take_routed_cancel_ack() {
            true
        } else {
            cancellation.cancel()
        };
        let status = if newly_cancelled {
            "cancelled"
        } else {
            "already_cancelled"
        };
        self.emit_control_outcome(
            &session_id,
            "session/cancel",
            status,
            "accepted",
            actor.clone(),
            serde_json::json!({"sessionId": session_id}),
            None,
        );
        if !id.is_null() {
            self.send_response(
                id,
                serde_json::json!({
                    "sessionId": session_id,
                    "status": status,
                    "_meta": {
                        "harn": {
                            "actor": actor,
                        }
                    }
                }),
            );
        }
    }

    /// Targeted preemption: stop one in-flight tool call without tearing
    /// down the whole session. Mirrors the `cancel_in_flight_tool_call`
    /// Harn builtin so hosts have a single semantic across protocol and
    /// in-VM call sites.
    fn handle_session_cancel_tool_call(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "session/cancel_tool_call requires sessionId");
            return;
        };
        let call_id = params
            .get("toolCallId")
            .or_else(|| params.get("tool_call_id"))
            .or_else(|| params.get("callId"))
            .or_else(|| params.get("call_id"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if call_id.is_empty() {
            self.send_error(id, -32602, "session/cancel_tool_call requires toolCallId");
            return;
        }
        let reason = params
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("host cancelled in-flight tool call")
            .to_string();
        let inject_reminder = params
            .get("injectReminder")
            .or_else(|| params.get("inject_reminder"))
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let outcome =
            harn_vm::tool_call_cancellations::cancel(session_id, call_id, reason, inject_reminder);
        let tool_name = outcome
            .tool_name
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        self.send_response(
            id,
            serde_json::json!({
                "status": outcome.status.as_str(),
                "callId": call_id,
                "tool": tool_name,
            }),
        );
    }

    fn handle_session_close(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return;
        };

        let Some(session) = self.sessions.remove(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return;
        };

        session.cancellation.cancel();
        self.session_cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
        self.inject_controls.remove(session_id);
        clear_session_sinks(session_id);
        #[cfg(feature = "hostlib")]
        {
            harn_hostlib::fs_snapshot::drop_session_snapshots(session_id);
        }
        harn_vm::agent_sessions::close_with_status(
            session_id,
            "client_request",
            "closed",
            serde_json::json!({
                "protocol": "acp",
                "method": method,
            }),
        );

        self.send_response(id, serde_json::json!({}));
    }

    async fn handle_session_inject(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/inject requires sessionId");
            return;
        };
        let Some(inject_state) = self.session_inject_state(id, params, "session/inject", true)
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        let mode = match bridge_mode_for_session_inject(params) {
            Ok(mode) => mode,
            Err(message) => {
                self.send_error(id, -32602, &message);
                self.emit_control_outcome(
                    &session_id,
                    "session/inject",
                    "rejected",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id}),
                    Some("invalid_mode"),
                );
                return;
            }
        };
        let (content, transcript_content) =
            match normalize_session_inject_content("session/inject", params) {
                Ok(content) => content,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    self.emit_control_outcome(
                        &session_id,
                        "session/inject",
                        "rejected",
                        "rejected",
                        actor,
                        serde_json::json!({"sessionId": session_id}),
                        Some("invalid_content"),
                    );
                    return;
                }
            };
        let message_id = inject_state
            .push_pending_user_message(content, transcript_content, mode)
            .await;
        self.inject_controls
            .entry(session_id.clone())
            .or_default()
            .insert(
                message_id.clone(),
                InjectControlRecord {
                    owner: actor.clone(),
                    status: "pending".to_string(),
                },
            );
        self.emit_control_outcome(
            &session_id,
            "session/inject",
            "accepted",
            "accepted",
            actor.clone(),
            serde_json::json!({"sessionId": session_id, "messageId": message_id}),
            None,
        );
        self.send_response(
            id,
            serde_json::json!({
                "messageId": message_id,
                "status": "accepted",
                "_meta": {
                    "harn": {
                        "actor": actor,
                    }
                }
            }),
        );
    }

    fn clear_active_prompt_transport(&mut self, session_id: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.host_bridge = None;
        }
        clear_session_sinks(session_id);
    }

    async fn handle_session_revoke_inject(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/revoke_inject requires sessionId");
            return;
        };
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/revoke_inject", false)
        else {
            return;
        };
        let Some(message_id) =
            self.pending_inject_message_id_param(id, params, "session/revoke_inject")
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        if let Some(owner) = self
            .inject_controls
            .get(&session_id)
            .and_then(|records| records.get(message_id))
            .map(|record| record.owner.clone())
        {
            if owner != actor && !actor_is_host_owner(&actor) {
                self.send_pending_inject_error_with_data(
                    id,
                    message_id,
                    "not_owner_or_not_authorized",
                    "pending inject is owned by another ACP actor",
                    serde_json::json!({
                        "actor": actor,
                        "owner": owner,
                    }),
                );
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "rejected",
                    "rejected",
                    control_actor_from_params(params),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("not_owner_or_not_authorized"),
                );
                return;
            }
        }
        match inject_state.revoke_pending_user_message(message_id).await {
            harn_vm::bridge::PendingUserMessageMutationResult::Mutated => {
                if let Some(record) = self
                    .inject_controls
                    .get_mut(&session_id)
                    .and_then(|records| records.get_mut(message_id))
                {
                    record.status = "revoked".to_string();
                }
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "revoked",
                    "accepted",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({"messageId": message_id, "status": "revoked"}),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyRevoked => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "already_revoked",
                    "idempotent",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({"messageId": message_id, "status": "already_revoked"}),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyDelivered => {
                if let Some(record) = self
                    .inject_controls
                    .get_mut(&session_id)
                    .and_then(|records| records.get_mut(message_id))
                {
                    record.status = "delivered".to_string();
                }
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "already_delivered",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_delivered"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_delivered",
                    "pending inject already delivered",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::UnknownMessageId => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "unknown_message_id",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("unknown_message_id"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "unknown_message_id",
                    "unknown pending inject messageId",
                );
            }
        }
    }

    async fn handle_session_replace_inject(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/replace_inject requires sessionId");
            return;
        };
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/replace_inject", false)
        else {
            return;
        };
        let Some(message_id) =
            self.pending_inject_message_id_param(id, params, "session/replace_inject")
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        if let Some(owner) = self
            .inject_controls
            .get(&session_id)
            .and_then(|records| records.get(message_id))
            .map(|record| record.owner.clone())
        {
            if owner != actor && !actor_is_host_owner(&actor) {
                self.send_pending_inject_error_with_data(
                    id,
                    message_id,
                    "not_owner_or_not_authorized",
                    "pending inject is owned by another ACP actor",
                    serde_json::json!({
                        "actor": actor,
                        "owner": owner,
                    }),
                );
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "rejected",
                    "rejected",
                    control_actor_from_params(params),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("not_owner_or_not_authorized"),
                );
                return;
            }
        }
        let (content, transcript_content) =
            match normalize_session_inject_content("session/replace_inject", params) {
                Ok(content) => content,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    self.emit_control_outcome(
                        &session_id,
                        "session/replace_inject",
                        "rejected",
                        "rejected",
                        actor,
                        serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                        Some("invalid_content"),
                    );
                    return;
                }
            };
        match inject_state
            .replace_pending_user_message(message_id, content, transcript_content)
            .await
        {
            harn_vm::bridge::PendingUserMessageMutationResult::Mutated => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "replaced",
                    "accepted",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({ "messageId": message_id, "status": "replaced" }),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyRevoked => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "already_revoked",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_revoked"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_revoked",
                    "pending inject already revoked",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyDelivered => {
                if let Some(record) = self
                    .inject_controls
                    .get_mut(&session_id)
                    .and_then(|records| records.get_mut(message_id))
                {
                    record.status = "delivered".to_string();
                }
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "already_delivered",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_delivered"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_delivered",
                    "pending inject already delivered",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::UnknownMessageId => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "unknown_message_id",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("unknown_message_id"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "unknown_message_id",
                    "unknown pending inject messageId",
                );
            }
        }
    }

    fn session_inject_state(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
        require_active_prompt: bool,
    ) -> Option<harn_vm::bridge::HostBridgeInjectionState> {
        let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return None;
        };
        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return None;
        };
        if require_active_prompt && session.host_bridge.is_none() {
            self.send_error(
                id,
                -32004,
                &format!("Session has no active prompt: {session_id}"),
            );
            return None;
        }
        Some(session.inject_state.clone())
    }

    fn pending_inject_message_id_param<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(message_id) = params.get("messageId").and_then(|v| v.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires messageId"));
            return None;
        };
        if message_id.trim().is_empty() {
            self.send_error(
                id,
                -32602,
                &format!("{method} requires non-empty messageId"),
            );
            return None;
        }
        Some(message_id)
    }

    fn pending_reminder_id_param<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(reminder_id) = params
            .get("reminderId")
            .or_else(|| params.get("reminder_id"))
            .and_then(|v| v.as_str())
        else {
            self.send_error(id, -32602, &format!("{method} requires reminderId"));
            return None;
        };
        if reminder_id.trim().is_empty() {
            self.send_error(
                id,
                -32602,
                &format!("{method} requires non-empty reminderId"),
            );
            return None;
        }
        Some(reminder_id)
    }

    fn send_pending_inject_error(
        &self,
        id: &serde_json::Value,
        message_id: &str,
        reason: &str,
        message: &str,
    ) {
        self.send_pending_inject_error_with_data(
            id,
            message_id,
            reason,
            message,
            serde_json::Value::Null,
        );
    }

    fn send_pending_inject_error_with_data(
        &self,
        id: &serde_json::Value,
        message_id: &str,
        reason: &str,
        message: &str,
        extra: serde_json::Value,
    ) {
        let mut data = serde_json::Map::new();
        data.insert("reason".to_string(), serde_json::json!(reason));
        data.insert("messageId".to_string(), serde_json::json!(message_id));
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                data.insert(key.clone(), value.clone());
            }
        }
        self.send_error_with_data(id, -32602, message, serde_json::Value::Object(data));
    }

    fn send_pending_reminder_error(
        &self,
        id: &serde_json::Value,
        reminder_id: &str,
        reason: &str,
        message: &str,
    ) {
        self.send_error_with_data(
            id,
            -32602,
            message,
            serde_json::json!({
                "reason": reason,
                "reminderId": reminder_id,
            }),
        );
    }

    async fn handle_session_remind(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            if !id.is_null() {
                self.send_error(id, -32602, "session/remind requires sessionId");
            }
            return;
        };
        let Some(session) = self.sessions.get(session_id) else {
            if !id.is_null() {
                self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            }
            return;
        };
        let Some(bridge) = session.host_bridge.clone() else {
            if !id.is_null() {
                self.send_error(
                    id,
                    -32004,
                    &format!("Session has no active bridge: {session_id}"),
                );
            }
            return;
        };
        match bridge.push_queued_session_remind_from_params(params).await {
            Ok(reminder_id) => {
                if !id.is_null() {
                    self.send_response(id, serde_json::json!({"reminderId": reminder_id}));
                }
            }
            Err(error) => {
                if !id.is_null() {
                    self.send_error(id, -32602, &format!("session/remind: {error}"));
                }
            }
        }
    }

    async fn handle_session_pending_injections(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/pending_injections", false)
        else {
            return;
        };
        self.send_response(id, inject_state.pending_injections_json().await);
    }

    async fn handle_session_revoke_reminder(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/revoke_reminder", false)
        else {
            return;
        };
        let Some(reminder_id) =
            self.pending_reminder_id_param(id, params, "session/revoke_reminder")
        else {
            return;
        };
        match inject_state.revoke_pending_reminder(reminder_id).await {
            harn_vm::bridge::PendingReminderMutationResult::Mutated => {
                self.send_response(
                    id,
                    serde_json::json!({"reminderId": reminder_id, "status": "revoked"}),
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::AlreadyRevoked => {
                self.send_response(
                    id,
                    serde_json::json!({"reminderId": reminder_id, "status": "already_revoked"}),
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::AlreadyDelivered => {
                self.send_pending_reminder_error(
                    id,
                    reminder_id,
                    "already_delivered",
                    "pending reminder already delivered",
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::UnknownReminderId => {
                self.send_pending_reminder_error(
                    id,
                    reminder_id,
                    "unknown_reminder_id",
                    "unknown pending reminderId",
                );
            }
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

    fn handle_session_list(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .filter(|(sid, session)| self.session_matches_list_filters(sid, session, params))
            .filter_map(|(sid, _)| self.session_item_json(sid, "live", None))
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }

    /// `mcp/catalog`: project the persisted enable/disable allowlist (plus
    /// optional per-project overlay) onto the advertised MCP items and
    /// return the effective catalog (servers → items + `enabled`). The
    /// merge/projection is harn-owned so thin clients (the burin-code TUI /
    /// GUI) render the toggle UI without storing any toggle state. See
    /// `harn_vm::mcp_allowlist`.
    fn handle_mcp_catalog(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let request: harn_vm::McpCatalogRequest = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                self.send_error(id, -32602, &format!("Invalid mcp/catalog params: {error}"));
                return;
            }
        };
        let catalog = harn_vm::mcp_catalog_for_request(&request);
        match serde_json::to_value(&catalog) {
            Ok(value) => self.send_response(id, value),
            Err(error) => self.send_error(
                id,
                -32000,
                &format!("failed to encode mcp catalog: {error}"),
            ),
        }
    }

    /// `mcp/authorize`: begin an interactive OAuth authorization for an MCP
    /// server. harn does discovery + client resolution + PKCE, registers the
    /// pending flow, and returns the browser URL plus the `state` the matching
    /// `mcp/oauth_callback` must echo. The client opens `authorizeUrl`; the
    /// redirect's `code`+`state` come back via `mcp/oauth_callback`. Token
    /// exchange and storage stay in harn.
    async fn handle_mcp_authorize(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(url) = params
            .get("url")
            .or_else(|| params.get("resource"))
            .and_then(|value| value.as_str())
        else {
            self.send_error(
                id,
                -32602,
                "mcp/authorize requires url (the MCP server URL)",
            );
            return;
        };
        let redirect_uri = params
            .get("redirectUri")
            .and_then(|value| value.as_str())
            .unwrap_or(MCP_DEFAULT_OAUTH_REDIRECT_URI)
            .to_string();
        let string_param = |key: &str| {
            params
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        // An explicit auth mode (cimd/dcr/static/byo) is optional; harn
        // auto-selects (CIMD-default) when the client omits it.
        let mode = match params.get("mode").and_then(|value| value.as_str()) {
            Some(raw) => match serde_json::from_value(serde_json::json!(raw)) {
                Ok(mode) => Some(mode),
                Err(_) => {
                    self.send_error(
                        id,
                        -32602,
                        "mcp/authorize: invalid mode (expected cimd|dcr|static|byo)",
                    );
                    return;
                }
            },
            None => None,
        };
        let request = harn_vm::mcp_oauth::BeginAuthorization {
            server_url: url.to_string(),
            redirect_uri,
            mode,
            client_id: string_param("clientId"),
            client_secret: string_param("clientSecret"),
            static_secret_id: string_param("staticSecretId"),
            scopes: string_param("scope"),
        };
        match harn_vm::mcp_oauth::begin_authorization(request).await {
            Ok(pending) => self.send_response(
                id,
                serde_json::json!({
                    "authorizeUrl": pending.authorize_url,
                    "state": pending.state,
                    "redirectUri": pending.redirect_uri,
                    "resource": pending.resource,
                    "issuer": pending.issuer,
                }),
            ),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    /// `mcp/oauth_callback`: complete an authorization begun by `mcp/authorize`.
    /// Accepts either explicit `state`+`code` (+optional `issuer`) or a full
    /// `redirectUrl` (the captured `burin://…?code=…&state=…&iss=…`) to parse
    /// them from. harn exchanges the code and stores the token.
    async fn handle_mcp_oauth_callback(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let parsed = params
            .get("redirectUrl")
            .and_then(|value| value.as_str())
            .map(parse_oauth_redirect_url);
        let (state, code, issuer) = match parsed {
            Some(Ok(parts)) => parts,
            Some(Err(error)) => {
                self.send_error(id, -32602, &error);
                return;
            }
            None => {
                let field = |key: &str| {
                    params
                        .get(key)
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                };
                let (Some(state), Some(code)) = (field("state"), field("code")) else {
                    self.send_error(
                        id,
                        -32602,
                        "mcp/oauth_callback requires state and code (or redirectUrl)",
                    );
                    return;
                };
                (state, code, field("issuer"))
            }
        };
        match harn_vm::mcp_oauth::complete_authorization(&state, &code, issuer.as_deref()).await {
            Ok(token) => self.send_response(
                id,
                serde_json::json!({
                    "ok": true,
                    "resource": token.resource,
                    "issuer": token.issuer,
                    "expiresAt": token.expires_at_unix,
                }),
            ),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    /// `mcp/import_token`: migrate a token minted by an older client-specific
    /// OAuth implementation into harn's canonical MCP OAuth store. Discovery,
    /// issuer binding, resource canonicalization, and keyring layout remain
    /// harn-owned; clients only hand over the legacy token material once.
    async fn handle_mcp_import_token(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let request: McpImportTokenParams = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid mcp/import_token params: {error}"),
                );
                return;
            }
        };
        let import = harn_vm::mcp_oauth::ImportStoredToken {
            server_url: request.url,
            access_token: request.access_token,
            refresh_token: request.refresh_token,
            expires_at_unix: request.expires_at,
            token_endpoint: request.token_endpoint,
            client_id: request.client_id,
            client_secret: request.client_secret,
            token_endpoint_auth_method: request.token_endpoint_auth_method,
            scopes: request.scope,
        };
        match harn_vm::mcp_oauth::import_stored_token(import).await {
            Ok(token) => self.send_response(
                id,
                serde_json::json!({
                    "ok": true,
                    "resource": token.resource,
                    "issuer": token.issuer,
                    "expiresAt": token.expires_at_unix,
                }),
            ),
            Err(error) => self.send_error(id, -32000, &error),
        }
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

    fn session_restore_result(&self, session_id: &str) -> Option<serde_json::Value> {
        let session = self.sessions.get(session_id)?;
        let session_value = self.session_item_json(session_id, "live", None)?;
        Some(serde_json::json!({
            "session": session_value,
            "modes": modes::session_mode_state(&session.current_mode_id),
            "configOptions": self.config_options_for_session(session_id, &session.current_mode_id),
        }))
    }

    fn session_item_json(
        &self,
        session_id: &str,
        live_state: &str,
        last_event_id: Option<u64>,
    ) -> Option<serde_json::Value> {
        let session = self.sessions.get(session_id)?;
        let workspace_anchor = harn_vm::agent_sessions::workspace_anchor(session_id);
        let snapshot = harn_vm::agent_sessions::snapshot(session_id)
            .map(|value| harn_vm::llm::vm_value_to_json(&value))
            .unwrap_or(serde_json::Value::Null);
        let active_prompt = session.host_bridge.is_some();
        let attachable_roles = serde_json::json!(["host_owner"]);
        let mut item = serde_json::json!({
            "sessionId": session_id,
            "cwd": session.cwd.display().to_string(),
            "liveState": live_state,
            "attachableRoles": attachable_roles,
            "currentModeId": session.current_mode_id,
            "activePrompt": active_prompt,
        });
        if let Some(created_at) = snapshot.get("created_at").cloned() {
            item["createdAt"] = created_at;
        }
        if let Some(last_event_id) = last_event_id {
            item["lastEventId"] = serde_json::json!(last_event_id);
        }
        if let Some(title) = session.info.title.as_ref() {
            item["title"] = serde_json::json!(title);
        }
        if let Some(anchor) = workspace_anchor.as_ref() {
            item["workspaceAnchor"] = anchor.to_json();
        }

        let mut meta = session.info.meta.clone();
        let mut harn_meta = match meta.remove("harn") {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        harn_meta.insert("liveState".to_string(), serde_json::json!(live_state));
        harn_meta.insert(
            "attachableRoles".to_string(),
            item["attachableRoles"].clone(),
        );
        harn_meta.insert(
            "currentModeId".to_string(),
            serde_json::json!(session.current_mode_id),
        );
        harn_meta.insert("activePrompt".to_string(), serde_json::json!(active_prompt));
        if let Some(last_event_id) = last_event_id {
            harn_meta.insert("lastEventId".to_string(), serde_json::json!(last_event_id));
        }
        if let Some(anchor) = workspace_anchor {
            harn_meta.insert("workspaceAnchor".to_string(), anchor.to_json());
        }
        meta.insert("harn".to_string(), serde_json::Value::Object(harn_meta));
        item["_meta"] = serde_json::Value::Object(meta);
        Some(item)
    }

    fn session_matches_list_filters(
        &self,
        session_id: &str,
        session: &Session,
        params: &serde_json::Value,
    ) -> bool {
        if let Some(cwd) = session_cwd_filter(params) {
            if session.cwd.to_string_lossy() != cwd {
                return false;
            }
        }
        let live_state = "live";
        let state_filter = session_live_state_filter(params);
        if !live_state_filter_matches(live_state, state_filter.as_deref()) {
            return false;
        }
        let workspace_anchor = harn_vm::agent_sessions::workspace_anchor(session_id);
        workspace_anchor_filter_matches(
            workspace_anchor.as_ref(),
            session_workspace_anchor_filter(params),
        )
    }

    fn restored_session_id<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return None;
        };

        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("unknown session: {session_id}"));
            return None;
        }

        harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        Some(session_id)
    }

    async fn handle_session_load(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = self.restored_session_id(id, params, "session/load") else {
            return;
        };

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

        let mut result = self
            .session_restore_result(session_id)
            .expect("validated session should still exist");
        result["replayed"] = serde_json::json!(replayed);
        self.send_response(id, result);
    }

    fn handle_session_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = self.restored_session_id(id, params, "session/resume") else {
            return;
        };
        let result = self
            .session_restore_result(session_id)
            .expect("validated session should still exist");
        self.send_response(id, result);
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

    /// Pin (or clear, with `None`) the LLM model selector for `session_id`.
    /// Returns `Ok(true)` when the value changed so callers can decide
    /// whether to broadcast a `config_option_update` notification.
    ///
    /// The harn-vm session is auto-created if it doesn't exist yet (e.g.
    /// when a client pins a model before its first prompt), keeping
    /// the wire surface order-independent.
    fn set_session_model(
        &mut self,
        session_id: &str,
        model: Option<String>,
    ) -> Result<bool, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("Unknown session: {session_id}"));
        }
        if !harn_vm::agent_sessions::exists(session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        }
        harn_vm::agent_sessions::set_pinned_model(session_id, model)
    }

    /// Read the currently pinned model for `session_id`, if any. Returns
    /// `None` for unknown sessions or sessions running on the ambient
    /// default — both are indistinguishable on the wire.
    fn pinned_model(&self, session_id: &str) -> Option<String> {
        harn_vm::agent_sessions::pinned_model(session_id)
    }

    /// Pin (or clear) the provider-aware reasoning policy for `session_id`.
    fn set_session_reasoning_policy(
        &mut self,
        session_id: &str,
        policy: Option<String>,
    ) -> Result<bool, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("Unknown session: {session_id}"));
        }
        if !harn_vm::agent_sessions::exists(session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        }
        harn_vm::agent_sessions::set_pinned_reasoning_policy(session_id, policy)
    }

    fn pinned_reasoning_policy(&self, session_id: &str) -> Option<String> {
        harn_vm::agent_sessions::pinned_reasoning_policy(session_id)
    }

    fn session_budget_config_value(&self, session_id: &str) -> Option<String> {
        match &self.sessions.get(session_id)?.budget {
            SessionBudget::Inherit => None,
            SessionBudget::Unlimited => Some(modes::BUDGET_OFF_VALUE.to_string()),
            SessionBudget::Custom(spec) => Some(budget_config_value(spec)),
        }
    }

    fn config_options_for_session(&self, session_id: &str, mode_id: &str) -> serde_json::Value {
        let budget_value = self.session_budget_config_value(session_id);
        modes::config_options_state(
            mode_id,
            self.pinned_model(session_id).as_deref(),
            self.pinned_reasoning_policy(session_id).as_deref(),
            budget_value.as_deref(),
        )
    }

    fn set_session_budget(
        &mut self,
        session_id: &str,
        budget: SessionBudget,
    ) -> Result<bool, String> {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.budget == budget {
            return Ok(false);
        }
        session.budget = budget;
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
                    "configOptions": self.config_options_for_session(session_id, mode_id),
                },
            }),
        );
    }

    #[cfg(feature = "hostlib")]
    fn emit_staged_writes_update(&self, session_id: &str) {
        let Ok(status) = harn_hostlib::fs::staged_status(session_id) else {
            return;
        };
        let mut update = bridge::progress_update(
            "fs_staging",
            "staged writes pending",
            Some(status.pending_writes.len() as i64),
            None,
            None,
        );
        let mut harn_meta = serde_json::Map::new();
        harn_meta.insert(
            "kind".to_string(),
            serde_json::Value::String("staged_writes_pending".to_string()),
        );
        harn_meta.insert(
            "pendingCount".to_string(),
            serde_json::Value::from(status.pending_writes.len() as u64),
        );
        harn_meta.insert(
            "totalBytes".to_string(),
            serde_json::Value::from(status.total_bytes_pending),
        );
        events::merge_harn_meta(&mut update, harn_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }

    #[cfg(feature = "hostlib")]
    fn handle_session_fs_mode(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/fs_mode requires sessionId");
            return;
        };
        let Some(mode_raw) = params.get("mode").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/fs_mode requires mode");
            return;
        };
        let mode = match mode_raw {
            "immediate" => harn_hostlib::fs::FsMode::Immediate,
            "staged" => harn_hostlib::fs::FsMode::Staged,
            other => {
                self.send_error(
                    id,
                    -32602,
                    &format!("session/fs_mode mode must be immediate or staged, got {other}"),
                );
                return;
            }
        };
        let Some(cwd) = self
            .sessions
            .get(session_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        };
        match harn_hostlib::fs::set_mode(session_id, mode, Some(&cwd)) {
            Ok(result) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "previousMode": result.previous_mode.as_str(),
                        "mode": mode.as_str(),
                    }),
                );
                self.emit_staged_writes_update(session_id);
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    fn handle_session_fs_mode(&mut self, id: &serde_json::Value, _params: &serde_json::Value) {
        self.send_error(id, -32601, "session/fs_mode requires the hostlib feature");
    }

    #[cfg(feature = "hostlib")]
    fn handle_session_fs_commit_staged(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/fs_commit_staged requires sessionId");
            return;
        };
        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        }
        let paths = params
            .get("paths")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match harn_hostlib::fs::commit_staged(session_id, &paths) {
            Ok(result) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "committedPaths": result.committed_paths,
                        "failedPathsWithReasons": result
                            .failed_paths_with_reasons
                            .into_iter()
                            .map(|(path, reason)| serde_json::json!({
                                "path": path,
                                "reason": reason,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                );
                self.emit_staged_writes_update(session_id);
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    fn handle_session_fs_commit_staged(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(
            id,
            -32601,
            "session/fs_commit_staged requires the hostlib feature",
        );
    }

    #[cfg(feature = "hostlib")]
    fn handle_session_restore_tool_call(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/restore_tool_call requires sessionId");
            return;
        };
        let Some(tool_call_id) = params
            .get("toolCallId")
            .or_else(|| params.get("tool_call_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/restore_tool_call requires toolCallId");
            return;
        };
        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        }
        let paths = params
            .get("paths")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match harn_hostlib::fs_snapshot::restore(session_id, tool_call_id, &paths) {
            Ok(result) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "toolCallId": &result.snapshot_id,
                        "restoredPaths": &result.restored_paths,
                        "skippedPathsWithReasons": result
                            .skipped_paths_with_reasons
                            .iter()
                            .map(|(path, reason)| serde_json::json!({
                                "path": path,
                                "reason": reason,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                );
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": &result.snapshot_id,
                    "status": "restored",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "kind".to_string(),
                    serde_json::Value::String("tool_call_restored".to_string()),
                );
                harn_meta.insert(
                    "restoredPaths".to_string(),
                    serde_json::to_value(&result.restored_paths).unwrap_or_default(),
                );
                if !result.skipped_paths_with_reasons.is_empty() {
                    harn_meta.insert(
                        "skippedPathsWithReasons".to_string(),
                        serde_json::to_value(
                            result
                                .skipped_paths_with_reasons
                                .iter()
                                .map(|(path, reason)| {
                                    serde_json::json!({
                                        "path": path,
                                        "reason": reason,
                                    })
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    );
                }
                events::merge_harn_meta(&mut update, harn_meta);
                self.send_notification(
                    "session/update",
                    serde_json::json!({
                        "sessionId": session_id,
                        "update": update,
                    }),
                );
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    fn handle_session_restore_tool_call(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(
            id,
            -32601,
            "session/restore_tool_call requires the hostlib feature",
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
        let Some(value) = params.get("value").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires value");
            return;
        };

        let session_id = session_id.to_string();
        match config_id {
            "mode" => self.apply_set_mode_config_option(id, &session_id, value),
            "model" => self.apply_set_model_config_option(id, &session_id, value),
            "thought_level" | "reasoning_policy" => {
                self.apply_set_reasoning_policy_config_option(id, &session_id, value);
            }
            "budget" => self.apply_set_budget_config_option(id, &session_id, value),
            other => self.send_error(
                id,
                -32602,
                &format!(
                    "Unknown config option '{other}'. Available: mode, model, thought_level, budget"
                ),
            ),
        }
    }

    fn apply_set_mode_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        mode_id: &str,
    ) {
        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, mode_id),
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

    fn apply_set_model_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let normalized = match modes::validate_model_selector(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_model(session_id, normalized) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    fn apply_set_reasoning_policy_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let normalized = match modes::validate_reasoning_policy_selector(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_reasoning_policy(session_id, normalized) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    fn apply_set_budget_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let budget = match parse_budget_config_value(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_budget(session_id, budget) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    /// Dispatch one incoming ACP JSON-RPC message.
    ///
    /// The same router backs stdio, WebSocket, and in-process channel
    /// transports. `msg` must be either a request/notification with `method`
    /// or a response with `id` for a pending host callback.
    pub async fn handle_incoming_message(&mut self, msg: serde_json::Value) {
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
            HARN_PROVIDER_CATALOG_METHOD => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_provider_catalog(&id);
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
            "session/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_resume(&id, &params);
            }
            "session/fork" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fork(&id, &params);
            }
            "session/truncate" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_truncate(&id, &params);
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
            "session/fs_mode" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fs_mode(&id, &params);
            }
            "session/fs_commit_staged" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fs_commit_staged(&id, &params);
            }
            "session/restore_tool_call" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_restore_tool_call(&id, &params);
            }
            "session/prompt" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_prompt(&id, &params).await;
            }
            "session/cancel" => {
                self.handle_session_cancel(&id, &params);
            }
            "session/cancel_tool_call" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_cancel_tool_call(&id, &params);
            }
            "session/close" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_close(&id, &params, "session/close");
            }
            "session/stop" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                tracing::warn!("ACP method session/stop is deprecated; use session/close instead");
                eprintln!(
                    "warning: ACP method session/stop is deprecated; use session/close instead"
                );
                self.handle_session_close(&id, &params, "session/stop");
            }
            "session/inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_inject(&id, &params).await;
            }
            "session/revoke_inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_revoke_inject(&id, &params).await;
            }
            "session/replace_inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_replace_inject(&id, &params).await;
            }
            "session/remind" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_remind(&id, &params).await;
            }
            "session/pending_injections" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_pending_injections(&id, &params).await;
            }
            "session/revoke_reminder" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_revoke_reminder(&id, &params).await;
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
                self.handle_session_list(&id, &params);
            }
            "mcp/catalog" | "harn.mcp.catalog" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_catalog(&id, &params);
            }
            "mcp/authorize" | "harn.mcp.authorize" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_authorize(&id, &params).await;
            }
            "mcp/oauth_callback" | "harn.mcp.oauth_callback" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_oauth_callback(&id, &params).await;
            }
            "mcp/import_token" | "harn.mcp.import_token" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_import_token(&id, &params).await;
            }
            _ => {
                if !id.is_null() {
                    self.send_error(&id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}
