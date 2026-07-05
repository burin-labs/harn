use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, OriginalUri, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{stream, Stream, StreamExt};
use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::adapters::acp::{run_acp_channel_server, AcpServerConfig};
use crate::auth::{AuthPolicy, AuthRequest, AuthorizationDecision};
use crate::permissions::{
    ActionClass, AuditFilter, DecisionScope, InMemoryPermissionStore, PermissionDecision,
    PermissionPolicy, PermissionRequest, PermissionStore, RememberRule, RememberSpec, RuleId,
};
use crate::tls::HttpTlsConfig;

mod artifacts;
mod events;
mod meta;
mod permissions;
mod sessions;
mod tasks;
mod workspaces;

const OPENAPI_YAML: &str = include_str!("../../../openapi.yaml");
const API_PROTOCOL_VERSION: &str = "agents-protocol-2026-04-25";
const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";

#[derive(Clone, Debug)]
pub struct ApiHttpServeOptions {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
    pub tls: HttpTlsConfig,
}

#[derive(Clone)]
pub struct ApiServerConfig {
    pub acp: AcpServerConfig,
    pub auth_policy: AuthPolicy,
    pub workspace_root: PathBuf,
}

impl ApiServerConfig {
    pub fn for_pipeline(path: impl Into<String>) -> Self {
        let path = path.into();
        let root = api_workspace_root_for_pipeline(&path);
        Self {
            acp: AcpServerConfig::for_pipeline(path),
            auth_policy: AuthPolicy::allow_all(),
            workspace_root: root,
        }
    }

    pub fn with_auth_policy(mut self, auth_policy: AuthPolicy) -> Self {
        self.auth_policy = auth_policy;
        self
    }

    pub fn with_profile(mut self, profile: crate::adapters::acp::AcpProfileConfig) -> Self {
        self.acp.profile = profile;
        self
    }
}

