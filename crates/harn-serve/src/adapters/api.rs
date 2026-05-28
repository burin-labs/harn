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
use crate::transport::{apply_transport_layers, TransportConfig};

const OPENAPI_YAML: &str = include_str!("../../openapi.yaml");
const API_PROTOCOL_VERSION: &str = "agents-protocol-2026-04-25";

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
    /// Response compression + ETag + (optional) CORS. Defaults to the
    /// standard stack (gzip/brotli/zstd negotiation + strong-ETag
    /// conditional GETs; CORS disabled).
    pub transport: TransportConfig,
}

impl ApiServerConfig {
    pub fn for_pipeline(path: impl Into<String>) -> Self {
        let path = path.into();
        let root = Path::new(&path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self {
            acp: AcpServerConfig::for_pipeline(path),
            auth_policy: AuthPolicy::allow_all(),
            workspace_root: root,
            transport: TransportConfig::default_enabled(),
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

    /// Replace the transport config wholesale. Use this to attach a
    /// CORS policy or to disable compression/ETag selectively.
    pub fn with_transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }
}

#[derive(Clone)]
pub struct ApiServer {
    state: ApiState,
    transport: TransportConfig,
}

impl ApiServer {
    pub fn new(mut config: ApiServerConfig) -> Self {
        config.acp.auth_policy = AuthPolicy::allow_all();
        let (client, response_rx) = AcpClient::start(config.acp);
        let (events_tx, _) = broadcast::channel(1024);
        let state = ApiState {
            acp: client.clone(),
            inner: Arc::new(Mutex::new(ApiStateInner::new(config.workspace_root))),
            events_tx,
            auth_policy: config.auth_policy,
            permissions: Arc::new(InMemoryPermissionStore::default()),
        };
        client.spawn_output_loop(response_rx, state.clone());
        Self {
            state,
            transport: config.transport,
        }
    }

    pub async fn run_http(self: Arc<Self>, options: ApiHttpServeOptions) -> Result<(), String> {
        let router = api_router(self.state.clone());
        let router = apply_transport_layers(router, &self.transport);
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
}

struct ApiStateInner {
    sessions: BTreeMap<String, Value>,
    messages: HashMap<String, Vec<Value>>,
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
                    "permissions",
                    "workspace.files.read"
                ],
                "connectors": [],
                "quota_id": null
            }),
        );
        Self {
            sessions: BTreeMap::new(),
            messages: HashMap::new(),
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
            "_harn/agentEvent" | "harn.hitl.requested" => {
                self.append_event(None, None, &method, message);
            }
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
            .pointer("/approvalRequest/id")
            .or_else(|| params.get("toolCallId"))
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
            "action": params.pointer("/approvalRequest/action")
                .or_else(|| params.get("toolName"))
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
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/openapi.json", get(openapi_json))
        .route("/v1", get(api_root))
        .route("/v1/runtime", get(runtime))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/tools", get(list_tools))
        .route("/v1/tools/{tool_id}", get(get_tool))
        .route(
            "/v1/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/v1/workspaces/{workspace_id}",
            get(get_workspace).patch(update_workspace),
        )
        .route(
            "/v1/workspaces/{workspace_id}/files",
            get(read_workspace_file).put(write_workspace_file),
        )
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/v1/sessions/{session_id}",
            get(get_session).patch(update_session),
        )
        .route("/v1/sessions/{session_id}/close", post(close_session))
        .route("/v1/sessions/{session_id}/fork", post(fork_session))
        .route("/v1/sessions/{session_id}/truncate", post(truncate_session))
        .route(
            "/v1/sessions/{session_id}/messages",
            get(list_session_messages).post(append_session_message),
        )
        .route(
            "/v1/sessions/{session_id}/tasks",
            get(list_session_tasks).post(submit_session_task),
        )
        .route("/v1/tasks", get(list_tasks).post(submit_task))
        .route("/v1/tasks/{task_id}", get(get_task))
        .route("/v1/tasks/{task_id}/cancel", post(cancel_task))
        .route("/v1/events", get(list_events))
        .route("/v1/events/stream", get(stream_events))
        .route("/v1/sessions/{session_id}/events", get(list_session_events))
        .route(
            "/v1/sessions/{session_id}/events/stream",
            get(stream_session_events),
        )
        .route("/v1/tasks/{task_id}/events", get(list_task_events))
        .route("/v1/tasks/{task_id}/events/stream", get(stream_task_events))
        .route("/v1/tasks/{task_id}/stream", get(stream_task_events))
        .route("/v1/permission-requests", get(list_permission_requests))
        .route(
            "/v1/tasks/{task_id}/permission-requests",
            get(list_task_permission_requests),
        )
        .route(
            "/v1/permission-requests/{request_id}/respond",
            post(respond_permission_request),
        )
        .route(
            "/v1/permissions/policy",
            get(get_permissions_policy).put(put_permissions_policy),
        )
        .route(
            "/v1/permissions/rules",
            get(list_permission_rules).post(create_permission_rule),
        )
        .route(
            "/v1/permissions/rules/{rule_id}",
            axum::routing::delete(revoke_permission_rule),
        )
        .route("/v1/permissions/history", get(get_permission_history))
        .route("/v1/permissions/check", post(check_permission))
        .layer(DefaultBodyLimit::max(crate::DEFAULT_HTTP_BODY_LIMIT_BYTES))
        .with_state(state)
}

