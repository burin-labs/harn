use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{stream, StreamExt};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot, Notify};
use uuid::Uuid;

use harn_vm::event_log::{EventLog, LogEvent, Topic};
use harn_vm::mcp_protocol;
use harn_vm::{append_secret_scan_audit, secret_scan_content, SecretFinding};

use crate::cli::{McpServeArgs, McpServeTransport, OrchestratorLocalArgs};
use crate::commands::orchestrator::common::{
    load_local_runtime, read_topic, synthetic_event_for_binding, trigger_fire, trigger_inspect_dlq,
    trigger_list, trigger_replay, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};
use crate::commands::orchestrator::inspect_data::{
    collect_orchestrator_inspect_data, OrchestratorInspectData,
};
use crate::commands::orchestrator::listener::ListenerAuth;
use crate::package::CollectedTriggerHandler;

use super::oauth_resource::{OAuthChallengeError, OAuthResourceServer, OAuthTokenError};
use super::prompts::FilePromptCatalog;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
const DEPRECATION_HEADER: &str = "deprecation";
const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const DEFAULT_TASK_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_TASK_TTL_MS: u64 = 60 * 60 * 1000;

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    config_path: PathBuf,
    state_dir: PathBuf,
    manifest_source: Arc<Mutex<String>>,
    auth: ListenerAuth,
    oauth: Option<OAuthResourceServer>,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    list_notify_tx: broadcast::Sender<JsonValue>,
    task_notify_tx: broadcast::Sender<McpTaskNotification>,
    tasks: Arc<Mutex<BTreeMap<String, McpTaskRecord>>>,
    _list_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

#[derive(Clone, Debug)]
struct McpTaskNotification {
    owner: String,
    message: JsonValue,
}

#[derive(Clone, Debug)]
struct McpTaskRecord {
    task: McpTaskState,
    result: Option<JsonValue>,
    notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
struct McpTaskState {
    task_id: String,
    owner: String,
    status: mcp_protocol::McpTaskStatus,
    status_message: Option<String>,
    created_at: String,
    last_updated_at: String,
    ttl: Option<u64>,
    poll_interval: Option<u64>,
}

impl McpTaskState {
    fn to_json(&self) -> JsonValue {
        let mut value = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttl": self.ttl,
        });
        if let Some(message) = &self.status_message {
            value["statusMessage"] = json!(message);
        }
        if let Some(poll_interval) = self.poll_interval {
            value["pollInterval"] = json!(poll_interval);
        }
        value
    }

    fn notification(&self) -> McpTaskNotification {
        McpTaskNotification {
            owner: self.owner.clone(),
            message: json!({
                "jsonrpc": "2.0",
                "method": mcp_protocol::METHOD_TASK_STATUS_NOTIFICATION,
                "params": self.to_json(),
            }),
        }
    }
}

#[derive(Clone, Debug)]
struct ConnectionState {
    initialized: bool,
    authenticated: bool,
    client_identity: String,
    protocol_version: String,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            initialized: false,
            authenticated: false,
            client_identity: "unknown".to_string(),
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        }
    }
}

struct HttpSession {
    state: Mutex<ConnectionState>,
    sse_tx: Mutex<Option<UnboundedSender<JsonValue>>>,
}

impl Default for HttpSession {
    fn default() -> Self {
        Self {
            state: Mutex::new(ConnectionState::default()),
            sse_tx: Mutex::new(None),
        }
    }
}

#[derive(Clone)]
struct RpcBridge {
    tx: mpsc::UnboundedSender<RpcRequest>,
}

struct RpcRequest {
    session: ConnectionState,
    request: JsonValue,
    response_tx: oneshot::Sender<(ConnectionState, JsonValue)>,
}

#[derive(Clone)]
struct HttpState {
    service: Arc<McpOrchestratorService>,
    rpc: RpcBridge,
    sessions: Arc<Mutex<HashMap<String, Arc<HttpSession>>>>,
    mcp_path: String,
    sse_path: String,
    messages_path: String,
}

#[derive(Clone, Copy)]
enum McpListChangeKind {
    Tools,
    Resources,
    Prompts,
}

