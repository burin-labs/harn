//! `harn serve site` — host `.harn` functions as live HTTP handlers.
//!
//! The API / A2A / MCP adapters expose a *fixed* protocol surface and
//! route every inbound call into one of a handful of hard-coded Rust
//! handlers. None of them let a `.harn` author answer a bare HTTP path
//! with their own function — yet `harn-vm` already ships the whole
//! request/response convention for it: the in-process `http_server`
//! primitive hands handlers a `req` dict and renders a tagged response
//! envelope, and [`crate::http_codec`] already knows how to turn that
//! envelope into an `axum::Response`. The only missing piece was the
//! bridge from a live socket to a `.harn` closure. This module is that
//! bridge.
//!
//! ## Routing
//!
//! Every exported `pub fn` whose [`crate::ExportedFunction::route`] is set
//! becomes an HTTP route. A function opts in two ways (see
//! [`crate::exports`]): an explicit `@route("METHOD", "/path")`
//! attribute, or the `handler_*` naming convention (`handler_health` →
//! `/health`, bare `handler` → `/`). Functions without a route stay
//! dispatch-only — they remain callable by name from a route handler or
//! from the other adapters, but no bare path reaches them.
//!
//! ## Request shape
//!
//! A handler receives one positional `req` dict mirroring the in-process
//! `http_server` convention:
//!
//! ```text
//! { method, path, route, path_params, params, query, headers,
//!   body, body_base64, content_length, client_ip, remote_addr }
//! ```
//!
//! `body` is the UTF-8-lossy view (convenient for JSON/text handlers);
//! `body_base64` is the standard-base64 encoding of the *raw* bytes, so a
//! binary handler recovers the exact payload with `bytes_from_base64(...)`
//! — multipart uploads survive losslessly through the JSON dispatch
//! boundary this way. `client_ip` is taken from `X-Forwarded-For` /
//! `X-Real-IP` when present (the common reverse-proxy shape).
//!
//! ## Responses
//!
//! A handler returns either a plain value (rendered `200 OK + JSON`) or a
//! tagged envelope from the `http_*` builtins (`http_ok`, `http_created`,
//! `http_not_modified`, `http_error`, `http_stream`, `http_sse`,
//! `http_upgrade_ws`, …). [`crate::http_codec`] renders all of them, so
//! status/headers/SSE/304 semantics come for free and stay identical to
//! every other harn-serve surface.
//!
//! ## WebSockets
//!
//! When a handler returns an `http_upgrade_ws(req, options)` envelope and
//! the request actually carried the upgrade headers, the adapter performs
//! the upgrade through [`crate::ws::ws_accept`] and drives the socket: each
//! inbound frame is dispatched to the `on_message` function named in the
//! envelope as a `{type, data}` / `{type, data_base64}` message dict, and
//! the function's return value is sent back (a string verbatim, any other
//! value as JSON, `nil` sends nothing). This reuses the exact subprotocol
//! negotiation and idle-keepalive machinery the other WS routes use.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{to_bytes, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{
    DefaultBodyLimit, FromRequestParts, MatchedPath, Query, RawPathParams, Request, State,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use base64::Engine;
use serde_json::{json, Map, Value};

use crate::adapter::DispatchRuntime;
use crate::auth::AuthRequest;
use crate::http_codec::{
    axum_response_from_call, axum_response_from_dispatch_error, classify_ws_upgrade,
    fresh_request_id,
};
use crate::tls::HttpTlsConfig;
use crate::ws::{ws_accept, WsConfig, WsMessage, WsSession};
use crate::{
    CallArguments, CallRequest, DispatchCore, ExportCatalog, RouteSpec, TransportConfig,
    DEFAULT_HTTP_BODY_LIMIT_BYTES,
};

/// Adapter identifier stamped on every [`CallRequest`] this host issues,
/// so trust records and span attributes attribute the dispatch to the
/// site host rather than one of the protocol adapters.
const SITE_ADAPTER: &str = "site";

/// Listener options for [`SiteServer::run_http`].
#[derive(Clone, Debug)]
pub struct SiteHttpServeOptions {
    pub bind: SocketAddr,
    /// URL advertised in the startup banner. Defaults to the bound
    /// address with the TLS-appropriate scheme.
    pub public_url: Option<String>,
    pub tls: HttpTlsConfig,
}

/// Static configuration for a [`SiteServer`].
pub struct SiteServerConfig {
    pub core: DispatchCore,
    /// Transport layers (compression / ETag / CORS) applied to the whole
    /// site router. Defaults to compression + ETag on, CORS off.
    pub transport: TransportConfig,
}

impl SiteServerConfig {
    pub fn new(core: DispatchCore) -> Self {
        Self {
            core,
            transport: TransportConfig::default_enabled(),
        }
    }