async fn health() -> Response {
    Json(json!({
        "ok": true,
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
    .into_response()
}

async fn version() -> Response {
    Json(json!({
        "object": "version",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION
    }))
    .into_response()
}

async fn openapi_json() -> Response {
    match serde_yml::from_str::<Value>(OPENAPI_YAML) {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "openapi_parse_failed",
            &error.to_string(),
        ),
    }
}

async fn api_root() -> Response {
    Json(json!({
        "object": "api",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION,
        "openapi": "/openapi.json",
        "resources": {
            "runtime": "/v1/runtime",
            "capabilities": "/v1/capabilities",
            "tools": "/v1/tools",
            "workspaces": "/v1/workspaces",
            "sessions": "/v1/sessions",
            "tasks": "/v1/tasks",
            "events": "/v1/events/stream",
            "permission_requests": "/v1/permission-requests",
            "permission_policy": "/v1/permissions/policy",
            "permission_rules": "/v1/permissions/rules",
            "permission_history": "/v1/permissions/history",
            "permission_check": "/v1/permissions/check"
        }
    }))
    .into_response()
}

async fn runtime(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(json!({
        "object": "runtime",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION,
        "adapter": "harn-serve-api",
        "workspace_root": inner.root_workspace_path,
        "session_count": inner.sessions.len(),
        "task_count": inner.tasks.len(),
        "capabilities": capability_values()
    }))
    .into_response()
}

async fn capabilities(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    Json(json!({
        "object": "capability_summary",
        "capabilities": capability_values()
    }))
    .into_response()
}

async fn list_tools(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    Json(list_response(tool_values())).into_response()
}

async fn get_tool(
    State(state): State<ApiState>,
    AxumPath(tool_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let Some(tool) = tool_values()
        .into_iter()
        .find(|tool| tool.get("id").and_then(Value::as_str) == Some(tool_id.as_str()))
    else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "tool not found");
    };
    Json(tool).into_response()
}

async fn list_workspaces(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(inner.workspaces.values().cloned().collect())).into_response()
}

async fn create_workspace(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let now = now_rfc3339();
    let id = format!("workspace_{}", Uuid::now_v7());
    let workspace = json!({
        "id": id,
        "object": "workspace",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "name": input.get("name").and_then(Value::as_str).unwrap_or("Workspace"),
        "root": input.get("root").and_then(Value::as_str).unwrap_or("."),
        "default_branch_id": null,
        "host": "local",
        "repository": null,
        "tenant_id": null,
        "capabilities": ["sessions", "tasks", "events", "permissions", "workspace.files.read"],
        "connectors": [],
        "quota_id": null
    });
    state
        .inner
        .lock()
        .expect("api state poisoned")
        .workspaces
        .insert(
            workspace["id"].as_str().unwrap_or_default().to_string(),
            workspace.clone(),
        );
    (StatusCode::CREATED, Json(workspace)).into_response()
}

async fn get_workspace(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.workspaces.get(&workspace_id).cloned() {
        Some(workspace) => Json(workspace).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    }
}

async fn update_workspace(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PATCH, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let mut inner = state.inner.lock().expect("api state poisoned");
    let Some(workspace) = inner.workspaces.get_mut(&workspace_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found");
    };
    merge_mutable_fields(workspace, &input, &["name", "metadata", "capabilities"]);
    workspace["updated_at"] = json!(now_rfc3339());
    Json(workspace.clone()).into_response()
}

