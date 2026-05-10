use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use harn_serve::FilePromptCatalog;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
const DEPRECATION_HEADER: &str = "deprecation";
const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const DEFAULT_TASK_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_TASK_TTL_MS: u64 = 60 * 60 * 1000;
const LOG_NOTIFICATION_CAPACITY: usize = 256;

/// Audit and observability topics surfaced to MCP clients via
/// `notifications/message`. Each binding gives the topic a stable
/// `logger` name (per the MCP logging spec) and a fallback severity
/// for events whose kind/headers do not signal a more specific level.
struct McpLogStreamBinding {
    topic: &'static str,
    logger: &'static str,
    default_level: mcp_protocol::McpLogLevel,
}

const LOG_STREAM_BINDINGS: &[McpLogStreamBinding] = &[
    McpLogStreamBinding {
        topic: harn_vm::SECRET_SCAN_AUDIT_TOPIC,
        logger: "harn.audit.secret_scan",
        default_level: mcp_protocol::McpLogLevel::Notice,
    },
    McpLogStreamBinding {
        topic: harn_vm::SIGNATURE_VERIFY_AUDIT_TOPIC,
        logger: "harn.audit.signature_verify",
        default_level: mcp_protocol::McpLogLevel::Notice,
    },
    McpLogStreamBinding {
        topic: harn_vm::egress::EGRESS_AUDIT_TOPIC,
        logger: "harn.connectors.egress.audit",
        default_level: mcp_protocol::McpLogLevel::Notice,
    },
    McpLogStreamBinding {
        topic: harn_vm::TRIGGER_OPERATION_AUDIT_TOPIC,
        logger: "harn.trigger.operations.audit",
        default_level: mcp_protocol::McpLogLevel::Notice,
    },
    McpLogStreamBinding {
        topic: TRIGGER_DLQ_TOPIC,
        logger: "harn.trigger.dlq",
        default_level: mcp_protocol::McpLogLevel::Warning,
    },
    McpLogStreamBinding {
        topic: ACTION_GRAPH_TOPIC,
        logger: "harn.observability.action_graph",
        default_level: mcp_protocol::McpLogLevel::Debug,
    },
];

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    config_path: PathBuf,
    state_dir: PathBuf,
    manifest_source: Arc<Mutex<String>>,
    auth: ListenerAuth,
    oauth: Option<OAuthResourceServer>,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    list_notify_tx: broadcast::Sender<JsonValue>,
    resource_notify_tx: broadcast::Sender<McpResourceNotification>,
    task_notify_tx: broadcast::Sender<McpTaskNotification>,
    log_notify_tx: broadcast::Sender<McpLogNotification>,
    #[allow(dead_code)]
    log_event_log: Option<Arc<harn_vm::event_log::AnyEventLog>>,
    #[allow(dead_code)]
    log_watchers_ready: Arc<LogWatcherReadiness>,
    tasks: Arc<Mutex<BTreeMap<String, McpTaskRecord>>>,
    resource_watchers: Arc<Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>>,
    _list_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    _log_watchers: Arc<AbortOnDrop>,
}

#[derive(Clone, Debug)]
struct McpResourceNotification {
    uri: String,
    message: JsonValue,
}

#[derive(Clone, Debug)]
struct McpTaskNotification {
    owner: String,
    message: JsonValue,
}

#[derive(Clone, Debug)]
struct McpLogNotification {
    level: mcp_protocol::McpLogLevel,
    message: JsonValue,
}

/// Counts the log topic watchers that have finished registering with
/// the event log so callers (currently tests) can deterministically
/// wait until the broadcast subscription is in place before publishing
/// events. Production code never blocks on this.
#[derive(Default)]
struct LogWatcherReadiness {
    ready: std::sync::atomic::AtomicUsize,
    expected: std::sync::atomic::AtomicUsize,
    notify: tokio::sync::Notify,
}

