use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::ACCEPT;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{stream, StreamExt};
use harn_vm::mcp_protocol;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
use uuid::Uuid;

use crate::{
    AdapterDescriptor, AuthRequest, CallArguments, CallRequest, CallResponse, DispatchCore,
    DispatchError, ExportCatalog, HttpTlsConfig, TransportAdapter,
};

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

const MCP_PROTOCOL_HEADER: &str = "mcp-protocol-version";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const DEPRECATION_HEADER: &str = "deprecation";

#[derive(Clone, Debug)]
pub struct McpHttpServeOptions {
    pub bind: SocketAddr,
    pub path: String,
    pub sse_path: String,
    pub messages_path: String,
    pub tls: HttpTlsConfig,
}

impl Default for McpHttpServeOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8765".parse().expect("valid bind addr"),
            path: "/mcp".to_string(),
            sse_path: "/sse".to_string(),
            messages_path: "/messages".to_string(),
            tls: HttpTlsConfig::plain(),
        }
    }
}

pub struct McpServerConfig {
    pub core: DispatchCore,
    pub server_name: Option<String>,
    pub server_card: Option<JsonValue>,
}

impl McpServerConfig {
    pub fn new(core: DispatchCore) -> Self {
        Self {
            server_name: Some(derived_server_name(core.catalog())),
            server_card: None,
            core,
        }
    }

    pub fn with_server_card(mut self, card: JsonValue) -> Self {
        self.server_card = Some(card);
        self
    }
}

pub type McpStdioServer = McpServer;

pub struct McpServer {
    descriptor: AdapterDescriptor,
    server_name: String,
    server_card: Option<JsonValue>,
    catalog: ExportCatalog,
    executor: ExecutionRuntime,
}

#[derive(Clone, Debug)]
struct ConnectionState {
    initialized: bool,
    client_identity: String,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            initialized: false,
            client_identity: "unknown".to_string(),
        }
    }
}

#[derive(Clone)]
struct ActiveCall {
    cancel_token: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct SessionState {
    connection: ConnectionState,
    active_calls: HashMap<String, ActiveCall>,
    stream_tx: Option<UnboundedSender<JsonValue>>,
}

#[derive(Clone)]
struct SharedSession {
    inner: Arc<Mutex<SessionState>>,
}

impl SharedSession {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionState::default())),
        }
    }

    fn connection(&self) -> ConnectionState {
        self.inner
            .lock()
            .expect("session poisoned")
            .connection
            .clone()
    }

    fn update_connection(&self, connection: ConnectionState) {
        self.inner.lock().expect("session poisoned").connection = connection;
    }

    fn insert_call(&self, request_id: String, active: ActiveCall) {
        self.inner
            .lock()
            .expect("session poisoned")
            .active_calls
            .insert(request_id, active);
    }

    fn remove_call(&self, request_id: &str) -> Option<ActiveCall> {
        self.inner
            .lock()
            .expect("session poisoned")
            .active_calls
            .remove(request_id)
    }

    fn cancel_call(&self, request_id: &str) -> bool {
        let mut guard = self.inner.lock().expect("session poisoned");
        let Some(active) = guard.active_calls.remove(request_id) else {
            return false;
        };
        active.cancelled.store(true, Ordering::SeqCst);
        active.cancel_token.store(true, Ordering::SeqCst);
        true
    }

    fn set_stream_tx(&self, tx: Option<UnboundedSender<JsonValue>>) {
        self.inner.lock().expect("session poisoned").stream_tx = tx;
    }

    fn stream_tx(&self) -> Option<UnboundedSender<JsonValue>> {
        self.inner
            .lock()
            .expect("session poisoned")
            .stream_tx
            .as_ref()
            .cloned()
    }
}

struct ExecutionRuntime {
    tx: mpsc::UnboundedSender<ExecutionJob>,
}

struct ExecutionJob {
    request: CallRequest,
    response_tx: oneshot::Sender<Result<CallResponse, DispatchError>>,
}

