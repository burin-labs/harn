//! Model Context Protocol (MCP) server.
//!
//! This is Harn's one MCP protocol server. It serves either the `pub fn`s a
//! module exports or the tools, resources, and prompts a script publishes
//! (see `source`); sessions, authorization, task ownership, result envelopes,
//! and the stdio and Streamable HTTP transports are the same for both.
//!
//! Module map: `source` owns what is served, `script` hosts a script registry
//! and its VM, `transport` owns HTTP/SSE routing, `schema` owns JSON-RPC
//! result shaping and call normalization, and `auth` owns request metadata
//! plus transport header validation.
//!
//! The method match below is hand-written rather than delegated to the
//! official SDK's server side. That SDK is already the client and owns the
//! wire types this server negotiates with, but its server handler does not
//! carry the stable per-request `_meta` profile, the tasks extension, the
//! result envelope, or `harn.hitl.respond`, so routing through it would add
//! adapters without removing any of this logic.

mod auth;
mod schema;
mod script;
mod source;
#[cfg(test)]
mod tests;
mod transport;

/// Streamable HTTP helpers shared by every MCP HTTP host.
pub mod streamable_http {
    pub use super::auth::{
        accepts_media, attach_http_headers, negotiate_routing_headers, should_stream_post_response,
        validate_origin,
    };
    pub use super::transport::sse_single_response;
}

use crate::transport::{
    read_jsonrpc_stdio_frame, write_jsonrpc_stdio_message, JsonRpcStdioFrameStyle,
};
use schema::{
    build_call_request, derived_server_name, parse_error_response, request_key, tool_call_error,
    tool_call_success,
};
pub use script::{McpScriptReloader, McpScriptRuntime};
use source::{ExportsSource, McpSource, ToolInvocation};
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
    self, negotiate_http_request, server_discover_result, standard_name_header_value,
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

const SERVER_CARD_URI: &str = "well-known://mcp-card";

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
    source: McpSource,
    pub server_name: Option<String>,
    pub server_card: Option<JsonValue>,
    pub auth_policy: AuthPolicy,
}

impl McpServerConfig {
    /// Serve the `pub fn`s `core` dispatches, under the core's auth policy.
    pub fn new(core: DispatchCore) -> Self {
        Self {
            server_name: Some(derived_server_name(core.catalog())),
            server_card: None,
            auth_policy: core.auth_policy().clone(),
            source: McpSource::Exports(Box::new(ExportsSource::new(core))),
        }
    }

    /// Serve a script-published registry. It names itself, so
    /// `server_name` stays unset.
    pub fn script(runtime: McpScriptRuntime) -> Self {
        Self {
            server_name: None,
            server_card: None,
            auth_policy: AuthPolicy::default(),
            source: McpSource::Script(runtime),
        }
    }

    pub fn with_server_card(mut self, card: JsonValue) -> Self {
        self.server_card = Some(card);
        self
    }

    pub fn with_auth_policy(mut self, auth_policy: AuthPolicy) -> Self {
        self.auth_policy = auth_policy;
        self
    }
}

pub type McpStdioServer = McpServer;

pub struct McpServer {
    descriptor: AdapterDescriptor,
    server_name: String,
    server_card: Option<JsonValue>,
    auth_policy: AuthPolicy,
    source: McpSource,
    /// Tasks this server handed out, owned by the principal that created
    /// each one. Same lifecycle as the orchestrator server
    /// (`harn_vm::mcp_tasks`).
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
    params: JsonValue,
    progress_token: Option<JsonValue>,
    request_profile: mcp_protocol::McpRequestProfile,
    context: RequestContext,
    /// Set when the client asked for a task. This lease owns both terminal
    /// completion and the exact cancellation token installed in dispatch.
    task: Option<harn_vm::mcp_tasks::McpTaskLease>,
}