    pub fn with_transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }
}

/// Hosts the `pub fn` HTTP handlers of one `.harn` script.
pub struct SiteServer {
    config: SiteServerConfig,
}

impl SiteServer {
    pub fn new(config: SiteServerConfig) -> Self {
        Self { config }
    }

    /// Build the site router without binding a socket. Exposed for tests
    /// and for embedding the site surface inside a larger router.
    /// Returns an error when two routes collide on the same method+path.
    pub fn router(self) -> Result<Router, String> {
        let SiteServerConfig { core, transport } = self.config;
        let catalog = Arc::new(core.catalog().clone());
        let runtime = Arc::new(DispatchRuntime::start("SITE", Arc::new(core)));
        build_site_router(&catalog, runtime, &transport)
    }

    /// Bind the configured socket and serve until the process exits.
    pub async fn run_http(self, options: SiteHttpServeOptions) -> Result<(), String> {
        let tls = options.tls.clone();
        let router = self.router()?;
        let router = crate::tls::apply_security_headers(router, &tls);
        let listener = crate::tls::bind_listener(options.bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        let advertised = options
            .public_url
            .clone()
            .unwrap_or_else(|| format!("{}://{local_addr}", tls.listener_scheme()));
        eprintln!("[harn] Site server ready on {advertised}");
        crate::tls::serve_router_from_tcp(listener, router, &tls)
            .await
            .map_err(|error| format!("Site server failed: {error}"))
    }
}

/// State shared by every site route handler: the dispatch executor plus
/// the method→function table for each mounted path.
#[derive(Clone)]
struct SiteState {
    runtime: Arc<DispatchRuntime>,
    /// `path → (method → function name)`. `method` is uppercased; `"*"`
    /// is the any-method fallback consulted when no exact method matches.
    routes: Arc<BTreeMap<String, BTreeMap<String, String>>>,
}

impl SiteState {
    /// Resolve the handler for a `(path, method)` pair, honouring the
    /// `*` any-method fallback. Returns the function name to dispatch.
    fn resolve(&self, path: &str, method: &Method) -> Option<&str> {
        let methods = self.routes.get(path)?;
        methods
            .get(method.as_str())
            .or_else(|| methods.get("*"))
            .map(String::as_str)
    }
}

/// Group the catalog's routed functions into a `path → method → fn` table
/// and mount one `any(..)` handler per path. Distinct methods on the same
/// path coexist; a duplicate method+path is a configuration error.
fn build_site_router(
    catalog: &ExportCatalog,
    runtime: Arc<DispatchRuntime>,
    transport: &TransportConfig,
) -> Result<Router, String> {
    let mut routes: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for function in catalog.functions.values() {
        let Some(RouteSpec { method, path }) = function.route.as_ref() else {
            continue;
        };
        let by_method = routes.entry(path.clone()).or_default();
        if let Some(existing) = by_method.insert(method.clone(), function.name.clone()) {
            return Err(format!(
                "route conflict: {method} {path} is claimed by both `{existing}` and \
                 `{}`; give one of them a distinct @route(...)",
                function.name
            ));
        }
    }

    if routes.is_empty() {
        return Err(
            "no HTTP routes found: export a `pub fn handler_*` or annotate a function with \
             @route(\"METHOD\", \"/path\") to serve it"
                .to_string(),
        );
    }

    let state = SiteState {
        runtime,
        routes: Arc::new(routes),
    };

    let mut router: Router<SiteState> = Router::new();
    for path in state.routes.keys() {
        router = router.route(path, any(site_dispatch));
    }
    let router = router
        .layer(DefaultBodyLimit::max(DEFAULT_HTTP_BODY_LIMIT_BYTES))
        .with_state(state);
    Ok(crate::apply_transport_layers(router, transport))
}

/// Single entry point for every site route. Resolves the handler,
/// assembles the `req` dict, dispatches on the `.harn` executor thread,
/// and renders the reply — short-circuiting into the WebSocket path when
/// the handler asked for an upgrade.
async fn site_dispatch(State(state): State<SiteState>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();

    // Path template + captured params come from the router, so extract
    // them from the half we keep before touching the body.
    let matched_path = MatchedPath::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|m| m.as_str().to_string());
    let raw_params = RawPathParams::from_request_parts(&mut parts, &())
        .await
        .ok();
    // axum's `Query` extractor (serde_urlencoded) gives last-wins
    // key/value pairs, matching the in-process `http_server` query shape;
    // a malformed query string degrades to an empty map rather than a 400.
    let query = Query::<BTreeMap<String, String>>::from_request_parts(&mut parts, &())
        .await
        .map(|q| q.0)
        .unwrap_or_default();

