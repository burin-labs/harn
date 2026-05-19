//! Model Context Protocol (MCP) adapter facade.
//!
//! Transport code, wire-shape helpers, and auth/header validation live in
//! focused child modules so the public server surface stays easy to audit.
//! Module map: `transport` owns HTTP/stdio/SSE routing, `schema` owns JSON-RPC
//! result shaping and call normalization, and `auth` owns request metadata plus
//! transport header validation.

mod auth;
mod schema;
#[cfg(test)]
mod tests;
mod transport;

use schema::{
    build_call_request, derived_server_name, paged_result, parse_error_response, request_key,
    tool_call_error, tool_call_success, tool_entry,
};
use transport::{
    http_delete_session, http_get_stream, http_post_request, legacy_sse_message, legacy_sse_stream,
    notify_channel,
};

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    mcp_context::McpContextCatalog, AdapterDescriptor, AuthPolicy, AuthRequest,
    AuthorizationDecision, CallArguments, CallRequest, CallResponse, DispatchCore, DispatchError,
    ExportCatalog, HttpTlsConfig, TransportAdapter,
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
    context: McpContextCatalog,
    auth_policy: AuthPolicy,
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
        let context = McpContextCatalog::discover(&catalog.script_path);
        let auth_policy = core.auth_policy().clone();
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
            context,
            auth_policy,
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

        if let Err(response) = self
            .authorize_protocol_method(id.clone(), method, &auth)
            .await
        {
            return ImmediateResult::Response(response);
        }
        if let Some(response) =
            mcp_protocol::unsupported_client_bound_method_response(id.clone(), method)
        {
            return ImmediateResult::Response(response);
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
                self.resources_templates_list_result(&params),
            )),
            "prompts/list" => ImmediateResult::Response(harn_vm::jsonrpc::response(
                id,
                self.prompts_list_result(&params),
            )),
            "prompts/get" => ImmediateResult::Response(self.handle_prompts_get(id, &params)),
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                ImmediateResult::Response(self.handle_completion_complete(id, &params))
            }
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
        if self.server_card.is_some() || self.context.has_resources() {
            capabilities.insert("resources".to_string(), json!({}));
        }
        if self.context.has_prompts() {
            capabilities.insert("prompts".to_string(), json!({}));
        }
        capabilities.insert("logging".to_string(), json!({}));
        if self.context.has_resources() || self.context.has_prompts() {
            capabilities.insert(
                "completions".to_string(),
                mcp_protocol::completions_capability(),
            );
        }

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

    async fn authorize_protocol_method(
        &self,
        id: JsonValue,
        method: &str,
        auth: &AuthRequest,
    ) -> Result<(), JsonValue> {
        if self.auth_policy.methods.is_empty() || !requires_protocol_auth(method) {
            return Ok(());
        }
        match self.auth_policy.authorize(auth).await {
            AuthorizationDecision::Authorized(_) => Ok(()),
            AuthorizationDecision::Rejected(message) => {
                Err(harn_vm::jsonrpc::error_response(id, -32001, &message))
            }
        }
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
        let Some(function) = self.catalog.function(&tool_name) else {
            return Err(harn_vm::jsonrpc::error_response(
                request_id,
                -32602,
                &format!("Unknown tool: {tool_name}"),
            ));
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // SPECULATIVE: validates draft MCP SEP-2356 file inputs.
        // Revisit this when the MCP proposal is ratified.
        if let Err(message) = harn_vm::mcp_file_upload::validate_file_inputs_for_call(
            &arguments,
            &function.input_schema,
        ) {
            return Err(harn_vm::jsonrpc::response(
                request_id,
                tool_call_error(message),
            ));
        }
        let progress_token = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(harn_vm::mcp_progress::is_valid_progress_token);
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

        let progress_ctx = job.progress_token.clone().map(|token| {
            let bus = harn_vm::mcp_progress::ProgressBus::new(notify.clone());
            harn_vm::mcp_progress::ProgressContext::new(bus, token)
        });

        let request = match build_call_request(
            &self.descriptor.id,
            &job.context.connection.client_identity,
            &job.tool_name,
            job.arguments,
            job.context.auth,
            cancel_token,
            progress_ctx,
        ) {
            Ok(request) => request,
            Err(error) => {
                job.context.session.remove_call(&job.request_key);
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
        let mut resources = Vec::new();
        if self.server_card.is_some() {
            resources.push(json!({
                "uri": "well-known://mcp-card",
                "name": "Server Card",
                "description": "MCP Server Card advertising this server's identity and capabilities",
                "mimeType": "application/json",
            }));
        }
        resources.extend(self.context.resource_entries());
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
        if let Some((text, mime_type)) = self.context.read_resource(uri) {
            return harn_vm::jsonrpc::response(
                id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "text": text,
                        "mimeType": mime_type,
                    }]
                }),
            );
        }
        harn_vm::jsonrpc::error_response(id, -32002, &format!("Resource not found: {uri}"))
    }

    fn resources_templates_list_result(&self, params: &JsonValue) -> JsonValue {
        paged_result(
            "resourceTemplates",
            self.context.resource_templates(),
            params,
        )
    }

    fn prompts_list_result(&self, params: &JsonValue) -> JsonValue {
        paged_result("prompts", self.context.prompt_entries(), params)
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
        match self.context.get_prompt(name, &arguments) {
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

    fn handle_completion_complete(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let Some(ref_type) = params.pointer("/ref/type").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(id, -32602, "completion ref.type is required");
        };
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
        match ref_type {
            "ref/prompt" => {
                let name = params
                    .pointer("/ref/name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                match self.context.complete_prompt(name, argument_name, value) {
                    Ok(completion) => {
                        harn_vm::jsonrpc::response(id, json!({ "completion": completion }))
                    }
                    Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
                }
            }
            "ref/resource" => {
                let uri_template = params
                    .pointer("/ref/uri")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                match self
                    .context
                    .complete_resource_template(uri_template, argument_name, value)
                {
                    Ok(completion) => {
                        harn_vm::jsonrpc::response(id, json!({ "completion": completion }))
                    }
                    Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
                }
            }
            other => harn_vm::jsonrpc::error_response(
                id,
                -32602,
                &format!("Unsupported completion ref.type: {other}"),
            ),
        }
    }
}

fn requires_protocol_auth(method: &str) -> bool {
    matches!(
        method,
        "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/read"
            | "resources/templates/list"
            | "prompts/list"
            | "prompts/get"
            | mcp_protocol::METHOD_COMPLETION_COMPLETE
    )
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