#[derive(Clone)]
struct HttpState {
    server: Arc<McpServer>,
    options: McpHttpServeOptions,
    sessions: Arc<Mutex<HashMap<String, SharedSession>>>,
}

#[derive(Clone)]
struct RequestContext {
    session: SharedSession,
    connection: ConnectionState,
    auth: AuthRequest,
}

enum ImmediateResult {
    Response(JsonValue),
    Accepted,
    Stream(Box<StreamJob>),
}

struct StreamJob {
    request_id: JsonValue,
    request_key: String,
    tool_name: String,
    arguments: JsonValue,
    progress_token: Option<JsonValue>,
    context: RequestContext,
}

impl McpServer {
    pub fn new(config: McpServerConfig) -> Self {
        let server_name = config
            .server_name
            .unwrap_or_else(|| derived_server_name(config.core.catalog()));
        let core = Arc::new(config.core);
        let catalog = core.catalog().clone();
        Self {
            descriptor: AdapterDescriptor {
                id: "mcp".to_string(),
                caller_shape: "tool".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            server_name,
            server_card: config.server_card,
            catalog,
            executor: ExecutionRuntime::start(core.clone()),
        }
    }

    pub async fn run_stdio(self: Arc<Self>) -> Result<(), String> {
        let session = SharedSession::new();
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        let mut stdout = tokio::io::stdout();
        let (tx, mut rx) = mpsc::unbounded_channel::<JsonValue>();

        let writer = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let mut encoded =
                    serde_json::to_string(&message).map_err(|error| error.to_string())?;
                encoded.push('\n');
                stdout
                    .write_all(encoded.as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
                stdout.flush().await.map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        });

        eprintln!("[harn] MCP workflow server ready on stdio");

        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let request = match serde_json::from_str::<JsonValue>(trimmed) {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(parse_error_response(&error.to_string()));
                    continue;
                }
            };
            let auth = AuthRequest {
                method: "STDIO".to_string(),
                path: String::new(),
                body: line.into_bytes(),
                headers: BTreeMap::new(),
                validated_oauth: None,
            };
            self.clone()
                .handle_stdio_message(request, session.clone(), auth, tx.clone())
                .await;
        }

        drop(tx);
        writer
            .await
            .map_err(|error| format!("stdio writer task failed: {error}"))?
    }

    pub async fn run_http(self: Arc<Self>, options: McpHttpServeOptions) -> Result<(), String> {
        let state = HttpState {
            server: self,
            options: options.clone(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };
        let router = Router::new()
            .route(
                &options.path,
                post(http_post_request)
                    .get(http_get_stream)
                    .delete(http_delete_session),
            )
            .route(
                &options.sse_path,
                get(legacy_sse_stream).post(legacy_sse_message),
            )
            .route(&options.messages_path, post(legacy_sse_message))
            .with_state(state.clone());
        let router = crate::tls::apply_security_headers(router, &options.tls);
        let listener = crate::tls::bind_listener(options.bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        eprintln!(
            "[harn] MCP workflow server ready on {}://{local_addr}{}",
            options.tls.listener_scheme(),
            options.path
        );
        crate::tls::serve_router_from_tcp(listener, router, &options.tls)
            .await
            .map_err(|error| format!("MCP HTTP server failed: {error}"))
    }

    async fn handle_stdio_message(
        self: Arc<Self>,
        request: JsonValue,
        session: SharedSession,
        auth: AuthRequest,
        tx: mpsc::UnboundedSender<JsonValue>,
    ) {
        match self.process_message(request, session.clone(), auth).await {
            ImmediateResult::Response(response) => {
                let _ = tx.send(response);
            }
            ImmediateResult::Accepted => {}
            ImmediateResult::Stream(job) => {
                tokio::spawn(async move {
                    let notifier = notify_channel(move |message| {
                        let _ = tx.send(message);
                    });
                    self.execute_streaming_job(*job, notifier).await;
                });
            }
        }
    }

    async fn process_message(
        &self,
        request: JsonValue,
        session: SharedSession,
        auth: AuthRequest,
    ) -> ImmediateResult {
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        if request.get("id").is_none() {
            if method == "notifications/cancelled" {
                self.handle_cancel_notification(&session, &params);
            }
            return ImmediateResult::Accepted;
        }

        if method == "initialize" {
            return ImmediateResult::Response(self.handle_initialize(id, &session, &params));
        }

        let connection = session.connection();
        if !connection.initialized && method != "ping" {
            return ImmediateResult::Response(harn_vm::jsonrpc::error_response(
                id,
                -32002,
                "server not initialized",
            ));
        }

        match method {
            "notifications/initialized" | "initialized" => ImmediateResult::Accepted,
            "ping" => ImmediateResult::Response(harn_vm::jsonrpc::response(id, json!({}))),
            "logging/setLevel" => {
                ImmediateResult::Response(harn_vm::jsonrpc::response(id, json!({})))
            }
            "tools/list" => ImmediateResult::Response(harn_vm::jsonrpc::response(
                id,
                self.tools_list_result(&params),
            )),
            "tools/call" => match self.prepare_stream_job(id, params, session, connection, auth) {
                Ok(job) => ImmediateResult::Stream(Box::new(job)),
                Err(response) => ImmediateResult::Response(response),
            },
            "resources/list" => ImmediateResult::Response(harn_vm::jsonrpc::response(
                id,
                self.resources_list_result(&params),
            )),
            "resources/read" => ImmediateResult::Response(self.handle_resources_read(id, &params)),
            "resources/templates/list" => ImmediateResult::Response(harn_vm::jsonrpc::response(
                id,
                paged_result("resourceTemplates", Vec::new(), &params),
            )),
            "prompts/list" => ImmediateResult::Response(harn_vm::jsonrpc::response(
                id,
                paged_result("prompts", Vec::new(), &params),
            )),
            "prompts/get" => ImmediateResult::Response(harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "Unknown prompt",
            )),
            _ if mcp_protocol::unsupported_latest_spec_method(method).is_some() => {
                ImmediateResult::Response(
                    mcp_protocol::unsupported_latest_spec_method_response(id, method)
                        .expect("checked unsupported MCP method"),
                )
            }
            _ => ImmediateResult::Response(harn_vm::jsonrpc::error_response(
                id,
                -32601,
                &format!("Method not found: {method}"),
            )),
        }
    }

    fn handle_initialize(
        &self,
        id: JsonValue,
        session: &SharedSession,
        params: &JsonValue,
    ) -> JsonValue {
        let requested = params
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if !requested.is_empty() && requested != MCP_PROTOCOL_VERSION {
            return harn_vm::jsonrpc::error_response_with_data(
                id,
                -32602,
                "Unsupported protocol version",
                json!({
                    "supported": [MCP_PROTOCOL_VERSION],
                    "requested": requested,
                }),
            );
        }

        let client_name = params
            .pointer("/clientInfo/name")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        let client_version = params
            .pointer("/clientInfo/version")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        session.update_connection(ConnectionState {
            initialized: true,
            client_identity: format!("{client_name}/{client_version}"),
        });

        let mut capabilities = serde_json::Map::new();
        if !self.catalog.functions.is_empty() {
            capabilities.insert("tools".to_string(), json!({}));
        }
        if self.server_card.is_some() {
            capabilities.insert("resources".to_string(), json!({}));
        }
        capabilities.insert("logging".to_string(), json!({}));

        let mut server_info = json!({
            "name": self.server_name,
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(card) = &self.server_card {
            server_info["card"] = card.clone();
        }

        harn_vm::jsonrpc::response(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": capabilities,
                "serverInfo": server_info,
            }),
        )
    }

    fn handle_cancel_notification(&self, session: &SharedSession, params: &JsonValue) {
        let Some(request_id) = params.get("requestId") else {
            return;
        };
        let request_key = request_key(request_id);
        let _ = session.cancel_call(&request_key);
    }

    fn prepare_stream_job(
        &self,
        request_id: JsonValue,
        params: JsonValue,
        session: SharedSession,
        connection: ConnectionState,
        auth: AuthRequest,
    ) -> Result<StreamJob, JsonValue> {
        let tool_name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        if mcp_protocol::requests_task_augmentation(&params) {
            return Err(mcp_protocol::unsupported_task_augmentation_response(
                request_id,
                "tools/call",
            ));
        }
        if self.catalog.function(&tool_name).is_none() {
            return Err(harn_vm::jsonrpc::error_response(
                request_id,
                -32602,
                &format!("Unknown tool: {tool_name}"),
            ));
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let progress_token = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(is_valid_progress_token);
        let request_key = request_key(&request_id);
        Ok(StreamJob {
            request_id: request_id.clone(),
            request_key,
            tool_name,
            arguments,
            progress_token,
            context: RequestContext {
                session,
                connection,
                auth,
            },
        })
    }

    async fn execute_streaming_job(
        &self,
        job: StreamJob,
        notify: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) {
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        job.context.session.insert_call(
            job.request_key.clone(),
            ActiveCall {
                cancel_token: cancel_token.clone(),
                cancelled: cancelled.clone(),
            },
        );

        let progress_stop = if let Some(progress_token) = job.progress_token.clone() {
            notify(progress_notification(
                progress_token.clone(),
                0.0,
                format!("Starting {}", job.tool_name),
            ));
            Some(spawn_progress_notifier(
                progress_token,
                job.tool_name.clone(),
                notify.clone(),
            ))
        } else {
            None
        };

        let request = match build_call_request(
            &self.descriptor.id,
            &job.context.connection.client_identity,
            &job.tool_name,
            job.arguments,
            job.context.auth,
            cancel_token,
        ) {
            Ok(request) => request,
            Err(error) => {
                job.context.session.remove_call(&job.request_key);
                if let Some(stop) = progress_stop {
                    let _ = stop.send(());
                }
                notify(harn_vm::jsonrpc::error_response(
                    job.request_id,
                    -32602,
                    &error,
                ));
                return;
            }
        };

        let result = self.executor.call(request).await;
        job.context.session.remove_call(&job.request_key);
        if let Some(stop) = progress_stop {
            let _ = stop.send(());
        }
        if cancelled.load(Ordering::SeqCst) {
            return;
        }

        match result {
            Ok(response) => notify(harn_vm::jsonrpc::response(
                job.request_id,
                tool_call_success(response),
            )),
            Err(DispatchError::Validation(message)) => notify(harn_vm::jsonrpc::error_response(
                job.request_id,
                -32602,
                &message,
            )),
            Err(DispatchError::Unauthorized(message)) => notify(harn_vm::jsonrpc::error_response(
                job.request_id,
                -32001,
                &message,
            )),
            Err(DispatchError::MissingExport(message)) => notify(harn_vm::jsonrpc::error_response(
                job.request_id,
                -32602,
                &message,
            )),
            Err(DispatchError::Execution(message))
            | Err(DispatchError::Cancelled(message))
            | Err(DispatchError::Io(message))
            | Err(DispatchError::Cache(message)) => notify(harn_vm::jsonrpc::response(
                job.request_id,
                tool_call_error(message),
            )),
        }
    }

    fn tools_list_result(&self, params: &JsonValue) -> JsonValue {
        let tools = self
            .catalog
            .functions
            .values()
            .map(tool_entry)
            .collect::<Vec<_>>();
        paged_result("tools", tools, params)
    }

    fn resources_list_result(&self, params: &JsonValue) -> JsonValue {
        let resources = self
            .server_card
            .as_ref()
            .map(|_| {
                json!({
                    "uri": "well-known://mcp-card",
                    "name": "Server Card",
                    "description": "MCP Server Card advertising this server's identity and capabilities",
                    "mimeType": "application/json",
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        paged_result("resources", resources, params)
    }

    fn handle_resources_read(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if uri == "well-known://mcp-card" {
            if let Some(card) = &self.server_card {
                return harn_vm::jsonrpc::response(
                    id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "text": serde_json::to_string(card).unwrap_or_else(|_| "{}".to_string()),
                            "mimeType": "application/json",
                        }]
                    }),
                );
            }
        }
        harn_vm::jsonrpc::error_response(id, -32002, &format!("Resource not found: {uri}"))
    }
}

#[async_trait::async_trait(?Send)]
impl TransportAdapter for McpServer {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
}

impl ExecutionRuntime {
    fn start(core: Arc<DispatchCore>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<ExecutionJob>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build MCP runtime");
            let local = LocalSet::new();
            local.block_on(&runtime, async move {
                while let Some(job) = rx.recv().await {
                    let core = core.clone();
                    tokio::task::spawn_local(async move {
                        let result = core.dispatch(job.request).await;
                        let _ = job.response_tx.send(result);
                    });
                }
            });
        });
        Self { tx }
    }

    async fn call(&self, request: CallRequest) -> Result<CallResponse, DispatchError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ExecutionJob {
                request,
                response_tx,
            })
            .map_err(|_| DispatchError::Execution("MCP executor is not running".to_string()))?;
        response_rx
            .await
            .map_err(|_| DispatchError::Execution("MCP executor dropped response".to_string()))?
    }
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

    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(parse_error_response(&error.to_string())),
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
            Err(response) => return *response,
        };
    let auth = http_auth_request(method, &state.options.path, body.to_vec(), &headers);

    match state.server.process_message(request, session, auth).await {
        ImmediateResult::Accepted => StatusCode::ACCEPTED.into_response(),
        ImmediateResult::Response(response) => {
            let mut http = if should_stream_post_response(&headers) {
                sse_single_response(response).into_response()
            } else {
                Json(response).into_response()
            };
            attach_http_headers(
                &mut http,
                created.then_some(session_id.as_str()),
                MCP_PROTOCOL_VERSION,
            );
            http
        }
        ImmediateResult::Stream(job) => {
            let stream = spawn_http_stream(state.server.clone(), *job);
            let mut http = stream.into_response();
            attach_http_headers(
                &mut http,
                created.then_some(session_id.as_str()),
                MCP_PROTOCOL_VERSION,
            );
            http
        }
    }
}