impl McpListChangeKind {
    fn method(self) -> &'static str {
        match self {
            Self::Tools => "notifications/tools/list_changed",
            Self::Resources => "notifications/resources/list_changed",
            Self::Prompts => "notifications/prompts/list_changed",
        }
    }

    fn notification(self) -> JsonValue {
        json!({
            "jsonrpc": "2.0",
            "method": self.method(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
struct TriggerListEntry {
    trigger_id: String,
    kind: String,
    provider: String,
    when: Option<String>,
    handler: JsonValue,
    version: u32,
    state: String,
    metrics: harn_vm::TriggerMetricsSnapshot,
}

#[derive(Clone, Debug, Serialize)]
struct QueuePreviewEntry {
    event_id: u64,
    kind: String,
    occurred_at_ms: i64,
    headers: BTreeMap<String, String>,
    payload: JsonValue,
}

#[derive(Clone, Debug, Serialize)]
struct QueueSnapshot {
    dispatcher: harn_vm::DispatcherStatsSnapshot,
    inbox: TopicPreview,
    outbox: TopicPreview,
    attempts: TopicPreview,
    dlq: TopicPreview,
}

#[derive(Clone, Debug, Serialize)]
struct TopicPreview {
    count: usize,
    head: Vec<QueuePreviewEntry>,
}

#[derive(Clone, Debug, Serialize)]
struct InspectPayload {
    dispatcher: harn_vm::DispatcherStatsSnapshot,
    #[serde(flatten)]
    inspect: OrchestratorInspectData,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecordedTriggerEvent {
    binding_id: String,
    binding_version: u32,
    replay_of_event_id: Option<String>,
    event: harn_vm::TriggerEvent,
}

#[derive(Clone, Debug, Deserialize)]
struct TriggerFireRequest {
    trigger_id: String,
    #[serde(default)]
    payload: JsonValue,
}

#[derive(Clone, Debug, Deserialize)]
struct TriggerReplayRequest {
    event_id: String,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    steer_from: Option<String>,
    #[serde(default)]
    to_decision: Option<JsonValue>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    applied_by: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct DlqRetryRequest {
    entry_id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SecretScanRequest {
    content: String,
}

fn trigger_replay_steering_from_request(
    request: &TriggerReplayRequest,
) -> Result<Option<crate::commands::trigger::replay::ReplaySteering>, String> {
    let Some(step) = request.steer_from.as_ref() else {
        if request.to_decision.is_some()
            || request.reason.is_some()
            || request.applied_by.is_some()
            || request.scope.is_some()
        {
            return Err(
                "harn.trigger.replay: steer_from is required for replay steering fields"
                    .to_string(),
            );
        }
        return Ok(None);
    };
    let to_decision = request
        .to_decision
        .clone()
        .ok_or_else(|| "harn.trigger.replay: steer_from requires to_decision".to_string())?;
    crate::commands::trigger::replay::ReplaySteering::new(
        step.clone(),
        to_decision,
        request.reason.clone(),
        request.applied_by.clone(),
        request.scope.as_deref(),
    )
    .map(Some)
}

#[derive(Clone, Debug, Deserialize)]
struct TrustQueryRequest {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    tier: Option<harn_vm::AutonomyTier>,
    #[serde(default)]
    outcome: Option<harn_vm::TrustOutcome>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    grouped_by_trace: bool,
}

pub(crate) async fn run(args: &McpServeArgs) -> Result<(), String> {
    let service = Arc::new(McpOrchestratorService::new(args)?);
    match args.transport {
        McpServeTransport::Stdio => run_stdio(service).await,
        McpServeTransport::Http => run_http(service, args).await,
    }
}

impl McpOrchestratorService {
    fn new(args: &McpServeArgs) -> Result<Self, String> {
        Self::new_local(args.local.clone())
    }

    pub(crate) fn new_local(local: OrchestratorLocalArgs) -> Result<Self, String> {
        let manifest_source = std::fs::read_to_string(&local.config).map_err(|error| {
            format!(
                "failed to read manifest {}: {error}",
                local.config.display()
            )
        })?;
        let auth = ListenerAuth::from_env(false, None)?;
        let oauth = OAuthResourceServer::from_env()?;
        let project_root = local
            .config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let prompt_catalog = Arc::new(Mutex::new(FilePromptCatalog::discover(
            &project_root,
            &manifest_source,
        )));
        let manifest_source = Arc::new(Mutex::new(manifest_source));
        let (list_notify_tx, _) = broadcast::channel(64);
        let (task_notify_tx, _) = broadcast::channel(64);
        let list_watcher = start_list_change_watcher(
            project_root,
            local.config.clone(),
            manifest_source.clone(),
            prompt_catalog.clone(),
            list_notify_tx.clone(),
        );
        Ok(Self {
            config_path: local.config,
            state_dir: local.state_dir,
            manifest_source,
            auth,
            oauth,
            prompt_catalog,
            list_notify_tx,
            task_notify_tx,
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            _list_watcher: Arc::new(Mutex::new(list_watcher)),
        })
    }

    fn local_args(&self) -> OrchestratorLocalArgs {
        OrchestratorLocalArgs {
            config: self.config_path.clone(),
            state_dir: self.state_dir.clone(),
        }
    }

    pub(crate) fn notify_manifest_reloaded(&self) {
        if let Ok(manifest_source) = std::fs::read_to_string(&self.config_path) {
            self.refresh_manifest_derived_state(manifest_source);
        }
        self.notify_list_changed(&[
            McpListChangeKind::Tools,
            McpListChangeKind::Resources,
            McpListChangeKind::Prompts,
        ]);
    }

    fn refresh_manifest_derived_state(&self, manifest_source: String) {
        *self
            .manifest_source
            .lock()
            .expect("manifest source poisoned") = manifest_source.clone();
        let project_root = self
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let updated = FilePromptCatalog::discover(&project_root, &manifest_source);
        *self.prompt_catalog.lock().expect("prompt catalog poisoned") = updated;
    }

    fn notify_list_changed(&self, kinds: &[McpListChangeKind]) {
        for kind in kinds {
            let _ = self.list_notify_tx.send(kind.notification());
        }
    }

    fn subscribe_list_notifications(&self) -> broadcast::Receiver<JsonValue> {
        self.list_notify_tx.subscribe()
    }

    fn subscribe_task_notifications(&self) -> broadcast::Receiver<McpTaskNotification> {
        self.task_notify_tx.subscribe()
    }

    async fn handle_request(&self, session: &mut ConnectionState, request: JsonValue) -> JsonValue {
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        if method == "initialize" {
            return self.handle_initialize(id, session, &params);
        }

        if request.get("id").is_none() {
            return JsonValue::Null;
        }

        if !session.initialized && method != "ping" {
            return harn_vm::jsonrpc::error_response(id, -32002, "server not initialized");
        }

        match method {
            "initialized" => JsonValue::Null,
            "ping" => harn_vm::jsonrpc::response(id, json!({})),
            "logging/setLevel" => harn_vm::jsonrpc::response(id, json!({})),
            "tools/list" => self.handle_tools_list(id, &params),
            "tools/call" => self.handle_tools_call(id, session, &params).await,
            mcp_protocol::METHOD_TASKS_GET => self.handle_tasks_get(id, session, &params),
            mcp_protocol::METHOD_TASKS_RESULT => {
                self.handle_tasks_result(id, session, &params).await
            }
            mcp_protocol::METHOD_TASKS_LIST => self.handle_tasks_list(id, session, &params),
            mcp_protocol::METHOD_TASKS_CANCEL => self.handle_tasks_cancel(id, session, &params),
            "resources/list" => self.handle_resources_list(id, &params).await,
            "resources/read" => self.handle_resources_read(id, &params).await,
            "resources/templates/list" => self.handle_resource_templates_list(id, &params),
            "prompts/list" => self.handle_prompts_list(id, &params),
            "prompts/get" => self.handle_prompts_get(id, &params),
            _ if mcp_protocol::unsupported_latest_spec_method(method).is_some() => {
                mcp_protocol::unsupported_latest_spec_method_response(id, method)
                    .expect("checked unsupported MCP method")
            }
            _ => {
                harn_vm::jsonrpc::error_response(id, -32601, &format!("Method not found: {method}"))
            }
        }
    }

    fn handle_initialize(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let client_name = params
            .pointer("/clientInfo/name")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        let client_version = params
            .pointer("/clientInfo/version")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        session.client_identity = format!("{client_name}/{client_version}");
        session.protocol_version = params
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or(MCP_PROTOCOL_VERSION)
            .to_string();

        if initialize_api_key(params).is_some() {
            eprintln!(
                "[harn] warning: MCP initialize capabilities.harn.apiKey is deprecated; use HTTP Authorization: Bearer tokens with OAuth protected-resource metadata instead"
            );
        }

        if self.auth.has_api_keys() && !session.authenticated {
            let api_key = initialize_api_key(params);
            if api_key.is_none_or(|value| !self.auth.matches_api_key(value)) {
                return harn_vm::jsonrpc::error_response(id, -32001, "unauthorized");
            }
            session.authenticated = true;
        } else {
            session.authenticated = true;
        }
        session.initialized = true;

        harn_vm::jsonrpc::response(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": true },
                    "resources": { "listChanged": true },
                    "prompts": { "listChanged": true },
                    "logging": {},
                    "tasks": mcp_protocol::tasks_capability(),
                },
                "serverInfo": {
                    "name": "harn-orchestrator",
                    "title": "Harn Orchestrator MCP",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Expose Harn trigger and orchestrator controls over MCP."
            }),
        )
    }

    fn handle_prompts_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let prompts = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .list();
        paginated_list_response(id, "prompts/list", "prompts", params, prompts)
    }

    fn handle_prompts_get(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .get(name, &arguments);
        match result {
            Ok(value) => harn_vm::jsonrpc::response(id, value),
            Err(error)
                if error.starts_with("Unknown prompt")
                    || error.starts_with("Missing required argument")
                    || error.starts_with("prompt arguments") =>
            {
                harn_vm::jsonrpc::error_response(id, -32602, &error)
            }
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    fn handle_tools_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let tools = vec![
            tool_def(
                "harn.secret_scan",
                "Scan content for high-signal secrets before commit or PR-open flows. The `harn::secret_scan` alias is also accepted.",
                json!({
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string" },
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": [
                            "detector",
                            "source",
                            "title",
                            "line",
                            "column_start",
                            "column_end",
                            "start_offset",
                            "end_offset",
                            "redacted",
                            "fingerprint"
                        ],
                        "properties": {
                            "detector": { "type": "string" },
                            "source": { "type": "string" },
                            "title": { "type": "string" },
                            "line": { "type": "integer" },
                            "column_start": { "type": "integer" },
                            "column_end": { "type": "integer" },
                            "start_offset": { "type": "integer" },
                            "end_offset": { "type": "integer" },
                            "redacted": { "type": "string" },
                            "fingerprint": { "type": "string" },
                        },
                    },
                })),
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
            tool_def(
                "harn.trigger.fire",
                "Dispatch a trigger inline and return its event id plus terminal status.",
                json!({
                    "type": "object",
                    "required": ["trigger_id", "payload"],
                    "properties": {
                        "trigger_id": { "type": "string" },
                        "payload": {},
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "object",
                    "required": ["event_id", "status"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "status": { "type": "string" },
                    },
                })),
                mcp_protocol::McpToolTaskSupport::Optional,
            ),
            tool_def(
                "harn.trigger.list",
                "List registered triggers and their kind/provider/when/handler metadata.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
            tool_def(
                "harn.trigger.replay",
                "Replay an existing trigger event, optionally resolving bindings as of a historical timestamp or recording a teaching correction.",
                json!({
                    "type": "object",
                    "required": ["event_id"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "as_of": { "type": "string" },
                        "steer_from": { "type": "string" },
                        "to_decision": {},
                        "reason": { "type": "string" },
                        "applied_by": { "type": "string" },
                        "scope": {
                            "type": "string",
                            "enum": ["this_run", "this_persona", "all"],
                        },
                    },
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Optional,
            ),
            tool_def(
                "harn.orchestrator.queue",
                "Return inbox/outbox/attempt/DLQ counts plus recent previews.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
            tool_def(
                "harn.orchestrator.dlq.list",
                "List pending dead-letter queue entries.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
            tool_def(
                "harn.orchestrator.dlq.retry",
                "Replay a pending dead-letter queue entry.",
                json!({
                    "type": "object",
                    "required": ["entry_id"],
                    "properties": {
                        "entry_id": { "type": "string" },
                    },
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Optional,
            ),
            tool_def(
                "harn.orchestrator.inspect",
                "Snapshot dispatcher state, triggers, flow-control state, and recent dispatches.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
            tool_def(
                "harn.trust.query",
                "Query trust-graph records with the same filters exposed by trust_query(filters).",
                json!({
                    "type": "object",
                    "properties": {
                        "agent": { "type": "string" },
                        "action": { "type": "string" },
                        "since": { "type": "string" },
                        "until": { "type": "string" },
                        "tier": {
                            "type": "string",
                            "enum": ["shadow", "suggest", "act_with_approval", "act_auto"]
                        },
                        "outcome": {
                            "type": "string",
                            "enum": ["success", "failure", "denied", "timeout"]
                        },
                        "limit": { "type": "integer", "minimum": 0 },
                        "grouped_by_trace": { "type": "boolean" }
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "object",
                    "required": ["grouped_by_trace", "results"],
                    "properties": {
                        "grouped_by_trace": { "type": "boolean" },
                        "results": { "type": "array" },
                    },
                })),
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
        ];
        paginated_list_response(id, "tools/list", "tools", params, tools)
    }

    fn handle_resource_templates_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        paginated_list_response(
            id,
            "resources/templates/list",
            "resourceTemplates",
            params,
            Vec::new(),
        )
    }

    async fn handle_tools_call(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        if !session.authenticated {
            return harn_vm::jsonrpc::error_response(id, -32001, "unauthorized");
        }

        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if mcp_protocol::requests_task_augmentation(params) {
            if let Err(response) = validate_taskable_tool(id.clone(), name) {
                return response;
            }
            let task_ttl = match parse_task_ttl(params) {
                Ok(ttl) => ttl,
                Err(error) => return harn_vm::jsonrpc::error_response(id, -32602, &error),
            };
            return self.create_tool_task(id, session, name.to_string(), params.clone(), task_ttl);
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());

        let result = self
            .execute_tool_call(name, session, &trace_id, arguments)
            .await;

        let _ = self
            .record_tool_call(name, &trace_id, &session.client_identity, &result)
            .await;
        if result.is_ok() && tool_call_changes_resources(name) {
            self.notify_list_changed(&[McpListChangeKind::Resources]);
        }

        match result {
            Ok(value) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string()),
                    }],
                    "structuredContent": value,
                    "isError": false,
                }),
            ),
            Err(error) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "content": [{ "type": "text", "text": error }],
                    "isError": true,
                }),
            ),
        }
    }

    async fn execute_tool_call(
        &self,
        name: &str,
        session: &ConnectionState,
        trace_id: &str,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        match name {
            "harn.secret_scan" | "harn::secret_scan" => self.tool_secret_scan(arguments).await,
            "harn.trigger.fire" => self.tool_trigger_fire(session, trace_id, arguments).await,
            "harn.trigger.list" => self.tool_trigger_list(arguments).await,
            "harn.trigger.replay" => self.tool_trigger_replay(arguments).await,
            "harn.orchestrator.queue" => self.tool_orchestrator_queue(arguments).await,
            "harn.orchestrator.dlq.list" => self.tool_orchestrator_dlq_list(arguments).await,
            "harn.orchestrator.dlq.retry" => self.tool_orchestrator_dlq_retry(arguments).await,
            "harn.orchestrator.inspect" => self.tool_orchestrator_inspect(arguments).await,
            "harn.trust.query" => self.tool_trust_query(arguments).await,
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    fn create_tool_task(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        name: String,
        params: JsonValue,
        ttl: Option<u64>,
    ) -> JsonValue {
        let task_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let task = McpTaskState {
            task_id: task_id.clone(),
            owner: session.client_identity.clone(),
            status: mcp_protocol::McpTaskStatus::Working,
            status_message: Some("The operation is now in progress.".to_string()),
            created_at: now.clone(),
            last_updated_at: now,
            ttl,
            poll_interval: Some(mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS),
        };
        let notify = Arc::new(Notify::new());
        self.tasks.lock().expect("MCP tasks poisoned").insert(
            task_id.clone(),
            McpTaskRecord {
                task: task.clone(),
                result: None,
                notify,
            },
        );
        let _ = self.task_notify_tx.send(task.notification());

        let service = self.clone();
        let task_session = session.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build MCP task runtime");
            runtime.block_on(async move {
                service
                    .run_tool_task(task_id, task_session, name, params)
                    .await;
            });
        });

        harn_vm::jsonrpc::response(
            id,
            json!({
                "task": task.to_json(),
                "_meta": {
                    "io.modelcontextprotocol/model-immediate-response": "The requested Harn tool is running as an MCP task.",
                }
            }),
        )
    }

    async fn run_tool_task(
        &self,
        task_id: String,
        session: ConnectionState,
        name: String,
        params: JsonValue,
    ) {
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());
        let result = self
            .execute_tool_call(&name, &session, &trace_id, arguments)
            .await;
        let _ = self
            .record_tool_call(&name, &trace_id, &session.client_identity, &result)
            .await;
        if result.is_ok() && tool_call_changes_resources(&name) {
            self.notify_list_changed(&[McpListChangeKind::Resources]);
        }
        self.complete_task(&task_id, result);
    }

    fn complete_task(&self, task_id: &str, result: Result<JsonValue, String>) {
        let Some((notification, wake)) = ({
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return;
            };
            if record.task.status == mcp_protocol::McpTaskStatus::Cancelled {
                return;
            }
            let wake = record.notify.clone();
            let now = now_rfc3339();
            record.task.last_updated_at = now;
            match result {
                Ok(value) => {
                    record.task.status = mcp_protocol::McpTaskStatus::Completed;
                    record.task.status_message =
                        Some("The task completed successfully.".to_string());
                    record.result = Some(tool_call_result_json(value, false));
                }
                Err(error) => {
                    record.task.status = mcp_protocol::McpTaskStatus::Failed;
                    record.task.status_message = Some(format!("Tool execution failed: {error}"));
                    record.result = Some(tool_call_result_json(json!(error), true));
                }
            }
            Some((record.task.notification(), wake))
        }) else {
            return;
        };
        let _ = self.task_notify_tx.send(notification);
        wake.notify_waiters();
    }

    fn handle_tasks_get(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        match self.task_record_for_session(session, params) {
            Ok(record) => harn_vm::jsonrpc::response(id, record.task.to_json()),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
        }
    }

    async fn handle_tasks_result(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let task_id = match params.get("taskId").and_then(JsonValue::as_str) {
            Some(task_id) if !task_id.is_empty() => task_id.to_string(),
            _ => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Failed to retrieve task: missing taskId",
                )
            }
        };

        loop {
            let notify = {
                let tasks = self.tasks.lock().expect("MCP tasks poisoned");
                let Some(record) = tasks.get(&task_id) else {
                    return harn_vm::jsonrpc::error_response(
                        id,
                        -32602,
                        "Failed to retrieve task: task not found",
                    );
                };
                if record.task.owner != session.client_identity {
                    return harn_vm::jsonrpc::error_response(
                        id,
                        -32602,
                        "Failed to retrieve task: task not found",
                    );
                }
                if record.task.status.is_terminal() {
                    let Some(result) = record.result.clone() else {
                        return harn_vm::jsonrpc::error_response(
                            id,
                            -32603,
                            "Failed to retrieve task: terminal task has no result",
                        );
                    };
                    return harn_vm::jsonrpc::response(
                        id,
                        attach_related_task_meta(result, &task_id),
                    );
                }
                record.notify.clone()
            };
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(
                    mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS,
                )) => {}
            }
        }
    }

    fn handle_tasks_list(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let matching = self
            .tasks
            .lock()
            .expect("MCP tasks poisoned")
            .values()
            .filter(|record| record.task.owner == session.client_identity)
            .map(|record| record.task.to_json())
            .collect::<Vec<_>>();
        paginated_list_response(
            id,
            mcp_protocol::METHOD_TASKS_LIST,
            "tasks",
            params,
            matching,
        )
    }

    fn handle_tasks_cancel(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let task_id = match params.get("taskId").and_then(JsonValue::as_str) {
            Some(task_id) if !task_id.is_empty() => task_id,
            _ => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: missing taskId",
                )
            }
        };
        let (task, notify) = {
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            };
            if record.task.owner != session.client_identity {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            }
            if record.task.status.is_terminal() {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!(
                        "Cannot cancel task: already in terminal status '{}'",
                        record.task.status.as_str()
                    ),
                );
            }
            record.task.status = mcp_protocol::McpTaskStatus::Cancelled;
            record.task.status_message = Some("The task was cancelled by request.".to_string());
            record.task.last_updated_at = now_rfc3339();
            record.result = Some(json!({
                "content": [{
                    "type": "text",
                    "text": "Task was cancelled by request.",
                }],
                "isError": true,
            }));
            (record.task.clone(), record.notify.clone())
        };
        let _ = self.task_notify_tx.send(task.notification());
        notify.notify_waiters();
        harn_vm::jsonrpc::response(id, task.to_json())
    }

    fn task_record_for_session(
        &self,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> Result<McpTaskRecord, String> {
        let task_id = params
            .get("taskId")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "Failed to retrieve task: missing taskId".to_string())?;
        let tasks = self.tasks.lock().expect("MCP tasks poisoned");
        let record = tasks
            .get(task_id)
            .ok_or_else(|| "Failed to retrieve task: task not found".to_string())?;
        if record.task.owner != session.client_identity {
            return Err("Failed to retrieve task: task not found".to_string());
        }
        Ok(record.clone())
    }

    async fn handle_resources_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        match self.list_resources().await {
            Ok(resources) => {
                paginated_list_response(id, "resources/list", "resources", params, resources)
            }
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    async fn handle_resources_read(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        match self.read_resource(uri).await {
            Ok((text, mime_type)) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "text": text,
                        "mimeType": mime_type,
                    }],
                }),
            ),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32002, &error),
        }
    }

    async fn tool_secret_scan(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: SecretScanRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let findings: Vec<SecretFinding> = secret_scan_content(&request.content);
        let ctx = load_local_runtime(&self.local_args()).await?;
        append_secret_scan_audit(
            ctx.event_log.as_ref(),
            "mcp.harn.secret_scan",
            request.content.len(),
            &findings,
        )
        .await
        .map_err(|error| error.to_string())?;
        serde_json::to_value(findings).map_err(|error| error.to_string())
    }

    async fn tool_trigger_fire(
        &self,
        session: &ConnectionState,
        trace_id: &str,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let request: TriggerFireRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let mut event = synthetic_event_for_binding(&ctx, &request.trigger_id)?;
        merge_json_object(&mut event, request.payload);
        inject_trace_headers(&mut event, &session.client_identity, trace_id);
        let handle = trigger_fire(&mut ctx, &request.trigger_id, event).await?;
        Ok(json!({
            "event_id": handle.event_id,
            "status": handle.status,
            "binding_id": handle.binding_id,
            "binding_version": handle.binding_version,
            "dlq_entry_id": handle.dlq_entry_id,
            "error": handle.error,
            "result": handle.result,
        }))
    }

    async fn tool_trigger_list(&self, _arguments: JsonValue) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let snapshots = trigger_list(&mut ctx).await?;
        let mut snapshots_by_id = BTreeMap::new();
        for snapshot in snapshots {
            snapshots_by_id.insert(snapshot.id.clone(), snapshot);
        }

        let mut triggers = Vec::new();
        for trigger in &ctx.collected_triggers {
            let Some(snapshot) = snapshots_by_id.get(&trigger.config.id) else {
                continue;
            };
            triggers.push(TriggerListEntry {
                trigger_id: trigger.config.id.clone(),
                kind: trigger_kind_name(trigger.config.kind).to_string(),
                provider: trigger.config.provider.as_str().to_string(),
                when: trigger.when.as_ref().map(|when| when.reference.raw.clone()),
                handler: handler_json(&trigger.handler),
                version: snapshot.version,
                state: snapshot.state.as_str().to_string(),
                metrics: snapshot.metrics.clone(),
            });
        }
        Ok(json!({ "triggers": triggers }))
    }

    async fn tool_trigger_replay(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: TriggerReplayRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let steering = trigger_replay_steering_from_request(&request)?;
        if request.as_of.is_some() || steering.is_some() {
            let workspace_root = self
                .config_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            let ctx = load_local_runtime(&self.local_args()).await?;
            let report = crate::commands::trigger::replay::replay_report_for_event_log(
                ctx.event_log.clone(),
                &workspace_root,
                &request.event_id,
                request.as_of.as_deref(),
                false,
                steering.as_ref(),
            )
            .await?;
            return serde_json::to_value(report).map_err(|error| error.to_string());
        }

        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let handle = trigger_replay(&mut ctx, &request.event_id).await?;
        serde_json::to_value(handle).map_err(|error| error.to_string())
    }

    async fn tool_orchestrator_queue(&self, _arguments: JsonValue) -> Result<JsonValue, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let dispatcher = harn_vm::snapshot_dispatcher_stats();
        let inbox_claims = read_topic(&ctx.event_log, TRIGGER_INBOX_CLAIMS_TOPIC).await?;
        let inbox_envelopes = read_topic(&ctx.event_log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
        let inbox_legacy = read_topic(&ctx.event_log, TRIGGER_INBOX_LEGACY_TOPIC).await?;
        let outbox = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;
        let attempts = read_topic(&ctx.event_log, TRIGGER_ATTEMPTS_TOPIC).await?;
        let dlq = read_topic(&ctx.event_log, TRIGGER_DLQ_TOPIC).await?;

        let queue = QueueSnapshot {
            dispatcher,
            inbox: TopicPreview {
                count: inbox_claims.len() + inbox_envelopes.len() + inbox_legacy.len(),
                head: preview_events(
                    inbox_claims
                        .into_iter()
                        .chain(inbox_envelopes)
                        .chain(inbox_legacy)
                        .collect(),
                ),
            },
            outbox: TopicPreview {
                count: outbox.len(),
                head: preview_events(outbox),
            },
            attempts: TopicPreview {
                count: attempts.len(),
                head: preview_events(attempts),
            },
            dlq: TopicPreview {
                count: dlq.len(),
                head: preview_events(dlq),
            },
        };
        serde_json::to_value(queue).map_err(|error| error.to_string())
    }

    async fn tool_orchestrator_dlq_list(&self, _arguments: JsonValue) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entries = trigger_inspect_dlq(&mut ctx).await?;
        Ok(json!({ "entries": entries }))
    }

    async fn tool_orchestrator_dlq_retry(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: DlqRetryRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entries = trigger_inspect_dlq(&mut ctx).await?;
        let entry = entries
            .iter()
            .find(|entry| entry.id == request.entry_id)
            .ok_or_else(|| format!("unknown pending DLQ entry '{}'", request.entry_id))?;
        let handle = trigger_replay(&mut ctx, &entry.event_id).await?;
        Ok(json!({
            "entry_id": entry.id,
            "handle": handle,
        }))
    }

    async fn tool_orchestrator_inspect(&self, _arguments: JsonValue) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let inspect = collect_orchestrator_inspect_data(&mut ctx).await?;
        let payload = InspectPayload {
            dispatcher: harn_vm::snapshot_dispatcher_stats(),
            inspect,
        };
        serde_json::to_value(payload).map_err(|error| error.to_string())
    }

    async fn tool_trust_query(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: TrustQueryRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let filters = harn_vm::TrustQueryFilters {
            agent: request.agent,
            action: request.action,
            since: request
                .since
                .as_deref()
                .map(parse_trust_query_timestamp)
                .transpose()?,
            until: request
                .until
                .as_deref()
                .map(parse_trust_query_timestamp)
                .transpose()?,
            tier: request.tier,
            outcome: request.outcome,
            limit: request.limit,
            grouped_by_trace: request.grouped_by_trace,
        };
        let ctx = load_local_runtime(&self.local_args()).await?;
        let records = harn_vm::query_trust_records(&ctx.event_log, &filters)
            .await
            .map_err(|error| error.to_string())?;
        let results = if filters.grouped_by_trace {
            serde_json::to_value(harn_vm::group_trust_records_by_trace(&records))
                .map_err(|error| error.to_string())?
        } else {
            serde_json::to_value(records).map_err(|error| error.to_string())?
        };
        Ok(json!({
            "grouped_by_trace": filters.grouped_by_trace,
            "results": results,
        }))
    }

    async fn list_resources(&self) -> Result<Vec<JsonValue>, String> {
        let mut resources = vec![json!({
            "uri": "harn://manifest",
            "name": "Manifest",
            "description": "The running orchestrator manifest",
            "mimeType": "application/toml",
        })];

        let ctx = load_local_runtime(&self.local_args()).await?;
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        for (event_id, event) in recorded {
            let Ok(record) = serde_json::from_value::<RecordedTriggerEvent>(event.payload) else {
                continue;
            };
            resources.push(json!({
                "uri": format!("harn://event/{}", record.event.id.0),
                "name": format!("Event {}", record.event.id.0),
                "description": format!("Trigger event log record #{event_id}"),
                "mimeType": "application/json",
            }));
        }

        let mut ctx = load_local_runtime(&self.local_args()).await?;
        for entry in trigger_inspect_dlq(&mut ctx).await? {
            resources.push(json!({
                "uri": format!("harn://dlq/{}", entry.id),
                "name": format!("DLQ {}", entry.id),
                "description": format!("Pending DLQ entry for event {}", entry.event_id),
                "mimeType": "application/json",
            }));
        }

        Ok(resources)
    }

    async fn read_resource(&self, uri: &str) -> Result<(String, &'static str), String> {
        if uri == "harn://manifest" {
            return Ok((
                self.manifest_source
                    .lock()
                    .expect("manifest source poisoned")
                    .clone(),
                "application/toml",
            ));
        }
        if let Some(event_id) = uri.strip_prefix("harn://event/") {
            let detail = self.event_resource(event_id).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        if let Some(entry_id) = uri.strip_prefix("harn://dlq/") {
            let detail = self.dlq_resource(entry_id).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        Err(format!("resource not found: {uri}"))
    }

    async fn event_resource(&self, event_id: &str) -> Result<JsonValue, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        let record = recorded
            .into_iter()
            .find_map(|(log_id, event)| {
                let parsed = serde_json::from_value::<RecordedTriggerEvent>(event.payload).ok()?;
                (parsed.event.id.0 == event_id).then_some((log_id, parsed))
            })
            .ok_or_else(|| format!("unknown trigger event id '{event_id}'"))?;
        let trace_id = record.1.event.trace_id.0.clone();
        let related_outbox = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_attempts = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_ATTEMPTS_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_dlq = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_DLQ_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_graph = filter_related_events(
            read_topic(&ctx.event_log, ACTION_GRAPH_TOPIC).await?,
            event_id,
            &trace_id,
        );
        Ok(json!({
            "log_event_id": record.0,
            "binding_id": record.1.binding_id,
            "binding_version": record.1.binding_version,
            "replay_of_event_id": record.1.replay_of_event_id,
            "event": record.1.event,
            "trace": {
                "trace_id": trace_id,
                "outbox": related_outbox,
                "attempts": related_attempts,
                "dlq": related_dlq,
                "action_graph": related_graph,
            },
        }))
    }

    async fn dlq_resource(&self, entry_id: &str) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entry = trigger_inspect_dlq(&mut ctx)
            .await?
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| format!("unknown DLQ entry '{entry_id}'"))?;
        serde_json::to_value(entry).map_err(|error| error.to_string())
    }

    async fn record_tool_call(
        &self,
        tool_name: &str,
        trace_id: &str,
        client_identity: &str,
        result: &Result<JsonValue, String>,
    ) -> Result<(), String> {
        let status = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        let outcome = if result.is_ok() { "success" } else { "error" };

        eprintln!(
            "[harn] mcp: client={} tool={} status={} trace_id={}",
            client_identity, tool_name, status, trace_id
        );

        let ctx = load_local_runtime(&self.local_args()).await?;
        let topic = Topic::new(ACTION_GRAPH_TOPIC).map_err(|error| error.to_string())?;
        let mut headers = BTreeMap::new();
        headers.insert("trace_id".to_string(), trace_id.to_string());
        headers.insert("mcp_client".to_string(), client_identity.to_string());
        headers.insert("tool_name".to_string(), tool_name.to_string());
        let payload = json!({
            "context": {
                "tool_name": tool_name,
                "client_identity": client_identity,
                "trace_id": trace_id,
            },
            "observability": {
                "schema_version": 1,
                "planner_rounds": [],
                "research_fact_count": 0,
                "action_graph_nodes": [{
                    "id": format!("mcp/{trace_id}"),
                    "label": tool_name,
                    "kind": "mcp_tool_call",
                    "status": status,
                    "outcome": outcome,
                    "trace_id": trace_id,
                }],
                "action_graph_edges": [],
                "worker_lineage": [],
                "verification_outcomes": [],
                "transcript_pointers": [],
                "compaction_events": [],
                "daemon_events": [],
            },
            "result": result.as_ref().ok(),
            "error": result.as_ref().err(),
        });
        ctx.event_log
            .append(
                &topic,
                LogEvent::new("action_graph_update", payload).with_headers(headers),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn parse_trust_query_timestamp(raw: &str) -> Result<OffsetDateTime, String> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        };
        return parsed.map_err(|error| format!("invalid timestamp '{raw}': {error}"));
    }
    Err(format!(
        "invalid timestamp '{raw}': expected RFC3339 or unix seconds/milliseconds"
    ))
}