fn api_workspace_root_for_pipeline(path: &str) -> PathBuf {
    if let Ok(root) = std::env::var("HARN_PROJECT_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[derive(Clone)]
pub struct ApiServer {
    state: ApiState,
}

impl ApiServer {
    pub fn new(mut config: ApiServerConfig) -> Self {
        config.acp.auth_policy = AuthPolicy::allow_all();
        let provider_catalog = ProviderCatalogRuntime {
            llm_config_overrides: config.acp.llm_config_overrides.clone(),
            llm_capability_overrides: config.acp.llm_capability_overrides.clone(),
        };
        let (client, response_rx) = AcpClient::start(config.acp);
        let (events_tx, _) = broadcast::channel(1024);
        let (event_log, event_log_error) =
            match harn_vm::event_log::install_default_for_base_dir(&config.workspace_root) {
                Ok(log) => (Some(log), None),
                Err(error) => (None, Some(error.to_string())),
            };
        let state = ApiState {
            acp: client.clone(),
            inner: Arc::new(Mutex::new(ApiStateInner::new(config.workspace_root))),
            events_tx,
            auth_policy: config.auth_policy,
            permissions: Arc::new(InMemoryPermissionStore::default()),
            provider_catalog,
            event_log,
            event_log_error,
        };
        client.spawn_output_loop(response_rx, state.clone());
        Self { state }
    }

    pub async fn run_http(self: Arc<Self>, options: ApiHttpServeOptions) -> Result<(), String> {
        let router = api_router(self.state.clone());
        let router =
            crate::apply_transport_layers(router, &crate::TransportConfig::default_enabled());
        let router = crate::tls::apply_security_headers(router, &options.tls);
        let listener = crate::tls::bind_listener(options.bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        let advertised = options
            .public_url
            .clone()
            .unwrap_or_else(|| format!("{}://{local_addr}", options.tls.advertised_scheme()));
        eprintln!("[harn] Agents API server ready on {advertised}");
        eprintln!("[harn] OpenAPI document: {advertised}/openapi.json");
        crate::tls::serve_router_from_tcp(listener, router, &options.tls)
            .await
            .map_err(|error| format!("Agents API server failed: {error}"))
    }
}

#[derive(Clone)]
struct ApiState {
    acp: AcpClient,
    inner: Arc<Mutex<ApiStateInner>>,
    events_tx: broadcast::Sender<ApiEvent>,
    auth_policy: AuthPolicy,
    /// The permission primitive every adapter delegates to. Lives at
    /// the state level so the REST routes, the ACP-suspend-respond
    /// path, and (eventually) the `harness.permissions.*` host calls
    /// all read and write the same store.
    permissions: Arc<InMemoryPermissionStore>,
    provider_catalog: ProviderCatalogRuntime,
    event_log: Option<Arc<AnyEventLog>>,
    event_log_error: Option<String>,
}

#[derive(Clone, Default)]
struct ProviderCatalogRuntime {
    llm_config_overrides: Option<harn_vm::llm_config::ProvidersConfig>,
    llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
}

impl ProviderCatalogRuntime {
    fn artifact(&self) -> harn_vm::provider_catalog::ProviderCatalogArtifact {
        harn_vm::provider_catalog::artifact_with_overrides(
            self.llm_config_overrides.as_ref(),
            self.llm_capability_overrides.as_ref(),
        )
    }
}

struct ApiStateInner {
    sessions: BTreeMap<String, Value>,
    messages: HashMap<String, Vec<Value>>,
    artifacts: BTreeMap<String, Value>,
    tasks: BTreeMap<String, Value>,
    permissions: BTreeMap<String, PendingPermission>,
    workspaces: BTreeMap<String, Value>,
    events: Vec<ApiEvent>,
    active_task_by_session: HashMap<String, String>,
    event_seq: u64,
    root_workspace_id: String,
    root_workspace_path: PathBuf,
}

impl ApiStateInner {
    fn new(workspace_root: PathBuf) -> Self {
        let now = now_rfc3339();
        let root_workspace_id = "local".to_string();
        let mut workspaces = BTreeMap::new();
        workspaces.insert(
            root_workspace_id.clone(),
            json!({
                "id": root_workspace_id,
                "object": "workspace",
                "created_at": now,
                "updated_at": now,
                "metadata": {},
                "name": "Local workspace",
                "root": workspace_root.to_string_lossy(),
                "default_branch_id": null,
                "host": "local",
                "repository": null,
                "tenant_id": null,
                "capabilities": [
                    "sessions",
                    "tasks",
                    "events",
                    "artifacts",
                    "permissions",
                    "workflow_trigger_runs",
                    "workspace.files.read"
                ],
                "connectors": [],
                "quota_id": null
            }),
        );
        Self {
            sessions: BTreeMap::new(),
            messages: HashMap::new(),
            artifacts: BTreeMap::new(),
            tasks: BTreeMap::new(),
            permissions: BTreeMap::new(),
            workspaces,
            events: Vec::new(),
            active_task_by_session: HashMap::new(),
            event_seq: 0,
            root_workspace_id,
            root_workspace_path: workspace_root,
        }
    }
}

#[derive(Clone)]
struct AcpClient {
    request_tx: mpsc::UnboundedSender<Value>,
    pending: Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
}

impl AcpClient {
    fn start(config: AcpServerConfig) -> (Self, mpsc::UnboundedReceiver<String>) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (response_tx, response_rx) = mpsc::unbounded_channel();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("start ACP runtime");
            runtime.block_on(run_acp_channel_server(config, request_rx, response_tx));
        });
        (
            Self {
                request_tx,
                pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                next_id: Arc::new(AtomicU64::new(1)),
            },
            response_rx,
        )
    }

    fn spawn_output_loop(&self, mut response_rx: mpsc::UnboundedReceiver<String>, state: ApiState) {
        let client = self.clone();
        tokio::spawn(async move {
            while let Some(line) = response_rx.recv().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("method").is_none() && message.get("id").is_some() {
                    client.resolve_response(message).await;
                    continue;
                }
                if message.get("method").is_some() && message.get("id").is_some() {
                    state.handle_acp_request(message).await;
                    continue;
                }
                state.handle_acp_notification(message);
            }
        });
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if self.request_tx.send(request).is_err() {
            self.pending.lock().await.remove(&id);
            return Err("ACP runtime is not running".to_string());
        }
        let response = rx
            .await
            .map_err(|_| "ACP runtime stopped before responding".to_string())?;
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| error.to_string()));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn resolve_response(&self, response: Value) {
        let Some(id) = response.get("id").and_then(Value::as_u64) else {
            return;
        };
        let sender = self.pending.lock().await.remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }

    fn send_raw(&self, message: Value) {
        let _ = self.request_tx.send(message);
    }
}

#[derive(Clone, Serialize)]
struct ApiEvent {
    id: String,
    object: &'static str,
    created_at: String,
    event: String,
    session_id: Option<String>,
    task_id: Option<String>,
    payload: Value,
}

#[derive(Clone)]
struct PendingPermission {
    public: Value,
    rpc_id: Option<u64>,
    hitl: bool,
}

