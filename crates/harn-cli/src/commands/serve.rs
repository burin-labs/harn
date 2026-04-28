use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{stream, StreamExt};
use harn_serve::{
    A2aHttpServeOptions, A2aServer, A2aServerConfig, ApiKeyAuthConfig, AuthMethodConfig,
    AuthPolicy, AuthRequest, AuthorizationDecision, DispatchCore, DispatchCoreConfig,
    ExportCatalog, ExportedCallableKind, HmacAuthConfig, HttpTlsConfig, McpHttpServeOptions,
    McpServer, McpServerConfig, MCP_PROTOCOL_VERSION,
};
use serde_json::Value as JsonValue;
use time::Duration;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use uuid::Uuid;

use crate::cli::{A2aServeArgs, McpServeTransport, ServeAcpArgs, ServeMcpArgs, ServeTlsMode};

pub(crate) async fn run_acp_server(args: &ServeAcpArgs) -> Result<(), String> {
    crate::acp::run_acp_server(Some(&args.file)).await;
    Ok(())
}

pub(crate) async fn run_a2a_server(args: &A2aServeArgs) -> Result<(), String> {
    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    let mut server_config = A2aServerConfig::new(core);
    server_config.card_signing_secret = args.card_signing_secret.clone();
    let server = Arc::new(A2aServer::new(server_config));
    server
        .run_http(A2aHttpServeOptions {
            bind: SocketAddr::from(([0, 0, 0, 0], args.port)),
            public_url: args.public_url.clone(),
            tls: build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?,
        })
        .await
}

pub(crate) async fn run_mcp_server(args: &ServeMcpArgs) -> Result<(), String> {
    if args.transport == McpServeTransport::Stdio
        && (!args.api_key.is_empty() || args.hmac_secret.is_some())
    {
        return Err("HTTP auth flags require `harn serve mcp --transport http`".to_string());
    }

    // Scripts that author the MCP surface explicitly through
    // `mcp_tools(registry)` / `mcp_resource(...)` / `mcp_prompt(...)`
    // typically don't expose any `pub fn` entrypoints. Dispatch those to
    // the script-driven runner that runs the script once,
    // collects the registered tools/resources/prompts, and serves them
    // over the requested transport. The DispatchCore-based adapter only knows how to
    // route incoming MCP calls to `pub fn` exports.
    let catalog = ExportCatalog::from_path(Path::new(&args.file))
        .map_err(|error| format!("failed to load script: {error}"))?;
    let has_pub_fn_exports = catalog
        .functions
        .values()
        .any(|function| function.kind == ExportedCallableKind::Function);

    if !has_pub_fn_exports {
        let mode = match args.transport {
            McpServeTransport::Stdio => crate::commands::run::RunFileMcpServeMode::Stdio,
            McpServeTransport::Http => crate::commands::run::RunFileMcpServeMode::Http {
                options: McpHttpServeOptions {
                    bind: args.bind,
                    path: args.path.clone(),
                    sse_path: args.sse_path.clone(),
                    messages_path: args.messages_path.clone(),
                    tls: build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?,
                },
                auth_policy: build_auth_policy(&args.api_key, args.hmac_secret.as_ref()),
            },
        };
        crate::commands::run::run_file_mcp_serve(&args.file, args.card.as_deref(), mode).await;
        return Ok(());
    }

    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    let mut server_config = McpServerConfig::new(core);
    if let Some(source) = args.card.as_deref() {
        server_config =
            server_config.with_server_card(crate::commands::run::resolve_card_source(source)?);
    }
    let server = Arc::new(McpServer::new(server_config));

    match args.transport {
        McpServeTransport::Stdio => server.run_stdio().await,
        McpServeTransport::Http => {
            server
                .run_http(McpHttpServeOptions {
                    bind: args.bind,
                    path: args.path.clone(),
                    sse_path: args.sse_path.clone(),
                    messages_path: args.messages_path.clone(),
                    tls: build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?,
                })
                .await
        }
    }
}