async fn run_stdio(service: Arc<McpOrchestratorService>) -> Result<(), String> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();
    let mut session = ConnectionState::default();
    let mut list_notifications = service.subscribe_list_notifications();
    let mut task_notifications = service.subscribe_task_notifications();

    eprintln!("[harn] MCP stdio server ready");

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line.map_err(|error| format!("stdin read failed: {error}"))? else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let request: JsonValue = match serde_json::from_str(trimmed) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let response = service.handle_request(&mut session, request).await;
                if !response.is_null() {
                    write_stdio_json(&mut stdout, &response).await?;
                }
            }
            notification = list_notifications.recv() => {
                match notification {
                    Ok(notification) => write_stdio_json(&mut stdout, &notification).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = task_notifications.recv() => {
                match notification {
                    Ok(notification) if notification.owner == session.client_identity => {
                        write_stdio_json(&mut stdout, &notification.message).await?;
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    Ok(())
}

async fn write_stdio_json(stdout: &mut tokio::io::Stdout, value: &JsonValue) -> Result<(), String> {
    let mut encoded =
        serde_json::to_string(value).map_err(|error| format!("serialize error: {error}"))?;
    encoded.push('\n');
    stdout
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| format!("stdout write failed: {error}"))?;
    stdout
        .flush()
        .await
        .map_err(|error| format!("stdout flush failed: {error}"))
}

async fn run_http(service: Arc<McpOrchestratorService>, args: &McpServeArgs) -> Result<(), String> {
    let router = http_router(
        service,
        args.path.clone(),
        args.sse_path.clone(),
        args.messages_path.clone(),
    );
    serve_http_router(router, args.bind, &args.path).await
}

fn start_list_change_watcher(
    project_root: PathBuf,
    config_path: PathBuf,
    manifest_source_cache: Arc<Mutex<String>>,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    list_notify_tx: broadcast::Sender<JsonValue>,
) -> Option<notify::RecommendedWatcher> {
    let project_root_for_callback = project_root.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let prompt_changed = event
            .paths
            .iter()
            .any(|path| is_prompt_reload_path(path.as_path()));
        let manifest_changed = event
            .paths
            .iter()
            .any(|path| is_manifest_reload_path(path.as_path()));
        let package_changed = event
            .paths
            .iter()
            .any(|path| is_package_reload_path(path.as_path(), &project_root_for_callback));

        if !prompt_changed && !manifest_changed && !package_changed {
            return;
        }

        if prompt_changed || manifest_changed || package_changed {
            let manifest_source = std::fs::read_to_string(&config_path).unwrap_or_default();
            *manifest_source_cache
                .lock()
                .expect("manifest source poisoned") = manifest_source.clone();
            let updated = FilePromptCatalog::discover(&project_root_for_callback, &manifest_source);
            *prompt_catalog.lock().expect("prompt catalog poisoned") = updated;
        }

        let mut kinds = Vec::new();
        if manifest_changed || package_changed {
            kinds.push(McpListChangeKind::Tools);
            kinds.push(McpListChangeKind::Resources);
        }
        if prompt_changed || manifest_changed || package_changed {
            kinds.push(McpListChangeKind::Prompts);
        }
        for kind in kinds {
            let _ = list_notify_tx.send(kind.notification());
        }
    })
    .ok()?;
    watcher
        .watch(&project_root, notify::RecursiveMode::Recursive)
        .ok()?;
    Some(watcher)
}

fn is_prompt_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml" || name.ends_with(".harn.prompt"))
}

