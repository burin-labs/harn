use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{stream, StreamExt};
use harn_serve::{
    A2aHttpServeOptions, A2aServer, A2aServerConfig, AcpProfileConfig, AcpWebSocketServeOptions,
    ApiHttpServeOptions, ApiKeyAuthConfig, ApiKeyEntry, ApiServer, ApiServerConfig,
    AuthMethodConfig, AuthPolicy, AuthRequest, AuthorizationDecision, DispatchCore,
    DispatchCoreConfig, ExportCatalog, ExportedCallableKind, HmacAuthConfig, HttpTlsConfig,
    McpHttpServeOptions, McpServer, McpServerConfig, SiteHttpServeOptions, SiteServer,
    SiteServerConfig, MCP_PROTOCOL_VERSION,
};
use serde_json::Value as JsonValue;
use time::Duration;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use uuid::Uuid;

use crate::cli::{
    A2aServeArgs, AcpServeTransport, ApiServeArgs, McpServeTransport, ServeAcpArgs, ServeCommand,
    ServeMcpArgs, ServeObsMode, ServeTlsMode, SiteServeArgs, WorkerServeArgs,
};

/// Default 10 MiB request-body cap applied to every `harn serve` HTTP
/// router. Mirrors `DEFAULT_MAX_BODY_BYTES` in the orchestrator
/// listener so large/runaway POSTs cannot exhaust process memory while
/// axum buffers a request.
pub(crate) const SERVE_DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// End a script-backed MCP server before its transport starts.
///
/// MCP reserves stdout for protocol messages, so pipeline output belongs on
/// stderr. An explicit Harn `exit(code)` remains terminal control flow rather
/// than a runtime error and must preserve its requested status.
pub(crate) fn exit_after_mcp_pipeline_error(vm: &harn_vm::Vm, error: &harn_vm::VmError) -> ! {
    let output = vm.output();
    if !output.is_empty() {
        eprint!("{output}");
    }
    if let Some(code) = error.process_exit_code() {
        std::process::exit(code);
    }
    eprint!("{}", vm.format_runtime_error(error));
    std::process::exit(1);
}