pub(crate) async fn run_script_mcp_http_server(
    server: harn_vm::McpServer,
    vm: harn_vm::Vm,
    options: McpHttpServeOptions,
    auth_policy: AuthPolicy,
) -> Result<(), String> {
    let state = ScriptMcpHttpState {
        runtime: ScriptMcpRuntime::start(server, vm),
        options: options.clone(),
        auth_policy,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let router = Router::new()
        .route(
            &options.path,
            post(script_http_post_request)
                .get(script_http_get_stream)
                .delete(script_http_delete_session),
        )
        .route(
            &options.sse_path,
            get(script_legacy_sse_stream).post(script_legacy_sse_message),
        )
        .route(&options.messages_path, post(script_legacy_sse_message))
        .with_state(state);
    let router = harn_serve::tls::apply_security_headers(router, &options.tls);
    let listener = harn_serve::tls::bind_listener(options.bind)?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to read local addr: {error}"))?;
    eprintln!(
        "[harn] MCP workflow server ready on {}://{local_addr}{}",
        options.tls.listener_scheme(),
        options.path
    );
    harn_serve::tls::serve_router_from_tcp(listener, router, &options.tls)
        .await
        .map_err(|error| format!("MCP HTTP server failed: {error}"))
}

#[derive(Clone)]
struct ScriptMcpHttpState {
    runtime: ScriptMcpRuntime,
    options: McpHttpServeOptions,
    auth_policy: AuthPolicy,
    sessions: Arc<Mutex<HashMap<String, SharedScriptSession>>>,
}

#[derive(Clone)]
struct ScriptMcpRuntime {
    tx: tokio_mpsc::UnboundedSender<ScriptMcpJob>,
}

struct ScriptMcpJob {
    request: JsonValue,
    response_tx: oneshot::Sender<Option<JsonValue>>,
}

#[derive(Default)]
struct ScriptSessionState {
    stream_tx: Option<UnboundedSender<JsonValue>>,
}

#[derive(Clone)]
struct SharedScriptSession {
    inner: Arc<Mutex<ScriptSessionState>>,
}

impl SharedScriptSession {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScriptSessionState::default())),
        }
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

impl ScriptMcpRuntime {
    fn start(server: harn_vm::McpServer, mut vm: harn_vm::Vm) -> Self {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel::<ScriptMcpJob>();
        tokio::task::spawn_local(async move {
            while let Some(job) = rx.recv().await {
                let response = server.handle_json_rpc(job.request, &mut vm).await;
                let _ = job.response_tx.send(response);
            }
        });
        Self { tx }
    }

    async fn call(&self, request: JsonValue) -> Result<Option<JsonValue>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ScriptMcpJob {
                request,
                response_tx,
            })
            .map_err(|_| "script MCP runtime is not running".to_string())?;
        response_rx
            .await
            .map_err(|_| "script MCP runtime dropped response".to_string())
    }
}

async fn script_http_post_request(
    State(state): State<ScriptMcpHttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_script_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_script_protocol_header(&headers) {
        return *response;
    }

    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(script_parse_error_response(&error.to_string())),
            )
                .into_response()
        }
    };
    let header_session = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (session_id, _session, created) =
        match script_lookup_or_create_session(&state, &request, header_session) {
            Ok(value) => value,
            Err(response) => return *response,
        };
    if let Err(response) = authorize_script_rpc(
        &state,
        &request,
        script_http_auth_request(method, &state.options.path, body.to_vec(), &headers),
    )
    .await
    {
        let mut http = Json(response).into_response();
        attach_script_http_headers(&mut http, created.then_some(session_id.as_str()));
        return http;
    }

    match state.runtime.call(request).await {
        Ok(Some(response)) => {
            let mut http = Json(response).into_response();
            attach_script_http_headers(&mut http, created.then_some(session_id.as_str()));
            http
        }
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            let mut http = Json(harn_vm::jsonrpc::error_response(
                JsonValue::Null,
                -32000,
                &error,
            ))
            .into_response();
            attach_script_http_headers(&mut http, created.then_some(session_id.as_str()));
            http
        }
    }
}

async fn script_http_get_stream(
    State(state): State<ScriptMcpHttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = validate_script_origin(&headers) {
        return *response;
    }
    let Some(session_id) = headers
        .get("mcp-session-id")
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
    script_sse_response(rx).into_response()
}

async fn script_http_delete_session(
    State(state): State<ScriptMcpHttpState>,
    headers: HeaderMap,
) -> Response {
    let Some(session_id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let removed = state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .remove(session_id);
    if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn script_legacy_sse_stream(
    State(state): State<ScriptMcpHttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = validate_script_origin(&headers) {
        return *response;
    }
    let session_id = Uuid::now_v7().to_string();
    let session = SharedScriptSession::new();
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
    let stream = stream::once(async move { Ok::<Event, Infallible>(endpoint_event) })
        .chain(script_sse_events(rx));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn script_legacy_sse_message(
    State(state): State<ScriptMcpHttpState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_script_origin(&headers) {
        return *response;
    }
    let Some(session_id) = query.get("session_id") else {
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
    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(script_parse_error_response(&error.to_string())),
            )
                .into_response()
        }
    };
    if let Err(response) = authorize_script_rpc(
        &state,
        &request,
        script_http_auth_request(
            Method::POST,
            &state.options.messages_path,
            body.to_vec(),
            &headers,
        ),
    )
    .await
    {
        if let Some(tx) = session.stream_tx() {
            let _ = tx.unbounded_send(response);
            return StatusCode::ACCEPTED.into_response();
        }
        return StatusCode::GONE.into_response();
    }
    match state.runtime.call(request).await {
        Ok(Some(response)) => {
            if let Some(tx) = session.stream_tx() {
                let _ = tx.unbounded_send(response);
                StatusCode::ACCEPTED.into_response()
            } else {
                StatusCode::GONE.into_response()
            }
        }
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            if let Some(tx) = session.stream_tx() {
                let _ = tx.unbounded_send(harn_vm::jsonrpc::error_response(
                    JsonValue::Null,
                    -32000,
                    &error,
                ));
                StatusCode::ACCEPTED.into_response()
            } else {
                StatusCode::GONE.into_response()
            }
        }
    }
}