fn is_manifest_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml")
}

fn is_package_reload_path(path: &Path, project_root: &Path) -> bool {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.lock")
    {
        return true;
    }

    let relative = path.strip_prefix(project_root).unwrap_or(path);
    let mut components = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        });
    matches!(components.next(), Some(".harn")) && matches!(components.next(), Some("packages"))
}

#[cfg(test)]
pub(crate) fn http_router_for_local(
    local: OrchestratorLocalArgs,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Result<Router, String> {
    let service = Arc::new(McpOrchestratorService::new_local(local)?);
    Ok(http_router_for_service(
        service,
        path,
        sse_path,
        messages_path,
    ))
}

pub(crate) fn http_router_for_service(
    service: Arc<McpOrchestratorService>,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Router {
    http_router(service, path, sse_path, messages_path)
}

fn http_router(
    service: Arc<McpOrchestratorService>,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Router {
    let rpc = RpcBridge::start(service.clone());
    let state = HttpState {
        service,
        rpc,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        mcp_path: path.clone(),
        sse_path: sse_path.clone(),
        messages_path: messages_path.clone(),
    };
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*path}",
            get(oauth_protected_resource_metadata),
        )
        .route(
            &path,
            post(http_post_request)
                .get(http_get_stream)
                .delete(http_delete_session),
        )
        .route(&sse_path, get(legacy_sse_stream))
        .route(&messages_path, post(legacy_sse_message))
        .with_state(state)
}

async fn serve_http_router(
    router: Router,
    bind: std::net::SocketAddr,
    path: &str,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("failed to bind {bind}: {error}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to read local addr: {error}"))?;
    eprintln!("[harn] MCP HTTP listener ready on http://{local_addr}{path}");
    axum::serve(listener, router)
        .await
        .map_err(|error| format!("MCP HTTP server failed: {error}"))
}

async fn oauth_protected_resource_metadata(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    let Some(oauth) = &state.service.oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(oauth.metadata(&headers, &state.mcp_path)).into_response()
}

async fn http_post_request(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }

    let authenticated = match authorize_http_request(
        &state,
        method.as_str(),
        &state.mcp_path,
        &headers,
        body.as_ref(),
    )
    .await
    {
        Ok(authenticated) => authenticated,
        Err(response) => return response,
    };

    let request: JsonValue = match serde_json::from_slice(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC request body: {error}"),
            )
                .into_response()
        }
    };

    let header_session = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (session_id, session, created) =
        match lookup_or_create_session(&state, &request, header_session) {
            Ok(value) => value,
            Err(response) => return response,
        };

    let mut current = session.state.lock().expect("HTTP session poisoned").clone();
    if authenticated {
        current.authenticated = true;
    }
    let (updated, response_json) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    *session.state.lock().expect("HTTP session poisoned") = updated;
    if response_json.is_null() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_streamable_headers(
            &mut response,
            created.then_some(session_id.as_str()),
            MCP_PROTOCOL_VERSION,
        );
        return response;
    }

    let mut response = if should_stream_post_response(&headers) {
        sse_single_response(response_json).into_response()
    } else {
        Json(response_json).into_response()
    };
    attach_streamable_headers(
        &mut response,
        created.then_some(session_id.as_str()),
        MCP_PROTOCOL_VERSION,
    );
    response
}

async fn http_get_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }
    if let Err(response) =
        authorize_http_request(&state, "GET", &state.mcp_path, &headers, &[]).await
    {
        return response;
    }
    if !accepts_media(&headers, "text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let Some(session_id) = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (tx, rx) = unbounded::<JsonValue>();
    *session.sse_tx.lock().expect("SSE sender poisoned") = Some(tx);
    if let Some(sender) = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned()
    {
        spawn_list_notification_forwarder(state.service.clone(), sender);
    }
    if let Some(sender) = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned()
    {
        spawn_task_notification_forwarder(state.service.clone(), sender, session.clone());
    }
    let mut response = sse_response(rx).into_response();
    attach_streamable_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

async fn http_delete_session(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }
    if let Err(response) =
        authorize_http_request(&state, "DELETE", &state.mcp_path, &headers, &[]).await
    {
        return response;
    }
    let Some(session_id) = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let removed = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .remove(session_id);
    let mut response = if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    };
    attach_streamable_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

