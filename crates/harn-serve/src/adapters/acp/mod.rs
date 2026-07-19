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
mod checkpoints;
mod commands;
mod core;
mod dispatch;
mod events;
mod execute;
mod inject;
mod integrations;
mod io;
mod modes;
mod prompt;
mod schema;
mod session;
mod session_state;
mod sessions;
mod setup;
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
    prepare_session_prompt, session_project_root_for_cwd, Session, SessionBudget,
    SessionCancellation, SessionInfo,
};
pub(crate) use transport::run_acp_channel_server_with_existing_handle;
pub use transport::{
    run_acp_channel_server, run_acp_channel_server_with_handle, run_acp_server,
    run_acp_websocket_server, AcpChannelHandle, AcpWebSocketServeOptions,
};
pub use types::{
    AcpContentBlock, AcpEmbeddedResource, AcpHarnMeta, AcpJsonRpcError, AcpJsonRpcErrorResponse,
    AcpJsonRpcId, AcpJsonRpcRequest, AcpJsonRpcResponse, AcpMeta, AcpPromptErrorData,
    AcpPromptErrorSchema, AcpSessionCancelToolCallParams, AcpSessionIdParams,
    AcpSessionInjectContent, AcpSessionInjectHostEventParams, AcpSessionInjectMode,
    AcpSessionInjectParams, AcpSessionMessageIdParams, AcpSessionNewParams,
    AcpSessionProfileConfig, AcpSessionPromptParams, AcpSessionPromptResult,
    AcpSessionReplaceInjectParams, AcpSessionRestoreResult, ACP_METHOD_INITIALIZE,
    ACP_METHOD_SESSION_CANCEL, ACP_METHOD_SESSION_CANCEL_TOOL_CALL, ACP_METHOD_SESSION_CLOSE,
    ACP_METHOD_SESSION_INJECT, ACP_METHOD_SESSION_INJECT_HOST_EVENT, ACP_METHOD_SESSION_LOAD,
    ACP_METHOD_SESSION_NEW, ACP_METHOD_SESSION_PENDING_INJECTIONS, ACP_METHOD_SESSION_PROMPT,
    ACP_METHOD_SESSION_REPLACE_INJECT, ACP_METHOD_SESSION_RESUME, ACP_METHOD_SESSION_REVOKE_INJECT,
    ACP_PROMPT_ERROR_DATA_SCHEMA,
};

#[cfg(feature = "hostlib")]
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use futures::StreamExt;
use harn_vm::agent_events::{
    clear_session_sinks, flush_and_clear_session_sinks, flush_session_sinks, register_sink,
    AgentEventSink,
};
use harn_vm::visible_text::VisibleTextState;
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
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

/// Tracks the ACP actor that owns a pending injected message so that
/// revoke/replace requests can enforce ownership.
#[derive(Clone)]
struct InjectControlRecord {
    owner: serde_json::Value,
}

struct TimelineSubscription {
    session_id: Option<String>,
    handle: tokio::task::JoinHandle<()>,
}

fn string_param(params: &serde_json::Value, camel: &str, snake: &str) -> Option<String> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn session_id_param(params: &serde_json::Value) -> Option<String> {
    string_param(params, "sessionId", "session_id")
}

