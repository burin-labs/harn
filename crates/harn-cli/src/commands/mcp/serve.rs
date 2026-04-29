use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::{stream, StreamExt};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};
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

use super::prompts::FilePromptCatalog;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const DEFAULT_RESOURCE_LIMIT: usize = 200;

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    config_path: PathBuf,
    state_dir: PathBuf,
    manifest_source: Arc<String>,
    auth: ListenerAuth,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    prompt_notify_tx: broadcast::Sender<JsonValue>,
    _prompt_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
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
    messages_path: String,
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
}

#[derive(Clone, Debug, Deserialize)]
struct DlqRetryRequest {
    entry_id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SecretScanRequest {
    content: String,
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
        let auth = ListenerAuth::from_env(false)?;
        let project_root = local
            .config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let prompt_catalog = Arc::new(Mutex::new(FilePromptCatalog::discover(
            &project_root,
            &manifest_source,
        )));
        let (prompt_notify_tx, _) = broadcast::channel(32);
        let prompt_watcher = start_prompt_watcher(
            project_root,
            local.config.clone(),
            prompt_catalog.clone(),
            prompt_notify_tx.clone(),
        );
        Ok(Self {
            config_path: local.config,
            state_dir: local.state_dir,
            manifest_source: Arc::new(manifest_source),
            auth,
            prompt_catalog,
            prompt_notify_tx,
            _prompt_watcher: Arc::new(Mutex::new(prompt_watcher)),
        })
    }

    fn local_args(&self) -> OrchestratorLocalArgs {
        OrchestratorLocalArgs {
            config: self.config_path.clone(),
            state_dir: self.state_dir.clone(),
        }
    }