pub(crate) async fn run_command(command: ServeCommand) {
    match command {
        ServeCommand::Acp(args) => {
            if let Err(error) = run_acp_server(&args).await {
                crate::command_error(&error);
            }
        }
        ServeCommand::A2a(args) => {
            if let Err(error) = run_a2a_server(&args).await {
                crate::command_error(&error);
            }
        }
        ServeCommand::Api(args) => {
            if let Err(error) = run_api_server(&args).await {
                crate::command_error(&error);
            }
        }
        ServeCommand::Mcp(args) => {
            if let Err(error) = run_mcp_server(&args).await {
                crate::command_error(&error);
            }
        }
        ServeCommand::Site(args) => {
            if let Err(error) = run_site_server(&args).await {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        ServeCommand::Worker(args) => {
            if let Err(error) = run_worker_server(&args).await {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        ServeCommand::Test => {
            if let Err(error) = crate::commands::test_worker::serve_stdio().await {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
}

/// Install the observability backend chosen by `harn serve --obs
/// <MODE>` before any handler runs. `Auto` defers to environment
/// detection (existing behaviour); the rest pin a single backend so the
/// operator gets predictable routing.
fn apply_obs_mode(mode: ServeObsMode) -> Result<(), String> {
    let backend = match mode {
        ServeObsMode::Auto => return Ok(()),
        ServeObsMode::Stdout => "pretty_stdout",
        ServeObsMode::Stderr => "pretty_stderr",
        ServeObsMode::Otel => "otel",
        ServeObsMode::Off => "test",
    };
    harn_vm::install_obs_default_backend(backend)
        .map_err(|error| format!("--obs {backend}: {error}"))
}

fn validate_obs_transport(
    mode: ServeObsMode,
    stdout_is_protocol: bool,
    surface: &str,
) -> Result<(), String> {
    if stdout_is_protocol && mode == ServeObsMode::Stdout {
        return Err(format!(
            "`harn serve {surface} --transport stdio` cannot use `--obs stdout`: stdout is \
             reserved for JSON-RPC protocol frames; use `--obs stderr`, `--obs otel`, or \
             `--obs off`"
        ));
    }
    Ok(())
}

/// Refuse to start an unauthenticated HTTP serve adapter on a
/// non-loopback bind. When the bind is loopback (`127.0.0.0/8`, `::1`)
/// the call returns `Ok(())` after emitting a WARN log; when the bind
/// exposes a non-loopback interface and there is no auth/TLS, the call
/// returns `Err(...)` so the operator gets a clear failure instead of a
/// silently public surface.
fn guard_serve_bind_auth(
    surface: &str,
    bind: SocketAddr,
    auth_policy: &AuthPolicy,
    tls: &harn_serve::HttpTlsConfig,
) -> Result<(), String> {
    let auth_configured = !auth_policy.methods.is_empty();
    let tls_configured = !matches!(tls, harn_serve::HttpTlsConfig::Plain);
    let is_loopback = bind.ip().is_loopback();
    if !is_loopback && !auth_configured && !tls_configured {
        return Err(format!(
            "refusing to start `harn serve {surface}` on non-loopback bind {bind} without auth \
             (--api-key/--hmac-secret) or TLS (--tls). To listen on a public interface, \
             configure auth or TLS; to keep the surface unauthenticated, bind to 127.0.0.1."
        ));
    }
    if is_loopback && !auth_configured && !tls_configured {
        tracing::warn!(
            target: "harn::serve",
            surface = surface,
            bind = %bind,
            "starting `harn serve {surface}` on loopback {bind} with no auth and no TLS; \
             do not expose this socket beyond localhost"
        );
    }
    Ok(())
}

pub(crate) async fn run_acp_server(args: &ServeAcpArgs) -> Result<(), String> {
    validate_obs_transport(args.obs, args.transport == AcpServeTransport::Stdio, "acp")?;
    apply_obs_mode(args.obs)?;
    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let profile = harn_serve::AcpProfileConfig {
        text: args.profile.text,
        json_path: args.profile.json_path.clone(),
    };
    match args.transport {
        AcpServeTransport::Stdio => {
            crate::acp::run_acp_server(args.file.as_deref(), auth_policy, args.trace, profile)
                .await;
            Ok(())
        }
        AcpServeTransport::Websocket => {
            let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
            guard_serve_bind_auth("acp", args.bind, &auth_policy, &tls)?;
            if args.trace {
                harn_vm::llm::enable_tracing();
            }
            crate::acp::ensure_acp_event_log(args.file.as_deref());
            let result = harn_serve::run_acp_websocket_server(
                crate::acp::server_config(args.file.clone(), auth_policy).with_profile(profile),
                AcpWebSocketServeOptions {
                    bind: args.bind,
                    path: args.path.clone(),
                    tls,
                },
            )
            .await;
            if args.trace {
                eprint!("{}", crate::commands::run::render_trace_summary());
            }
            result
        }
    }
}

pub(crate) async fn run_a2a_server(args: &A2aServeArgs) -> Result<(), String> {
    apply_obs_mode(args.obs)?;
    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
    let bind = args
        .bind
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], args.port)));
    guard_serve_bind_auth("a2a", bind, &auth_policy, &tls)?;

    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = auth_policy;
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    harn_serve::emit_export_diagnostics(core.catalog().diagnostics());
    let mut server_config = A2aServerConfig::new(core);
    server_config.card_signing_secret = args.card_signing_secret.clone();
    let server = Arc::new(A2aServer::new(server_config));
    server
        .run_http(A2aHttpServeOptions {
            bind,
            public_url: args.public_url.clone(),
            tls,
        })
        .await
}

pub(crate) async fn run_api_server(args: &ApiServeArgs) -> Result<(), String> {
    apply_obs_mode(args.obs)?;
    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
    guard_serve_bind_auth("api", args.bind, &auth_policy, &tls)?;

    let config = ApiServerConfig::for_pipeline(args.file.clone())
        .with_auth_policy(auth_policy)
        .with_profile(AcpProfileConfig {
            text: args.trace || args.profile.text,
            json_path: args.profile.json_path.clone(),
        });
    let server = Arc::new(ApiServer::new(config));
    server
        .run_http(ApiHttpServeOptions {
            bind: args.bind,
            public_url: args.public_url.clone(),
            tls,
        })
        .await
}

pub(crate) async fn run_site_server(args: &SiteServeArgs) -> Result<(), String> {
    apply_obs_mode(args.obs)?;
    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
    guard_serve_bind_auth("site", args.bind, &auth_policy, &tls)?;

    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = auth_policy;
    // An HTTP host must run its handler on every request — caching the
    // reply to an identical second POST would skip the handler's side
    // effects — so swap the default replay cache for the no-op one.
    config.replay_cache = Arc::new(harn_serve::NoReplayCache);
    let core = DispatchCore::new(config).map_err(|error| error.to_string())?;
    harn_serve::emit_export_diagnostics(core.catalog().diagnostics());
    SiteServer::new(SiteServerConfig::new(core).with_trusted_proxies(args.trusted_proxies.clone()))
        .run_http(SiteHttpServeOptions {
            bind: args.bind,
            public_url: args.public_url.clone(),
            tls,
        })
        .await
}

pub(crate) async fn run_worker_server(args: &WorkerServeArgs) -> Result<(), String> {
    apply_obs_mode(args.obs)?;
    let script_path = Path::new(&args.file).to_path_buf();
    let options = harn_serve::WorkerServeOptions {
        consumer_id: args.consumer_id.clone(),
        claim_ttl: StdDuration::from_secs(args.claim_ttl_secs),
        drain_timeout: StdDuration::from_secs(args.drain_timeout_secs),
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let server = harn_serve::start_worker_server(&script_path, options)
                .await
                .map_err(|error| error.to_string())?;
            let job_count = server.jobs().len();
            let queue_count = server
                .jobs()
                .iter()
                .filter_map(|job| job.queue.as_deref())
                .collect::<BTreeSet<_>>()
                .len();
            eprintln!(
                "[harn] worker ready: jobs={job_count} queues={queue_count} script={}",
                script_path.display()
            );

            tokio::signal::ctrl_c()
                .await
                .map_err(|error| format!("failed to wait for shutdown signal: {error}"))?;
            let report = server.shutdown().await.map_err(|error| error.to_string())?;
            if !report.drained {
                return Err(format!(
                    "worker shutdown timed out with {} in-flight dispatch(es), {} retry item(s), and {} DLQ item(s)",
                    report.in_flight, report.retry_queue_depth, report.dlq_depth
                ));
            }
            eprintln!(
                "[harn] worker stopped: jobs={} queues={} dlq={}",
                report.jobs, report.queues, report.dlq_depth
            );
            Ok(())
        })
        .await
}

pub(crate) async fn run_mcp_server(args: &ServeMcpArgs) -> Result<(), String> {
    validate_obs_transport(args.obs, args.transport == McpServeTransport::Stdio, "mcp")?;
    apply_obs_mode(args.obs)?;
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
    harn_serve::emit_export_diagnostics(catalog.diagnostics());
    let has_pub_fn_exports = catalog
        .functions
        .values()
        .any(|function| function.kind == ExportedCallableKind::Function);

    if !has_pub_fn_exports {
        let mode = match args.transport {
            McpServeTransport::Stdio => crate::commands::run::RunFileMcpServeMode::Stdio,
            McpServeTransport::Http => {
                let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
                let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
                guard_serve_bind_auth("mcp", args.bind, &auth_policy, &tls)?;
                crate::commands::run::RunFileMcpServeMode::Http(Box::new(
                    crate::commands::run::RunFileMcpServeHttp {
                        options: McpHttpServeOptions {
                            bind: args.bind,
                            path: args.path.clone(),
                            sse_path: args.sse_path.clone(),
                            messages_path: args.messages_path.clone(),
                            tls,
                        },
                        auth_policy,
                    },
                ))
            }
        };
        crate::commands::run::run_file_mcp_serve(&args.file, args.card.as_deref(), mode).await;
        return Ok(());
    }

    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let mut config = DispatchCoreConfig::for_script(&args.file);
    config.auth_policy = auth_policy.clone();
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
            let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
            guard_serve_bind_auth("mcp", args.bind, &auth_policy, &tls)?;
            server
                .run_http(McpHttpServeOptions {
                    bind: args.bind,
                    path: args.path.clone(),
                    sse_path: args.sse_path.clone(),
                    messages_path: args.messages_path.clone(),
                    tls,
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
        .layer(DefaultBodyLimit::max(SERVE_DEFAULT_MAX_BODY_BYTES))
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
    /// Per-session elicitation bus to install before invoking the
    /// handler. The bus lives for the whole session, so a request that
    /// arrives before the client opens its event stream still gets one;
    /// its outbound requests queue until the stream registers (or fail
    /// loudly after `SESSION_STREAM_GRACE` if it never does).
    bus: Option<harn_vm::mcp_elicit::ElicitationBus>,
}

/// How long a queued server-to-client request (e.g. `elicitation/create`)
/// may wait for the client to open its event stream before it fails
/// loudly. Only a client that never opens a stream reaches this bound;
/// the ordinary POST-vs-GET arrival race resolves as soon as the GET
/// lands, with no timing dependence.
const SESSION_STREAM_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct ScriptSessionState {
    stream_tx: Option<UnboundedSender<JsonValue>>,
}

#[derive(Clone)]
struct SharedScriptSession {
    inner: Arc<Mutex<ScriptSessionState>>,
    /// Session-lifetime bus for server-to-client requests. Created with
    /// the session — NOT with the event stream — so a `tools/call` POST
    /// that outruns the client's GET can still elicit; the delivery task
    /// holds the payload until the stream registers.
    bus: harn_vm::mcp_elicit::ElicitationBus,
    stream_watch: tokio::sync::watch::Sender<Option<UnboundedSender<JsonValue>>>,
}

impl SharedScriptSession {
    fn new() -> Self {
        let (stream_watch, watch_rx) = tokio::sync::watch::channel(None);
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel::<JsonValue>();
        let bus = harn_vm::mcp_elicit::ElicitationBus::new(outbound_tx);
        tokio::spawn(deliver_session_requests(outbound_rx, watch_rx, bus.clone()));
        Self {
            inner: Arc::new(Mutex::new(ScriptSessionState::default())),
            bus,
            stream_watch,
        }
    }

    fn set_stream_tx(&self, tx: Option<UnboundedSender<JsonValue>>) {
        self.inner.lock().expect("session poisoned").stream_tx = tx.clone();
        let _ = self.stream_watch.send(tx);
    }

    fn stream_tx(&self) -> Option<UnboundedSender<JsonValue>> {
        self.inner
            .lock()
            .expect("session poisoned")
            .stream_tx
            .clone()
    }

    fn bus(&self) -> harn_vm::mcp_elicit::ElicitationBus {
        self.bus.clone()
    }
}

/// Per-session delivery task: forward the bus's outbound server-to-client
/// requests onto whichever event stream the session currently has. A
/// payload that arrives while no stream is open waits — event-driven, via
/// the `watch` channel — for one to register instead of failing, which
/// closes the POST-vs-GET arrival race that made loopback elicitation
/// scenarios flaky under CI load. If no stream registers within
/// `SESSION_STREAM_GRACE`, the pending request is failed loudly by
/// synthesizing a JSON-RPC error response back onto the bus.
async fn deliver_session_requests(
    mut outbound: tokio_mpsc::UnboundedReceiver<JsonValue>,
    mut stream_watch: tokio::sync::watch::Receiver<Option<UnboundedSender<JsonValue>>>,
    bus: harn_vm::mcp_elicit::ElicitationBus,
) {
    while let Some(msg) = outbound.recv().await {
        if stream_watch.borrow().is_none() {
            eprintln!(
                "[harn] queueing a server-to-client request until the client opens its event stream"
            );
        }
        let deadline = tokio::time::Instant::now() + SESSION_STREAM_GRACE;
        let mut delivered = false;
        loop {
            let target = stream_watch.borrow().clone();
            if let Some(tx) = target {
                if tx.unbounded_send(msg.clone()).is_ok() {
                    delivered = true;
                    break;
                }
                // The stream this sender belonged to closed; wait for a
                // reconnect below rather than dropping the payload.
            }
            match tokio::time::timeout_at(deadline, stream_watch.changed()).await {
                Ok(Ok(())) => continue,
                // Session dropped or grace expired: stop waiting.
                Ok(Err(_)) | Err(_) => break,
            }
        }
        if !delivered {
            if let Some(error) = undeliverable_client_request_error(&msg) {
                let _ = bus.route_response(&error);
            }
        }
    }
}

/// Synthesize the JSON-RPC error response that wakes a pending
/// server-to-client request whose payload could not be delivered because
/// the client never opened an event stream. Notifications (no id) have
/// no waiter to wake and are dropped.
fn undeliverable_client_request_error(msg: &JsonValue) -> Option<JsonValue> {
    let id = msg.get("id")?.clone();
    Some(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32001,
            "message": "client never opened an event stream; server-to-client request undeliverable",
        },
    }))
}

impl ScriptMcpRuntime {
    fn start(server: harn_vm::McpServer, mut vm: harn_vm::Vm) -> Self {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel::<ScriptMcpJob>();
        tokio::task::spawn_local(async move {
            while let Some(job) = rx.recv().await {
                let _previous = harn_vm::mcp_elicit::install_bus(job.bus);
                let response = server.handle_json_rpc(job.request, &mut vm).await;
                harn_vm::mcp_elicit::install_bus(_previous);
                let _ = job.response_tx.send(response);
            }
        });
        Self { tx }
    }

    async fn call(
        &self,
        request: JsonValue,
        bus: Option<harn_vm::mcp_elicit::ElicitationBus>,
    ) -> Result<Option<JsonValue>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ScriptMcpJob {
                request,
                response_tx,
                bus,
            })
            .map_err(|_| "script MCP runtime is not running".to_string())?;
        response_rx
            .await
            .map_err(|_| "script MCP runtime dropped response".to_string())
    }
}