async fn http_get_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
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
        .expect("sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (tx, rx) = unbounded::<JsonValue>();
    session.set_stream_tx(Some(tx));
    let mut response = sse_response(rx).into_response();
    attach_http_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

async fn http_delete_session(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
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
        .expect("sessions poisoned")
        .remove(session_id);
    let mut response = if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    };
    attach_http_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

async fn legacy_sse_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let session_id = Uuid::now_v7().to_string();
    let session = SharedSession::new();
    let (tx, rx) = unbounded::<JsonValue>();
    session.set_stream_tx(Some(tx));
    state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .insert(session_id.clone(), session);
    let endpoint_event = Event::default().event("endpoint").data(format!(
        "{}?session_id={session_id}",
        state.options.messages_path
    ));
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(endpoint_event) }).chain(sse_events(rx));
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
    let Some(session_id) = query.get("session_id") else {
        let mut response = StatusCode::BAD_REQUEST.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        let mut response = StatusCode::NOT_FOUND.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                Json(parse_error_response(&error.to_string())),
            )
                .into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let auth = http_auth_request(
        Method::POST,
        &state.options.messages_path,
        body.to_vec(),
        &headers,
    );
    match state
        .server
        .process_message(request, session.clone(), auth)
        .await
    {
        ImmediateResult::Accepted => {
            let mut response = StatusCode::ACCEPTED.into_response();
            attach_legacy_deprecation_headers(&mut response);
            response
        }
        ImmediateResult::Response(response) => {
            if let Some(tx) = session.stream_tx() {
                let _ = tx.unbounded_send(response);
                let mut response = StatusCode::ACCEPTED.into_response();
                attach_legacy_deprecation_headers(&mut response);
                response
            } else {
                let mut response = StatusCode::GONE.into_response();
                attach_legacy_deprecation_headers(&mut response);
                response
            }
        }
        ImmediateResult::Stream(job) => {
            let Some(tx) = session.stream_tx() else {
                let mut response = StatusCode::GONE.into_response();
                attach_legacy_deprecation_headers(&mut response);
                return response;
            };
            tokio::spawn(async move {
                let notifier = notify_channel(move |message| {
                    let _ = tx.unbounded_send(message);
                });
                state.server.execute_streaming_job(*job, notifier).await;
            });
            let mut response = StatusCode::ACCEPTED.into_response();
            attach_legacy_deprecation_headers(&mut response);
            response
        }
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

fn spawn_http_stream(
    server: Arc<McpServer>,
    job: StreamJob,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = unbounded::<JsonValue>();
    tokio::spawn(async move {
        let notifier = notify_channel(move |message| {
            let _ = tx.unbounded_send(message);
        });
        server.execute_streaming_job(job, notifier).await;
    });
    sse_response(rx)
}

fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let stream = stream::once(async move { Ok::<Event, Infallible>(prime) }).chain(sse_events(rx));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_events(
    rx: UnboundedReceiver<JsonValue>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    rx.map(|message| {
        Ok(Event::default()
            .id(Uuid::now_v7().to_string())
            .event("message")
            .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
    })
}

fn spawn_progress_notifier(
    progress_token: JsonValue,
    tool_name: String,
    notify: Arc<dyn Fn(JsonValue) + Send + Sync>,
) -> oneshot::Sender<()> {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut progress = 0.0;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = ticker.tick() => {
                    progress += 1.0;
                    notify(progress_notification(
                        progress_token.clone(),
                        progress,
                        format!("Running {tool_name}"),
                    ));
                }
            }
        }
    });
    stop_tx
}