async fn read_workspace_file(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let root = match workspace_root(&state, &workspace_id) {
        Some(root) => root,
        None => return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    };
    let path = query.path.as_deref().unwrap_or(".");
    let Some(full_path) = safe_read_path(&root, path) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must stay in workspace",
        );
    };
    let root_display = root.canonicalize().unwrap_or(root.clone());
    if full_path.is_dir() {
        let mut entries = Vec::new();
        match std::fs::read_dir(&full_path) {
            Ok(read_dir) => {
                for entry in read_dir.flatten().take(500) {
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    entries.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "path": entry.path().strip_prefix(&root_display).unwrap_or(entry.path().as_path()).to_string_lossy(),
                        "kind": if metadata.is_dir() { "directory" } else { "file" },
                        "size": metadata.len()
                    }));
                }
                entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
                return Json(json!({
                    "object": "file_listing",
                    "workspace_id": workspace_id,
                    "path": path,
                    "entries": entries
                }))
                .into_response();
            }
            Err(error) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file_error",
                    &error.to_string(),
                )
            }
        }
    }
    match std::fs::read_to_string(&full_path) {
        Ok(content) => Json(json!({
            "object": "file",
            "workspace_id": workspace_id,
            "path": path,
            "encoding": "utf-8",
            "content": content
        }))
        .into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_error",
            &error.to_string(),
        ),
    }
}

async fn write_workspace_file(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PUT, &uri, &headers, body.clone()).await {
        return response;
    }
    let root = match workspace_root(&state, &workspace_id) {
        Some(root) => root,
        None => return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    };
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .or(query.path.as_deref())
        .unwrap_or_default();
    let Some(full_path) = safe_write_path(&root, path) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must stay in workspace",
        );
    };
    let Some(content) = input.get("content").and_then(Value::as_str) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_content",
            "content is required",
        );
    };
    if let Some(parent) = full_path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "file_error",
                &error.to_string(),
            );
        }
    }
    match std::fs::write(&full_path, content) {
        Ok(()) => Json(json!({
            "object": "file",
            "workspace_id": workspace_id,
            "path": path,
            "encoding": "utf-8",
            "bytes": content.len()
        }))
        .into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_error",
            &error.to_string(),
        ),
    }
}

async fn list_sessions(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(inner.sessions.values().cloned().collect())).into_response()
}

async fn create_session(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let input = parse_json_body(&body).unwrap_or_else(|_| json!({}));
    let (workspace_id, workspace_root) = {
        let inner = state.inner.lock().expect("api state poisoned");
        let workspace_id = input
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or(&inner.root_workspace_id)
            .to_string();
        let root = workspace_root_locked(&inner, &workspace_id)
            .unwrap_or_else(|| inner.root_workspace_path.clone());
        (workspace_id, root)
    };
    let result = match state
        .acp
        .call(
            "session/new",
            json!({
                "cwd": workspace_root
            }),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("session_{}", Uuid::now_v7()));
    let now = now_rfc3339();
    let session = json!({
        "id": session_id,
        "object": "session",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "workspace_id": workspace_id,
        "state": "IDLE",
        "transcript": {
            "source": "harn.acp",
            "session_id": session_id
        },
        "persona_id": input.get("persona_id").cloned().unwrap_or(Value::Null),
        "root_session_id": null,
        "parent_session_id": null,
        "branch_id": null,
        "last_event_id": null,
        "summary": null,
        "expires_at": null
    });
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.insert(session_id.clone(), session.clone());
        inner.messages.entry(session_id.clone()).or_default();
    }
    state.append_event(Some(session_id), None, "session.created", session.clone());
    (StatusCode::CREATED, Json(session)).into_response()
}

async fn get_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.sessions.get(&session_id).cloned() {
        Some(session) => Json(session).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "session not found"),
    }
}

async fn update_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PATCH, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        merge_mutable_fields(session, &input, &["summary", "metadata"]);
        session["updated_at"] = json!(now_rfc3339());
        session.clone()
    };
    state.append_event(Some(session_id), None, "session.updated", session.clone());
    Json(session).into_response()
}

async fn close_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body).await {
        return response;
    }
    state.acp.send_raw(
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}),
    );
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        session["state"] = json!("CLOSED");
        session["updated_at"] = json!(now_rfc3339());
        session.clone()
    };
    state.append_event(Some(session_id), None, "session.closed", session.clone());
    Json(session).into_response()
}

async fn fork_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let input = parse_json_body(&body).unwrap_or_else(|_| json!({}));
    let parent = {
        let inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.get(&session_id).cloned()
    };
    let Some(parent) = parent else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    };
    let result = match state
        .acp
        .call(
            "session/fork",
            json!({
                "sessionId": session_id,
                "branchName": input.get("branch_id").and_then(Value::as_str)
            }),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let new_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("session_{}", Uuid::now_v7()));
    let now = now_rfc3339();
    let mut session = parent.clone();
    session["id"] = json!(new_id);
    session["created_at"] = json!(now);
    session["updated_at"] = json!(now);
    session["state"] = json!("IDLE");
    session["parent_session_id"] = json!(session_id);
    session["root_session_id"] = parent
        .get("root_session_id")
        .cloned()
        .filter(|value| !value.is_null())
        .unwrap_or_else(|| parent.get("id").cloned().unwrap_or(Value::Null));
    session["branch_id"] = input.get("branch_id").cloned().unwrap_or(Value::Null);
    session["metadata"] = input.get("metadata").cloned().unwrap_or_else(|| json!({}));
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.insert(new_id.clone(), session.clone());
        inner.messages.entry(new_id.clone()).or_default();
    }
    state.append_event(Some(new_id), None, "session.forked", session.clone());
    (StatusCode::CREATED, Json(session)).into_response()
}

