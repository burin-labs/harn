use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
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

use crate::cli::{
    A2aServeArgs, AcpServeTransport, ApiServeArgs, McpServeTransport, ServeAcpArgs, ServeCommand,
    ServeMcpArgs, ServeObsMode, ServeTlsMode, SiteServeArgs, WorkerServeArgs,
};

/// Default 10 MiB request-body cap applied to every `harn serve` HTTP
/// router. Mirrors `DEFAULT_MAX_BODY_BYTES` in the orchestrator
/// listener so large/runaway POSTs cannot exhaust process memory while
/// axum buffers a request.
pub(crate) const SERVE_DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Build the dispatch configuration for a file-backed server.
///
/// The source project's manifest owns privileged host-dispatch authority. All
/// server adapters must consume that declaration just as `check`, `test`,
/// `run`, and ACP do; callers may still add transport-specific policy after
/// this shared projection.
fn dispatch_core_config_for_source(path: &str) -> DispatchCoreConfig {
    let mut config = DispatchCoreConfig::for_script(path);
    config.trusted_host_dispatch =
        crate::compiler_context::trusted_host_dispatch_for_source(Path::new(path));
    config
}

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

    let mut config = dispatch_core_config_for_source(&args.file);
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

    let config = api_server_config(args, auth_policy);
    let server = Arc::new(ApiServer::new(config));
    server
        .run_http(ApiHttpServeOptions {
            bind: args.bind,
            public_url: args.public_url.clone(),
            tls,
        })
        .await
}

fn api_server_config(args: &ApiServeArgs, auth_policy: AuthPolicy) -> ApiServerConfig {
    let profile = AcpProfileConfig {
        text: args.trace || args.profile.text,
        json_path: args.profile.json_path.clone(),
    };
    let acp = crate::acp::server_config(Some(args.file.clone()), AuthPolicy::allow_all())
        .with_profile(profile);
    let mut config = ApiServerConfig::for_pipeline(args.file.clone()).with_auth_policy(auth_policy);
    config.acp = acp;
    config
}

pub(crate) async fn run_site_server(args: &SiteServeArgs) -> Result<(), String> {
    apply_obs_mode(args.obs)?;
    let auth_policy = build_auth_policy(&args.api_key, args.hmac_secret.as_ref());
    let tls = build_tls_config(args.tls, args.cert.as_ref(), args.key.as_ref())?;
    guard_serve_bind_auth("site", args.bind, &auth_policy, &tls)?;

    let mut config = dispatch_core_config_for_source(&args.file);
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
    let consumer_id = args.consumer_id.clone();
    let claim_ttl = StdDuration::from_secs(args.claim_ttl_secs);
    let drain_timeout = StdDuration::from_secs(args.drain_timeout_secs);
    let tenant_id = args.tenant.clone();
    let tenant_state_dir = args.tenant_state_dir.clone();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let extensions = crate::package::try_load_runtime_extensions(&script_path)
                .map_err(|error| format!("failed to load worker package: {error}"))?;
            let connector_registry =
                crate::build_connector_registry(&extensions.provider_connectors)
                    .await
                    .map_err(|error| format!("failed to load worker connectors: {error}"))?;
            let tenant_scope = crate::worker_tenant::resolve_worker_tenant_scope(
                &script_path,
                tenant_id.as_deref(),
                tenant_state_dir.as_deref(),
            )?;
            let options = harn_serve::WorkerServeOptions {
                consumer_id,
                claim_ttl,
                drain_timeout,
                connector_registry: Some(connector_registry),
                tenant_scope,
            };
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
    // usually don't expose any `pub fn` entrypoints, so auto can recognize
    // them. `--surface script` owns the intentional mixed case. Dispatch to
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

    let use_script_surface = match args.surface {
        crate::cli::McpServeSurface::Auto => !has_pub_fn_exports,
        crate::cli::McpServeSurface::Script => true,
        crate::cli::McpServeSurface::Exports => false,
    };

    if use_script_surface {
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
    let mut config = dispatch_core_config_for_source(&args.file);
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
    };
    let router = Router::new()
        .route(&options.path, post(script_http_post_request))
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
}

#[derive(Clone)]
pub(crate) struct ScriptMcpRuntime {
    tx: tokio_mpsc::UnboundedSender<ScriptMcpJob>,
}

struct ScriptMcpJob {
    request: JsonValue,
    response_tx: oneshot::Sender<Option<JsonValue>>,
}