async fn legacy_sse_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let authenticated =
        match authorize_http_request(&state, "GET", &state.sse_path, &headers, &[]).await {
            Ok(authenticated) => authenticated,
            Err(mut response) => {
                attach_legacy_deprecation_headers(&mut response);
                return response;
            }
        };

    if authenticated {
        eprintln!(
            "[harn] warning: legacy MCP SSE transport is deprecated; use Streamable HTTP at {}",
            state.mcp_path
        );
    }

    let session_id = Uuid::now_v7().to_string();
    let session = Arc::new(HttpSession::default());
    if authenticated {
        session
            .state
            .lock()
            .expect("legacy SSE session poisoned")
            .authenticated = true;
    }
    let (tx, rx) = unbounded::<JsonValue>();
    *session.sse_tx.lock().expect("SSE sender poisoned") = Some(tx);
    let list_tx = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(list_tx) = list_tx {
        spawn_list_notification_forwarder(state.service.clone(), list_tx);
    }
    let task_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(task_tx) = task_tx {
        spawn_task_notification_forwarder(state.service.clone(), task_tx, session.clone());
    }
    state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .insert(session_id.clone(), session);
    let endpoint = format!("{}?session_id={session_id}", state.messages_path);
    let endpoint_event = Event::default().event("endpoint").data(endpoint);
    let stream = stream::once(async move { Ok::<Event, Infallible>(endpoint_event) }).chain(
        rx.map(|message| {
            Ok(Event::default()
                .id(Uuid::now_v7().to_string())
                .event("message")
                .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
        }),
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    attach_legacy_deprecation_headers(&mut response);
    response
}

async fn legacy_sse_message(
    State(state): State<HttpState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let authenticated = match authorize_http_request(
        &state,
        "POST",
        &state.messages_path,
        &headers,
        body.as_ref(),
    )
    .await
    {
        Ok(authenticated) => authenticated,
        Err(mut response) => {
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let Some(session_id) = query.get("session_id") else {
        let mut response = (StatusCode::BAD_REQUEST, "missing session_id").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        let mut response = (StatusCode::NOT_FOUND, "unknown session").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let request: JsonValue = match serde_json::from_slice(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC request body: {error}"),
            )
                .into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let mut current = session
        .state
        .lock()
        .expect("legacy SSE session poisoned")
        .clone();
    if authenticated {
        current.authenticated = true;
    }
    let (updated, response) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => {
            let mut response = (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    *session.state.lock().expect("legacy SSE session poisoned") = updated;
    if response.is_null() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    }
    let Some(sender) = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned()
    else {
        let mut response = (StatusCode::GONE, "session stream closed").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    if sender.unbounded_send(response).is_err() {
        let mut response = (StatusCode::GONE, "session stream closed").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    }
    let mut response = StatusCode::ACCEPTED.into_response();
    attach_legacy_deprecation_headers(&mut response);
    response
}

#[allow(clippy::result_large_err)] // axum::Response is large but short-lived on the error path.
fn lookup_or_create_session(
    state: &HttpState,
    request: &JsonValue,
    header_session: Option<String>,
) -> Result<(String, Arc<HttpSession>, bool), Response> {
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let mut sessions = state.sessions.lock().expect("MCP sessions poisoned");
    if let Some(session_id) = header_session {
        if let Some(session) = sessions.get(&session_id).cloned() {
            return Ok((session_id, session, false));
        }
        return Err((StatusCode::NOT_FOUND, "unknown MCP session").into_response());
    }
    if method != "initialize" {
        return Err((StatusCode::BAD_REQUEST, "missing MCP session").into_response());
    }
    let session_id = Uuid::now_v7().to_string();
    let session = Arc::new(HttpSession::default());
    sessions.insert(session_id.clone(), session.clone());
    Ok((session_id, session, true))
}

impl RpcBridge {
    fn start(service: Arc<McpOrchestratorService>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<RpcRequest>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build MCP worker runtime");
            runtime.block_on(async move {
                while let Some(request) = rx.recv().await {
                    let mut session = request.session;
                    let response = service.handle_request(&mut session, request.request).await;
                    let _ = request.response_tx.send((session, response));
                }
            });
        });
        Self { tx }
    }

    async fn call(
        &self,
        session: ConnectionState,
        request: JsonValue,
    ) -> Result<(ConnectionState, JsonValue), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RpcRequest {
                session,
                request,
                response_tx,
            })
            .map_err(|_| "MCP worker is not running".to_string())?;
        response_rx
            .await
            .map_err(|_| "MCP worker dropped the response channel".to_string())
    }
}

fn sse_single_response(
    message: JsonValue,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let message = Event::default()
        .id(Uuid::now_v7().to_string())
        .event("message")
        .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string()));
    Sse::new(stream::iter([
        Ok::<Event, Infallible>(prime),
        Ok::<Event, Infallible>(message),
    ]))
    .keep_alive(KeepAlive::default())
}

fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(prime) }).chain(rx.map(|message| {
            Ok(Event::default()
                .id(Uuid::now_v7().to_string())
                .event("message")
                .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
        }));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn spawn_list_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
) {
    let mut notifications = service.subscribe_list_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(message) => {
                    if sender.unbounded_send(message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_task_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_task_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let owner = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .client_identity
                        .clone();
                    if notification.owner != owner {
                        continue;
                    }
                    if sender.unbounded_send(notification.message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn attach_streamable_headers(response: &mut Response, session_id: Option<&str>, protocol: &str) {
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response
                .headers_mut()
                .insert(HeaderName::from_static(MCP_SESSION_HEADER), value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_PROTOCOL_HEADER), value);
    }
}

fn attach_legacy_deprecation_headers(response: &mut Response) {
    response.headers_mut().insert(
        HeaderName::from_static(DEPRECATION_HEADER),
        HeaderValue::from_static("true"),
    );
}

fn should_stream_post_response(headers: &HeaderMap) -> bool {
    accepts_media(headers, "text/event-stream") && !accepts_media(headers, "application/json")
}

fn accepts_media(headers: &HeaderMap, media_type: &str) -> bool {
    let Some(value) = headers.get(ACCEPT).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    value.split(',').any(|entry| {
        let media = entry
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        media == media_type || media == "*/*"
    })
}

fn validate_protocol_header(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(value) = headers
        .get(MCP_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };
    if value == MCP_PROTOCOL_VERSION || value == "2025-03-26" {
        Ok(())
    } else {
        Err(Box::new(StatusCode::BAD_REQUEST.into_response()))
    }
}

fn validate_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return Ok(());
    };
    let Ok(url) = url::Url::parse(origin) else {
        return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
    };
    match url.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1") => Ok(()),
        _ => Err(Box::new(StatusCode::FORBIDDEN.into_response())),
    }
}

async fn authorize_http_request(
    state: &HttpState,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<bool, Response> {
    if state.service.auth.has_api_keys()
        && authorize_legacy_http_request(state, method, path, headers, body)
            .await
            .is_ok()
    {
        return Ok(true);
    }

    if let Some(oauth) = &state.service.oauth {
        let Some(token) = bearer_token(headers) else {
            return Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                None,
                StatusCode::UNAUTHORIZED,
            ));
        };
        return match oauth.validate_bearer(token, headers, &state.mcp_path).await {
            Ok(()) => Ok(true),
            Err(OAuthTokenError::InsufficientScope) => Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                Some(OAuthChallengeError::InsufficientScope),
                StatusCode::FORBIDDEN,
            )),
            Err(OAuthTokenError::InvalidToken(error)) => Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                Some(OAuthChallengeError::InvalidToken(error)),
                StatusCode::UNAUTHORIZED,
            )),
        };
    }

    if state.service.auth.has_api_keys() {
        return Err((StatusCode::UNAUTHORIZED, "auth failed").into_response());
    }

    Ok(false)
}

async fn authorize_legacy_http_request(
    state: &HttpState,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Response> {
    let auth_log = auth_event_log(&state.service.state_dir)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error).into_response())?;
    state
        .service
        .auth
        .authorize(
            auth_log.as_ref(),
            method,
            path,
            &normalized_headers(headers),
            body,
        )
        .await
        .map_err(|()| (StatusCode::UNAUTHORIZED, "auth failed").into_response())
}

fn oauth_challenge_response(
    oauth: &OAuthResourceServer,
    headers: &HeaderMap,
    mcp_path: &str,
    error: Option<OAuthChallengeError>,
    status: StatusCode,
) -> Response {
    let mut response = status.into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        oauth.challenge_header(headers, mcp_path, error),
    );
    response
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let (scheme, value) = authorization.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let value = value.trim();
        (!value.is_empty()).then_some(value)
    } else {
        None
    }
}

fn initialize_api_key(params: &JsonValue) -> Option<&str> {
    params
        .pointer("/capabilities/harn/apiKey")
        .and_then(JsonValue::as_str)
        .or_else(|| {
            params
                .pointer("/_meta/harn/apiKey")
                .and_then(JsonValue::as_str)
        })
        .or_else(|| {
            params
                .pointer("/capabilities/experimental/harn/apiKey")
                .and_then(JsonValue::as_str)
        })
}

fn paginated_list_response(
    id: JsonValue,
    method: &str,
    result_key: &str,
    params: &JsonValue,
    items: Vec<JsonValue>,
) -> JsonValue {
    let page = match mcp_protocol::mcp_list_page(params, items.len(), method) {
        Ok(page) => page,
        Err(error) => return harn_vm::jsonrpc::error_response(id, -32602, &error),
    };
    let page_len = page.end - page.start;
    let page_items = items
        .into_iter()
        .skip(page.start)
        .take(page_len)
        .collect::<Vec<_>>();
    let mut result = serde_json::Map::new();
    result.insert(result_key.to_string(), JsonValue::Array(page_items));
    if let Some(next_cursor) = page.next_cursor {
        result.insert("nextCursor".to_string(), JsonValue::String(next_cursor));
    }
    harn_vm::jsonrpc::response(id, JsonValue::Object(result))
}

fn tool_def(
    name: &str,
    description: &str,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    task_support: mcp_protocol::McpToolTaskSupport,
) -> JsonValue {
    let mut value = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "execution": mcp_protocol::tool_execution(task_support),
    });
    if let Some(output_schema) = output_schema {
        value["outputSchema"] = output_schema;
    }
    value
}

fn task_support_for_tool(name: &str) -> Option<mcp_protocol::McpToolTaskSupport> {
    match name {
        "harn.trigger.fire" | "harn.trigger.replay" | "harn.orchestrator.dlq.retry" => {
            Some(mcp_protocol::McpToolTaskSupport::Optional)
        }
        "harn.secret_scan"
        | "harn::secret_scan"
        | "harn.trigger.list"
        | "harn.orchestrator.queue"
        | "harn.orchestrator.dlq.list"
        | "harn.orchestrator.inspect"
        | "harn.trust.query" => Some(mcp_protocol::McpToolTaskSupport::Forbidden),
        _ => None,
    }
}

fn validate_taskable_tool(id: JsonValue, name: &str) -> Result<(), JsonValue> {
    match task_support_for_tool(name) {
        Some(mcp_protocol::McpToolTaskSupport::Optional)
        | Some(mcp_protocol::McpToolTaskSupport::Required) => Ok(()),
        Some(mcp_protocol::McpToolTaskSupport::Forbidden) => {
            Err(mcp_protocol::task_augmentation_error_response(
                id,
                "tools/call",
                -32602,
                "Tool does not support MCP task-augmented execution",
                &format!("Tool '{name}' advertises execution.taskSupport=\"forbidden\"."),
            ))
        }
        None => Err(harn_vm::jsonrpc::error_response(
            id,
            -32602,
            &format!("unknown tool '{name}'"),
        )),
    }
}