async fn truncate_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let keep_first = match input
        .get("keep_first")
        .or_else(|| input.get("keepFirst"))
        .and_then(Value::as_i64)
    {
        Some(value) if value >= 0 => value as usize,
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "keep_first must be a non-negative integer",
            )
        }
    };
    {
        let inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
    }

    let mut acp_params = json!({
        "sessionId": session_id.clone(),
        "keepFirst": keep_first,
    });
    if let Some(reason) = input.get("reason").and_then(Value::as_str) {
        acp_params["reason"] = json!(reason);
    }
    let result = match state.acp.call("session/truncate", acp_params).await {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let kept_turn_count = result
        .get("keptTurnCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let removed_turn_count = result
        .get("removedTurnCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let new_tip_turn_id = result.get("newTipTurnId").cloned().unwrap_or(Value::Null);

    let (session, canceled_task) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        if let Some(messages) = inner.messages.get_mut(&session_id) {
            messages.truncate(keep_first);
        }
        let canceled_task_id = inner.active_task_by_session.remove(&session_id);
        let now = now_rfc3339();
        let canceled_task = canceled_task_id.and_then(|task_id| {
            let task = inner.tasks.get_mut(&task_id)?;
            if task.get("status").and_then(Value::as_str) != Some("CANCELED") {
                task["status"] = json!("CANCELED");
                task["updated_at"] = json!(&now);
                task["canceled_at"] = json!(&now);
            }
            Some((task_id, task.clone()))
        });
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        if session.get("state").and_then(Value::as_str) != Some("CLOSED") {
            session["state"] = json!("IDLE");
        }
        session["updated_at"] = json!(now);
        (session.clone(), canceled_task)
    };
    if let Some((task_id, task)) = canceled_task {
        state.append_event(
            Some(session_id.clone()),
            Some(task_id),
            "task.canceled",
            task,
        );
    }
    let response = json!({
        "object": "session.truncate_result",
        "session_id": session_id,
        "kept_turn_count": kept_turn_count,
        "removed_turn_count": removed_turn_count,
        "new_tip_turn_id": new_tip_turn_id,
        "session": session,
    });
    state.append_event(
        Some(session_id),
        None,
        "session.truncated",
        response.clone(),
    );
    Json(response).into_response()
}

async fn list_session_messages(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    let Some(messages) = inner.messages.get(&session_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    };
    Json(list_response(limit_values(messages.clone(), query.limit))).into_response()
}

async fn append_session_message(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let message_input = input.get("message").cloned().unwrap_or(input.clone());
    let message = message_resource(&session_id, None, message_input.clone());
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
        inner
            .messages
            .entry(session_id.clone())
            .or_default()
            .push(message.clone());
    }
    state.append_event(
        Some(session_id.clone()),
        None,
        "message.created",
        message.clone(),
    );
    if input.get("run").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(prompt) = prompt_text(&message_input) {
            let result = state
                .acp
                .call(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt}]
                    }),
                )
                .await;
            if let Err(error) = result {
                return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error);
            }
        }
    }
    (StatusCode::CREATED, Json(message)).into_response()
}

async fn list_session_tasks(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    if !inner.sessions.contains_key(&session_id) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    }
    let tasks = inner
        .tasks
        .values()
        .filter(|task| task.get("session_id").and_then(Value::as_str) == Some(session_id.as_str()))
        .cloned()
        .collect();
    Json(list_response(limit_values(tasks, query.limit))).into_response()
}

async fn submit_session_task(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    submit_task_inner(state, Some(session_id), uri, headers, body).await
}

async fn submit_task(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    submit_task_inner(state, None, uri, headers, body).await
}

