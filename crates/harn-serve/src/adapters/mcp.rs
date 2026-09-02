//! Model Context Protocol (MCP) adapter facade.
//!
//! Transport code, wire-shape helpers, and auth/header validation live in
//! focused child modules so the public server surface stays easy to audit.
//! Module map: `transport` owns HTTP/stdio routing, `schema` owns JSON-RPC
//! result shaping and call normalization, and `auth` owns request metadata plus
//! transport header validation.

mod auth;
mod schema;
#[cfg(test)]
mod tests;
mod transport;

use crate::transport::{
    read_jsonrpc_stdio_frame, write_jsonrpc_stdio_message, JsonRpcStdioFrameStyle,
};
use schema::{
    build_call_request, derived_server_name, paged_result, parse_error_response, request_key,
    tool_call_error, tool_call_success,
};
use transport::{http_post_request, notify_channel};

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::ACCEPT;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::{stream, StreamExt};
use harn_vm::mcp_protocol::{
    self, apply_result_envelope, negotiate_http_request, server_discover_result,
    standard_name_header_value, McpCacheHint,
};
use serde_json::{json, Value as JsonValue};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    mcp_context::McpContextCatalog, AdapterDescriptor, AuthPolicy, AuthRequest,
    AuthorizationDecision, CallArguments, CallRequest, CallResponse, DispatchCore, DispatchError,
    DispatchRuntime, ExportCatalog, HttpTlsConfig, TransportAdapter,
};

pub const MCP_PROTOCOL_VERSION: &str = mcp_protocol::PROTOCOL_VERSION;

const MCP_PROTOCOL_HEADER: &str = mcp_protocol::MCP_HEADER_PROTOCOL_VERSION;

#[derive(Clone, Debug)]
pub struct McpHttpServeOptions {
    pub bind: SocketAddr,
    pub path: String,
    pub tls: HttpTlsConfig,
}

impl Default for McpHttpServeOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8765".parse().expect("valid bind addr"),
            path: "/mcp".to_string(),
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
    tool_catalog: harn_vm::tool_registry::ToolCatalog,
    mcp_tools: Vec<JsonValue>,
    context: McpContextCatalog,
    auth_policy: AuthPolicy,
    executor: DispatchRuntime,
    /// Tasks handed out for `@job` exports. Same lifecycle as the orchestrator
    /// and script servers (`harn_vm::mcp_tasks`).
    tasks: Arc<harn_vm::mcp_tasks::McpTaskStore>,
}