    let method = parts.method.clone();
    let route_template = matched_path.unwrap_or_else(|| parts.uri.path().to_string());
    let Some(function) = state.resolve(&route_template, &method).map(str::to_string) else {
        return method_not_allowed(&state, &route_template);
    };

    let wants_upgrade = is_websocket_upgrade(&parts.headers);

    // A WebSocket handshake carries no body; reading it would block on a
    // socket that never sends one. Every other method buffers up to the
    // configured limit (the `DefaultBodyLimit` layer rejects larger).
    let body_bytes = if wants_upgrade {
        Bytes::new()
    } else {
        match to_bytes(body, DEFAULT_HTTP_BODY_LIMIT_BYTES).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(json!({
                        "code": "request_body_too_large",
                        "message": format!(
                            "request body exceeds the {DEFAULT_HTTP_BODY_LIMIT_BYTES}-byte limit"
                        ),
                    })),
                )
                    .into_response();
            }
        }
    };

    let request_id = fresh_request_id();
    let req_value = build_request_value(
        &method,
        &parts.uri,
        &route_template,
        raw_params.as_ref(),
        &query,
        &parts.headers,
        &body_bytes,
    );
    let auth = AuthRequest::from_http(
        &method,
        parts.uri.path(),
        body_bytes.to_vec(),
        &parts.headers,
    );

    let call = CallRequest {
        adapter: SITE_ADAPTER.to_string(),
        function: function.clone(),
        arguments: CallArguments::Positional(vec![req_value]),
        auth,
        caller: SITE_ADAPTER.to_string(),
        replay_key: None,
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: None,
        agent_session_id: None,
        progress: None,
        tenant_id: None,
        request_id: Some(request_id.clone()),
    };

    let response = match state.runtime.call(call).await {
        Ok(response) => response,
        Err(error) => return axum_response_from_dispatch_error(error, &request_id),
    };

    // A handler that returned `http_upgrade_ws(...)` on a request that
    // actually carried the upgrade headers gets the real socket; anything
    // else renders as plain HTTP. If the envelope asked to upgrade but the
    // request was not a WebSocket handshake, fall through to the codec,
    // which emits the loud `ws_upgrade_not_routed` 500 — the misuse should
    // surface, not silently 101 a plain GET.
    if wants_upgrade {
        if let Some(spec) = classify_ws_upgrade(&response) {
            return upgrade_websocket(&mut parts, state.runtime, spec).await;
        }
    }

    axum_response_from_call(response, &request_id)
}

/// Render the `405 Method Not Allowed` for a known path with an `Allow`
/// header listing the methods that *are* served there.
fn method_not_allowed(state: &SiteState, path: &str) -> Response {
    let allow = state
        .routes
        .get(path)
        .map(|methods| {
            methods
                .keys()
                .filter(|method| method.as_str() != "*")
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut response = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "code": "method_not_allowed",
            "message": format!("no handler for this method on {path}"),
        })),
    )
        .into_response();
    if !allow.is_empty() {
        if let Ok(value) = allow.parse() {
            response
                .headers_mut()
                .insert(axum::http::header::ALLOW, value);
        }
    }
    response
}

/// Whether the request is a WebSocket handshake: an `Upgrade: websocket`
/// token plus the `Sec-WebSocket-Key` the protocol requires.
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let has_upgrade = headers
        .get(axum::http::header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    has_upgrade && headers.contains_key("sec-websocket-key")
}

/// Complete the upgrade and drive the socket, dispatching each inbound
/// frame to the envelope's `on_message` function.
///
/// Takes the *original* request `Parts`: the `WebSocketUpgrade` extractor
/// consumes the `OnUpgrade` extension hyper stashed there during
/// `into_parts`, so a freshly assembled parts set (headers only) would not
/// upgrade. Subprotocol negotiation reads the same headers, keeping the
/// echoed `Sec-WebSocket-Protocol` consistent with the envelope's offer.
async fn upgrade_websocket(
    parts: &mut axum::http::request::Parts,
    runtime: Arc<DispatchRuntime>,
    spec: harn_vm::WsUpgradeSpec,
) -> Response {
    let config = ws_config_from_spec(&spec);
    let on_message = spec.on_message.clone();
    let headers = parts.headers.clone();
    let upgrade = match WebSocketUpgrade::from_request_parts(parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };

    ws_accept(config, headers, upgrade, move |session| {
        let runtime = runtime.clone();
        let on_message = on_message.clone();
        async move {
            drive_ws_session(session, runtime, on_message).await;
        }
    })
    .await
}