impl ApiState {
    async fn handle_acp_request(&self, message: Value) {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str) {
            Some("host/capabilities") => {
                self.acp.send_raw(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                }));
            }
            Some("session/request_permission") => {
                self.register_permission_request(message);
            }
            Some(method) => {
                self.acp.send_raw(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Unsupported local API host request: {method}")
                    }
                }));
            }
            None => {}
        }
    }

    fn handle_acp_notification(&self, message: Value) {
        let Some(method) = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return;
        };
        match method.as_str() {
            "session/update" => self.register_session_update(message),
            _ => {
                self.append_event(None, None, &method, message);
            }
        }
    }

    fn register_permission_request(&self, message: Value) {
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let request_id = params
            .pointer("/toolCall/toolCallId")
            .or_else(|| params.pointer("/toolCall/_meta/harn/approvalRequest/id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("permission_{}", Uuid::now_v7()));
        let task_id = session_id
            .as_ref()
            .and_then(|session_id| self.active_task_id(session_id));
        let now = now_rfc3339();
        let public = json!({
            "id": request_id,
            "object": "permission_request",
            "created_at": now,
            "updated_at": now,
            "metadata": {},
            "session_id": session_id,
            "task_id": task_id,
            "status": "pending",
            "source": "acp",
            "action": params.pointer("/toolCall/_meta/harn/toolName")
                .or_else(|| params.pointer("/toolCall/title"))
                .cloned()
                .unwrap_or(Value::Null),
            "request": params,
            "response": null
        });
        let rpc_id = message.get("id").and_then(Value::as_u64);
        let pending = PendingPermission {
            public: public.clone(),
            rpc_id,
            hitl: false,
        };
        {
            let mut inner = self.inner.lock().expect("api state poisoned");
            inner.permissions.insert(
                public["id"].as_str().expect("permission id").to_string(),
                pending,
            );
            if let Some(task_id) = public.get("task_id").and_then(Value::as_str) {
                set_task_status(&mut inner.tasks, task_id, "INPUT_REQUIRED");
            }
        }
        let event_session_id = public["session_id"].as_str().map(str::to_string);
        let event_task_id = public["task_id"].as_str().map(str::to_string);
        self.append_event_from_resource(
            event_session_id,
            event_task_id,
            "permission.requested",
            public,
        );
    }

    fn register_session_update(&self, message: Value) {
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let task_id = session_id
            .as_ref()
            .and_then(|session_id| self.active_task_id(session_id));
        if params
            .pointer("/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("hitl_request")
        {
            self.register_hitl_request(session_id.clone(), task_id.clone(), &params);
        }
        if params
            .pointer("/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("artifact")
        {
            artifacts::register_harn_session_artifact(
                self,
                session_id.clone(),
                task_id.clone(),
                &params,
            );
        }
        self.append_event(session_id, task_id, "session.update", params);
    }

    fn register_hitl_request(
        &self,
        session_id: Option<String>,
        task_id: Option<String>,
        params: &Value,
    ) {
        let update = params.get("update").cloned().unwrap_or_else(|| json!({}));
        let harn = update
            .pointer("/_meta/harn")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let request_id = harn
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("hitl_{}", Uuid::now_v7()));
        let now = now_rfc3339();
        let public = json!({
            "id": request_id,
            "object": "permission_request",
            "created_at": now,
            "updated_at": now,
            "metadata": {},
            "session_id": session_id,
            "task_id": task_id,
            "status": "pending",
            "source": "hitl",
            "action": harn.get("kind").cloned().unwrap_or(Value::Null),
            "request": harn.get("payload").cloned().unwrap_or(Value::Null),
            "response": null
        });
        let pending = PendingPermission {
            public: public.clone(),
            rpc_id: None,
            hitl: true,
        };
        {
            let mut inner = self.inner.lock().expect("api state poisoned");
            inner.permissions.insert(request_id, pending);
            if let Some(task_id) = public.get("task_id").and_then(Value::as_str) {
                set_task_status(&mut inner.tasks, task_id, "INPUT_REQUIRED");
            }
        }
        self.append_event_from_resource(
            public["session_id"].as_str().map(str::to_string),
            public["task_id"].as_str().map(str::to_string),
            "permission.requested",
            public,
        );
    }

    fn active_task_id(&self, session_id: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("api state poisoned")
            .active_task_by_session
            .get(session_id)
            .cloned()
    }

    fn append_event(
        &self,
        session_id: Option<String>,
        task_id: Option<String>,
        event: &str,
        payload: Value,
    ) -> ApiEvent {
        self.append_event_from_resource(session_id, task_id, event, payload)
    }

    fn append_event_from_resource(
        &self,
        session_id: Option<String>,
        task_id: Option<String>,
        event: &str,
        payload: Value,
    ) -> ApiEvent {
        let mut inner = self.inner.lock().expect("api state poisoned");
        inner.event_seq += 1;
        let id = format!("event_{:016x}", inner.event_seq);
        let api_event = ApiEvent {
            id: id.clone(),
            object: "event",
            created_at: now_rfc3339(),
            event: event.to_string(),
            session_id,
            task_id,
            payload,
        };
        if let Some(session_id) = api_event.session_id.as_deref() {
            if let Some(session) = inner.sessions.get_mut(session_id) {
                session["last_event_id"] = json!(id);
                session["updated_at"] = json!(api_event.created_at.clone());
            }
        }
        inner.events.push(api_event.clone());
        drop(inner);
        let _ = self.events_tx.send(api_event.clone());
        api_event
    }

    fn history(&self, filter: &EventFilter) -> Vec<ApiEvent> {
        let inner = self.inner.lock().expect("api state poisoned");
        inner
            .events
            .iter()
            .filter(|event| filter.matches(event))
            .cloned()
            .collect()
    }
}

#[derive(Clone, Default, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    after: Option<String>,
    workspace_id: Option<String>,
    session_id: Option<String>,
    task_id: Option<String>,
    path: Option<String>,
}

#[derive(Clone, Default)]
struct EventFilter {
    session_id: Option<String>,
    task_id: Option<String>,
    after: Option<String>,
}

impl EventFilter {
    fn from_query(query: &ListQuery, session_id: Option<String>, task_id: Option<String>) -> Self {
        Self {
            session_id: session_id.or_else(|| query.session_id.clone()),
            task_id: task_id.or_else(|| query.task_id.clone()),
            after: query.after.clone().or_else(|| query.cursor.clone()),
        }
    }

    fn after_seq(&self) -> u64 {
        self.after.as_deref().map(event_seq).unwrap_or(0)
    }

    fn matches(&self, event: &ApiEvent) -> bool {
        if event_seq(&event.id) <= self.after_seq() {
            return false;
        }
        if let Some(session_id) = self.session_id.as_deref() {
            if event.session_id.as_deref() != Some(session_id) {
                return false;
            }
        }
        if let Some(task_id) = self.task_id.as_deref() {
            if event.task_id.as_deref() != Some(task_id) {
                return false;
            }
        }
        true
    }
}

fn api_router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(meta::health))
        .route("/version", get(meta::version))
        .route("/openapi.json", get(meta::openapi_json))
        .route("/v1", get(meta::api_root))
        .route("/v1/runtime", get(meta::runtime))
        .route("/v1/capabilities", get(meta::capabilities))
        .route("/v1/provider-catalog", get(meta::provider_catalog))
        .route("/v1/tools", get(meta::list_tools))
        .route("/v1/tools/{tool_id}", get(meta::get_tool))
        .route(
            "/v1/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
        .route(
            "/v1/workspaces/{workspace_id}",
            get(workspaces::get_workspace).patch(workspaces::update_workspace),
        )
        .route(
            "/v1/workspaces/{workspace_id}/files",
            get(workspaces::read_workspace_file).put(workspaces::write_workspace_file),
        )
        .route(
            "/v1/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/v1/sessions/{session_id}",
            get(sessions::get_session).patch(sessions::update_session),
        )
        .route(
            "/v1/sessions/{session_id}/view",
            get(sessions::get_session_view),
        )
        .route(
            "/v1/sessions/{session_id}/close",
            post(sessions::close_session),
        )
        .route(
            "/v1/sessions/{session_id}/fork",
            post(sessions::fork_session),
        )
        .route(
            "/v1/sessions/{session_id}/truncate",
            post(sessions::truncate_session),
        )
        .route(
            "/v1/sessions/{session_id}/messages",
            get(sessions::list_session_messages).post(sessions::append_session_message),
        )
        .route(
            "/v1/sessions/{session_id}/tasks",
            get(sessions::list_session_tasks).post(sessions::submit_session_task),
        )
        .route("/v1/tasks", get(tasks::list_tasks).post(tasks::submit_task))
        .route("/v1/tasks/{task_id}", get(tasks::get_task))
        .route("/v1/tasks/{task_id}/cancel", post(tasks::cancel_task))
        .route(
            "/v1/artifacts",
            get(artifacts::list_artifacts).post(artifacts::register_artifact),
        )
        .route("/v1/artifacts/{artifact_id}", get(artifacts::get_artifact))
        .route(
            "/v1/artifacts/{artifact_id}/content",
            get(artifacts::download_artifact_content),
        )
        .route("/v1/events", get(events::list_events))
        .route(
            "/v1/workflow-trigger-runs",
            get(events::list_workflow_trigger_runs),
        )
        .route("/v1/events/stream", get(events::stream_events))
        .route(
            "/v1/sessions/{session_id}/events",
            get(events::list_session_events),
        )
        .route(
            "/v1/sessions/{session_id}/events/stream",
            get(events::stream_session_events),
        )
        .route("/v1/tasks/{task_id}/events", get(events::list_task_events))
        .route(
            "/v1/tasks/{task_id}/events/stream",
            get(events::stream_task_events),
        )
        .route(
            "/v1/tasks/{task_id}/stream",
            get(events::stream_task_events),
        )
        .route(
            "/v1/permission-requests",
            get(permissions::list_permission_requests),
        )
        .route(
            "/v1/tasks/{task_id}/permission-requests",
            get(permissions::list_task_permission_requests),
        )
        .route(
            "/v1/permission-requests/{request_id}/respond",
            post(permissions::respond_permission_request),
        )
        .route(
            "/v1/permissions/policy",
            get(permissions::get_permissions_policy).put(permissions::put_permissions_policy),
        )
        .route(
            "/v1/permissions/rules",
            get(permissions::list_permission_rules).post(permissions::create_permission_rule),
        )
        .route(
            "/v1/permissions/rules/{rule_id}",
            axum::routing::delete(permissions::revoke_permission_rule),
        )
        .route(
            "/v1/permissions/history",
            get(permissions::get_permission_history),
        )
        .route("/v1/permissions/check", post(permissions::check_permission))
        .layer(DefaultBodyLimit::max(crate::DEFAULT_HTTP_BODY_LIMIT_BYTES))
        .with_state(state)
}