impl McpServer {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            tasks: Arc::new(harn_vm::mcp_tasks::McpTaskStore::new()),
            descriptor: AdapterDescriptor {
                id: "mcp".to_string(),
                caller_shape: "tool".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            server_name: config.server_name.unwrap_or_else(|| "harn".to_string()),
            server_card: config.server_card,
            auth_policy: config.auth_policy,
            source: config.source,
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
        let list_changes = self.spawn_list_change_notifier(session.clone(), tx.clone());

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
            // Stdio is request/response. A stray response has no
            // server-initiated request to match.
            if request.get("method").is_none() {
                continue;
            }
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

        if let Some(list_changes) = list_changes {
            list_changes.abort();
        }
        drop(tx);
        writer
            .await
            .map_err(|error| format!("stdio writer task failed: {error}"))?
    }

    /// Tell an initialized stdio client when a reloaded script changed what
    /// is listed.
    fn spawn_list_change_notifier(
        &self,
        session: SharedSession,
        tx: mpsc::UnboundedSender<JsonValue>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let McpSource::Script(script) = &self.source else {
            return None;
        };
        if !script.list_changes() {
            return None;
        }
        let mut changes = script.subscribe_changes();
        Some(tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                if !session.with_connection(|connection| connection.is_ready_for_notifications()) {
                    continue;
                }
                for method in [
                    "notifications/tools/list_changed",
                    "notifications/resources/list_changed",
                    "notifications/prompts/list_changed",
                ] {
                    if tx
                        .send(harn_vm::jsonrpc::notification(method, json!({})))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }))
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