impl LogWatcherReadiness {
    fn record_ready(&self) {
        self.ready.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// Owns spawned tokio tasks and aborts them when the wrapper is
/// dropped. The log topic watchers each hold an `Arc<AnyEventLog>` and
/// would otherwise outlive a dropped service, leaking tasks across
/// test cases that build and drop multiple `McpOrchestratorService`
/// instances on the same runtime.
struct AbortOnDrop(Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
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
    subscribed_resources: BTreeSet<String>,
    log_level: mcp_protocol::McpLogLevel,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            initialized: false,
            authenticated: false,
            client_identity: "unknown".to_string(),
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            subscribed_resources: BTreeSet::new(),
            log_level: mcp_protocol::McpLogLevel::Info,
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
    /// Optional SSE sender already attached to the calling session.
    /// When present, the worker installs a [`harn_vm::mcp_progress::ProgressBus`]
    /// pointing at it for the duration of `handle_request`, allowing
    /// long-running tools to emit `notifications/progress` updates that
    /// stream out the session's open GET endpoint.
    progress_sender: Option<UnboundedSender<JsonValue>>,
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

#[derive(Clone, Debug)]
struct ResourceSubscription {
    uri: String,
    topic: Topic,
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
        let (resource_notify_tx, _) = broadcast::channel(128);
        let (task_notify_tx, _) = broadcast::channel(64);
        let (log_notify_tx, _) = broadcast::channel(LOG_NOTIFICATION_CAPACITY);
        let list_watcher = start_list_change_watcher(
            project_root,
            local.config.clone(),
            manifest_source.clone(),
            prompt_catalog.clone(),
            list_notify_tx.clone(),
        );
        let log_watchers_ready = Arc::new(LogWatcherReadiness::default());
        let (log_event_log, log_watchers) = spawn_log_topic_watchers(
            &local.state_dir,
            log_notify_tx.clone(),
            log_watchers_ready.clone(),
        );
        Ok(Self {
            config_path: local.config,
            state_dir: local.state_dir,
            manifest_source,
            auth,
            oauth,
            prompt_catalog,
            list_notify_tx,
            resource_notify_tx,
            task_notify_tx,
            log_notify_tx,
            log_event_log,
            log_watchers_ready,
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            resource_watchers: Arc::new(Mutex::new(BTreeMap::new())),
            _list_watcher: Arc::new(Mutex::new(list_watcher)),
            _log_watchers: Arc::new(AbortOnDrop(log_watchers)),
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

    fn subscribe_resource_notifications(&self) -> broadcast::Receiver<McpResourceNotification> {
        self.resource_notify_tx.subscribe()
    }

    fn subscribe_task_notifications(&self) -> broadcast::Receiver<McpTaskNotification> {
        self.task_notify_tx.subscribe()
    }

    fn subscribe_log_notifications(&self) -> broadcast::Receiver<McpLogNotification> {
        self.log_notify_tx.subscribe()
    }

    #[cfg(test)]
    async fn wait_for_log_watchers_ready(&self) {
        use std::sync::atomic::Ordering;
        loop {
            let notified = self.log_watchers_ready.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let expected = self.log_watchers_ready.expected.load(Ordering::SeqCst);
            if expected > 0 && self.log_watchers_ready.ready.load(Ordering::SeqCst) >= expected {
                return;
            }
            notified.await;
        }
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
        if let Some(response) =
            mcp_protocol::unsupported_client_bound_method_response(id.clone(), method)
        {
            return response;
        }

        match method {
            "initialized" => JsonValue::Null,
            "ping" => harn_vm::jsonrpc::response(id, json!({})),
            mcp_protocol::METHOD_LOGGING_SET_LEVEL => {
                self.handle_logging_set_level(id, session, &params)
            }
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
            "resources/subscribe" => self.handle_resources_subscribe(id, session, &params).await,
            "resources/unsubscribe" => self.handle_resources_unsubscribe(id, session, &params),
            "resources/templates/list" => self.handle_resource_templates_list(id, &params),
            "prompts/list" => self.handle_prompts_list(id, &params),
            "prompts/get" => self.handle_prompts_get(id, &params),
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                self.handle_completion_complete(id, &params).await
            }
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
                    "resources": { "listChanged": true, "subscribe": true },
                    "prompts": { "listChanged": true },
                    "logging": mcp_protocol::logging_capability(),
                    "tasks": mcp_protocol::tasks_capability(),
                    "completions": mcp_protocol::completions_capability(),
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

    fn handle_logging_set_level(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let Some(level_str) = params.get("level").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "logging/setLevel requires params.level",
            );
        };
        let Some(level) = mcp_protocol::McpLogLevel::from_str_ci(level_str) else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                &format!("logging/setLevel: unsupported level '{level_str}'"),
            );
        };
        session.log_level = level;
        harn_vm::jsonrpc::response(id, json!({}))
    }

    async fn handle_completion_complete(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let Some(ref_type) = params.pointer("/ref/type").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(id, -32602, "completion ref.type is required");
        };
        match ref_type {
            "ref/prompt" => self.handle_prompt_completion(id, params),
            "ref/resource" => self.handle_resource_completion(id, params).await,
            other => harn_vm::jsonrpc::error_response(
                id,
                -32602,
                &format!("Unsupported completion ref.type: {other}"),
            ),
        }
    }

    fn handle_prompt_completion(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let name = params
            .pointer("/ref/name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let Some(argument_name) = params
            .pointer("/argument/name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "completion argument.name is required",
            );
        };
        let value = params
            .pointer("/argument/value")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let result = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .complete(name, argument_name, value);
        match result {
            Ok(completion) => harn_vm::jsonrpc::response(id, json!({ "completion": completion })),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
        }
    }

    async fn handle_resource_completion(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let uri_template = params
            .pointer("/ref/uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let Some(argument_name) = params
            .pointer("/argument/name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "completion argument.name is required",
            );
        };
        let value = params
            .pointer("/argument/value")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();

        let candidates = match (uri_template, argument_name) {
            ("harn://topic/{name}", "name") => match self.resource_template_topic_names().await {
                Ok(candidates) => candidates,
                Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
            },
            ("harn://event/{event_id}", "event_id") => {
                match self.resource_template_event_ids().await {
                    Ok(candidates) => candidates,
                    Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
                }
            }
            ("harn://dlq/{entry_id}", "entry_id") => {
                match self.resource_template_dlq_entry_ids().await {
                    Ok(candidates) => candidates,
                    Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
                }
            }
            ("harn://topic/{name}", other)
            | ("harn://event/{event_id}", other)
            | ("harn://dlq/{entry_id}", other) => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!("Unknown resource template argument: {other}"),
                );
            }
            (other, _) => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!("Unknown resource template: {other}"),
                );
            }
        };

        harn_vm::jsonrpc::response(
            id,
            json!({
                "completion": mcp_protocol::completion_payload(candidates, value),
            }),
        )
    }

    fn handle_tools_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let tools = vec![
            tool_def(
                "harn.secret_scan",
                "Scan content for high-signal secrets before commit or PR-open flows. The `harn::secret_scan` alias is also accepted.",
                read_only_tool_annotations("Secret Scan"),
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
                mutating_open_world_tool_annotations("Fire Trigger"),
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
                read_only_tool_annotations("List Triggers"),
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
                mutating_open_world_tool_annotations("Replay Trigger"),
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
                read_only_tool_annotations("Inspect Orchestrator Queue"),
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
                read_only_tool_annotations("List Dead Letter Queue"),
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
                mutating_open_world_tool_annotations("Retry Dead Letter Queue Entry"),
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
                read_only_tool_annotations("Inspect Orchestrator"),
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
                read_only_tool_annotations("Query Trust Records"),
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
            vec![
                json!({
                    "uriTemplate": "harn://topic/{name}",
                    "name": "topic",
                    "title": "EventLog Topic",
                    "description": "Read a Harn EventLog topic by name.",
                    "mimeType": "application/json",
                }),
                json!({
                    "uriTemplate": "harn://event/{event_id}",
                    "name": "trigger-event",
                    "title": "Trigger Event",
                    "description": "Read a recorded trigger event plus related replay and trace artifacts.",
                    "mimeType": "application/json",
                }),
                json!({
                    "uriTemplate": "harn://dlq/{entry_id}",
                    "name": "dead-letter-entry",
                    "title": "Dead-Letter Entry",
                    "description": "Read one pending dead-letter queue entry.",
                    "mimeType": "application/json",
                }),
            ],
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

        // Bind the request's progressToken to the active outbound bus
        // (installed by the transport) for the duration of the tool
        // call. Built-in tool implementations and any nested Harn
        // handlers can then call `mcp_report_progress(...)` without
        // taking a token argument.
        let progress_ctx = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(harn_vm::mcp_progress::is_valid_progress_token)
            .and_then(|token| {
                harn_vm::mcp_progress::active_bus()
                    .map(|bus| harn_vm::mcp_progress::ProgressContext::new(bus, token))
            });

        // Box-pin the tool-call future before scoping it: handle_tools_call
        // is a deep async state machine and adding another async wrapper
        // grew the stack frame past the test runtime's 2 MiB budget.
        let result = harn_vm::mcp_progress::scope_context(
            progress_ctx,
            Box::pin(self.execute_tool_call(name, session, &trace_id, arguments)),
        )
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

    async fn handle_resources_subscribe(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let subscription = match self.resource_subscription(uri).await {
            Ok(subscription) => subscription,
            Err(error) => return harn_vm::jsonrpc::error_response(id, -32002, &error),
        };
        session
            .subscribed_resources
            .insert(subscription.uri.clone());
        match self.ensure_resource_update_watcher(subscription).await {
            Ok(()) => harn_vm::jsonrpc::response(id, json!({})),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    fn handle_resources_unsubscribe(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        if let Some(uri) = params.get("uri").and_then(JsonValue::as_str) {
            session.subscribed_resources.remove(uri);
        }
        harn_vm::jsonrpc::response(id, json!({}))
    }

    fn notify_topic_resource_changed(&self, topic_name: &str) {
        for uri in resource_uris_for_topic(topic_name) {
            let _ = self.resource_notify_tx.send(McpResourceNotification {
                uri: uri.clone(),
                message: resource_updated_notification(&uri),
            });
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
        report_milestone(0.1, "loading runtime");
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        report_milestone(0.3, "preparing event");
        let mut event = synthetic_event_for_binding(&ctx, &request.trigger_id)?;
        merge_json_object(&mut event, request.payload);
        inject_trace_headers(&mut event, &session.client_identity, trace_id);
        report_milestone(0.5, "firing trigger");
        let handle = trigger_fire(&mut ctx, &request.trigger_id, event).await?;
        report_milestone(0.95, "trigger complete");
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
        resources.extend(static_topic_resources());

        let ctx = load_local_runtime(&self.local_args()).await?;
        for topic in ctx
            .event_log
            .topics()
            .await
            .map_err(|error| error.to_string())?
        {
            if is_agent_transcript_topic(topic.as_str()) {
                resources.push(topic_resource_def(
                    topic.as_str(),
                    topic.as_str(),
                    "Agent transcript event stream",
                ));
            }
        }
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

    async fn resource_template_topic_names(&self) -> Result<Vec<String>, String> {
        let mut names = BTreeSet::from([
            "trigger.inbox".to_string(),
            TRIGGER_OUTBOX_TOPIC.to_string(),
        ]);
        let ctx = load_local_runtime(&self.local_args()).await?;
        for topic in ctx
            .event_log
            .topics()
            .await
            .map_err(|error| error.to_string())?
        {
            if is_agent_transcript_topic(topic.as_str()) {
                names.insert(topic.as_str().to_string());
            }
        }
        Ok(names.into_iter().collect())
    }

    async fn resource_template_event_ids(&self) -> Result<Vec<String>, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        let mut ids = recorded
            .into_iter()
            .filter_map(|(_, event)| {
                serde_json::from_value::<RecordedTriggerEvent>(event.payload)
                    .ok()
                    .map(|record| record.event.id.0)
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn resource_template_dlq_entry_ids(&self) -> Result<Vec<String>, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let mut ids = trigger_inspect_dlq(&mut ctx)
            .await?
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        Ok(ids)
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
        if uri.starts_with("harn://topic/") {
            let detail = self.topic_resource(uri).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        Err(format!("resource not found: {uri}"))
    }

    async fn topic_resource(&self, uri: &str) -> Result<JsonValue, String> {
        let subscription = self.resource_subscription(uri).await?;
        let ctx = load_local_runtime(&self.local_args()).await?;
        let events = ctx
            .event_log
            .read_range(&subscription.topic, None, usize::MAX)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "uri": subscription.uri,
            "topic": subscription.topic.as_str(),
            "events": preview_events(events),
        }))
    }

    async fn resource_subscription(&self, uri: &str) -> Result<ResourceSubscription, String> {
        let topic_name = topic_name_for_resource_uri(uri)
            .ok_or_else(|| format!("resource is not subscribable: {uri}"))?;
        let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
        if is_static_subscribable_topic(topic.as_str()) {
            return Ok(ResourceSubscription {
                uri: uri.to_string(),
                topic,
            });
        }

        if is_agent_transcript_topic(topic.as_str()) {
            let ctx = load_local_runtime(&self.local_args()).await?;
            let exists = ctx
                .event_log
                .topics()
                .await
                .map_err(|error| error.to_string())?
                .iter()
                .any(|existing| existing.as_str() == topic.as_str());
            if exists {
                return Ok(ResourceSubscription {
                    uri: uri.to_string(),
                    topic,
                });
            }
        }

        Err(format!("resource not found: {uri}"))
    }

    async fn ensure_resource_update_watcher(
        &self,
        subscription: ResourceSubscription,
    ) -> Result<(), String> {
        if self
            .resource_watchers
            .lock()
            .expect("resource watchers poisoned")
            .contains_key(&subscription.uri)
        {
            return Ok(());
        }

        let ctx = load_local_runtime(&self.local_args()).await?;
        let start_from = ctx
            .event_log
            .latest(&subscription.topic)
            .await
            .map_err(|error| error.to_string())?;
        let mut stream = ctx
            .event_log
            .clone()
            .subscribe(&subscription.topic, start_from)
            .await
            .map_err(|error| error.to_string())?;
        let event_log = ctx.event_log.clone();
        let topic = subscription.topic.clone();
        let tx = self.resource_notify_tx.clone();
        let uri = subscription.uri.clone();
        let handle = tokio::spawn(async move {
            let mut last_seen = start_from.unwrap_or(0);
            let mut poll = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                tokio::select! {
                    received = stream.next() => {
                        match received {
                            Some(Ok((event_id, _))) if event_id > last_seen => {
                                last_seen = event_id;
                                let _ = tx.send(McpResourceNotification {
                                    uri: uri.clone(),
                                    message: resource_updated_notification(&uri),
                                });
                            }
                            Some(Ok(_)) => {}
                            Some(Err(_)) | None => break,
                        }
                    }
                    _ = poll.tick() => {
                        match event_log.latest(&topic).await {
                            Ok(Some(event_id)) if event_id > last_seen => {
                                last_seen = event_id;
                                let _ = tx.send(McpResourceNotification {
                                    uri: uri.clone(),
                                    message: resource_updated_notification(&uri),
                                });
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        let mut watchers = self
            .resource_watchers
            .lock()
            .expect("resource watchers poisoned");
        if let std::collections::btree_map::Entry::Vacant(entry) = watchers.entry(subscription.uri)
        {
            entry.insert(handle);
        } else {
            handle.abort();
        }
        Ok(())
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
    let mut lines = stdin.lines();
    let mut session = ConnectionState::default();
    let mut list_notifications = service.subscribe_list_notifications();
    let mut resource_notifications = service.subscribe_resource_notifications();
    let mut task_notifications = service.subscribe_task_notifications();
    let mut log_notifications = service.subscribe_log_notifications();

    // Single mpsc fan-in for everything we write to stdout: per-request
    // responses, broadcast notifications, and progress updates emitted
    // by tool handlers via `harn_vm::mcp_progress`. Funnelling all
    // outbound JSON through one writer task means progress lines and
    // their final response can never interleave mid-line, and the
    // ProgressBus can hand its sender to handle_tools_call without a
    // separate channel per request.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<JsonValue>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = out_rx.recv().await {
            if write_stdio_json(&mut stdout, &message).await.is_err() {
                break;
            }
        }
    });

    let progress_bus = harn_vm::mcp_progress::ProgressBus::from_mpsc(out_tx.clone());
    let _bus_guard = harn_vm::mcp_progress::ActiveBusGuard::install(Some(progress_bus));

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
                    let _ = out_tx.send(response);
                }
            }
            notification = list_notifications.recv() => {
                match notification {
                    Ok(notification) => { let _ = out_tx.send(notification); }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = resource_notifications.recv() => {
                match notification {
                    Ok(notification) if session.subscribed_resources.contains(&notification.uri) => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = task_notifications.recv() => {
                match notification {
                    Ok(notification) if notification.owner == session.client_identity => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = log_notifications.recv() => {
                match notification {
                    Ok(notification) if notification.level >= session.log_level => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    // Drop both senders for `out_tx` (the loop's clone and the
    // ProgressBus's clone, held by the install guard) so the writer
    // task observes a closed channel and exits — otherwise it would
    // block on `recv()` forever and the awaited join would hang.
    drop(_bus_guard);
    drop(out_tx);
    let _ = writer.await;
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
    // If the client opened a session-wide SSE (GET /mcp), wire the
    // active progress bus to it so per-request progress notifications
    // stream through the same channel as broadcast notifications.
    // Without an open SSE, progress is silently dropped (the spec
    // permits this — clients that want updates open the stream).
    let progress_sender = session
        .sse_tx
        .lock()
        .expect("HTTP session SSE sender poisoned")
        .clone();
    let (updated, response_json) = match state
        .rpc
        .call_with_progress(current, request, progress_sender)
        .await
    {
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
        spawn_resource_notification_forwarder(state.service.clone(), sender, session.clone());
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
    if let Some(sender) = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned()
    {
        spawn_log_notification_forwarder(state.service.clone(), sender, session.clone());
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
    let resource_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(resource_tx) = resource_tx {
        spawn_resource_notification_forwarder(state.service.clone(), resource_tx, session.clone());
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
    let log_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(log_tx) = log_tx {
        spawn_log_notification_forwarder(state.service.clone(), log_tx, session.clone());
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
                    let progress_bus = request.progress_sender.map(|sender| {
                        harn_vm::mcp_progress::ProgressBus::new(Arc::new(move |message| {
                            let _ = sender.unbounded_send(message);
                        }))
                    });
                    let _bus_guard = harn_vm::mcp_progress::ActiveBusGuard::install(progress_bus);
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
        self.call_with_progress(session, request, None).await
    }

    async fn call_with_progress(
        &self,
        session: ConnectionState,
        request: JsonValue,
        progress_sender: Option<UnboundedSender<JsonValue>>,
    ) -> Result<(ConnectionState, JsonValue), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RpcRequest {
                session,
                request,
                response_tx,
                progress_sender,
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

fn spawn_resource_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_resource_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let subscribed = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .subscribed_resources
                        .contains(&notification.uri);
                    if !subscribed {
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

fn spawn_log_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_log_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let subscribed_level = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .log_level;
                    if notification.level < subscribed_level {
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

/// Open the orchestrator event log and subscribe to each
/// `LOG_STREAM_BINDINGS` topic, fanning new events out as MCP
/// `notifications/message` envelopes on `log_notify_tx`.
///
/// Returns the spawned handles so the service can keep them alive for
/// its lifetime; the watchers terminate when the broadcast sender is
/// dropped.
fn spawn_log_topic_watchers(
    state_dir: &Path,
    log_notify_tx: broadcast::Sender<McpLogNotification>,
    readiness: Arc<LogWatcherReadiness>,
) -> (
    Option<Arc<harn_vm::event_log::AnyEventLog>>,
    Vec<tokio::task::JoinHandle<()>>,
) {
    let event_log = match auth_event_log(state_dir) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("[harn] warning: MCP log stream disabled: {error}");
            return (None, Vec::new());
        }
    };
    let watchers: Vec<_> = LOG_STREAM_BINDINGS
        .iter()
        .filter_map(|binding| {
            spawn_log_topic_watcher(
                event_log.clone(),
                binding,
                log_notify_tx.clone(),
                readiness.clone(),
            )
        })
        .collect();
    readiness
        .expected
        .store(watchers.len(), std::sync::atomic::Ordering::SeqCst);
    readiness.notify.notify_waiters();
    (Some(event_log), watchers)
}

fn spawn_log_topic_watcher(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    binding: &'static McpLogStreamBinding,
    log_notify_tx: broadcast::Sender<McpLogNotification>,
    readiness: Arc<LogWatcherReadiness>,
) -> Option<tokio::task::JoinHandle<()>> {
    let topic = match Topic::new(binding.topic) {
        Ok(topic) => topic,
        Err(error) => {
            eprintln!(
                "[harn] warning: MCP log stream skipped invalid topic {}: {error}",
                binding.topic
            );
            return None;
        }
    };
    Some(tokio::spawn(async move {
        let start_from = match event_log.latest(&topic).await {
            Ok(latest) => latest,
            Err(error) => {
                eprintln!(
                    "[harn] warning: MCP log stream cannot read topic {}: {error}",
                    binding.topic
                );
                return;
            }
        };
        let mut stream = match event_log.clone().subscribe(&topic, start_from).await {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!(
                    "[harn] warning: MCP log stream cannot subscribe to topic {}: {error}",
                    binding.topic
                );
                return;
            }
        };
        readiness.record_ready();
        while let Some(item) = stream.next().await {
            let Ok((event_id, event)) = item else {
                continue;
            };
            let level = severity_for_event(binding, &event);
            let data = json!({
                "event_id": event_id,
                "kind": event.kind,
                "occurred_at_ms": event.occurred_at_ms,
                "headers": event.headers,
                "payload": event.payload,
            });
            let message =
                mcp_protocol::logging_message_notification(level, Some(binding.logger), data);
            if log_notify_tx
                .send(McpLogNotification { level, message })
                .is_err()
            {
                continue;
            }
        }
    }))
}

/// Pick the MCP severity for an event_log entry. Honors an explicit
/// `severity` header when present so producers can opt into a specific
/// level; otherwise heuristics on the event kind elevate failures and
/// errors above the topic's default level.
fn severity_for_event(
    binding: &McpLogStreamBinding,
    event: &LogEvent,
) -> mcp_protocol::McpLogLevel {
    if let Some(level) = event
        .headers
        .get("severity")
        .and_then(|value| mcp_protocol::McpLogLevel::from_str_ci(value))
    {
        return level;
    }
    let kind = event.kind.to_ascii_lowercase();
    if kind.contains("error") || kind.contains("panic") {
        return mcp_protocol::McpLogLevel::Error;
    }
    if kind.contains("fail")
        || kind.contains("denied")
        || kind.contains("blocked")
        || kind.contains("rejected")
        || kind.contains("dropped")
        || kind.contains("dlq")
    {
        return mcp_protocol::McpLogLevel::Warning;
    }
    binding.default_level
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
    annotations: JsonValue,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    task_support: mcp_protocol::McpToolTaskSupport,
) -> JsonValue {
    let mut value = json!({
        "name": name,
        "description": description,
        "annotations": annotations,
        "inputSchema": input_schema,
        "execution": mcp_protocol::tool_execution(task_support),
    });
    if let Some(title) = value["annotations"].get("title").cloned() {
        value["title"] = title;
    }
    if let Some(output_schema) = output_schema {
        value["outputSchema"] = output_schema;
    }
    value
}

fn read_only_tool_annotations(title: &str) -> JsonValue {
    json!({
        "title": title,
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

fn mutating_open_world_tool_annotations(title: &str) -> JsonValue {
    json!({
        "title": title,
        "readOnlyHint": false,
        "destructiveHint": true,
        "idempotentHint": false,
        "openWorldHint": true,
    })
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

/// Emit a `notifications/progress` update from a built-in tool when the
/// caller opted in via `_meta.progressToken`. Silently no-ops otherwise,
/// so call sites can sprinkle milestones without conditional logic.
fn report_milestone(progress: f64, message: &str) {
    if let Some(ctx) = harn_vm::mcp_progress::current_context() {
        ctx.report(progress, Some(1.0), Some(message.to_string()));
    }
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

fn static_topic_resources() -> Vec<JsonValue> {
    vec![
        topic_resource_def(
            "trigger.inbox",
            "Trigger Inbox",
            "Queued trigger inbox events",
        ),
        topic_resource_def(
            TRIGGER_OUTBOX_TOPIC,
            "Trigger Outbox",
            "Dispatched trigger outbox events",
        ),
    ]
}

fn topic_resource_def(topic_name: &str, name: &str, description: &str) -> JsonValue {
    json!({
        "uri": topic_resource_uri(topic_name),
        "name": name,
        "description": description,
        "mimeType": "application/json",
    })
}

fn topic_resource_uri(topic_name: &str) -> String {
    format!("harn://topic/{topic_name}")
}

fn topic_name_for_resource_uri(uri: &str) -> Option<&str> {
    let topic_name = uri.strip_prefix("harn://topic/")?;
    match topic_name {
        "trigger.inbox" => Some(TRIGGER_INBOX_ENVELOPES_TOPIC),
        TRIGGER_OUTBOX_TOPIC => Some(TRIGGER_OUTBOX_TOPIC),
        value if is_agent_transcript_topic(value) => Some(value),
        _ => None,
    }
}

fn resource_uris_for_topic(topic_name: &str) -> Vec<String> {
    match topic_name {
        TRIGGER_INBOX_ENVELOPES_TOPIC => vec![topic_resource_uri("trigger.inbox")],
        TRIGGER_OUTBOX_TOPIC => vec![topic_resource_uri(TRIGGER_OUTBOX_TOPIC)],
        value if is_agent_transcript_topic(value) => vec![topic_resource_uri(value)],
        _ => Vec::new(),
    }
}

fn is_static_subscribable_topic(topic_name: &str) -> bool {
    matches!(
        topic_name,
        TRIGGER_INBOX_ENVELOPES_TOPIC | TRIGGER_OUTBOX_TOPIC
    )
}

fn is_agent_transcript_topic(topic_name: &str) -> bool {
    topic_name.starts_with("agent.transcript.")
}

fn resource_updated_notification(uri: &str) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/resources/updated",
        "params": { "uri": uri },
    })
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
#[path = "serve_tests.rs"]
mod serve_tests;