async fn authorize(
    state: &ApiState,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<(), Response> {
    let request = AuthRequest::from_http(&method, uri.path(), body.to_vec(), headers);
    match state.auth_policy.authorize(&request).await {
        AuthorizationDecision::Authorized(_) => Ok(()),
        AuthorizationDecision::Rejected(message) => Err(api_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            &message,
        )),
        // Today this REST adapter calls `authorize` with no per-route
        // scopes, so reaching `MissingScope` requires an explicit
        // future caller. Wire it defensively so the surface returns the
        // structured 403 rather than dropping into a non-exhaustive
        // match panic.
        AuthorizationDecision::MissingScope { required, granted } => {
            Err(forbidden_api_error(&required, &granted))
        }
        // `authorize_mcp` is the only call site that produces this
        // variant. The REST API edge never invokes it, so any leak here
        // is a policy-wiring bug — surface a 403 with the policy's own
        // reason string so the upstream operator gets actionable text.
        AuthorizationDecision::McpNotAllowlisted { reason, .. } => Err(api_error(
            StatusCode::FORBIDDEN,
            "mcp_not_allowlisted",
            &reason,
        )),
    }
}

/// Render a scope mismatch as the REST API's standard `forbidden` error
/// payload, mirroring how a cloud gateway reports the same case.
/// Uses the same `kind`/`required_scopes`/`granted_scopes`/`missing_scopes`
/// fields as the JSON-RPC adapters; the only difference is the REST
/// adapter inlines them under a single `error` envelope object instead
/// of nesting under `error.data`.
fn forbidden_api_error(required: &BTreeSet<String>, granted: &BTreeSet<String>) -> Response {
    let mut body = crate::forbidden_data_payload(required, granted);
    if let Some(map) = body.as_object_mut() {
        map.insert(
            "message".to_string(),
            json!(crate::forbidden_message(required, granted)),
        );
    }
    (StatusCode::FORBIDDEN, Json(json!({ "error": body }))).into_response()
}