async fn submit_task_inner(
    state: ApiState,
    path_session_id: Option<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let Some(session_id) = path_session_id.or_else(|| {
        input
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    }) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_session",
            "session_id is required",
        );
    };
    let (workspace_id, task, message_input) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        let workspace_id = session
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or(&inner.root_workspace_id)
            .to_string();
        let task_id = format!("task_{}", Uuid::now_v7());
        let now = now_rfc3339();
        let input_value = input.get("input").cloned().unwrap_or_else(|| json!({}));
        let task = json!({
            "id": task_id,
            "object": "task",
            "created_at": now,
            "updated_at": now,
            "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
            "session_id": session_id,
            "workspace_id": workspace_id,
            "status": "WORKING",
            "input": input_value,
            "created_by": "api",
            "persona_id": input.get("persona_id").cloned().unwrap_or(Value::Null),
            "branch_id": input.get("branch_id").cloned().unwrap_or(Value::Null),
            "parent_task_id": input.get("parent_task_id").cloned().unwrap_or(Value::Null),
            "assigned_agent_id": null,
            "receipt_id": null,
            "outcome_id": null,
            "quota_id": null,
            "started_at": now,
            "completed_at": null,
            "canceled_at": null,
            "failure": null
        });
        inner.tasks.insert(
            task["id"].as_str().unwrap_or_default().to_string(),
            task.clone(),
        );
        inner.active_task_by_session.insert(
            task["session_id"].as_str().unwrap_or_default().to_string(),
            task["id"].as_str().unwrap_or_default().to_string(),
        );
        (workspace_id, task, input_value)
    };
    let task_id = task["id"].as_str().unwrap_or_default().to_string();
    let message = message_resource(&session_id, Some(&task_id), message_input.clone());
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner
            .messages
            .entry(session_id.clone())
            .or_default()
            .push(message);
    }
    state.append_event(
        Some(session_id.clone()),
        Some(task_id.clone()),
        "task.started",
        task.clone(),
    );
    let prompt = prompt_text(&message_input);
    let task_state = state.clone();
    tokio::spawn(async move {
        let result = match prompt {
            Some(prompt) => {
                task_state
                    .acp
                    .call(
                        "session/prompt",
                        json!({
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": prompt}]
                        }),
                    )
                    .await
            }
            None => Err("task input did not contain prompt text".to_string()),
        };
        let (status, event, payload) = match result {
            Ok(result) => ("COMPLETED", "task.completed", result),
            Err(error) => (
                "FAILED",
                "task.failed",
                json!({
                    "error": error
                }),
            ),
        };
        let mut task_snapshot = None;
        {
            let mut inner = task_state.inner.lock().expect("api state poisoned");
            if let Some(task) = inner.tasks.get_mut(&task_id) {
                if task.get("status").and_then(Value::as_str) != Some("CANCELED") {
                    let now = now_rfc3339();
                    task["status"] = json!(status);
                    task["updated_at"] = json!(&now);
                    if status == "COMPLETED" {
                        task["completed_at"] = json!(now);
                        task["outcome_id"] = json!(format!("outcome_{task_id}"));
                    } else {
                        task["failure"] = json!({
                            "code": "task_failed",
                            "message": payload.get("error").and_then(Value::as_str).unwrap_or("task failed")
                        });
                    }
                    task_snapshot = Some(task.clone());
                }
            }
            inner.active_task_by_session.remove(&session_id);
        }
        if let Some(task) = task_snapshot {
            task_state.append_event(Some(session_id), Some(task_id), event, task);
        }
    });
    let mut task = task;
    task["workspace_id"] = json!(workspace_id);
    (StatusCode::ACCEPTED, Json(task)).into_response()
}

async fn list_tasks(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    let tasks = inner
        .tasks
        .values()
        .filter(|task| {
            query.workspace_id.as_deref().is_none_or(|workspace_id| {
                task.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
            }) && query.session_id.as_deref().is_none_or(|session_id| {
                task.get("session_id").and_then(Value::as_str) == Some(session_id)
            })
        })
        .cloned()
        .collect();
    Json(list_response(limit_values(tasks, query.limit))).into_response()
}

async fn get_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.tasks.get(&task_id).cloned() {
        Some(task) => Json(task).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "task not found"),
    }
}

async fn cancel_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body).await {
        return response;
    }
    let (session_id, task) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let (session_id, task_snapshot) = {
            let Some(task) = inner.tasks.get_mut(&task_id) else {
                return api_error(StatusCode::NOT_FOUND, "not_found", "task not found");
            };
            let session_id = task
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let now = now_rfc3339();
            task["status"] = json!("CANCELED");
            task["updated_at"] = json!(&now);
            task["canceled_at"] = json!(now);
            (session_id, task.clone())
        };
        inner.active_task_by_session.remove(&session_id);
        (session_id, task_snapshot)
    };
    state.acp.send_raw(
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}),
    );
    state.append_event(
        Some(session_id),
        Some(task_id),
        "task.canceled",
        task.clone(),
    );
    Json(task).into_response()
}

async fn list_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, None, None);
    Json(list_response(state.history(&filter))).into_response()
}

async fn list_session_events(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, Some(session_id), None);
    Json(list_response(state.history(&filter))).into_response()
}

async fn list_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, None, Some(task_id));
    Json(list_response(state.history(&filter))).into_response()
}

async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, None, None),
        uri,
        headers,
    )
    .await
}