    fn subscribe_prompt_notifications(&self) -> broadcast::Receiver<JsonValue> {
        self.prompt_notify_tx.subscribe()
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
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(id, session, &params).await,
            "resources/list" => self.handle_resources_list(id).await,
            "resources/read" => self.handle_resources_read(id, &params).await,
            "resources/templates/list" => {
                harn_vm::jsonrpc::response(id, json!({"resourceTemplates": []}))
            }
            "prompts/list" => self.handle_prompts_list(id),
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

        if self.auth.has_api_keys() {
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
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false },
                    "prompts": { "listChanged": true },
                    "logging": {},
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

    fn handle_prompts_list(&self, id: JsonValue) -> JsonValue {
        let prompts = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .list();
        harn_vm::jsonrpc::response(id, json!({ "prompts": prompts }))
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

    fn handle_tools_list(&self, id: JsonValue) -> JsonValue {
        harn_vm::jsonrpc::response(
            id,
            json!({
                "tools": [
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
                    ),
                    tool_def(
                        "harn.trigger.replay",
                        "Replay an existing trigger event, optionally resolving bindings as of a historical timestamp.",
                        json!({
                            "type": "object",
                            "required": ["event_id"],
                            "properties": {
                                "event_id": { "type": "string" },
                                "as_of": { "type": "string" },
                            },
                            "additionalProperties": false,
                        }),
                        None,
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
                    ),
                ]
            }),
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
            return mcp_protocol::unsupported_task_augmentation_response(id, "tools/call");
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());

        let result = match name {
            "harn.secret_scan" | "harn::secret_scan" => self.tool_secret_scan(arguments).await,
            "harn.trigger.fire" => self.tool_trigger_fire(session, &trace_id, arguments).await,
            "harn.trigger.list" => self.tool_trigger_list(arguments).await,
            "harn.trigger.replay" => self.tool_trigger_replay(arguments).await,
            "harn.orchestrator.queue" => self.tool_orchestrator_queue(arguments).await,
            "harn.orchestrator.dlq.list" => self.tool_orchestrator_dlq_list(arguments).await,
            "harn.orchestrator.dlq.retry" => self.tool_orchestrator_dlq_retry(arguments).await,
            "harn.orchestrator.inspect" => self.tool_orchestrator_inspect(arguments).await,
            "harn.trust.query" => self.tool_trust_query(arguments).await,
            _ => Err(format!("unknown tool '{name}'")),
        };

        let _ = self
            .record_tool_call(name, &trace_id, &session.client_identity, &result)
            .await;

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

    async fn handle_resources_list(&self, id: JsonValue) -> JsonValue {
        match self.list_resources().await {
            Ok(resources) => harn_vm::jsonrpc::response(id, json!({ "resources": resources })),
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
        if let Some(as_of) = request.as_of.as_deref() {
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
                Some(as_of),
                false,
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
        for (event_id, event) in recorded.into_iter().take(DEFAULT_RESOURCE_LIMIT) {
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
            return Ok(((*self.manifest_source).clone(), "application/toml"));
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
    let mut prompt_notifications = service.subscribe_prompt_notifications();

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
            notification = prompt_notifications.recv() => {
                match notification {
                    Ok(notification) => write_stdio_json(&mut stdout, &notification).await?,
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

fn start_prompt_watcher(
    project_root: PathBuf,
    config_path: PathBuf,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    prompt_notify_tx: broadcast::Sender<JsonValue>,
) -> Option<notify::RecommendedWatcher> {
    let project_root_for_callback = project_root.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        if !event
            .paths
            .iter()
            .any(|path| is_prompt_reload_path(path.as_path()))
        {
            return;
        }
        let manifest_source = std::fs::read_to_string(&config_path).unwrap_or_default();
        let updated = FilePromptCatalog::discover(&project_root_for_callback, &manifest_source);
        *prompt_catalog.lock().expect("prompt catalog poisoned") = updated;
        let _ = prompt_notify_tx.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/prompts/list_changed",
        }));
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

pub(crate) fn http_router_for_local(
    local: OrchestratorLocalArgs,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Result<Router, String> {
    let service = Arc::new(McpOrchestratorService::new_local(local)?);
    Ok(http_router(service, path, sse_path, messages_path))
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
        messages_path: messages_path.clone(),
    };
    Router::new()
        .route(&path, post(http_post_request).delete(http_delete_session))
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

async fn http_post_request(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let normalized = normalized_headers(&headers);
    if state.service.auth.has_api_keys() {
        let auth_log = match auth_event_log(&state.service.state_dir) {
            Ok(log) => log,
            Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
        };
        if let Err(()) = state
            .service
            .auth
            .authorize(
                auth_log.as_ref(),
                method.as_str(),
                &state.mcp_path,
                &normalized,
                body.as_ref(),
            )
            .await
        {
            return (StatusCode::UNAUTHORIZED, "auth failed").into_response();
        }
    }

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

    let current = session.state.lock().expect("HTTP session poisoned").clone();
    let (updated, response_json) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    *session.state.lock().expect("HTTP session poisoned") = updated;
    let mut response = Json(response_json).into_response();
    if created {
        response.headers_mut().insert(
            HeaderName::from_static(MCP_SESSION_HEADER),
            HeaderValue::from_str(&session_id)
                .unwrap_or_else(|_| HeaderValue::from_static("invalid")),
        );
    }
    response.headers_mut().insert(
        HeaderName::from_static(MCP_PROTOCOL_HEADER),
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    response
}

async fn http_delete_session(State(state): State<HttpState>, headers: HeaderMap) -> Response {
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
    if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn legacy_sse_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let normalized = normalized_headers(&headers);
    if state.service.auth.has_api_keys() {
        let auth_log =
            match harn_vm::event_log::EventLogConfig::for_base_dir(&state.service.state_dir)
                .ok()
                .and_then(|config| harn_vm::event_log::open_event_log(&config).ok())
            {
                Some(log) => log,
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to open event log",
                    )
                        .into_response()
                }
            };
        if let Err(()) = state
            .service
            .auth
            .authorize(
                auth_log.as_ref(),
                "GET",
                &state.messages_path,
                &normalized,
                &[],
            )
            .await
        {
            return (StatusCode::UNAUTHORIZED, "auth failed").into_response();
        }
    }

    let session_id = Uuid::now_v7().to_string();
    let session = Arc::new(HttpSession::default());
    let (tx, rx) = unbounded::<JsonValue>();
    *session.sse_tx.lock().expect("SSE sender poisoned") = Some(tx);
    let mut prompt_notifications = state.service.subscribe_prompt_notifications();
    let prompt_tx = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(prompt_tx) = prompt_tx {
        tokio::spawn(async move {
            loop {
                match prompt_notifications.recv().await {
                    Ok(message) => {
                        if prompt_tx.unbounded_send(message).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
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
                .event("message")
                .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
        }),
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn legacy_sse_message(
    State(state): State<HttpState>,
    Query(query): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> Response {
    let Some(session_id) = query.get("session_id") else {
        return (StatusCode::BAD_REQUEST, "missing session_id").into_response();
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        return (StatusCode::NOT_FOUND, "unknown session").into_response();
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
    let current = session
        .state
        .lock()
        .expect("legacy SSE session poisoned")
        .clone();
    let (updated, response) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    *session.state.lock().expect("legacy SSE session poisoned") = updated;
    if response.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let Some(sender) = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned()
    else {
        return (StatusCode::GONE, "session stream closed").into_response();
    };
    if sender.unbounded_send(response).is_err() {
        return (StatusCode::GONE, "session stream closed").into_response();
    }
    StatusCode::ACCEPTED.into_response()
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

fn tool_def(
    name: &str,
    description: &str,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
) -> JsonValue {
    let mut value = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    });
    if let Some(output_schema) = output_schema {
        value["outputSchema"] = output_schema;
    }
    value
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
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use crate::tests::common::harn_state_lock::lock_harn_state;

    fn write_file(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
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
            response["result"]["capabilities"]["prompts"]["listChanged"],
            json!(true)
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
        let mut notifications = service.subscribe_prompt_notifications();

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
        let notification =
            tokio::time::timeout(std::time::Duration::from_secs(5), notifications.recv())
                .await
                .expect("timed out waiting for prompt list_changed")
                .expect("prompt notification channel closed");
        assert_eq!(
            notification["method"],
            json!("notifications/prompts/list_changed")
        );
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
}