fn parse_task_ttl(params: &JsonValue) -> Result<Option<u64>, String> {
    let task = params
        .get("task")
        .ok_or_else(|| "missing task params".to_string())?;
    let Some(object) = task.as_object() else {
        return Err("task must be an object".to_string());
    };
    let Some(ttl) = object.get("ttl") else {
        return Ok(Some(DEFAULT_TASK_TTL_MS));
    };
    let Some(ttl) = ttl.as_u64() else {
        return Err("task.ttl must be an unsigned integer number of milliseconds".to_string());
    };
    Ok(Some(ttl.min(MAX_TASK_TTL_MS)))
}

fn tool_call_result_json(value: JsonValue, is_error: bool) -> JsonValue {
    if is_error {
        return json!({
            "content": [{
                "type": "text",
                "text": value.as_str().unwrap_or("Tool execution failed"),
            }],
            "isError": true,
        });
    }
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        }],
        "structuredContent": value,
        "isError": false,
    })
}

fn attach_related_task_meta(mut result: JsonValue, task_id: &str) -> JsonValue {
    let related = mcp_protocol::related_task_meta(task_id);
    if let Some(result_object) = result.as_object_mut() {
        let meta = result_object.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta_object) = meta.as_object_mut() {
            if let Some(related_object) = related.as_object() {
                for (key, value) in related_object {
                    meta_object.insert(key.clone(), value.clone());
                }
            }
        } else {
            result_object.insert("_meta".to_string(), related);
        }
    }
    result
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn tool_call_changes_resources(name: &str) -> bool {
    matches!(
        name,
        "harn.trigger.fire" | "harn.trigger.replay" | "harn.orchestrator.dlq.retry"
    )
}

fn handler_json(handler: &CollectedTriggerHandler) -> JsonValue {
    match handler {
        CollectedTriggerHandler::Local { reference, .. } => json!({
            "kind": "local",
            "reference": reference.raw,
        }),
        CollectedTriggerHandler::A2a { target, .. } => json!({
            "kind": "a2a",
            "target": target,
        }),
        CollectedTriggerHandler::Worker { queue } => json!({
            "kind": "worker",
            "queue": queue,
        }),
        CollectedTriggerHandler::Persona { binding } => json!({
            "kind": "persona",
            "name": binding.name,
            "entry_workflow": binding.entry_workflow,
        }),
    }
}

fn inject_trace_headers(event: &mut JsonValue, client_identity: &str, trace_id: &str) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object.insert("trace_id".to_string(), json!(trace_id));
    let headers = object
        .entry("headers")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(headers) = headers {
        headers.insert("x-harn-mcp-client".to_string(), json!(client_identity));
        headers.insert("x-harn-mcp-trace-id".to_string(), json!(trace_id));
    }
}

fn merge_json_object(target: &mut JsonValue, patch: JsonValue) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn preview_events(events: Vec<(u64, LogEvent)>) -> Vec<QueuePreviewEntry> {
    let mut preview = events
        .into_iter()
        .map(|(event_id, event)| QueuePreviewEntry {
            event_id,
            kind: event.kind,
            occurred_at_ms: event.occurred_at_ms,
            headers: event.headers,
            payload: event.payload,
        })
        .collect::<Vec<_>>();
    preview.sort_by_key(|entry| entry.event_id);
    preview
        .into_iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn filter_related_events(
    events: Vec<(u64, LogEvent)>,
    event_id: &str,
    trace_id: &str,
) -> Vec<JsonValue> {
    events
        .into_iter()
        .filter_map(|(id, event)| {
            let matches_event = event
                .headers
                .get("event_id")
                .is_some_and(|value| value == event_id)
                || event
                    .headers
                    .get("trace_id")
                    .is_some_and(|value| value == trace_id)
                || event
                    .payload
                    .pointer("/context/event_id")
                    .and_then(JsonValue::as_str)
                    == Some(event_id);
            matches_event.then_some(json!({
                "id": id,
                "kind": event.kind,
                "occurred_at_ms": event.occurred_at_ms,
                "headers": event.headers,
                "payload": event.payload,
            }))
        })
        .collect()
}

fn normalized_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn trigger_kind_name(kind: crate::package::TriggerKind) -> &'static str {
    match kind {
        crate::package::TriggerKind::Webhook => "webhook",
        crate::package::TriggerKind::Cron => "cron",
        crate::package::TriggerKind::Poll => "poll",
        crate::package::TriggerKind::Stream => "stream",
        crate::package::TriggerKind::Predicate => "predicate",
        crate::package::TriggerKind::A2aPush => "a2a-push",
    }
}

fn auth_event_log(state_dir: &Path) -> Result<Arc<harn_vm::event_log::AnyEventLog>, String> {
    let config = harn_vm::event_log::EventLogConfig::for_base_dir(state_dir)
        .map_err(|error| format!("failed to build auth event log config: {error}"))?;
    harn_vm::event_log::open_event_log(&config)
        .map_err(|error| format!("failed to open auth event log: {error}"))
}