async fn stream_session_events(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, Some(session_id), None),
        uri,
        headers,
    )
    .await
}

async fn stream_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, None, Some(task_id)),
        uri,
        headers,
    )
    .await
}

async fn stream_events_response(
    state: ApiState,
    filter: EventFilter,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let history = state.history(&filter);
    let replay = stream::iter(history.into_iter().map(|event| Ok(sse_event(&event))));
    let live = live_event_stream(state.events_tx.subscribe(), filter);
    Sse::new(replay.chain(live))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn live_event_stream(
    rx: broadcast::Receiver<ApiEvent>,
    filter: EventFilter,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((rx, filter), |(mut rx, filter)| async move {
        loop {
            match rx.recv().await {
                Ok(event) if filter.matches(&event) => {
                    return Some((Ok(sse_event(&event)), (rx, filter)));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
}

async fn list_permission_requests(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(limit_values(
        inner
            .permissions
            .values()
            .filter(|permission| {
                query.session_id.as_deref().is_none_or(|session_id| {
                    permission.public.get("session_id").and_then(Value::as_str) == Some(session_id)
                }) && query.task_id.as_deref().is_none_or(|task_id| {
                    permission.public.get("task_id").and_then(Value::as_str) == Some(task_id)
                })
            })
            .map(|permission| permission.public.clone())
            .collect(),
        query.limit,
    )))
    .into_response()
}

async fn list_task_permission_requests(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(limit_values(
        inner
            .permissions
            .values()
            .filter(|permission| {
                permission.public.get("task_id").and_then(Value::as_str) == Some(task_id.as_str())
            })
            .map(|permission| permission.public.clone())
            .collect(),
        query.limit,
    )))
    .into_response()
}

async fn respond_permission_request(
    State(state): State<ApiState>,
    AxumPath(request_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let approved = input
        .get("approved")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            matches!(
                input.get("outcome").and_then(Value::as_str),
                Some("approved" | "approve" | "selected")
            )
        });
    let outcome = if approved { "approved" } else { "denied" };
    let (permission, rpc_id, hitl) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let (permission, rpc_id, hitl, task_id) = {
            let Some(permission) = inner.permissions.get_mut(&request_id) else {
                return api_error(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "permission request not found",
                );
            };
            permission.public["status"] = json!(outcome);
            permission.public["updated_at"] = json!(now_rfc3339());
            permission.public["response"] = input.clone();
            let task_id = permission
                .public
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            (
                permission.public.clone(),
                permission.rpc_id,
                permission.hitl,
                task_id,
            )
        };
        if let Some(task_id) = task_id {
            set_task_status(&mut inner.tasks, &task_id, "WORKING");
        }
        (permission, rpc_id, hitl)
    };
    if hitl {
        let hitl_response = json!({
            "request_id": request_id,
            "approved": approved,
            "accepted": approved,
            "answer": input.get("answer").cloned().unwrap_or(Value::Null),
            "reviewer": input.get("reviewer").cloned().unwrap_or_else(|| json!("api")),
            "reason": input.get("reason").cloned().unwrap_or(Value::Null),
            "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({}))
        });
        if let Err(error) = state.acp.call("harn.hitl.respond", hitl_response).await {
            return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error);
        }
    } else if let Some(rpc_id) = rpc_id {
        let result = if approved {
            json!({"outcome": "approved"})
        } else {
            json!({
                "outcome": "denied",
                "reason": input.get("reason").and_then(Value::as_str).unwrap_or("denied by API client")
            })
        };
        state.acp.send_raw(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": result
        }));
    }
    record_permission_response(&state, &permission, &input, approved).await;
    state.append_event(
        permission["session_id"].as_str().map(str::to_string),
        permission["task_id"].as_str().map(str::to_string),
        "permission.responded",
        permission.clone(),
    );
    Json(permission).into_response()
}