#[derive(Clone)]
struct ActiveCall {
    cancel_token: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct SessionState {
    connection: mcp_protocol::McpServerSession,
    active_calls: HashMap<String, ActiveCall>,
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

    fn connection(&self) -> mcp_protocol::McpServerSession {
        self.inner
            .lock()
            .expect("session poisoned")
            .connection
            .clone()
    }

    fn with_connection<T>(&self, f: impl FnOnce(&mut mcp_protocol::McpServerSession) -> T) -> T {
        f(&mut self.inner.lock().expect("session poisoned").connection)
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
}

#[derive(Clone)]
struct HttpState {
    server: Arc<McpServer>,
    options: McpHttpServeOptions,
}

#[derive(Clone)]
struct RequestContext {
    session: SharedSession,
    connection: mcp_protocol::McpServerSession,
    auth: AuthRequest,
}

enum ImmediateResult {
    Response(JsonValue),
    Accepted,
    Stream(Box<StreamJob>),
    /// The client already has its `tools/call` answer -- a task id -- and the
    /// work still has to run. The transport writes `immediate` now and drives
    /// `job` in the background; the job files its own terminal response against
    /// the task rather than writing it to the wire.
    TaskStream {
        immediate: JsonValue,
        job: Box<StreamJob>,
    },
}

struct StreamJob {
    request_id: JsonValue,
    request_key: String,
    tool_name: String,
    arguments: JsonValue,
    progress_token: Option<JsonValue>,
    request_profile: mcp_protocol::McpRequestProfile,
    context: RequestContext,
    /// Set when the client asked for a task. This lease owns both terminal
    /// completion and the exact cancellation token installed in dispatch.
    task: Option<harn_vm::mcp_tasks::McpTaskLease>,
}

impl McpServer {
    pub fn new(config: McpServerConfig) -> Self {
        let server_name = config
            .server_name
            .unwrap_or_else(|| derived_server_name(config.core.catalog()));
        let core = Arc::new(config.core);
        let catalog = core.catalog().clone();
        let tool_catalog = core.tool_catalog().clone();
        let mcp_tools = core.mcp_tools().to_vec();
        let context = McpContextCatalog::discover(&catalog.script_path);
        let auth_policy = core.auth_policy().clone();
        Self {
            tasks: Arc::new(harn_vm::mcp_tasks::McpTaskStore::new()),
            descriptor: AdapterDescriptor {
                id: "mcp".to_string(),
                caller_shape: "tool".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            server_name,
            server_card: config.server_card,
            catalog,
            tool_catalog,
            mcp_tools,
            context,
            auth_policy,
            executor: DispatchRuntime::start("MCP", core),
        }
    }

    pub async fn run_stdio(self: Arc<Self>) -> Result<(), String> {
        let session = SharedSession::new();
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let (tx, mut rx) = mpsc::unbounded_channel::<JsonValue>();
        let output_style = Arc::new(Mutex::new(JsonRpcStdioFrameStyle::default()));
        let writer_style = output_style.clone();

        let writer = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            while let Some(message) = rx.recv().await {
                let style = *writer_style.lock().expect("stdio frame style poisoned");
                write_jsonrpc_stdio_message(&mut stdout, &message, style).await?;
            }
            Ok::<(), String>(())
        });

        eprintln!("[harn] MCP workflow server ready on stdio");

        while let Some(frame) = read_jsonrpc_stdio_frame(&mut stdin).await? {
            *output_style.lock().expect("stdio frame style poisoned") = frame.style;
            let request = match frame.parse_json() {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(parse_error_response(&error.to_string()));
                    continue;
                }
            };
            let auth = AuthRequest {
                method: "STDIO".to_string(),
                path: String::new(),
                body: frame.body,
                headers: BTreeMap::new(),
                ..AuthRequest::default()
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
        let listener = crate::tls::bind_listener(options.bind)?;
        self.run_http_from_listener(listener, options).await
    }

    /// Serve over a pre-bound TCP listener. Tests use this entry point
    /// so they can capture the assigned ephemeral port before starting
    /// the server and avoid bind/poll handshakes.
    pub async fn run_http_from_listener(
        self: Arc<Self>,
        listener: std::net::TcpListener,
        options: McpHttpServeOptions,
    ) -> Result<(), String> {
        let state = HttpState {
            server: self,
            options: options.clone(),
        };
        let router = Router::new()
            .route(&options.path, post(http_post_request))
            .layer(DefaultBodyLimit::max(crate::DEFAULT_HTTP_BODY_LIMIT_BYTES))
            .with_state(state.clone());
        let router = crate::tls::apply_security_headers(router, &options.tls);
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
            ImmediateResult::TaskStream { immediate, job } => {
                let _ = tx.send(immediate);
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

        if method == "initialize" {
            let result = session.with_connection(|connection| {
                connection.initialize(
                    &params,
                    JsonValue::Object(self.server_capabilities()),
                    self.server_info(),
                    self.catalog.instructions.as_deref(),
                )
            });
            return match result {
                Ok(result) => ImmediateResult::Response(harn_vm::jsonrpc::response(id, result)),
                Err(error) => {
                    ImmediateResult::Response(harn_vm::jsonrpc::error_response(id, -32602, &error))
                }
            };
        }

        if request.get("id").is_none() {
            if method == "notifications/cancelled" {
                self.handle_cancel_notification(&session, &params);
            }
            return ImmediateResult::Accepted;
        }

        let request_profile = match session
            .with_connection(|connection| connection.accept_request(&id, method, &params))
        {
            Ok(profile) => profile,
            Err(response) => return ImmediateResult::Response(response),
        };

        if method == mcp_protocol::METHOD_SERVER_DISCOVER {
            return ImmediateResult::Response(self.handle_server_discover(id));
        }

        let connection = session.connection();

        let task_access = match self
            .authorize_protocol_method(id.clone(), method, &auth)
            .await
        {
            Ok(access) => access,
            Err(response) => return ImmediateResult::Response(response),
        };
        if let Some(response) =
            mcp_protocol::explicit_unsupported_method_response(id.clone(), method)
        {
            return ImmediateResult::Response(response);
        }
        if request_profile.uses_result_envelope()
            && mcp_protocol::is_task_method(method)
            && !mcp_protocol::client_supports_tasks(&params)
        {
            return ImmediateResult::Response(mcp_protocol::missing_tasks_capability_response(id));
        }

        let response = match method {
            "ping" => harn_vm::jsonrpc::response(id, json!({})),
            "tools/list" => harn_vm::jsonrpc::response(id, self.tools_list_result(&params)),
            "tools/call" => match self.prepare_stream_job(
                id,
                params,
                request_profile,
                session,
                connection,
                auth,
                task_access,
            ) {
                Ok((job, Some(immediate))) => {
                    return ImmediateResult::TaskStream {
                        immediate,
                        job: Box::new(job),
                    }
                }
                Ok((job, None)) => return ImmediateResult::Stream(Box::new(job)),
                Err(response) => return ImmediateResult::Response(response),
            },
            mcp_protocol::METHOD_TASKS_GET => self.tasks.handle_get(&task_access, id, &params),
            mcp_protocol::METHOD_TASKS_UPDATE => {
                self.tasks.handle_update(&task_access, id, &params)
            }
            mcp_protocol::METHOD_TASKS_CANCEL => {
                self.tasks.handle_cancel(&task_access, id, &params)
            }
            "resources/list" => harn_vm::jsonrpc::response(id, self.resources_list_result(&params)),
            "resources/read" => self.handle_resources_read(id, &params),
            "resources/templates/list" => {
                harn_vm::jsonrpc::response(id, self.resources_templates_list_result(&params))
            }
            "prompts/list" => harn_vm::jsonrpc::response(id, self.prompts_list_result(&params)),
            "prompts/get" => self.handle_prompts_get(id, &params),
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                self.handle_completion_complete(id, &params)
            }
            _ => {
                harn_vm::jsonrpc::error_response(id, -32601, &format!("Method not found: {method}"))
            }
        };
        if request_profile.uses_result_envelope() {
            ImmediateResult::Response(envelope(response, cache_hint_for_method(method)))
        } else {
            ImmediateResult::Response(response)
        }
    }

    fn handle_server_discover(&self, id: JsonValue) -> JsonValue {
        let result = server_discover_result(
            JsonValue::Object(self.server_capabilities()),
            self.server_info(),
            self.catalog.instructions.as_deref(),
        );
        harn_vm::jsonrpc::response(id, result)
    }

    fn server_capabilities(&self) -> serde_json::Map<String, JsonValue> {
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
        if self.context.has_resources() || self.context.has_prompts() {
            capabilities.insert(
                "completions".to_string(),
                mcp_protocol::completions_capability(),
            );
        }
        // Advertised only when at least one export declares `@job`. A server
        // whose script has no long-running entrypoint has nothing to serve as a
        // task, and telling a client otherwise would leave it unable to
        // distinguish "no tasks here" from "your task is gone".
        if self.has_task_capable_export() {
            capabilities.insert("extensions".to_string(), mcp_protocol::tasks_capability());
        }
        capabilities
    }

    /// Whether any export declares `@job`, and so could be run as a task.
    fn has_task_capable_export(&self) -> bool {
        self.catalog
            .functions
            .values()
            .any(|function| function.job.is_some())
    }

    fn server_info(&self) -> JsonValue {
        let mut server_info = json!({
            "name": self.server_name,
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(title) = self
            .catalog
            .instructions
            .as_deref()
            .and_then(|text| text.lines().next())
            .map(str::trim)
            .filter(|line| !line.is_empty() && line.chars().count() <= 80)
        {
            server_info["title"] = json!(title);
        }
        if let Some(card) = &self.server_card {
            server_info["card"] = card.clone();
        }
        if let Some(icons) = self.context.server_icons() {
            server_info["icons"] = icons;
        }
        server_info
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
    ) -> Result<harn_vm::mcp_tasks::McpTaskAccess, JsonValue> {
        if self.auth_policy.methods.is_empty() || !requires_protocol_auth(method) {
            return Ok(harn_vm::mcp_tasks::McpTaskAccess::unscoped());
        }
        match self.auth_policy.authorize(auth).await {
            AuthorizationDecision::Authorized(principal) => Ok(task_access(&principal)),
            AuthorizationDecision::Rejected(message) => {
                Err(harn_vm::jsonrpc::error_response(id, -32001, &message))
            }
            // `authorize()` passes an empty required-scopes set, so the
            // emptiness rule (`empty ⊆ anything`) makes this branch
            // unreachable. Treat as forbidden defensively if invariants
            // ever shift.
            AuthorizationDecision::MissingScope { required, granted } => {
                Err(harn_vm::jsonrpc::error_response(
                    id,
                    -32003,
                    &crate::forbidden_message(&required, &granted),
                ))
            }
            // `authorize_mcp` is the only producer of this variant and
            // is never called from this adapter — keep the match
            // exhaustive without a wildcard so adding new variants
            // continues to break here on purpose.
            AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
                Err(harn_vm::jsonrpc::error_response(id, -32003, &reason))
            }
        }
    }

    fn prepare_stream_job(
        &self,
        request_id: JsonValue,
        params: JsonValue,
        request_profile: mcp_protocol::McpRequestProfile,
        session: SharedSession,
        connection: mcp_protocol::McpServerSession,
        auth: AuthRequest,
        task_access: harn_vm::mcp_tasks::McpTaskAccess,
    ) -> Result<(StreamJob, Option<JsonValue>), JsonValue> {
        let tool_name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
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
            .filter(harn_vm::mcp_progress::is_valid_progress_token);
        // `@job` is the declaration; the client capability is the request. Both
        // have to be present, so neither side can force the other into a
        // lifecycle it did not ask for.
        let task = (self
            .catalog
            .function(&tool_name)
            .is_some_and(|function| function.job.is_some())
            && mcp_protocol::client_supports_tasks(&params))
        .then(|| {
            self.tasks
                .begin(task_access, Some(harn_vm::mcp_tasks::DEFAULT_TASK_TTL_MS))
        })
        .transpose()
        .map_err(|error| {
            harn_vm::jsonrpc::error_response(request_id.clone(), -32000, &error.to_string())
        })?;
        let task_response = task.as_ref().map(|task| {
            harn_vm::mcp_tasks::task_created_response(
                request_id.clone(),
                task.task(),
                "The requested Harn job is running as an MCP task.",
            )
        });
        let request_key = request_key(&request_id);
        let job = StreamJob {
            request_id,
            request_key,
            tool_name,
            arguments,
            progress_token,
            request_profile,
            context: RequestContext {
                session,
                connection,
                auth,
            },
            task,
        };
        Ok((job, task_response))
    }

    async fn execute_streaming_job(
        &self,
        job: StreamJob,
        notify: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) {
        let cancel_token = job
            .task
            .as_ref()
            .map(harn_vm::mcp_tasks::McpTaskLease::cancel_token)
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
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
            job.context.connection.client_identity(),
            &job.tool_name,
            job.arguments,
            job.context.auth,
            cancel_token,
            progress_ctx,
            Some(mcp_request_id_to_string(&job.request_id)),
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

        let result = if let Some(task) = job.task.as_ref() {
            tokio::select! {
                biased;
                result = self.executor.call(request) => Some(result),
                () = task.cancelled() => None,
            }
        } else {
            Some(self.executor.call(request).await)
        };
        job.context.session.remove_call(&job.request_key);
        let Some(result) = result else {
            if let Some(task) = job.task {
                task.cancel();
            }
            return;
        };
        let execution_cancelled = matches!(&result, Err(DispatchError::Cancelled(_)));
        if cancelled.load(Ordering::SeqCst) {
            if let Some(task) = job.task {
                task.cancel();
            }
            return;
        }

        let response = match result {
            Ok(response) => harn_vm::jsonrpc::response(
                job.request_id,
                tool_call_success(response, &self.tool_catalog),
            ),
            Err(DispatchError::Validation(message)) => {
                harn_vm::jsonrpc::error_response(job.request_id, -32602, &message)
            }
            Err(DispatchError::Unauthorized(message)) => {
                harn_vm::jsonrpc::error_response(job.request_id, -32001, &message)
            }
            Err(DispatchError::Forbidden { required, granted }) => {
                forbidden_jsonrpc_error_response(job.request_id, &required, &granted)
            }
            // `@policy(kinds:)` is a `harn serve site` admission gate; the
            // MCP transport never raises it, but map it to the same
            // authorization error code defensively, using its tenant-safe
            // message.
            Err(error @ DispatchError::ForbiddenPrincipalKind { .. }) => {
                harn_vm::jsonrpc::error_response(job.request_id, -32001, &error.message())
            }
            Err(DispatchError::MissingExport(message)) => {
                harn_vm::jsonrpc::error_response(job.request_id, -32602, &message)
            }
            Err(DispatchError::Application(error)) => harn_vm::jsonrpc::response(
                job.request_id,
                harn_vm::tool_registry::application_error_mcp_result(&error),
            ),
            Err(DispatchError::Contract(error)) => {
                harn_vm::jsonrpc::response(job.request_id, tool_call_error(error.to_string()))
            }
            Err(DispatchError::Execution(message))
            | Err(DispatchError::Cancelled(message))
            | Err(DispatchError::Io(message))
            | Err(DispatchError::Cache(message)) => {
                harn_vm::jsonrpc::response(job.request_id, tool_call_error(message))
            }
            Err(error @ DispatchError::RateLimited { .. })
            | Err(error @ DispatchError::BudgetExceeded { .. })
            | Err(error @ DispatchError::SecretBackend(_)) => {
                harn_vm::jsonrpc::response(job.request_id, tool_call_error(error.message()))
            }
        };
        // A task call's client is polling `tasks/get`, not reading this stream,
        // so the terminal response is filed rather than written. Progress
        // notifications above still went to the wire: those are addressed by
        // `progressToken`, which is a property of the request and independent
        // of how the result comes back.
        if let Some(task) = job.task {
            if execution_cancelled {
                task.cancel();
                return;
            }
            match task_outcome(response) {
                Ok(result) => task
                    .complete_with_tool_result(result, job.request_profile.uses_result_envelope()),
                Err(error) => task.complete(Err(error), job.request_profile.uses_result_envelope()),
            }
            return;
        }
        let response = if job.request_profile.uses_result_envelope() {
            envelope(response, None)
        } else {
            response
        };
        notify(response);
    }

    fn tools_list_result(&self, params: &JsonValue) -> JsonValue {
        paged_result("tools", self.mcp_tools.clone(), params)
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

fn task_access(principal: &crate::AuthenticatedPrincipal) -> harn_vm::mcp_tasks::McpTaskAccess {
    if principal.is_anonymous() {
        return harn_vm::mcp_tasks::McpTaskAccess::unscoped();
    }
    harn_vm::mcp_tasks::McpTaskAccess::authenticated(
        principal.scheme.as_str(),
        principal.subject.as_str(),
        principal.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
    )
}

/// Render a JSON-RPC request id into a stable string for the obs
/// ambient `request_id`. Numbers and strings round-trip as-is; `null`
/// and any other unexpected shape fall back to a fresh `req_*` id so
/// every dispatch still carries one through to `harness.obs.*`.
fn mcp_request_id_to_string(id: &JsonValue) -> String {
    match id {
        JsonValue::String(text) => text.clone(),
        JsonValue::Number(number) => number.to_string(),
        _ => crate::http_codec::fresh_request_id(),
    }
}

fn envelope(mut response: JsonValue, cache: Option<&'static McpCacheHint>) -> JsonValue {
    if let Some(result) = response.get_mut("result") {
        apply_result_envelope(result, cache);
    }
    response
}

/// Map a JSON-RPC method to its conservative cache hint. Read/list
/// methods get a TTL; everything else is `None`, which still routes
/// through [`envelope`] so Stable clients see `resultType`.
fn cache_hint_for_method(method: &str) -> Option<&'static McpCacheHint> {
    const LIST: McpCacheHint = McpCacheHint::list_default();
    const READ: McpCacheHint = McpCacheHint::read_default();
    match method {
        "tools/list" | "resources/list" | "resources/templates/list" | "prompts/list" => {
            Some(&LIST)
        }
        "resources/read" => Some(&READ),
        _ => None,
    }
}

/// Standard JSON-RPC error response for a scope mismatch. The error
/// body carries the canonical `forbidden` payload so MCP clients can
/// render an actionable prompt without parsing the message string.
fn forbidden_jsonrpc_error_response(
    id: JsonValue,
    required: &std::collections::BTreeSet<String>,
    granted: &std::collections::BTreeSet<String>,
) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32003,
            "message": crate::forbidden_message(required, granted),
            "data": crate::forbidden_data_payload(required, granted),
        }
    })
}

/// Reduce a finished JSON-RPC response to the MCP result the task store files.
///
/// A JSON-RPC `error` means the request failed before it produced a tool
/// result, so the task itself fails. A `result` with `isError` is still a
/// completed tool result and is preserved intact by the task store.
fn task_outcome(response: JsonValue) -> Result<JsonValue, String> {
    if let Some(error) = response.get("error") {
        return Err(error
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("Tool execution failed")
            .to_string());
    }
    Ok(response.get("result").cloned().unwrap_or(json!({})))
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
            | mcp_protocol::METHOD_TASKS_GET
            | mcp_protocol::METHOD_TASKS_UPDATE
            | mcp_protocol::METHOD_TASKS_CANCEL
    )
}

#[async_trait::async_trait(?Send)]
impl TransportAdapter for McpServer {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
}