#[cfg(test)]
// Tests here mutate harn_vm process-global state (`HARN_STATE_DIR` env,
// thread-local `ACTIVE_EVENT_LOG`, trigger registry) through the shared
// `lock_harn_state` guard in `crate::tests::common::harn_state_lock`.
// The guard is a `std::sync::Mutex` held across `.await` points; it is
// dropped when each `#[tokio::test]` future resolves, so holding across
// awaits is safe in practice despite the clippy lint.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::extract::Form;
    use axum::http::Request;
    use axum::routing::post;
    use axum::Router as AxumRouter;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::env_guard::ScopedEnvVar;
    use crate::tests::common::env_lock::lock_env;
    use crate::tests::common::harn_state_lock::lock_harn_state;

    fn write_file(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn trigger_replay_steering_request_validates_pairs() {
        let request = TriggerReplayRequest {
            event_id: "evt-1".to_string(),
            as_of: None,
            steer_from: None,
            to_decision: Some(json!({"status": "skipped"})),
            reason: None,
            applied_by: None,
            scope: None,
        };
        assert!(trigger_replay_steering_from_request(&request).is_err());

        let request = TriggerReplayRequest {
            steer_from: Some("outcome".to_string()),
            scope: Some("this_persona".to_string()),
            ..request
        };
        let steering = trigger_replay_steering_from_request(&request)
            .expect("valid steering")
            .expect("steering present");
        assert_eq!(steering.step, "outcome");
        assert_eq!(steering.scope, harn_vm::CorrectionScope::ThisPersona);
    }

    fn fixture_args(temp: &TempDir) -> McpServeArgs {
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        McpServeArgs {
            local: OrchestratorLocalArgs {
                config: temp.path().join("harn.toml"),
                state_dir,
            },
            transport: McpServeTransport::Stdio,
            bind: "127.0.0.1:0".parse().unwrap(),
            path: "/mcp".to_string(),
            sse_path: "/sse".to_string(),
            messages_path: "/messages".to_string(),
        }
    }

    fn write_fixture(temp: &TempDir) {
        write_file(
            temp.path(),
            "harn.toml",
            r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "cron-ok"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_ok"

[[triggers]]
id = "cron-fail"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_fail"
retry = { max = 1, backoff = "immediate", retention_days = 7 }
"#,
        );
        write_file(
            temp.path(),
            "lib.harn",
            r#"
import "std/triggers"

pub fn on_ok(event: TriggerEvent) -> dict {
  log("ok:" + event.kind)
  return {kind: event.kind, event_id: event.id, trace_id: event.trace_id}
}

pub fn on_fail(event: TriggerEvent) -> any {
  throw "boom:" + event.kind
}
"#,
        );
    }

    async fn init_session(service: &McpOrchestratorService) -> ConnectionState {
        let mut session = ConnectionState::default();
        let response = service
            .handle_request(
                &mut session,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": { "name": "test-client", "version": "1.0.0" }
                    }
                }),
            )
            .await;
        assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            json!(true)
        );
        assert_eq!(
            response["result"]["capabilities"]["resources"]["listChanged"],
            json!(true)
        );
        assert_eq!(
            response["result"]["capabilities"]["prompts"]["listChanged"],
            json!(true)
        );
        assert_eq!(
            response["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"],
            json!({})
        );
        session
    }

    async fn call_tool(
        service: &McpOrchestratorService,
        session: &mut ConnectionState,
        name: &str,
        arguments: JsonValue,
    ) -> JsonValue {
        let response = service
            .handle_request(
                session,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": name,
                        "arguments": arguments,
                    }
                }),
            )
            .await;
        assert_eq!(response["result"]["isError"], false, "response={response}");
        response["result"]["structuredContent"].clone()
    }

    async fn read_resource(
        service: &McpOrchestratorService,
        session: &mut ConnectionState,
        uri: &str,
    ) -> JsonValue {
        let response = service
            .handle_request(
                session,
                json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "resources/read",
                    "params": { "uri": uri }
                }),
            )
            .await;
        let text = response["result"]["contents"][0]["text"]
            .as_str()
            .expect("resource text");
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }

    // Wait for the next non-lagged notification, treating broadcast
    // `Lagged` errors as benign (production listeners at lines 1154 /
    // 1786 do the same). Without this, fixture-write storms during test
    // setup can fill the 64-slot broadcast buffer faster than the
    // subscriber drains it on a loaded CI runner, producing a spurious
    // `Lagged(N)` panic.
    async fn recv_next_notification(
        notifications: &mut broadcast::Receiver<JsonValue>,
    ) -> JsonValue {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for notification");
            match tokio::time::timeout(remaining, notifications.recv())
                .await
                .expect("timed out waiting for notification")
            {
                Ok(msg) => return msg,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("notification channel closed")
                }
            }
        }
    }

    async fn collect_notification_methods(
        notifications: &mut broadcast::Receiver<JsonValue>,
        expected: &[&str],
    ) -> std::collections::BTreeSet<String> {
        let expected = expected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let mut seen = std::collections::BTreeSet::new();
        while !expected.iter().all(|method| seen.contains(*method)) {
            let notification = recv_next_notification(notifications).await;
            if let Some(method) = notification.get("method").and_then(JsonValue::as_str) {
                seen.insert(method.to_string());
            }
        }
        seen
    }

    async fn recv_next_task_notification(
        notifications: &mut broadcast::Receiver<McpTaskNotification>,
    ) -> McpTaskNotification {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for task notification"
            );
            match tokio::time::timeout(remaining, notifications.recv())
                .await
                .expect("timed out waiting for task notification")
            {
                Ok(msg) => return msg,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("task notification channel closed")
                }
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn latest_spec_gap_methods_return_explicit_json_rpc_errors() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        for method in mcp_protocol::UNSUPPORTED_LATEST_SPEC_METHODS
            .iter()
            .map(|entry| entry.method)
        {
            let response = service
                .handle_request(
                    &mut session,
                    harn_vm::jsonrpc::request(99, method, json!({})),
                )
                .await;
            assert_eq!(response["error"]["code"], json!(-32601), "{method}");
            assert_eq!(response["error"]["data"]["method"], json!(method));
            assert_eq!(response["error"]["data"]["status"], json!("unsupported"));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_prompt_and_resource_template_lists_roundtrip() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let templates = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(10, "resources/templates/list", json!({})),
            )
            .await;
        assert_eq!(templates["result"]["resourceTemplates"], json!([]));

        let prompts = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(11, "prompts/list", json!({})),
            )
            .await;
        assert_eq!(prompts["result"]["prompts"], json!([]));

        let prompt = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(12, "prompts/get", json!({"name": "missing"})),
            )
            .await;
        assert_eq!(prompt["error"]["code"], json!(-32602));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_backed_prompts_list_render_and_notify_changes() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        write_file(temp.path(), "pixel.png", "fake");
        write_file(
            temp.path(),
            "review.harn.prompt",
            r#"---
id = "review"
description = "Review code"
images = [{ path = "pixel.png", mime_type = "image/png" }]
[[arguments]]
name = "code"
description = "Code to review"
required = true
---
Review this: {{ code }}
"#,
        );
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;
        let mut notifications = service.subscribe_list_notifications();

        let prompts = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(20, "prompts/list", json!({})),
            )
            .await;
        assert_eq!(prompts["result"]["prompts"][0]["name"], json!("review"));
        assert_eq!(
            prompts["result"]["prompts"][0]["arguments"][0]["description"],
            json!("Code to review")
        );

        let missing = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(21, "prompts/get", json!({"name": "review"})),
            )
            .await;
        assert_eq!(missing["error"]["code"], json!(-32602));

        let prompt = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(
                    22,
                    "prompts/get",
                    json!({"name": "review", "arguments": {"code": "fn main() {}"}}),
                ),
            )
            .await;
        assert!(prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("fn main"));
        assert_eq!(
            prompt["result"]["messages"][1]["content"]["type"],
            json!("image")
        );
        assert_eq!(
            prompt["result"]["messages"][1]["content"]["data"],
            json!("ZmFrZQ==")
        );

        write_file(
            temp.path(),
            "review.harn.prompt",
            r#"---
id = "review"
[[arguments]]
name = "code"
required = true
---
Updated: {{ code }}
"#,
        );
        // Writing a `.harn.prompt` file can also trigger the tools/resources
        // watchers, so the order of incoming notifications is not deterministic.
        // Drain until we observe `prompts/list_changed`; ignore any others.
        let seen = collect_notification_methods(
            &mut notifications,
            &["notifications/prompts/list_changed"],
        )
        .await;
        assert!(seen.contains("notifications/prompts/list_changed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn package_metadata_changes_notify_tools_resources_and_prompts() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut notifications = service.subscribe_list_notifications();

        write_file(
            temp.path(),
            "harn.lock",
            r#"
[[package]]
name = "prompt-pack"
version = "0.1.0"
"#,
        );

        let seen = collect_notification_methods(
            &mut notifications,
            &[
                "notifications/tools/list_changed",
                "notifications/resources/list_changed",
                "notifications/prompts/list_changed",
            ],
        )
        .await;
        assert!(seen.contains("notifications/tools/list_changed"));
        assert!(seen.contains("notifications/resources/list_changed"));
        assert!(seen.contains("notifications/prompts/list_changed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_list_advertises_task_support_per_tool() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let response = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(30, "tools/list", json!({})),
            )
            .await;
        let tools = response["result"]["tools"].as_array().unwrap();
        let trigger_fire = tools
            .iter()
            .find(|tool| tool["name"] == "harn.trigger.fire")
            .unwrap();
        let trigger_list = tools
            .iter()
            .find(|tool| tool["name"] == "harn.trigger.list")
            .unwrap();
        assert_eq!(trigger_fire["execution"]["taskSupport"], json!("optional"));
        assert_eq!(trigger_list["execution"]["taskSupport"], json!("forbidden"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_endpoints_page_with_cursor() {
        let _env_lock = lock_env().lock().await;
        let _guard = lock_harn_state();
        let _page_size = ScopedEnvVar::set(mcp_protocol::MCP_LIST_PAGE_SIZE_ENV, "1");
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        write_file(
            temp.path(),
            "first.harn.prompt",
            "---\nid = \"first\"\n---\nFirst",
        );
        write_file(
            temp.path(),
            "second.harn.prompt",
            "---\nid = \"second\"\n---\nSecond",
        );
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;
        call_tool(
            &service,
            &mut session,
            "harn.trigger.fire",
            json!({
                "trigger_id": "cron-ok",
                "payload": { "headers": { "x-page-test": "1" } }
            }),
        )
        .await;

        let first_tools = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(40, "tools/list", json!({})),
            )
            .await;
        assert_eq!(first_tools["result"]["tools"].as_array().unwrap().len(), 1);
        let tools_cursor = first_tools["result"]["nextCursor"].as_str().unwrap();
        let next_tools = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(41, "tools/list", json!({"cursor": tools_cursor})),
            )
            .await;
        assert_eq!(next_tools["result"]["tools"].as_array().unwrap().len(), 1);
        assert_ne!(
            first_tools["result"]["tools"][0]["name"],
            next_tools["result"]["tools"][0]["name"]
        );

        let first_prompts = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(42, "prompts/list", json!({})),
            )
            .await;
        assert_eq!(
            first_prompts["result"]["prompts"].as_array().unwrap().len(),
            1
        );
        let prompts_cursor = first_prompts["result"]["nextCursor"].as_str().unwrap();
        let next_prompts = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(43, "prompts/list", json!({"cursor": prompts_cursor})),
            )
            .await;
        assert_eq!(
            next_prompts["result"]["prompts"].as_array().unwrap().len(),
            1
        );
        assert_ne!(
            first_prompts["result"]["prompts"][0]["name"],
            next_prompts["result"]["prompts"][0]["name"]
        );

        let first_resources = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(44, "resources/list", json!({})),
            )
            .await;
        assert_eq!(
            first_resources["result"]["resources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let resources_cursor = first_resources["result"]["nextCursor"].as_str().unwrap();
        let next_resources = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(
                    45,
                    "resources/list",
                    json!({"cursor": resources_cursor}),
                ),
            )
            .await;
        assert_eq!(
            next_resources["result"]["resources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_ne!(
            first_resources["result"]["resources"][0]["uri"],
            next_resources["result"]["resources"][0]["uri"]
        );

        let templates = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(46, "resources/templates/list", json!({})),
            )
            .await;
        assert_eq!(templates["result"]["resourceTemplates"], json!([]));

        let invalid = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(47, "resources/list", json!({"cursor": "nope"})),
            )
            .await;
        assert_eq!(invalid["error"]["code"], json!(-32602));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_rejects_task_augmentation() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let response = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(
                    100,
                    "tools/call",
                    json!({
                        "name": "harn.trigger.list",
                        "arguments": {},
                        "task": {"title": "async please"}
                    }),
                ),
            )
            .await;
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_fire_task_roundtrip_polls_and_retrieves_result() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;
        let mut task_notifications = service.subscribe_task_notifications();

        let created = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(
                    101,
                    "tools/call",
                    json!({
                        "name": "harn.trigger.fire",
                        "arguments": {
                            "trigger_id": "cron-ok",
                            "payload": {}
                        },
                        "task": {"ttl": 60_000}
                    }),
                ),
            )
            .await;
        assert_eq!(created["result"]["task"]["status"], json!("working"));
        assert_eq!(created["result"]["task"]["ttl"], json!(60_000));
        let task_id = created["result"]["task"]["taskId"].as_str().unwrap();

        let listed = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(102, "tasks/list", json!({})),
            )
            .await;
        assert_eq!(listed["result"]["tasks"][0]["taskId"], json!(task_id));

        let result = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(103, "tasks/result", json!({ "taskId": task_id })),
            )
            .await;
        assert_eq!(result["result"]["isError"], json!(false), "result={result}");
        assert_eq!(
            result["result"]["_meta"][mcp_protocol::RELATED_TASK_META_KEY]["taskId"],
            json!(task_id)
        );
        assert_eq!(
            result["result"]["structuredContent"]["status"],
            json!("dispatched")
        );

        let task = service
            .handle_request(
                &mut session,
                harn_vm::jsonrpc::request(104, "tasks/get", json!({ "taskId": task_id })),
            )
            .await;
        assert_eq!(task["result"]["status"], json!("completed"));

        let mut statuses = std::collections::BTreeSet::new();
        while !statuses.contains("completed") {
            let notification = recv_next_task_notification(&mut task_notifications).await;
            if notification.owner == session.client_identity {
                let status = notification.message["params"]["status"].as_str().unwrap();
                statuses.insert(status.to_string());
            }
        }
        assert!(statuses.contains("working"));
        assert!(statuses.contains("completed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_list_tool_returns_manifest_bindings() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let result = call_tool(&service, &mut session, "harn.trigger.list", json!({})).await;
        let triggers = result["triggers"].as_array().unwrap();
        assert_eq!(triggers.len(), 2);
        assert!(triggers
            .iter()
            .any(|trigger| trigger["trigger_id"] == "cron-ok"));
        assert!(triggers
            .iter()
            .any(|trigger| trigger["trigger_id"] == "cron-fail"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn secret_scan_tool_returns_findings_and_audits_them() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let result = call_tool(
            &service,
            &mut session,
            "harn.secret_scan",
            json!({
                "content": r#"token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB""#,
            }),
        )
        .await;
        let findings = result.as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["detector"], "github-token");

        let ctx = load_local_runtime(&service.local_args()).await.unwrap();
        let events = read_topic(&ctx.event_log, harn_vm::SECRET_SCAN_AUDIT_TOPIC)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.payload["caller"], "mcp.harn.secret_scan");
        assert_eq!(events[0].1.payload["finding_count"], 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_fire_roundtrip_records_event_resource_and_action_graph() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let fire = call_tool(
            &service,
            &mut session,
            "harn.trigger.fire",
            json!({
                "trigger_id": "cron-ok",
                "payload": {
                    "headers": { "x-test": "1" }
                }
            }),
        )
        .await;
        assert_eq!(fire["status"], "dispatched");
        let event_id = fire["event_id"].as_str().unwrap();
        let event =
            read_resource(&service, &mut session, &format!("harn://event/{event_id}")).await;
        assert_eq!(
            event["event"]["headers"]["x-harn-mcp-client"],
            "test-client/1.0.0"
        );

        let ctx = load_local_runtime(&service.local_args()).await.unwrap();
        let action_graph = read_topic(&ctx.event_log, ACTION_GRAPH_TOPIC)
            .await
            .unwrap();
        assert!(
            action_graph.iter().any(|(_, event)| {
                event.payload["context"]["tool_name"] == json!("harn.trigger.fire")
            }),
            "action_graph={action_graph:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trigger_replay_tool_replays_event() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;
        let fire = call_tool(
            &service,
            &mut session,
            "harn.trigger.fire",
            json!({ "trigger_id": "cron-ok", "payload": {} }),
        )
        .await;
        let replay = call_tool(
            &service,
            &mut session,
            "harn.trigger.replay",
            json!({ "event_id": fire["event_id"] }),
        )
        .await;
        assert_eq!(replay["status"], "dispatched");
        assert_eq!(replay["replay_of_event_id"], fire["event_id"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dlq_tools_roundtrip_and_resource_read() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let fire = call_tool(
            &service,
            &mut session,
            "harn.trigger.fire",
            json!({ "trigger_id": "cron-fail", "payload": {} }),
        )
        .await;
        assert_eq!(fire["status"], "dlq");
        let entries = call_tool(
            &service,
            &mut session,
            "harn.orchestrator.dlq.list",
            json!({}),
        )
        .await;
        let entry_id = entries["entries"][0]["id"].as_str().unwrap();
        let detail = read_resource(&service, &mut session, &format!("harn://dlq/{entry_id}")).await;
        assert_eq!(detail["id"], entry_id);

        let retry = call_tool(
            &service,
            &mut session,
            "harn.orchestrator.dlq.retry",
            json!({ "entry_id": entry_id }),
        )
        .await;
        assert_eq!(retry["entry_id"], entry_id);
        assert_eq!(retry["handle"]["replay_of_event_id"], fire["event_id"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queue_and_inspect_tools_return_snapshots() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let _ = call_tool(
            &service,
            &mut session,
            "harn.trigger.fire",
            json!({ "trigger_id": "cron-ok", "payload": {} }),
        )
        .await;
        let queue = call_tool(&service, &mut session, "harn.orchestrator.queue", json!({})).await;
        assert!(queue["outbox"]["count"].as_u64().unwrap() >= 1);

        let inspect = call_tool(
            &service,
            &mut session,
            "harn.orchestrator.inspect",
            json!({}),
        )
        .await;
        assert_eq!(inspect["triggers"].as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trust_query_returns_filtered_trace_groups() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let ctx = load_local_runtime(&service.local_args()).await.unwrap();
        harn_vm::append_trust_record(
            &ctx.event_log,
            &harn_vm::TrustRecord::new(
                "ide-bot",
                "issue.opened",
                None,
                harn_vm::TrustOutcome::Success,
                "trace-1",
                harn_vm::AutonomyTier::ActAuto,
            ),
        )
        .await
        .unwrap();
        harn_vm::append_trust_record(
            &ctx.event_log,
            &harn_vm::TrustRecord::new(
                "ide-bot",
                "issue.closed",
                None,
                harn_vm::TrustOutcome::Success,
                "trace-2",
                harn_vm::AutonomyTier::ActAuto,
            ),
        )
        .await
        .unwrap();
        harn_vm::append_trust_record(
            &ctx.event_log,
            &harn_vm::TrustRecord::new(
                "ide-bot",
                "issue.commented",
                None,
                harn_vm::TrustOutcome::Failure,
                "trace-2",
                harn_vm::AutonomyTier::ActAuto,
            ),
        )
        .await
        .unwrap();

        let result = call_tool(
            &service,
            &mut session,
            "harn.trust.query",
            json!({
                "agent": "ide-bot",
                "grouped_by_trace": true,
                "limit": 2
            }),
        )
        .await;
        assert_eq!(result["grouped_by_trace"], json!(true));
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
        assert_eq!(result["results"][0]["trace_id"], "trace-2");
        assert_eq!(result["results"][0]["records"].as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_resource_reads_raw_manifest() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let service = McpOrchestratorService::new(&fixture_args(&temp)).unwrap();
        let mut session = init_session(&service).await;

        let manifest = read_resource(&service, &mut session, "harn://manifest").await;
        let manifest = manifest.as_str().unwrap();
        assert!(manifest.contains("[[triggers]]"));
        assert!(manifest.contains("cron-ok"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_http_endpoint_supports_sse_get_delete_and_session_headers() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let args = fixture_args(&temp);
        let service = Arc::new(McpOrchestratorService::new_local(args.local.clone()).unwrap());
        let router = http_router_for_service(
            service.clone(),
            "/mcp".to_string(),
            "/sse".to_string(),
            "/messages".to_string(),
        );

        let init = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "application/json, text/event-stream")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "initialize",
                            "params": {
                                "protocolVersion": MCP_PROTOCOL_VERSION,
                                "capabilities": {},
                                "clientInfo": { "name": "streamable-test", "version": "1.0.0" }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(init.status(), StatusCode::OK);
        assert_eq!(
            init.headers()
                .get(MCP_PROTOCOL_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(MCP_PROTOCOL_VERSION)
        );
        let session_id = init
            .headers()
            .get(MCP_SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id")
            .to_string();

        let tools = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "text/event-stream")
                    .header(MCP_SESSION_HEADER, &session_id)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        harn_vm::jsonrpc::request(2, "tools/list", json!({})).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tools.status(), StatusCode::OK);
        assert!(tools
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")));
        let body = to_bytes(tools.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("event: message"), "{body}");
        assert!(body.contains("harn.trigger.list"), "{body}");

        let get = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/mcp")
                    .header("accept", "text/event-stream")
                    .header(MCP_SESSION_HEADER, &session_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        assert!(get
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")));
        let mut stream = get.into_body().into_data_stream();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for SSE prime event")
            .expect("SSE stream ended")
            .expect("SSE body error");
        service.notify_manifest_reloaded();
        let mut streamed = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !streamed.contains("notifications/tools/list_changed")
            || !streamed.contains("notifications/resources/list_changed")
        {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for list_changed SSE notifications; body={streamed}"
            );
            let chunk = tokio::time::timeout(remaining, stream.next())
                .await
                .expect("timed out waiting for SSE notification")
                .expect("SSE stream ended")
                .expect("SSE body error");
            streamed.push_str(std::str::from_utf8(&chunk).unwrap());
        }

        let delete = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/mcp")
                    .header(MCP_SESSION_HEADER, &session_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::NO_CONTENT);

        let after_delete = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "application/json")
                    .header(MCP_SESSION_HEADER, &session_id)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        harn_vm::jsonrpc::request(3, "tools/list", json!({})).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_metadata_and_challenge_are_served_when_configured() {
        let _env_lock = lock_env().lock().await;
        // Acquire the harn-state lock BEFORE setting env vars. Rust drops
        // bindings in reverse declaration order, so env vars must be
        // declared *after* the lock to be cleared before another test
        // can acquire `lock_harn_state()` and read leaked OAuth config.
        let _guard = lock_harn_state();
        let _auth_servers = ScopedEnvVar::set(
            "HARN_MCP_OAUTH_AUTHORIZATION_SERVERS",
            "https://auth.example.test",
        );
        let _introspection = ScopedEnvVar::set(
            "HARN_MCP_OAUTH_INTROSPECTION_URL",
            "https://auth.example.test/introspect",
        );
        let _resource =
            ScopedEnvVar::set("HARN_MCP_OAUTH_RESOURCE", "https://mcp.example.test/mcp");
        let _audience =
            ScopedEnvVar::set("HARN_MCP_OAUTH_AUDIENCE", "https://mcp.example.test/mcp");
        let _scopes = ScopedEnvVar::set("HARN_MCP_OAUTH_SCOPES", "harn:mcp");
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let args = fixture_args(&temp);
        let router = http_router_for_local(
            args.local.clone(),
            "/mcp".to_string(),
            "/sse".to_string(),
            "/messages".to_string(),
        )
        .unwrap();

        let metadata = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/.well-known/oauth-protected-resource/mcp")
                    .header("host", "mcp.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metadata.status(), StatusCode::OK);
        let body = to_bytes(metadata.into_body(), usize::MAX).await.unwrap();
        let metadata: JsonValue = serde_json::from_slice(&body).unwrap();
        assert_eq!(metadata["resource"], json!("https://mcp.example.test/mcp"));
        assert_eq!(
            metadata["authorization_servers"],
            json!(["https://auth.example.test"])
        );
        assert_eq!(metadata["scopes_supported"], json!(["harn:mcp"]));

        let unauthorized = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("host", "mcp.example.test")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let challenge = unauthorized
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(challenge.starts_with("Bearer "), "{challenge}");
        assert!(
            challenge.contains(
                "resource_metadata=\"http://mcp.example.test/.well-known/oauth-protected-resource/mcp\""
            ),
            "{challenge}"
        );
        assert!(challenge.contains("scope=\"harn:mcp\""), "{challenge}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_introspection_accepts_valid_token_and_rejects_wrong_audience() {
        async fn introspect(Form(form): Form<BTreeMap<String, String>>) -> Json<JsonValue> {
            match form.get("token").map(String::as_str) {
                Some("valid-token") => Json(json!({
                    "active": true,
                    "aud": "mcp://harn-test",
                    "scope": "harn:mcp",
                    "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
                })),
                Some("wrong-audience") => Json(json!({
                    "active": true,
                    "aud": "mcp://other",
                    "scope": "harn:mcp",
                    "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
                })),
                Some("expired-token") => Json(json!({
                    "active": true,
                    "aud": "mcp://harn-test",
                    "scope": "harn:mcp",
                    "exp": OffsetDateTime::now_utc().unix_timestamp() - 1
                })),
                Some("missing-scope") => Json(json!({
                    "active": true,
                    "aud": "mcp://harn-test",
                    "scope": "other:scope",
                    "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600
                })),
                _ => Json(json!({ "active": false })),
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let auth_addr = listener.local_addr().unwrap();
        let auth_server = tokio::spawn(async move {
            axum::serve(
                listener,
                AxumRouter::new().route("/introspect", post(introspect)),
            )
            .await
            .unwrap();
        });

        let _env_lock = lock_env().lock().await;
        // Acquire the harn-state lock BEFORE setting env vars so they
        // are dropped (cleared) before another test can re-enter the
        // lock — see the matching comment in
        // `oauth_metadata_and_challenge_are_served_when_configured`.
        let _guard = lock_harn_state();
        let auth_server_url = format!("http://{auth_addr}");
        let introspection_url = format!("{auth_server_url}/introspect");
        let _auth_servers =
            ScopedEnvVar::set("HARN_MCP_OAUTH_AUTHORIZATION_SERVERS", &auth_server_url);
        let _introspection =
            ScopedEnvVar::set("HARN_MCP_OAUTH_INTROSPECTION_URL", &introspection_url);
        let _audience = ScopedEnvVar::set("HARN_MCP_OAUTH_AUDIENCE", "mcp://harn-test");
        let _scopes = ScopedEnvVar::set("HARN_MCP_OAUTH_SCOPES", "harn:mcp");
        let _resource = ScopedEnvVar::set("HARN_MCP_OAUTH_RESOURCE", "mcp://harn-test");
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let args = fixture_args(&temp);
        let router = http_router_for_local(
            args.local.clone(),
            "/mcp".to_string(),
            "/sse".to_string(),
            "/messages".to_string(),
        )
        .unwrap();

        let initialize_body = Body::from(
            harn_vm::jsonrpc::request(
                1,
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "oauth-test", "version": "1.0.0" }
                }),
            )
            .to_string(),
        );
        let valid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, "Bearer valid-token")
                    .body(initialize_body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(valid.status(), StatusCode::OK);

        for token in ["wrong-audience", "expired-token"] {
            let rejected = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/mcp")
                        .header("accept", "application/json")
                        .header("content-type", "application/json")
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::from(
                            harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED, "token={token}");
            assert!(rejected
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|challenge| challenge.contains("error=\"invalid_token\"")));
        }

        let insufficient_scope = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, "Bearer missing-scope")
                    .body(Body::from(
                        harn_vm::jsonrpc::request(1, "initialize", json!({})).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(insufficient_scope.status(), StatusCode::FORBIDDEN);
        assert!(insufficient_scope
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(
                |challenge| challenge.contains("error=\"insufficient_scope\"")
                    && challenge.contains("scope=\"harn:mcp\"")
            ));

        auth_server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_sse_routes_are_marked_deprecated() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);
        let args = fixture_args(&temp);
        let router = http_router_for_local(
            args.local.clone(),
            "/mcp".to_string(),
            "/sse".to_string(),
            "/messages".to_string(),
        )
        .unwrap();

        let sse = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sse")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(sse.status(), StatusCode::OK);
        assert_eq!(
            sse.headers()
                .get(DEPRECATION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        drop(sse);

        let messages = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/messages")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(messages.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            messages
                .headers()
                .get(DEPRECATION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
    }
}