fn script_sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(prime) }).chain(script_sse_events(rx));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn script_sse_events(
    rx: UnboundedReceiver<JsonValue>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    rx.map(|message| {
        Ok(Event::default()
            .id(Uuid::now_v7().to_string())
            .event("message")
            .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
    })
}

async fn authorize_script_rpc(
    state: &ScriptMcpHttpState,
    request: &JsonValue,
    auth: AuthRequest,
) -> Result<(), JsonValue> {
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    if !script_method_requires_auth(method) {
        return Ok(());
    }
    match state.auth_policy.authorize(&auth).await {
        AuthorizationDecision::Authorized(_) => Ok(()),
        AuthorizationDecision::Rejected(message) => Err(harn_vm::jsonrpc::error_response(
            request.get("id").cloned().unwrap_or(JsonValue::Null),
            -32001,
            &message,
        )),
    }
}

fn script_method_requires_auth(method: &str) -> bool {
    matches!(method, "tools/call" | "resources/read" | "prompts/get")
}

fn script_http_auth_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    headers: &HeaderMap,
) -> AuthRequest {
    AuthRequest {
        method: method.as_str().to_string(),
        path: path.to_string(),
        body,
        headers: script_normalized_headers(headers),
        validated_oauth: None,
    }
}

fn script_normalized_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
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

fn script_lookup_or_create_session(
    state: &ScriptMcpHttpState,
    request: &JsonValue,
    header_session: Option<String>,
) -> Result<(String, SharedScriptSession, bool), Box<Response>> {
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
    let session = SharedScriptSession::new();
    sessions.insert(session_id.clone(), session.clone());
    Ok((session_id, session, true))
}

fn attach_script_http_headers(response: &mut Response, session_id: Option<&str>) {
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response
                .headers_mut()
                .insert(HeaderName::from_static("mcp-session-id"), value);
        }
    }
    response.headers_mut().insert(
        HeaderName::from_static("mcp-protocol-version"),
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
}

fn validate_script_protocol_header(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(value) = headers
        .get("mcp-protocol-version")
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

fn validate_script_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
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

fn script_parse_error_response(message: &str) -> JsonValue {
    harn_vm::jsonrpc::error_response(JsonValue::Null, -32700, &format!("Parse error: {message}"))
}

fn build_tls_config(
    mode: ServeTlsMode,
    cert: Option<&std::path::PathBuf>,
    key: Option<&std::path::PathBuf>,
) -> Result<HttpTlsConfig, String> {
    match (mode, cert, key) {
        (ServeTlsMode::Plain, None, None) => Ok(HttpTlsConfig::plain()),
        (ServeTlsMode::Plain, Some(cert), Some(key))
        | (ServeTlsMode::Pem, Some(cert), Some(key)) => {
            Ok(HttpTlsConfig::pem_files(cert.clone(), key.clone()))
        }
        (ServeTlsMode::Pem, None, None) => {
            Err("`--tls pem` requires `--cert` and `--key`".to_string())
        }
        (_, Some(_), None) => Err("`--cert` requires `--key`".to_string()),
        (_, None, Some(_)) => Err("`--key` requires `--cert`".to_string()),
        (ServeTlsMode::Edge, None, None) => Ok(HttpTlsConfig::edge_terminated()),
        (ServeTlsMode::SelfSignedDev, None, None) => Ok(HttpTlsConfig::self_signed_dev()),
        (ServeTlsMode::Edge | ServeTlsMode::SelfSignedDev, Some(_), Some(_)) => Err(
            "`--cert` and `--key` are only valid with `--tls pem` or default TLS mode".to_string(),
        ),
    }
}

fn build_auth_policy(api_keys: &[String], hmac_secret: Option<&String>) -> AuthPolicy {
    let mut methods = Vec::new();
    if !api_keys.is_empty() {
        methods.push(AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: api_keys.iter().cloned().collect::<BTreeSet<_>>(),
        }));
    }
    if let Some(secret) = hmac_secret {
        methods.push(AuthMethodConfig::Hmac(HmacAuthConfig {
            shared_secret: secret.clone(),
            provider: "harn-serve".to_string(),
            timestamp_window: Duration::seconds(300),
        }));
    }
    AuthPolicy { methods }
}