#[cfg(feature = "hostlib")]
fn staged_fs_paths_param(params: &serde_json::Value) -> Vec<String> {
    params
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_session_timeline_query(
    params: &serde_json::Value,
) -> Result<harn_vm::session_timeline::SessionTimelineQuery, String> {
    let source = params.get("query").unwrap_or(params);
    let mut query: harn_vm::session_timeline::SessionTimelineQuery =
        serde_json::from_value(source.clone())
            .map_err(|error| format!("invalid session timeline query: {error}"))?;
    if query.session_id.is_none() {
        query.session_id = session_id_param(params);
    }
    if query.run_id.is_none() {
        query.run_id = string_param(params, "runId", "run_id");
    }
    if query.run_path.is_none() {
        query.run_path = string_param(params, "runPath", "run_path");
    }
    if query.project_id.is_none() {
        query.project_id = string_param(params, "projectId", "project_id");
    }
    Ok(query)
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

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
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

fn mount_mode_param(
    params: &serde_json::Value,
) -> Result<Option<harn_vm::workspace_anchor::MountMode>, String> {
    let Some(raw) = params
        .get("mountMode")
        .or_else(|| params.get("mount_mode"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    harn_vm::workspace_anchor::MountMode::parse(&normalized).map(Some)
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

    /// Host-verified provider endpoints that travel with this server's
    /// runtime configuration rather than its serializable provider catalog.
    fn runtime_provider_endpoint_overrides(
        &self,
    ) -> harn_vm::llm_config::RuntimeProviderEndpointOverrides {
        Default::default()
    }
}

#[derive(Clone, Default)]
pub struct NoopAcpRuntimeConfigurator;

#[async_trait(?Send)]
impl AcpRuntimeConfigurator for NoopAcpRuntimeConfigurator {}

#[derive(Clone)]
struct EndpointOverrideRuntimeConfigurator {
    inner: Arc<dyn AcpRuntimeConfigurator>,
    endpoints: harn_vm::llm_config::RuntimeProviderEndpointOverrides,
}

#[async_trait(?Send)]
impl AcpRuntimeConfigurator for EndpointOverrideRuntimeConfigurator {
    async fn configure(
        &self,
        vm: &mut harn_vm::Vm,
        source_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        self.inner.configure(vm, source_path).await
    }

    fn runtime_provider_endpoint_overrides(
        &self,
    ) -> harn_vm::llm_config::RuntimeProviderEndpointOverrides {
        self.endpoints.clone()
    }
}

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
    pub sandbox: AcpSandboxConfig,
}

/// Filesystem policy the ACP embedder contributes to each sandboxed turn.
///
/// `read_only_roots` widens Harn file reads for host-owned assets such as
/// bundled pipelines. `process` is process-only and affects the OS child
/// process sandbox without granting Harn file builtins access to those paths.
#[derive(Clone, Debug, Default)]
pub struct AcpSandboxConfig {
    pub read_only_roots: Vec<String>,
    pub process: harn_vm::orchestration::ProcessSandboxPolicy,
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
            sandbox: AcpSandboxConfig::default(),
        }
    }

    pub fn for_pipeline(path: impl Into<String>) -> Self {
        Self::new(Some(path.into()))
    }

    pub fn with_runtime_configurator(
        mut self,
        runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    ) -> Self {
        let endpoints = self
            .runtime_configurator
            .runtime_provider_endpoint_overrides();
        self.runtime_configurator = if endpoints.is_empty() {
            runtime_configurator
        } else {
            Arc::new(EndpointOverrideRuntimeConfigurator {
                inner: runtime_configurator,
                endpoints,
            })
        };
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

    /// Route one provider through a host-verified endpoint for this ACP
    /// server. The endpoint is scoped to requests and never enters the TOML
    /// catalog or provider-catalog projection.
    pub fn with_runtime_provider_endpoint(
        mut self,
        provider: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, String> {
        let mut endpoints = self
            .runtime_configurator
            .runtime_provider_endpoint_overrides();
        endpoints.insert(provider, base_url)?;
        self.runtime_configurator = Arc::new(EndpointOverrideRuntimeConfigurator {
            inner: self.runtime_configurator,
            endpoints,
        });
        Ok(self)
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

    pub fn with_sandbox(mut self, sandbox: AcpSandboxConfig) -> Self {
        self.sandbox = sandbox.canonicalized();
        self
    }
}

impl AcpSandboxConfig {
    /// Whether the embedder actually contributed any sandbox configuration.
    ///
    /// A default (empty) config means "embedder said nothing" — the no-config
    /// path that must behave exactly as before. A non-default config (any
    /// read-only root, or any process preset/read/write root) is the signal
    /// that the embedder opted into confinement, which the ActAuto `code` mode
    /// now honors as a `Worktree`-level OS sandbox.
    pub fn is_configured(&self) -> bool {
        !self.read_only_roots.is_empty()
            || self.process.presets.is_some()
            || !self.process.read_roots.is_empty()
            || !self.process.write_roots.is_empty()
    }

    pub fn with_read_only_roots(roots: Vec<String>) -> Self {
        Self {
            read_only_roots: canonicalize_sandbox_roots(roots),
            process: harn_vm::orchestration::ProcessSandboxPolicy::default(),
        }
    }

    pub fn with_process(process: harn_vm::orchestration::ProcessSandboxPolicy) -> Self {
        Self {
            read_only_roots: Vec::new(),
            process,
        }
    }

    fn canonicalized(mut self) -> Self {
        self.read_only_roots = canonicalize_sandbox_roots(self.read_only_roots);
        self.process.read_roots = canonicalize_sandbox_roots(self.process.read_roots);
        self.process.write_roots = canonicalize_sandbox_roots(self.process.write_roots);
        self
    }
}

/// Canonicalize embedder-supplied sandbox roots, dropping blank entries.
/// Falls back to the trimmed input when canonicalization fails so a root
/// that does not exist on disk yet is still carried verbatim.
fn canonicalize_sandbox_roots(roots: Vec<String>) -> Vec<String> {
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
    /// Live timeline subscriptions created by Harn-specific ACP extension
    /// methods. Each task fans event-log appends into notifications.
    timeline_subscriptions: HashMap<String, TimelineSubscription>,
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
    /// Host-verified endpoint sidecar paired with the catalog overlay.
    runtime_provider_endpoint_overrides: harn_vm::llm_config::RuntimeProviderEndpointOverrides,
    llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    /// Server-level budget inherited by sessions unless they override it.
    default_budget: Option<BudgetSpec>,
    sandbox: AcpSandboxConfig,
    /// Active bulk-OAuth driver from the most recent `mcp/authorize_batch`
    /// (harn#3357). Held across requests so a follow-up `mcp/oauth_callback`
    /// whose `state` belongs to a batch flow routes through the driver
    /// (streaming `Exchanging`/`Connected` status notifications) instead of the
    /// single-URL completion path. `None` until the first batch is begun;
    /// replaced wholesale by each new batch.
    active_bulk_auth: std::sync::Mutex<Option<Arc<harn_vm::mcp_bulk_auth::McpBulkAuth>>>,
}

impl Drop for AcpServer {
    fn drop(&mut self) {
        for (_, subscription) in self.timeline_subscriptions.drain() {
            subscription.handle.abort();
        }
    }
}