/// Bridge from the ACP-style `respond_permission_request` flow to the
/// new permissions store. Materializes a [`PermissionRequest`] from the
/// pending permission payload, records the audit entry, and optionally
/// installs a remember-rule when the responder asked to "remember"
/// their answer. The reconstruction is best-effort: ACP today does not
/// carry the full action/target shape, so missing fields fall back to
/// the public payload's `action` value or the literal "unknown" string.
async fn record_permission_response(
    state: &ApiState,
    permission: &Value,
    input: &Value,
    approved: bool,
) {
    let session_id = permission
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let action_value = permission.get("action").cloned().unwrap_or(Value::Null);
    let action = action_value
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            action_value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let target = action_value
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| action.clone());
    let class = input
        .get("class")
        .and_then(Value::as_str)
        .and_then(parse_action_class)
        .unwrap_or(ActionClass::Custom);
    let actor = input
        .get("reviewer")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "api".to_string());
    let mut request = PermissionRequest::new(
        permission
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        session_id,
        actor.clone(),
        class,
        action,
        target,
    );
    if let Some(reason) = input.get("reason").and_then(Value::as_str) {
        request.reason = Some(reason.to_string());
    }
    let policy_version = state.permissions.policy().await.version();
    let scope = input
        .get("scope")
        .and_then(Value::as_str)
        .and_then(parse_decision_scope)
        .unwrap_or(DecisionScope::Session);
    let expires_at = input
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|raw| OffsetDateTime::parse(raw, &Rfc3339).ok());
    let decision = if approved {
        PermissionDecision::Granted {
            scope,
            policy_version,
            reason: request.reason.clone(),
            expires_at,
            rule_id: None,
        }
    } else {
        PermissionDecision::Denied {
            scope,
            policy_version,
            reason: request.reason.clone(),
            rule_id: None,
        }
    };
    let remember = input
        .get("remember")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| RememberSpec {
            scope,
            action_pattern: input
                .get("action_pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            target_pattern: input
                .get("target_pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at,
        });
    state
        .permissions
        .record_decision(&request, &decision, Some(actor), remember)
        .await;
}

fn parse_action_class(raw: &str) -> Option<ActionClass> {
    match raw {
        "read" => Some(ActionClass::Read),
        "write" => Some(ActionClass::Write),
        "exec" => Some(ActionClass::Exec),
        "net" => Some(ActionClass::Net),
        "llm" => Some(ActionClass::Llm),
        "custom" => Some(ActionClass::Custom),
        _ => None,
    }
}

fn parse_decision_scope(raw: &str) -> Option<DecisionScope> {
    match raw {
        "session" => Some(DecisionScope::Session),
        "workspace" => Some(DecisionScope::Workspace),
        "user" => Some(DecisionScope::User),
        "always" => Some(DecisionScope::Always),
        _ => None,
    }
}

async fn get_permissions_policy(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let policy = state.permissions.policy().await;
    let version = policy.version();
    Json(json!({
        "object": "permission_policy",
        "version": version.as_str(),
        "policy": policy,
    }))
    .into_response()
}

async fn put_permissions_policy(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PUT, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let policy_value = input.get("policy").cloned().unwrap_or(input.clone());
    let policy: PermissionPolicy = match serde_json::from_value(policy_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_policy",
                &format!("policy did not deserialize: {error}"),
            );
        }
    };
    if let Err(errors) = policy.lint() {
        let messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_policy",
            &messages.join("; "),
        );
    }
    let version = state.permissions.install_policy(policy.clone()).await;
    Json(json!({
        "object": "permission_policy",
        "version": version.as_str(),
        "policy": policy,
    }))
    .into_response()
}

async fn list_permission_rules(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let rules = state.permissions.rules().await;
    let data: Vec<Value> = rules
        .into_iter()
        .map(|rule| serde_json::to_value(rule).unwrap_or(Value::Null))
        .collect();
    Json(list_response(data)).into_response()
}

async fn create_permission_rule(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let rule: RememberRule = match serde_json::from_value(input) {
        Ok(rule) => rule,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_rule",
                &format!("rule did not deserialize: {error}"),
            );
        }
    };
    state.permissions.add_rule(rule.clone()).await;
    Json(serde_json::to_value(&rule).unwrap_or(Value::Null)).into_response()
}

async fn revoke_permission_rule(
    State(state): State<ApiState>,
    AxumPath(rule_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::DELETE, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let id = RuleId(rule_id);
    if state.permissions.revoke_rule(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "not_found", "rule not found")
    }
}

#[derive(Deserialize, Default)]
struct PermissionHistoryQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
    tenant_id: Option<String>,
    actor: Option<String>,
    outcome: Option<crate::permissions::AuditOutcome>,
    limit: Option<usize>,
}

async fn get_permission_history(
    State(state): State<ApiState>,
    Query(query): Query<PermissionHistoryQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = AuditFilter {
        tenant_id: query.tenant_id,
        session_id: query.session_id,
        workspace_id: query.workspace_id,
        actor: query.actor,
        outcome: query.outcome,
        since: None,
        limit: query.limit,
    };
    let entries = state.permissions.history(&filter).await;
    let data: Vec<Value> = entries
        .into_iter()
        .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
        .collect();
    Json(list_response(data)).into_response()
}

async fn check_permission(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be JSON",
        );
    };
    let mut request: PermissionRequest = match serde_json::from_value(input) {
        Ok(request) => request,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("permission request did not deserialize: {error}"),
            );
        }
    };
    if request.id.is_empty() {
        request.id = format!("permission_{}", Uuid::now_v7());
    }
    let decision = state.permissions.evaluate(&request).await;
    state
        .permissions
        .record_decision(&request, &decision, None, None)
        .await;
    Json(json!({
        "object": "permission_decision",
        "request_id": request.id,
        "decision": decision,
    }))
    .into_response()
}