fn notify_channel<F>(notify: F) -> Arc<dyn Fn(JsonValue) + Send + Sync>
where
    F: Fn(JsonValue) + Send + Sync + 'static,
{
    Arc::new(notify)
}

fn build_call_request(
    adapter: &str,
    caller: &str,
    tool_name: &str,
    arguments: JsonValue,
    auth: AuthRequest,
    cancel_token: Arc<AtomicBool>,
) -> Result<CallRequest, String> {
    let arguments = match arguments {
        JsonValue::Null => CallArguments::Named(BTreeMap::new()),
        JsonValue::Object(values) => CallArguments::Named(
            values
                .into_iter()
                .collect::<BTreeMap<String, serde_json::Value>>(),
        ),
        JsonValue::Array(values) => CallArguments::Positional(values),
        _ => {
            return Err("tool arguments must be an object, array, or null".to_string());
        }
    };
    Ok(CallRequest {
        adapter: adapter.to_string(),
        function: tool_name.to_string(),
        arguments,
        auth,
        caller: caller.to_string(),
        replay_key: None,
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: Some(cancel_token),
        agent_session_id: None,
    })
}

fn paged_result(key: &str, entries: Vec<JsonValue>, params: &JsonValue) -> JsonValue {
    let (offset, page_size) = parse_cursor(params);
    let page_end = (offset + page_size).min(entries.len());
    let page_entries = entries[offset..page_end].to_vec();
    let mut result = json!({ key: page_entries });
    if page_end < entries.len() {
        result["nextCursor"] = json!(encode_cursor(page_end));
    }
    result
}