/// Recognize a JSON-RPC payload as a response (rather than a request /
/// notification): MUST have an `id` and exactly one of `result` /
/// `error`, and MUST NOT have a `method`. Conservative on purpose so
/// we don't accidentally swallow a client-initiated request that
/// happens to share an id with a recent elicitation.
fn looks_like_response(value: &JsonValue) -> bool {
    if value.get("method").is_some() {
        return false;
    }
    if value.get("id").is_none() {
        return false;
    }
    value.get("result").is_some() || value.get("error").is_some()
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
    let (session_id, session, created) =
        match script_lookup_or_create_session(&state, &request, header_session) {
            Ok(value) => value,
            Err(response) => return *response,
        };

    // Streamable-HTTP clients reply to server-to-client requests
    // (notably `elicitation/create`) by POSTing the JSON-RPC response
    // back to the same `/mcp` endpoint. Detect that here and route to
    // the session's elicitation bus so `mcp_elicit(...)` can wake. Per
    // JSON-RPC etiquette we *never* reply to a response — even a stale
    // one with no matching pending — so this path is fully terminal.
    if looks_like_response(&request) {
        let _ = session.bus().route_response(&request);
        let mut http = StatusCode::ACCEPTED.into_response();
        attach_script_http_headers(&mut http, created.then_some(session_id.as_str()));
        return http;
    }

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

    let job_bus = Some(session.bus());
    match state.runtime.call(request, job_bus).await {
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
    if let Err(response) = validate_script_protocol_header(&headers) {
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
    if let Err(response) = validate_script_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_script_protocol_header(&headers) {
        return *response;
    }
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
    // Legacy SSE clients reply to server-to-client requests over the
    // same `/messages` endpoint. Route responses to the session bus so
    // a tool handler awaiting `mcp_elicit(...)` wakes up. As above we
    // never reply to a response, even a stale one.
    if looks_like_response(&request) {
        let _ = session.bus().route_response(&request);
        return StatusCode::ACCEPTED.into_response();
    }
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
    let job_bus = Some(session.bus());
    match state.runtime.call(request, job_bus).await {
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
        // Defensive: this call site passes no per-route scopes, so the
        // emptiness rule (`empty ⊆ anything`) makes `MissingScope`
        // unreachable. If a future caller threads scopes through, fall
        // back to the canonical forbidden envelope.
        AuthorizationDecision::MissingScope { required, granted } => {
            Err(harn_vm::jsonrpc::error_response(
                request.get("id").cloned().unwrap_or(JsonValue::Null),
                -32003,
                &harn_serve::forbidden_message(&required, &granted),
            ))
        }
        // `authorize_mcp` is the only producer of this variant and
        // belongs to the `harness.mcp.*` dispatch path inside harn-vm,
        // not this MCP-server auth gate. Surfacing it here would
        // indicate the wrong policy hook was called; render the
        // policy's reason string under the standard 403 envelope.
        AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
            Err(harn_vm::jsonrpc::error_response(
                request.get("id").cloned().unwrap_or(JsonValue::Null),
                -32003,
                &reason,
            ))
        }
    }
}

/// Returns true when an MCP method MUST clear the configured auth policy
/// before the runtime executes it. The list is deny-by-default: only
/// `initialize` (required to establish the session) and `ping`
/// (connectivity check) are exempt; every other method — including
/// catalog and listing methods that expose the script's
/// tool/resource/prompt surface — goes through `AuthPolicy::authorize`.
///
/// New MCP methods (notifications/*, completion/complete,
/// sampling/createMessage, elicitation/create, custom RPCs) are
/// covered automatically because anything outside the small allowlist
/// requires auth.
fn script_method_requires_auth(method: &str) -> bool {
    !matches!(method, "initialize" | "ping")
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
        ..AuthRequest::default()
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
            keys: api_keys
                .iter()
                .map(|key| ApiKeyEntry::new(key.clone(), BTreeSet::new()))
                .collect(),
        }));
    }
    if let Some(secret) = hmac_secret {
        methods.push(AuthMethodConfig::Hmac(HmacAuthConfig {
            shared_secret: secret.clone(),
            provider: "harn-serve".to_string(),
            timestamp_window: Duration::seconds(300),
            granted_scopes: BTreeSet::new(),
            tenant_id: None,
        }));
    }
    AuthPolicy {
        methods,
        mcp_allowlist: None,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    use super::*;

    fn parse_serve_command(args: &[&str]) -> ServeCommand {
        let cli = Cli::try_parse_from(args).expect("parse serve command");
        let Some(Command::Serve(serve)) = cli.command else {
            panic!("expected serve command");
        };
        serve.command
    }

    #[tokio::test]
    async fn acp_stdio_rejects_stdout_observability_before_serving() {
        let ServeCommand::Acp(args) =
            parse_serve_command(&["harn", "serve", "acp", "--obs", "stdout"])
        else {
            panic!("expected serve acp");
        };

        let error = run_acp_server(&args)
            .await
            .expect_err("ACP stdio must reject stdout observability");
        assert_eq!(
            error,
            "`harn serve acp --transport stdio` cannot use `--obs stdout`: stdout is reserved for \
             JSON-RPC protocol frames; use `--obs stderr`, `--obs otel`, or `--obs off`"
        );
    }

    #[tokio::test]
    async fn mcp_stdio_rejects_stdout_observability_before_loading_script() {
        let ServeCommand::Mcp(args) =
            parse_serve_command(&["harn", "serve", "mcp", "--obs", "stdout", "missing.harn"])
        else {
            panic!("expected serve mcp");
        };

        let error = run_mcp_server(&args)
            .await
            .expect_err("MCP stdio must reject stdout observability");
        assert_eq!(
            error,
            "`harn serve mcp --transport stdio` cannot use `--obs stdout`: stdout is reserved for \
             JSON-RPC protocol frames; use `--obs stderr`, `--obs otel`, or `--obs off`"
        );
    }

    #[test]
    fn non_stdio_transports_allow_stdout_observability() {
        let cases: &[&[&str]] = &[
            &[
                "harn",
                "serve",
                "acp",
                "--transport",
                "websocket",
                "--obs",
                "stdout",
            ],
            &[
                "harn",
                "serve",
                "mcp",
                "--transport",
                "http",
                "--obs",
                "stdout",
                "server.harn",
            ],
        ];
        for args in cases {
            match parse_serve_command(args) {
                ServeCommand::Acp(args) => {
                    validate_obs_transport(
                        args.obs,
                        args.transport == AcpServeTransport::Stdio,
                        "acp",
                    )
                    .expect("ACP WebSocket does not reserve stdout");
                }
                ServeCommand::Mcp(args) => {
                    validate_obs_transport(
                        args.obs,
                        args.transport == McpServeTransport::Stdio,
                        "mcp",
                    )
                    .expect("MCP HTTP does not reserve stdout");
                }
                _ => panic!("expected ACP or MCP serve command"),
            }
        }
    }

    fn test_script_mcp_state() -> ScriptMcpHttpState {
        let (tx, _rx) = tokio_mpsc::unbounded_channel();
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .expect("sessions")
            .insert("session-1".to_string(), SharedScriptSession::new());
        ScriptMcpHttpState {
            runtime: ScriptMcpRuntime { tx },
            options: McpHttpServeOptions {
                bind: "127.0.0.1:0".parse().expect("bind"),
                path: "/mcp".to_string(),
                sse_path: "/sse".to_string(),
                messages_path: "/messages".to_string(),
                tls: HttpTlsConfig::plain(),
            },
            auth_policy: AuthPolicy::allow_all(),
            sessions,
        }
    }

    #[tokio::test]
    async fn script_mcp_delete_rejects_remote_origin_without_deleting_session() {
        let state = test_script_mcp_state();
        let sessions = state.sessions.clone();
        let response = script_http_delete_session(
            State(state),
            HeaderMap::from_iter([
                (
                    HeaderName::from_static("mcp-session-id"),
                    HeaderValue::from_static("session-1"),
                ),
                (
                    HeaderName::from_static("origin"),
                    HeaderValue::from_static("https://attacker.example"),
                ),
            ]),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(sessions.lock().expect("sessions").contains_key("session-1"));
    }

    #[tokio::test]
    async fn script_mcp_delete_rejects_unsupported_protocol_version() {
        let state = test_script_mcp_state();
        let sessions = state.sessions.clone();
        let response = script_http_delete_session(
            State(state),
            HeaderMap::from_iter([
                (
                    HeaderName::from_static("mcp-session-id"),
                    HeaderValue::from_static("session-1"),
                ),
                (
                    HeaderName::from_static("mcp-protocol-version"),
                    HeaderValue::from_static("1999-01-01"),
                ),
            ]),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(sessions.lock().expect("sessions").contains_key("session-1"));
    }

    #[test]
    fn script_method_requires_auth_denies_by_default() {
        // initialize/ping must stay open so clients can complete the
        // session handshake / connectivity check.
        assert!(!script_method_requires_auth("initialize"));
        assert!(!script_method_requires_auth("ping"));
        // The previous allowlist gated only these three; they must
        // continue to require auth under the inverted policy.
        assert!(script_method_requires_auth("tools/call"));
        assert!(script_method_requires_auth("resources/read"));
        assert!(script_method_requires_auth("prompts/get"));
        // The reason for the inversion: every other method (catalog
        // listings, notifications, sampling, elicitation, custom RPCs) must
        // require auth before exposing the script's surface.
        assert!(script_method_requires_auth("tools/list"));
        assert!(script_method_requires_auth("resources/list"));
        assert!(script_method_requires_auth("prompts/list"));
        assert!(script_method_requires_auth("notifications/initialized"));
        assert!(script_method_requires_auth("completion/complete"));
        assert!(script_method_requires_auth("sampling/createMessage"));
        assert!(script_method_requires_auth("elicitation/create"));
        assert!(script_method_requires_auth("some/custom/method"));
    }

    #[test]
    fn guard_serve_bind_auth_refuses_public_bind_without_auth_or_tls() {
        let bind: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let policy = AuthPolicy {
            methods: vec![],
            mcp_allowlist: None,
        };
        let tls = harn_serve::HttpTlsConfig::plain();
        let result = guard_serve_bind_auth("mcp", bind, &policy, &tls);
        let error = result.expect_err("public bind without auth should be refused");
        assert!(
            error.contains("refusing") && error.contains("non-loopback"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn guard_serve_bind_auth_allows_public_bind_with_auth() {
        let bind: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single(
                "test-key",
            ))],
            mcp_allowlist: None,
        };
        let tls = harn_serve::HttpTlsConfig::plain();
        let result = guard_serve_bind_auth("mcp", bind, &policy, &tls);
        assert!(
            result.is_ok(),
            "public bind with auth should be allowed: {result:?}"
        );
    }

    #[test]
    fn guard_serve_bind_auth_allows_loopback_without_auth() {
        let bind: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let policy = AuthPolicy {
            methods: vec![],
            mcp_allowlist: None,
        };
        let tls = harn_serve::HttpTlsConfig::plain();
        // Loopback + no auth is allowed (the function emits a WARN log
        // but does not refuse the start — the operator may be running
        // a local dev server intentionally).
        let result = guard_serve_bind_auth("mcp", bind, &policy, &tls);
        assert!(result.is_ok(), "loopback with no auth should be allowed");
    }

    #[test]
    fn guard_serve_bind_auth_allows_public_bind_with_tls() {
        let bind: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let policy = AuthPolicy {
            methods: vec![],
            mcp_allowlist: None,
        };
        let tls = harn_serve::HttpTlsConfig::self_signed_dev();
        let result = guard_serve_bind_auth("api", bind, &policy, &tls);
        assert!(
            result.is_ok(),
            "public bind with TLS should be allowed: {result:?}"
        );
    }
}