async fn authorize(
    state: &ApiState,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<(), Response> {
    let request = AuthRequest {
        method: method.as_str().to_string(),
        path: uri.path().to_string(),
        body: body.to_vec(),
        headers: headers_to_map(headers),
        validated_oauth: None,
        tenant_id: None,
    };
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
/// payload, mirroring how `harn-cloud-gateway` reports the same case.
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

fn headers_to_map(headers: &HeaderMap) -> BTreeMap<String, String> {
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

fn sse_event(event: &ApiEvent) -> Event {
    Event::default()
        .id(event.id.clone())
        .event(event.event.clone())
        .json_data(event)
        .unwrap_or_else(|_| Event::default().event("error").data("{}"))
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

fn workspace_root(state: &ApiState, workspace_id: &str) -> Option<PathBuf> {
    let inner = state.inner.lock().expect("api state poisoned");
    workspace_root_locked(&inner, workspace_id)
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

fn clean_relative_components(relative: &str) -> Option<Vec<OsString>> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    Some(
        relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_os_string()),
                Component::CurDir => None,
                _ => None,
            })
            .collect(),
    )
}

fn safe_read_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let candidate = clean_relative_components(relative)?.into_iter().fold(
        root.clone(),
        |mut path, component| {
            path.push(component);
            path
        },
    );
    let resolved = candidate.canonicalize().ok()?;
    resolved.starts_with(&root).then_some(resolved)
}

fn safe_write_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let components = clean_relative_components(relative)?;
    let mut path = root.clone();
    for (index, component) in components.iter().enumerate() {
        let next = path.join(component);
        if next.exists() {
            let resolved = next.canonicalize().ok()?;
            if !resolved.starts_with(&root) {
                return None;
            }
            path = resolved;
        } else {
            path = next;
            for remaining in components.iter().skip(index + 1) {
                path.push(remaining);
            }
            return Some(path);
        }
    }
    Some(path)
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

fn capability_values() -> Vec<Value> {
    vec![
        json!({"id": "sessions", "description": "Create, inspect, fork, truncate, update, and close ACP-backed Harn sessions."}),
        json!({"id": "tasks", "description": "Submit prompts asynchronously, track task status, and abort active tasks."}),
        json!({"id": "events", "description": "Read snapshots and stream live session, task, tool, permission, and runtime events over SSE."}),
        json!({"id": "permissions", "description": "Approve or deny host permission and HITL requests through the same ACP runtime path."}),
        json!({"id": "tools", "description": "Inspect the local control-plane tool registry exposed by this server."}),
        json!({"id": "workspace.files", "description": "Read and write UTF-8 workspace files under registered workspace roots."}),
    ]
}

fn tool_values() -> Vec<Value> {
    vec![
        json!({
            "id": "harn.session.prompt",
            "object": "tool",
            "name": "session.prompt",
            "description": "Submit text to a Harn session through ACP session/prompt.",
            "input_schema": {"type": "object", "required": ["session_id", "text"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.session.cancel",
            "object": "tool",
            "name": "session.cancel",
            "description": "Cancel the active prompt for a Harn session.",
            "input_schema": {"type": "object", "required": ["session_id"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.session.truncate",
            "object": "tool",
            "name": "session.truncate",
            "description": "Drop a Harn session transcript after the first N turns.",
            "input_schema": {"type": "object", "required": ["session_id", "keep_first"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.permission.respond",
            "object": "tool",
            "name": "permission.respond",
            "description": "Approve or deny ACP permission and HITL requests.",
            "input_schema": {"type": "object", "required": ["request_id", "approved"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.events.stream",
            "object": "tool",
            "name": "events.stream",
            "description": "Stream Harn local API events as Server-Sent Events.",
            "input_schema": {"type": "object"},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.workspace.file",
            "object": "tool",
            "name": "workspace.file",
            "description": "Read or write UTF-8 workspace files below a registered root.",
            "input_schema": {"type": "object", "required": ["workspace_id", "path"]},
            "output_schema": {"type": "object"}
        }),
    ]
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

    #[cfg(unix)]
    #[test]
    fn workspace_file_paths_reject_parent_and_symlink_escape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside"))
            .expect("create symlink");

        assert!(safe_read_path(dir.path(), "../secret.txt").is_none());
        assert!(safe_write_path(dir.path(), "../secret.txt").is_none());
        assert!(safe_read_path(dir.path(), "outside").is_none());
        assert!(safe_write_path(dir.path(), "outside/secret.txt").is_none());
    }
}