fn encode_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

fn parse_cursor(params: &JsonValue) -> (usize, usize) {
    let offset = params
        .get("cursor")
        .and_then(JsonValue::as_str)
        .and_then(|cursor| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(cursor)
                .ok()?;
            let text = String::from_utf8(bytes).ok()?;
            text.parse::<usize>().ok()
        })
        .unwrap_or(0);
    (offset, 50)
}

fn tool_entry(function: &crate::ExportedFunction) -> JsonValue {
    let mut entry = json!({
        "name": function.name,
        "description": format!("Invoke exported Harn function '{}'.", function.name),
        "inputSchema": function.input_schema,
    });
    if let Some(output_schema) = function.output_schema.clone() {
        entry["outputSchema"] = output_schema;
    }
    entry
}

fn tool_call_success(response: CallResponse) -> JsonValue {
    let mut result = json!({
        "content": content_blocks(&response.value),
        "isError": false,
    });
    if let JsonValue::Object(map) = response.value {
        result["structuredContent"] = JsonValue::Object(map);
    }
    result
}

fn tool_call_error(message: String) -> JsonValue {
    json!({
        "content": [{
            "type": "text",
            "text": message,
        }],
        "isError": true,
    })
}

fn content_blocks(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::String(text) => json!([{ "type": "text", "text": text }]),
        _ => json!([{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]),
    }
}