/// 400 response for a request body that failed to parse as JSON.
fn invalid_json_response() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_json",
        "request body must be JSON",
    )
}

fn parse_json_body(body: &Bytes) -> Result<Value, serde_json::Error> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body)
}

fn list_response<T: Serialize>(data: Vec<T>) -> Value {
    json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "next_cursor": null
    })
}

fn limit_values(mut values: Vec<Value>, limit: Option<usize>) -> Vec<Value> {
    if let Some(limit) = limit {
        values.truncate(limit);
    }
    values
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message": message
            }
        })),
    )
        .into_response()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn event_seq(id: &str) -> u64 {
    id.strip_prefix("event_")
        .and_then(|value| u64::from_str_radix(value, 16).ok())
        .unwrap_or(0)
}

fn merge_mutable_fields(target: &mut Value, input: &Value, fields: &[&str]) {
    for field in fields {
        if let Some(value) = input.get(*field) {
            target[*field] = value.clone();
        }
    }
}

fn set_task_status(tasks: &mut BTreeMap<String, Value>, task_id: &str, status: &str) {
    if let Some(task) = tasks.get_mut(task_id) {
        task["status"] = json!(status);
        task["updated_at"] = json!(now_rfc3339());
    }
}

fn workspace_root_locked(inner: &ApiStateInner, workspace_id: &str) -> Option<PathBuf> {
    if workspace_id == inner.root_workspace_id {
        return Some(inner.root_workspace_path.clone());
    }
    inner
        .workspaces
        .get(workspace_id)
        .and_then(|workspace| workspace.get("root").and_then(Value::as_str))
        .map(PathBuf::from)
}

fn message_resource(session_id: &str, task_id: Option<&str>, input: Value) -> Value {
    let now = now_rfc3339();
    let message = normalize_message_input(input);
    json!({
        "id": format!("message_{}", Uuid::now_v7()),
        "object": "message",
        "created_at": now,
        "updated_at": now,
        "metadata": message.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "session_id": session_id,
        "task_id": task_id,
        "role": message.get("role").and_then(Value::as_str).unwrap_or("user"),
        "parts": message.get("parts").cloned().unwrap_or_else(|| json!([]))
    })
}