/// Translate the envelope's negotiation hints into a [`WsConfig`].
fn ws_config_from_spec(spec: &harn_vm::WsUpgradeSpec) -> WsConfig {
    let mut config = WsConfig {
        subprotocols: spec.offered.clone(),
        ..WsConfig::default()
    };
    if let Some(ms) = spec.idle_ping_ms {
        config.idle_ping = Some(std::time::Duration::from_millis(ms));
    }
    if let Some(bytes) = spec.max_message_bytes {
        config.max_message_bytes = bytes as usize;
    }
    config
}

/// Frame pump: dispatch each inbound message to `on_message` and write the
/// return value back. When the envelope named no handler the socket is
/// inbound-only — frames are drained and discarded (suiting server-push
/// handlers that never read from the client).
async fn drive_ws_session(
    session: WsSession,
    runtime: Arc<DispatchRuntime>,
    on_message: Option<String>,
) {
    let Some(handler) = on_message else {
        while let Ok(Some(_)) = session.recv().await {}
        return;
    };

    while let Ok(Some(message)) = session.recv().await {
        let message_value = match &message {
            WsMessage::Text(text) => json!({ "type": "text", "data": text }),
            WsMessage::Binary(bytes) => json!({
                "type": "binary",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
        };
        let call = CallRequest {
            adapter: SITE_ADAPTER.to_string(),
            function: handler.clone(),
            arguments: CallArguments::Positional(vec![message_value]),
            auth: AuthRequest::default(),
            caller: SITE_ADAPTER.to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            progress: None,
            tenant_id: None,
            request_id: Some(fresh_request_id()),
        };
        match runtime.call(call).await {
            Ok(response) => {
                if !send_ws_reply(&session, response.value).await {
                    break;
                }
            }
            // A handler error closes the socket with an internal-error code
            // rather than hanging the client on a half-open connection.
            Err(_) => {
                let _ = session.close(1011, "handler error").await;
                break;
            }
        }
    }
}

/// Send a handler's return value over the socket. `nil`/`null` sends
/// nothing; a string is sent verbatim; anything else is JSON-encoded.
/// Returns `false` when the send failed (socket gone), signalling the
/// pump to stop.
async fn send_ws_reply(session: &WsSession, value: Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => session.send(text).await.is_ok(),
        other => match serde_json::to_string(&other) {
            Ok(text) => session.send(text).await.is_ok(),
            Err(_) => true,
        },
    }
}

/// Assemble the `req` dict handed to a `.harn` handler. Mirrors the
/// in-process `http_server` shape so handlers are portable between the
/// embedded server and a hosted site, with `body_base64` added as the
/// binary-safe channel through the JSON dispatch boundary.
fn build_request_value(
    method: &Method,
    uri: &axum::http::Uri,
    route_template: &str,
    raw_params: Option<&RawPathParams>,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Value {
    let mut path_params = Map::new();
    if let Some(params) = raw_params {
        // `&RawPathParams` yields `(&str, &str)` pairs via `IntoIterator`.
        for (name, value) in params {
            path_params.insert(name.to_string(), Value::String(value.to_string()));
        }
    }

    let query = query
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect::<Map<_, _>>();

    let mut header_map = Map::new();
    for (name, value) in headers.iter() {
        if let Ok(text) = value.to_str() {
            header_map.insert(
                name.as_str().to_ascii_lowercase(),
                Value::String(text.to_string()),
            );
        }
    }

    let path_params = Value::Object(path_params);
    let mut request = Map::new();
    request.insert("method".into(), Value::String(method.as_str().to_string()));
    request.insert("path".into(), Value::String(uri.path().to_string()));
    request.insert("route".into(), Value::String(route_template.to_string()));
    // `params` is the in-process server's alias for `path_params`; keep
    // both so handlers written against either surface work here. The
    // clone feeds the alias before `path_params` is moved into its slot.
    request.insert("params".into(), path_params.clone());
    request.insert("path_params".into(), path_params);
    request.insert("query".into(), Value::Object(query));
    request.insert("headers".into(), Value::Object(header_map));
    request.insert(
        "body".into(),
        Value::String(String::from_utf8_lossy(body).into_owned()),
    );
    request.insert(
        "body_base64".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(body)),
    );
    request.insert("content_length".into(), Value::from(body.len()));
    request.insert("client_ip".into(), client_ip(headers));
    // Without `ConnectInfo` wiring the peer address isn't available
    // through `serve_router_from_tcp`; handlers behind a proxy should read
    // `client_ip` (X-Forwarded-For / X-Real-IP) instead.
    request.insert("remote_addr".into(), Value::Null);
    Value::Object(request)
}

/// Best-effort client IP from the conventional reverse-proxy headers.
fn client_ip(headers: &HeaderMap) -> Value {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let real_ip = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match forwarded.or(real_ip) {
        Some(ip) => Value::String(ip.to_string()),
        None => Value::Null,
    }
}