fn progress_notification(progress_token: JsonValue, progress: f64, message: String) -> JsonValue {
    harn_vm::jsonrpc::notification(
        "notifications/progress",
        json!({
            "progressToken": progress_token,
            "progress": progress,
            "message": message,
        }),
    )
}

fn request_key(id: &JsonValue) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_string())
}

fn parse_error_response(message: &str) -> JsonValue {
    harn_vm::jsonrpc::error_response(JsonValue::Null, -32700, &format!("Parse error: {message}"))
}

fn is_valid_progress_token(value: &JsonValue) -> bool {
    matches!(value, JsonValue::String(_) | JsonValue::Number(_))
}

fn derived_server_name(catalog: &ExportCatalog) -> String {
    catalog
        .script_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("harn-serve")
        .to_string()
}

fn http_auth_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    headers: &HeaderMap,
) -> AuthRequest {
    AuthRequest {
        method: method.as_str().to_string(),
        path: path.to_string(),
        body,
        headers: normalized_headers(headers),
        validated_oauth: None,
    }
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

fn lookup_or_create_session(
    state: &HttpState,
    request: &JsonValue,
    header_session: Option<String>,
) -> Result<(String, SharedSession, bool), Box<Response>> {
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let mut sessions = state.sessions.lock().expect("sessions poisoned");
    if let Some(session_id) = header_session {
        if let Some(session) = sessions.get(&session_id).cloned() {
            return Ok((session_id, session, false));
        }
        return Err(Box::new(StatusCode::NOT_FOUND.into_response()));
    }
    if method != "initialize" {
        return Err(Box::new(StatusCode::BAD_REQUEST.into_response()));
    }
    let session_id = Uuid::now_v7().to_string();
    let session = SharedSession::new();
    sessions.insert(session_id.clone(), session.clone());
    Ok((session_id, session, true))
}

fn attach_http_headers(response: &mut Response, session_id: Option<&str>, protocol: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DispatchCoreConfig;

    #[tokio::test]
    async fn tools_list_exposes_public_functions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = McpServer::new(McpServerConfig::new(core));
        let tools = server.tools_list_result(&json!({}));
        assert_eq!(tools["tools"][0]["name"], "greet");
        assert_eq!(tools["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(tools["tools"][0]["outputSchema"]["type"], "string");
    }

    #[tokio::test]
    async fn initialize_and_resources_expose_server_card() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = McpServer::new(
            McpServerConfig::new(core)
                .with_server_card(json!({"name": "fixture-card", "version": "1"})),
        );

        let session = SharedSession::new();
        let init = server.handle_initialize(
            json!(1),
            &session,
            &json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "clientInfo": {"name": "test", "version": "1"}
            }),
        );
        assert_eq!(
            init["result"]["serverInfo"]["card"]["name"],
            json!("fixture-card")
        );
        assert!(init["result"]["capabilities"]["resources"].is_object());

        let resources = server.resources_list_result(&json!({}));
        assert_eq!(
            resources["resources"][0]["uri"],
            json!("well-known://mcp-card")
        );
        let read = server.handle_resources_read(json!(2), &json!({"uri": "well-known://mcp-card"}));
        assert!(read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fixture-card"));
    }

    #[tokio::test]
    async fn latest_spec_gap_methods_return_explicit_json_rpc_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = McpServer::new(McpServerConfig::new(core));
        let session = SharedSession::new();
        let _ = server.handle_initialize(
            json!(1),
            &session,
            &json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "clientInfo": {"name": "test", "version": "1"}
            }),
        );

        for method in harn_vm::mcp_protocol::UNSUPPORTED_LATEST_SPEC_METHODS
            .iter()
            .map(|entry| entry.method)
        {
            let response = match server
                .process_message(
                    harn_vm::jsonrpc::request(2, method, json!({})),
                    session.clone(),
                    AuthRequest::default(),
                )
                .await
            {
                ImmediateResult::Response(response) => response,
                ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
                    panic!("expected error response for {method}")
                }
            };
            assert_eq!(response["error"]["code"], json!(-32601), "{method}");
            assert_eq!(response["error"]["data"]["method"], json!(method));
            assert_eq!(response["error"]["data"]["status"], json!("unsupported"));
        }
    }

    #[tokio::test]
    async fn tool_call_rejects_task_augmentation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = McpServer::new(McpServerConfig::new(core));
        let session = SharedSession::new();
        let _ = server.handle_initialize(
            json!(1),
            &session,
            &json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "clientInfo": {"name": "test", "version": "1"}
            }),
        );

        let response = match server
            .process_message(
                harn_vm::jsonrpc::request(
                    2,
                    "tools/call",
                    json!({
                        "name": "greet",
                        "arguments": {"name": "alice"},
                        "task": {"title": "async please"}
                    }),
                ),
                session,
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::Response(response) => response,
            ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
                panic!("expected task-augmentation error response")
            }
        };

        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
    }

    #[test]
    fn paged_result_returns_next_cursor_and_decodes_it() {
        let entries = (0..55)
            .map(|index| json!({"name": format!("tool-{index}")}))
            .collect::<Vec<_>>();
        let first = paged_result("tools", entries.clone(), &json!({}));
        assert_eq!(first["tools"].as_array().unwrap().len(), 50);
        assert_eq!(first["tools"][49]["name"], json!("tool-49"));

        let second = paged_result(
            "tools",
            entries,
            &json!({"cursor": first["nextCursor"].as_str().unwrap()}),
        );
        assert_eq!(second["tools"].as_array().unwrap().len(), 5);
        assert_eq!(second["tools"][0]["name"], json!("tool-50"));
        assert!(second.get("nextCursor").is_none());
    }

    #[test]
    fn build_call_request_accepts_named_arguments() {
        let request = build_call_request(
            "mcp",
            "tester",
            "greet",
            json!({"name": "alice"}),
            AuthRequest::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("call request");
        match request.arguments {
            CallArguments::Named(values) => assert_eq!(values["name"], json!("alice")),
            other => panic!("expected named arguments, got {other:?}"),
        }
    }

    #[test]
    fn streamable_http_accept_negotiation_uses_sse_only_when_json_is_absent() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        assert!(should_stream_post_response(&headers));

        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        assert!(!should_stream_post_response(&headers));
    }

    #[test]
    fn legacy_deprecation_header_is_attached() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_legacy_deprecation_headers(&mut response);
        assert_eq!(
            response
                .headers()
                .get(DEPRECATION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
    }
}