impl ScriptMcpRuntime {
    pub(crate) fn start(server: harn_vm::McpServer, mut vm: harn_vm::Vm) -> Self {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel::<ScriptMcpJob>();
        tokio::task::spawn_local(async move {
            while let Some(job) = rx.recv().await {
                let response = server.handle_json_rpc(job.request, &mut vm).await;
                let _ = job.response_tx.send(response);
            }
        });
        Self { tx }
    }

    pub(crate) async fn call(&self, request: JsonValue) -> Result<Option<JsonValue>, String> {
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
    if let Err(response) = authorize_script_rpc(
        &state,
        &request,
        script_http_auth_request(method, &state.options.path, body.to_vec(), &headers),
    )
    .await
    {
        let mut http = Json(response).into_response();
        attach_script_http_headers(&mut http);
        return http;
    }

    match state.runtime.call(request).await {
        Ok(Some(response)) => {
            let mut http = Json(response).into_response();
            attach_script_http_headers(&mut http);
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
            attach_script_http_headers(&mut http);
            http
        }
    }
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
        AuthorizationDecision::MissingScope { required, granted } => {
            Err(harn_vm::jsonrpc::error_response(
                request.get("id").cloned().unwrap_or(JsonValue::Null),
                -32003,
                &harn_serve::forbidden_message(&required, &granted),
            ))
        }
        AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
            Err(harn_vm::jsonrpc::error_response(
                request.get("id").cloned().unwrap_or(JsonValue::Null),
                -32003,
                &reason,
            ))
        }
    }
}

/// Discovery and connectivity checks are public; every method that exposes or
/// invokes the script's catalog clears the configured authorization policy.
fn script_method_requires_auth(method: &str) -> bool {
    !matches!(
        method,
        harn_vm::mcp_protocol::METHOD_SERVER_DISCOVER | "ping"
    )
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

fn attach_script_http_headers(response: &mut Response) {
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
    if value == MCP_PROTOCOL_VERSION {
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

    async fn api_resolves_manifest_privileged_trigger(declared: bool) -> Result<(), String> {
        harn_vm::reset_thread_local_state();
        crate::compiler_context::ensure_builtin_signatures_installed();
        let project = tempfile::tempdir().expect("temp project");
        let script =
            crate::tests::common::host_dispatch_project::write_host_dispatch_trigger_project(
                project.path(),
                declared,
                r#"
pub fn on_tick(_event) -> nil {
  const _ = host_call("runtime.pipeline_input", {})
  return nil
}
"#,
            );
        let ServeCommand::Api(args) = parse_serve_command(&[
            "harn",
            "serve",
            "api",
            script.to_str().expect("UTF-8 fixture path"),
        ]) else {
            panic!("expected serve api");
        };
        let config = api_server_config(&args, AuthPolicy::allow_all());
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        let result = async {
            config
                .acp
                .runtime_configurator
                .configure(&mut vm, Some(&script))
                .await?;
            let extensions = crate::package::load_runtime_extensions(&script);
            let collected = crate::package::collect_manifest_triggers(&mut vm, &extensions)
                .await
                .map_err(|error| error.to_string())?;
            let crate::package::CollectedTriggerHandler::Local { callable, .. } =
                &collected[0].handler
            else {
                return Err("fixture trigger must use a local handler".to_string());
            };
            vm.resolve_callable(callable)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        .await;
        harn_vm::reset_thread_local_state();
        result
    }

    #[tokio::test]
    async fn api_uses_manifest_authority_before_loading_the_pipeline_graph() {
        api_resolves_manifest_privileged_trigger(true)
            .await
            .expect("declared API project resolves its privileged trigger on dispatch");

        let error = api_resolves_manifest_privileged_trigger(false)
            .await
            .expect_err("undeclared API project remains unprivileged on dispatch");
        assert!(
            error.contains("host_call") && error.contains("not callable source API"),
            "unexpected refusal: {error}"
        );
    }

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

    #[test]
    fn script_method_requires_auth_denies_by_default() {
        // Discovery and ping stay open for capability and connectivity checks.
        assert!(!script_method_requires_auth("server/discover"));
        assert!(!script_method_requires_auth("ping"));
        // The previous allowlist gated only these three; they must
        // continue to require auth under the inverted policy.
        assert!(script_method_requires_auth("tools/call"));
        assert!(script_method_requires_auth("resources/read"));
        assert!(script_method_requires_auth("prompts/get"));
        // The reason for the inversion: every other method (catalog
        // listings, sampling, elicitation, custom RPCs) must
        // require auth before exposing the script's surface.
        assert!(script_method_requires_auth("tools/list"));
        assert!(script_method_requires_auth("resources/list"));
        assert!(script_method_requires_auth("prompts/list"));
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