fn normalize_message_input(input: Value) -> Value {
    if input.get("role").is_some() && input.get("parts").is_some() {
        return input;
    }
    if let Some(text) = input
        .as_str()
        .or_else(|| input.get("text").and_then(Value::as_str))
    {
        return json!({
            "role": input.get("role").and_then(Value::as_str).unwrap_or("user"),
            "parts": [{
                "type": "text",
                "text": text,
                "visibility": "public"
            }],
            "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({}))
        });
    }
    let role = input
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_string();
    json!({
        "role": role,
        "parts": [{
            "type": "json",
            "value": input,
            "visibility": "public"
        }],
        "metadata": {}
    })
}

fn prompt_text(input: &Value) -> Option<String> {
    if let Some(text) = input
        .as_str()
        .or_else(|| input.get("text").and_then(Value::as_str))
    {
        return Some(text.to_string());
    }
    if let Some(message) = input.get("message") {
        return prompt_text(message);
    }
    if let Some(value) = input.get("input") {
        return prompt_text(value);
    }
    let parts = input.get("parts")?.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn openapi_json_is_served_from_canonical_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let response = api_router(server.state)
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["openapi"], "3.1.0");
        assert!(value["paths"]["/v1/sessions"].is_object());
    }

    #[tokio::test]
    async fn local_api_creates_session_and_accepts_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let app = api_router(server.state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"workspace_id":"local"}"#))
                    .expect("request"),
            )
            .await
            .expect("session response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let session: Value = serde_json::from_slice(&body).expect("session");
        let session_id = session["id"].as_str().expect("session id");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{session_id}/tasks"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"input":{"role":"user","parts":[{"type":"text","text":"hello","visibility":"public"}]}}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("task response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let task: Value = serde_json::from_slice(&body).expect("task");
        assert_eq!(task["object"], "task");
        assert_eq!(task["status"], "WORKING");
        assert_eq!(task["session_id"], session_id);
    }

    #[tokio::test]
    async fn local_api_registers_and_downloads_file_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let report = dir.path().join("report.pdf");
        std::fs::write(&report, b"%PDF-1.7\n").expect("write report");
        let report_uri = url::Url::from_file_path(&report)
            .expect("file url")
            .to_string();
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let app = api_router(server.state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/artifacts")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "kind": "file",
                            "mime_type": "application/pdf",
                            "uri": report_uri,
                            "visibility": "public",
                            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                            "name": "report.pdf",
                            "size_bytes": 9
                        }))
                        .expect("artifact json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("artifact response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let artifact: Value = serde_json::from_slice(&body).expect("artifact");
        let artifact_id = artifact["id"].as_str().expect("artifact id");
        assert_eq!(artifact["object"], "artifact");
        assert_eq!(artifact["kind"], "file");
        assert_eq!(artifact["mime_type"], "application/pdf");
        assert_eq!(artifact["name"], "report.pdf");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/artifacts")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("list response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let list: Value = serde_json::from_slice(&body).expect("list");
        assert_eq!(list["data"].as_array().expect("data").len(), 1);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/artifacts/{artifact_id}/content"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("content response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/pdf")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"%PDF-1.7\n");
    }

    #[tokio::test]
    async fn local_api_indexes_harn_artifact_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let state = server.state.clone();

        state.register_session_update(json!({
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "artifact",
                    "_meta": {
                        "harn": {
                            "artifactId": "artifact-file",
                            "kind": "file",
                            "title": "Report PDF",
                            "mimeType": "application/pdf",
                            "spec": {
                                "uri": "file:///tmp/report.pdf",
                                "name": "report.pdf",
                                "mime_type": "application/pdf",
                                "size_bytes": 1234,
                                "sha256": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                            },
                            "fallback": "File artifact: report.pdf"
                        }
                    }
                }
            }
        }));

        let inner = state.inner.lock().expect("api state poisoned");
        let artifact = inner.artifacts.get("artifact-file").expect("artifact");
        assert_eq!(artifact["kind"], "file");
        assert_eq!(artifact["mime_type"], "application/pdf");
        assert_eq!(artifact["name"], "report.pdf");
        assert_eq!(
            artifact["sha256"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(artifact["metadata"]["harn_kind"], "file");
        assert!(inner
            .events
            .iter()
            .any(|event| event.event == "artifact.created"));
    }

    #[tokio::test]
    async fn local_api_returns_session_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let app = api_router(server.state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"workspace_id":"local"}"#))
                    .expect("request"),
            )
            .await
            .expect("session response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let session: Value = serde_json::from_slice(&body).expect("session");
        let session_id = session["id"].as_str().expect("session id");

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/sessions/{session_id}/view"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("view response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let view: Value = serde_json::from_slice(&body).expect("view");
        assert_eq!(view["schema"], "harn.session_view.v1");
        assert_eq!(view["session"]["session_id"], session_id);
        assert_eq!(view["session"]["last_event_id"], 1);
        assert_eq!(view["metadata"]["event_count"], 1);
    }

    #[tokio::test]
    async fn local_api_truncates_session_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let app = api_router(server.state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"workspace_id":"local"}"#))
                    .expect("request"),
            )
            .await
            .expect("session response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let session: Value = serde_json::from_slice(&body).expect("session");
        let session_id = session["id"].as_str().expect("session id");

        for text in ["alpha", "beta"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/sessions/{session_id}/messages"))
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"role":"user","parts":[{{"type":"text","text":"{text}","visibility":"public"}}]}}"#
                        )))
                        .expect("request"),
                )
                .await
                .expect("message response");
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{session_id}/truncate"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"keep_first":1,"reason":"user_edit"}"#))
                    .expect("request"),
            )
            .await
            .expect("truncate response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let result: Value = serde_json::from_slice(&body).expect("truncate json");
        assert_eq!(result["object"], "session.truncate_result");
        assert_eq!(result["session_id"], session_id);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/sessions/{session_id}/messages"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("messages response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let messages: Value = serde_json::from_slice(&body).expect("messages json");
        assert_eq!(messages["data"].as_array().expect("messages").len(), 1);
        assert_eq!(messages["data"][0]["parts"][0]["text"], "alpha");
    }

    #[tokio::test]
    async fn authenticated_api_rejects_missing_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let config = ApiServerConfig::for_pipeline(script.to_string_lossy().to_string())
            .with_auth_policy(AuthPolicy {
                methods: vec![crate::auth::AuthMethodConfig::ApiKey(
                    crate::auth::ApiKeyAuthConfig::single("secret"),
                )],
                mcp_allowlist: None,
            });
        let server = ApiServer::new(config);
        let response = api_router(server.state)
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn workflow_trigger_runs_endpoint_projects_dispatch_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        let event_log = server.state.event_log.as_ref().expect("event log").clone();
        let outbox_topic = Topic::new(harn_vm::TRIGGER_OUTBOX_TOPIC).expect("outbox topic");
        let action_graph_topic = Topic::new(ACTION_GRAPH_TOPIC).expect("action graph topic");

        let mut dispatch_headers = BTreeMap::new();
        dispatch_headers.insert("trigger_id".to_string(), "github.comment".to_string());
        dispatch_headers.insert("event_id".to_string(), "evt-123".to_string());
        dispatch_headers.insert("binding_key".to_string(), "github-comment".to_string());
        dispatch_headers.insert("attempt".to_string(), "2".to_string());

        event_log
            .append(
                &outbox_topic,
                LogEvent {
                    kind: "dispatch_succeeded".to_string(),
                    payload: json!({
                        "handler_kind": "workflow",
                        "target_uri": "harn://workflows/comment_triage",
                        "result": {"session_id": "session-123"}
                    }),
                    headers: dispatch_headers,
                    occurred_at_ms: 2_000,
                },
            )
            .await
            .expect("append dispatch");
        event_log
            .append(
                &outbox_topic,
                LogEvent {
                    kind: "diagnostic".to_string(),
                    payload: json!({}),
                    headers: BTreeMap::new(),
                    occurred_at_ms: 2_001,
                },
            )
            .await
            .expect("append ignored event");

        let mut graph_headers = BTreeMap::new();
        graph_headers.insert("event_id".to_string(), "evt-123".to_string());
        event_log
            .append(
                &action_graph_topic,
                LogEvent {
                    kind: "action_graph_observed".to_string(),
                    payload: json!({
                        "observability": {
                            "action_graph_nodes": [{"id": "trigger", "label": "GitHub comment"}],
                            "action_graph_edges": [{"from": "trigger", "to": "workflow"}]
                        }
                    }),
                    headers: graph_headers,
                    occurred_at_ms: 2_002,
                },
            )
            .await
            .expect("append graph");

        let response = api_router(server.state)
            .oneshot(
                Request::builder()
                    .uri("/v1/workflow-trigger-runs?limit=1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let data = body["data"].as_array().expect("data");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["object"], "workflow_trigger_run");
        assert_eq!(data[0]["status"], "succeeded");
        assert_eq!(data[0]["trigger_id"], "github.comment");
        assert_eq!(data[0]["event_id"], "evt-123");
        assert_eq!(data[0]["binding_key"], "github-comment");
        assert_eq!(data[0]["attempt"], 2);
        assert_eq!(data[0]["handler_kind"], "workflow");
        assert_eq!(data[0]["target_uri"], "harn://workflows/comment_triage");
        assert_eq!(data[0]["result"]["session_id"], "session-123");
        assert_eq!(data[0]["action_graph"]["nodes"][0]["id"], "trigger");
    }

    async fn build_test_router() -> axum::Router {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let server = ApiServer::new(ApiServerConfig::for_pipeline(
            script.to_string_lossy().to_string(),
        ));
        // Leak the tempdir so the workspace_root stays alive for the
        // lifetime of the test; the router holds a path-only reference
        // and we don't need to clean up the on-disk artifact.
        std::mem::forget(dir);
        api_router(server.state)
    }

    async fn read_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn provider_catalog_endpoint_matches_export_artifact_with_overrides() {
        let _reset = crate::test_support::LlmOverrideReset;
        let overlay = crate::test_support::fixture_provider_overlay();
        let capability_overlay = crate::test_support::fixture_capability_overlay();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n")
            .expect("write script");
        let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().to_string());
        config.acp = config
            .acp
            .with_llm_overrides(Some(overlay.clone()), Some(capability_overlay.clone()));
        let server = ApiServer::new(config);

        let response = api_router(server.state)
            .oneshot(
                Request::builder()
                    .uri("/v1/provider-catalog")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let expected = serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
            Some(&overlay),
            Some(&capability_overlay),
        ))
        .expect("expected catalog json");
        assert_eq!(body, expected);

        let providers = body["providers"].as_array().expect("providers");
        let provider = providers
            .iter()
            .find(|provider| provider["id"] == "fixture_runtime")
            .expect("fixture provider");
        assert_eq!(provider["classification"], "hosted");
        assert_eq!(
            provider["auth"],
            json!({
                "style": "bearer",
                "env": ["FIXTURE_RUNTIME_API_KEY"],
                "required": true
            })
        );

        let models = body["models"].as_array().expect("models");
        let model = models
            .iter()
            .find(|model| model["id"] == "fixture-model-v1")
            .expect("fixture model");
        assert_eq!(model["context_window"], 12345);
        assert_eq!(model["pricing"]["input_per_mtok"], 1.25);
        assert_eq!(model["aliases"], json!(["fixture-default"]));
        assert_eq!(model["tool_support"]["native"], true);
        assert_eq!(model["tool_support"]["tool_search"], json!(["hosted"]));
        assert_eq!(
            model["capability_tags"],
            json!([
                "streaming",
                "tools",
                "tool_search",
                "vision",
                "prompt_caching",
                "thinking",
                "extended_thinking",
                "structured_output"
            ])
        );
    }

    #[tokio::test]
    async fn permissions_policy_installs_and_lints() {
        let app = build_test_router().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/permissions/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"read":["src/**"],"escalate_to":["user"]}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["object"], "permission_policy");
        let version = body["version"].as_str().expect("version");
        assert!(version.starts_with("policy-"));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/permissions/policy")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["policy"]["read"][0], "src/**");

        // Linter rejects empty patterns.
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/permissions/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"read":[""]}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn permission_rules_round_trip_and_check_uses_them() {
        let app = build_test_router().await;
        let rule = RememberRule::new(
            DecisionScope::Session,
            Some("s1".to_string()),
            ActionClass::Read,
            "fs.*",
            "src/**",
            true,
            "alice",
        )
        .expect("rule compiles");
        let rule_body = serde_json::to_string(&rule).expect("rule json");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/rules")
                    .header("content-type", "application/json")
                    .body(Body::from(rule_body))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let body = read_json(response).await;
        assert_eq!(status, StatusCode::OK, "create rule failed: {body}");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/permissions/rules")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["data"].as_array().expect("rules").len(), 1);

        let check_request = PermissionRequest::new(
            "p1",
            "s1",
            "alice",
            ActionClass::Read,
            "fs.read",
            "src/lib.rs",
        );
        let check_body = serde_json::to_string(&check_request).expect("request json");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("content-type", "application/json")
                    .body(Body::from(check_body))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let body = read_json(response).await;
        assert_eq!(status, StatusCode::OK, "check failed: {body}");
        assert_eq!(body["decision"]["outcome"], "granted");
        assert_eq!(body["decision"]["scope"], "session");

        let history = app
            .oneshot(
                Request::builder()
                    .uri("/v1/permissions/history?session_id=s1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = read_json(history).await;
        assert_eq!(body["data"].as_array().expect("history").len(), 1);
    }

    #[tokio::test]
    async fn permission_check_returns_suspend_when_no_rule_or_policy() {
        let app = build_test_router().await;
        let request = PermissionRequest::new(
            "p1",
            "s1",
            "alice",
            ActionClass::Exec,
            "shell.exec",
            "rm -rf /",
        );
        let body = serde_json::to_string(&request).expect("request json");
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let body = read_json(response).await;
        assert_eq!(status, StatusCode::OK, "check failed: {body}");
        assert_eq!(body["decision"]["outcome"], "suspend");
    }
}