    /// Answer one in-process JSON-RPC message for a host that embeds this
    /// server, such as a local app shell. The request is authorized as an
    /// in-process caller; a `tools/call` runs to completion and its terminal
    /// response is returned. Notifications return `None`.
    ///
    /// Boxed so an embedding host's own handler frame stays small; the
    /// request future carries the whole dispatch path.
    pub fn handle_request(
        self: Arc<Self>,
        request: JsonValue,
    ) -> futures::future::BoxFuture<'static, Option<JsonValue>> {
        Box::pin(async move {
            let auth = AuthRequest {
                method: "IN_PROCESS".to_string(),
                ..AuthRequest::default()
            };
            match self
                .process_message(request, SharedSession::new(), auth)
                .await
            {
                ImmediateResult::Response(response) => Some(response),
                ImmediateResult::Accepted => None,
                ImmediateResult::Stream(job) => self.run_job_to_response(*job).await,
                ImmediateResult::TaskStream { immediate, job } => {
                    tokio::spawn(async move { self.execute_streaming_job(*job, discard()).await });
                    Some(immediate)
                }
            }
        })
    }

    /// Run a streaming job and keep only its terminal response.
    async fn run_job_to_response(&self, job: StreamJob) -> Option<JsonValue> {
        let request_id = job.request_id.clone();
        let terminal = Arc::new(Mutex::new(None));
        let sink = terminal.clone();
        self.execute_streaming_job(
            job,
            notify_channel(move |message| {
                if message.get("method").is_none() && message.get("id") == Some(&request_id) {
                    *sink.lock().expect("terminal response poisoned") = Some(message);
                }
            }),
        )
        .await;
        let response = terminal.lock().expect("terminal response poisoned").take();
        response
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
                    self.source.identity().instructions.as_deref(),
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
            match method {
                "notifications/initialized" => session
                    .with_connection(|connection| connection.accept_initialized_notification()),
                "notifications/cancelled" => self.handle_cancel_notification(&session, &params),
                _ => {}
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
            "harn.hitl.respond" => handle_hitl_respond(id, &params).await,
            "tools/list" => mcp_protocol::list_response(
                id,
                method,
                "tools",
                &params,
                self.source.tool_entries(),
            ),
            "tools/call" => match self
                .prepare_stream_job(
                    id,
                    params,
                    request_profile,
                    session,
                    connection,
                    auth,
                    task_access,
                )
                .await
            {
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
            "resources/list" => mcp_protocol::list_response(
                id,
                method,
                "resources",
                &params,
                self.resource_entries(),
            ),
            "resources/read" => self.handle_resources_read(id, &params).await,
            "resources/templates/list" => mcp_protocol::list_response(
                id,
                method,
                "resourceTemplates",
                &params,
                self.source.resource_template_entries(),
            ),
            "prompts/list" => mcp_protocol::list_response(
                id,
                method,
                "prompts",
                &params,
                self.source.prompt_entries(),
            ),
            "prompts/get" => self.source.get_prompt(id, &params).await,
            mcp_protocol::METHOD_COMPLETION_COMPLETE => self.source.complete(id, &params).await,
            _ => {
                harn_vm::jsonrpc::error_response(id, -32601, &format!("Method not found: {method}"))
            }
        };
        if request_profile.uses_result_envelope() {
            ImmediateResult::Response(mcp_protocol::apply_response_envelope(response, method))
        } else {
            ImmediateResult::Response(response)
        }
    }

    fn handle_server_discover(&self, id: JsonValue) -> JsonValue {
        let result = server_discover_result(
            JsonValue::Object(self.server_capabilities()),
            self.server_info(),
            self.source.identity().instructions.as_deref(),
        );
        harn_vm::jsonrpc::response(id, result)
    }

    fn server_capabilities(&self) -> serde_json::Map<String, JsonValue> {
        let source = self.source.capabilities();
        let family = if source.list_changed {
            json!({"listChanged": true})
        } else {
            json!({})
        };
        let mut capabilities = serde_json::Map::new();
        if source.tools || source.list_changed {
            capabilities.insert("tools".to_string(), family.clone());
        }
        if source.resources || self.server_card.is_some() || source.list_changed {
            capabilities.insert("resources".to_string(), family.clone());
        }
        if source.prompts || source.list_changed {
            capabilities.insert("prompts".to_string(), family);
        }
        if source.resources || source.prompts {
            capabilities.insert(
                "completions".to_string(),
                mcp_protocol::completions_capability(),
            );
        }
        // Tasks are advertised only when some tool may run as one. A server
        // with no task-capable tool has nothing to serve as a task, and telling
        // a client otherwise would leave it unable to distinguish "no tasks
        // here" from "your task is gone".
        let mut extensions = serde_json::Map::new();
        if source.tasks {
            if let JsonValue::Object(tasks) = mcp_protocol::tasks_capability() {
                extensions.extend(tasks);
            }
        }
        if source.mcp_apps {
            extensions.insert(
                "io.modelcontextprotocol/ui".to_string(),
                json!({"mimeTypes": ["text/html;profile=mcp-app"]}),
            );
        }
        if !extensions.is_empty() {
            capabilities.insert("extensions".to_string(), JsonValue::Object(extensions));
        }
        capabilities
    }

    fn server_info(&self) -> JsonValue {
        let identity = self.source.identity();
        let mut server_info = json!({
            "name": identity.name.as_deref().unwrap_or(&self.server_name),
            "version": identity.version,
        });
        if let Some(title) = identity
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
        if let Some(icons) = identity.icons {
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

    async fn prepare_stream_job(
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
        let Some(task_support) = self.source.task_support(&tool_name) else {
            return Err(harn_vm::jsonrpc::error_response(
                request_id,
                -32602,
                &format!("Unknown tool: {tool_name}"),
            ));
        };
        // The tool's declaration and the client's capability both have to
        // allow a task, so neither side can force the other into a lifecycle
        // it did not ask for. A tool that requires one refuses a plain call
        // outright, so a client cannot get the work done by not asking.
        let wants_task = mcp_protocol::client_supports_tasks(&params);
        if task_support == harn_vm::mcp_tasks::McpTaskSupport::Required && !wants_task {
            return Err(mcp_protocol::missing_tasks_capability_response(request_id));
        }
        let as_task = task_support.allows_task() && wants_task;
        if as_task {
            if let Some(refusal) = self.source.admit_tool(&request_id, &params).await {
                return Err(refusal);
            }
        }
        let task = as_task
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
                "The requested Harn tool is running as an MCP task.",
            )
        });
        let progress_token = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(harn_vm::mcp_progress::is_valid_progress_token);
        let request_key = request_key(&request_id);
        let job = StreamJob {
            request_id,
            request_key,
            tool_name,
            params,
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

        let progress = job.progress_token.clone().map(|token| {
            let bus = harn_vm::mcp_progress::ProgressBus::new(notify.clone());
            harn_vm::mcp_progress::ProgressContext::new(bus, token)
        });
        let invocation = ToolInvocation {
            request_id: job.request_id,
            tool_name: job.tool_name,
            params: job.params,
            client_identity: job.context.connection.client_identity().to_string(),
            auth: job.context.auth,
            cancel_token,
            progress,
            as_task: job.task.is_some(),
        };

        let outcome = if let Some(task) = job.task.as_ref() {
            run_until_task_cancelled(task, self.source.call_tool(invocation)).await
        } else {
            Some(self.source.call_tool(invocation).await)
        };
        job.context.session.remove_call(&job.request_key);
        let Some(outcome) = outcome else {
            if let Some(task) = job.task {
                task.cancel();
            }
            return;
        };
        if cancelled.load(Ordering::SeqCst) {
            if let Some(task) = job.task {
                task.cancel();
            }
            return;
        }

        // A task call's client is polling `tasks/get`, not reading this stream,
        // so the terminal response is filed rather than written. Progress
        // notifications above still went to the wire: those are addressed by
        // `progressToken`, which is a property of the request and independent
        // of how the result comes back.
        if let Some(task) = job.task {
            if outcome.cancelled {
                task.cancel();
                return;
            }
            match task_outcome(outcome.response) {
                Ok(result) => task
                    .complete_with_tool_result(result, job.request_profile.uses_result_envelope()),
                Err(error) => task.complete(Err(error), job.request_profile.uses_result_envelope()),
            }
            return;
        }
        let response = if job.request_profile.uses_result_envelope() {
            mcp_protocol::apply_response_envelope(outcome.response, "tools/call")
        } else {
            outcome.response
        };
        notify(response);
    }

    fn resource_entries(&self) -> Vec<JsonValue> {
        let mut resources = Vec::new();
        if self.server_card.is_some() {
            resources.push(json!({
                "uri": SERVER_CARD_URI,
                "name": "Server Card",
                "description": "MCP Server Card advertising this server's identity and capabilities",
                "mimeType": "application/json",
            }));
        }
        resources.extend(self.source.resource_entries());
        resources
    }

    async fn handle_resources_read(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if uri == SERVER_CARD_URI {
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
        self.source.read_resource(id, params).await
    }
}

/// Record a host's answer to a pending human-in-the-loop request in the
/// working directory's event log, where the waiting handler reads it.
///
/// Boxed so the event log's backend futures do not deepen the type of every
/// request future; crates that embed the HTTP router would otherwise overflow
/// the compiler's recursion limit proving that future `Send`.
fn handle_hitl_respond(
    id: JsonValue,
    params: &JsonValue,
) -> futures::future::BoxFuture<'static, JsonValue> {
    let response = serde_json::from_value::<harn_vm::HitlHostResponse>(params.clone());
    Box::pin(async move {
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!("invalid harn.hitl.respond params: {error}"),
                )
            }
        };
        let cwd = std::env::current_dir().ok();
        match harn_vm::append_hitl_response(cwd.as_deref(), response).await {
            Ok(_) => harn_vm::jsonrpc::response(id, json!({ "ok": true })),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32000, &error),
        }
    })
}

fn discard() -> Arc<dyn Fn(JsonValue) + Send + Sync> {
    notify_channel(|_| {})
}

/// Await execution while giving an already-observed task cancellation the
/// deterministic first claim when both futures become ready together.
async fn run_until_task_cancelled<T>(
    task: &harn_vm::mcp_tasks::McpTaskLease,
    execution: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = task.cancelled() => None,
        result = execution => Some(result),
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

/// Every method that reads or acts on what the server serves requires an
/// authorized caller when the server has an auth policy. Only liveness and
/// discovery stay public, so a method added later is protected by default.
fn requires_protocol_auth(method: &str) -> bool {
    !matches!(method, "ping" | mcp_protocol::METHOD_SERVER_DISCOVER)
}

#[async_trait::async_trait(?Send)]
impl TransportAdapter for McpServer {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
}
