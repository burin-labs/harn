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
//!   body, body_kind, body_base64, content_length, client_ip, remote_addr }
//! ```
//!
//! `body` is present only when the payload is valid UTF-8 (convenient for
//! JSON/text handlers); binary payloads set `body` to `nil`, `body_kind` to
//! `base64`, and `body_base64` to the standard-base64 encoding of the raw
//! bytes. A binary handler recovers the exact payload with
//! `bytes_from_base64(...)` — multipart uploads survive losslessly through
//! the JSON dispatch boundary this way.
//!
//! `remote_addr` is the real transport peer (`ip:port`) wired through from
//! the listener. `client_ip` is the *originating* client IP: by default it
//! equals the peer's IP and the spoofable `X-Forwarded-For` / `X-Real-IP`
//! headers are ignored. Configure [`SiteServerConfig::with_trusted_proxies`]
//! (CLI `--trusted-proxy <CIDR>`) with the CIDR ranges of your reverse
//! proxies to opt in: the forwarded chain is then honoured only when the
//! direct peer is itself a trusted proxy, and the client is taken as the
//! rightmost hop that is *not* a trusted proxy — never a blind
//! `split(',').next()`, which any direct caller could forge.
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
//! ## Streamed bodies (`@stream`)
//!
//! The dispatch boundary is JSON in/out, so a `.harn` handler's response
//! is fully buffered before it is rendered — fine for pages and APIs,
//! wrong for SSE. A route whose export carries the bare `@stream`
//! attribute therefore never dispatches into the VM: after the
//! [`SiteAuth`] hook and the route's `@scopes` admit the request, the
//! adapter hands the request head to the embedder's
//! [`SiteStreamProvider`] (installed with
//! [`SiteServerConfig::with_stream_provider`]) and forwards its
//! `Response` verbatim. The provider may return *any* response — an SSE
//! or chunked stream, but equally a buffered binary download with its
//! own `Content-Type`/`Content-Disposition` — so `@stream` is also the
//! seam for binary response bodies: the bytes never cross the JSON
//! dispatch boundary and are never utf8-lossied. See the trait docs for
//! the contract.
//!
//! ## Raw request bodies (`@raw`)
//!
//! `@stream` never reads the request body, which rules it out for
//! binary/multipart *uploads* (a pack publish). A route carrying the
//! bare `@raw` attribute is the complement: like `@stream` it skips the
//! VM and is answered by the same [`SiteStreamProvider`] after the same
//! admission, but the request body *is* buffered (up to the configured
//! body limit) and handed to the provider as raw [`Bytes`] — exact
//! payload, no utf8-lossy view, no base64 envelope. The provider parses
//! multipart/binary itself (it has the head dict with the
//! `content-type` boundary) and shapes any response it likes.
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
//!
//! ## Embedder-driven WebSockets (`@ws`)
//!
//! The envelope path above runs the socket *in the VM* — each frame is a
//! `.harn` dispatch. An embedder that owns the socket in Rust (a CLI
//! proxy, a fan-out event hub) needs the raw upgrade handle instead. A
//! route carrying the bare `@ws` attribute is that seam: like `@stream`
//! it skips the VM and is answered by the same [`SiteStreamProvider`]
//! after the same admission ([`SiteAuth`] hook + `@scopes`), but the
//! adapter extracts the [`WebSocketUpgrade`] (auth gates it *before* the
//! connection is handed off) and calls
//! [`SiteStreamProvider::upgrade`], forwarding the `101` it returns. The
//! provider drives the socket; the `.harn` function is a
//! declaration-only stub that owns the route, its `@scopes`, and its
//! docs. A non-WebSocket request to a `@ws`-only route is refused with
//! the correct 4xx by axum's extractor — never a stray upgrade.
//!
//! ## One route, two transports (`@ws` + `@stream`)
//!
//! A single route may carry **both** `@ws` and `@stream` — the seam the
//! gateway `/acp` carve-out needs: one route that runs auth + `@scopes`
//! once, then serves a genuine WebSocket handshake *and* an SSE/stream
//! response from the same admission. After admission the adapter sniffs
//! the request head's `Upgrade: websocket` / `Connection: upgrade`
//! headers (before any extractor or the body is consumed): a genuine
//! handshake takes the `@ws` [`SiteStreamProvider::upgrade`] path, while
//! every other request falls through to the `@stream`
//! [`SiteStreamProvider::open`] path — never a 4xx for the non-upgrade
//! caller. Auth gates both branches identically; the same provider
//! implements both `open` and `upgrade`. (`@ws` + `@raw` stays a
//! conflict: a handshake carries no body, but `@raw` exists to buffer
//! one.)

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{to_bytes, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequestParts, MatchedPath, Query, RawPathParams, Request,
    State,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use base64::Engine;
use harn_vm::TenantId;
use ipnet::IpNet;
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
    CallArguments, CallRequest, DispatchCore, DispatchError, ExportCatalog, RouteSpec,
    TransportConfig, DEFAULT_HTTP_BODY_LIMIT_BYTES,
};

/// Adapter identifier stamped on every [`CallRequest`] this host issues,
/// so trust records and span attributes attribute the dispatch to the
/// site host rather than one of the protocol adapters.
const SITE_ADAPTER: &str = "site";

/// Embedder-supplied per-request authenticator for the site surface
/// (#3212). Installed with [`SiteServerConfig::with_auth`]; without one
/// the adapter behaves exactly as before — no edge auth, every dispatch
/// tenant-less, and `@scopes` enforced only by the configured
/// [`crate::AuthPolicy`].
///
/// The hook runs once per matched route, after routing but before the
/// request body is buffered, so it sees the request head
/// ([`axum::http::request::Parts`]: method, URI, headers, extensions)
/// plus the matched [`RouteSpec`]. That is enough for the embedder
/// auth schemes a cloud platform needs: bearer API keys, cookie sessions,
/// token-in-path public routes, worker enrollment tokens.
#[async_trait]
pub trait SiteAuth: Send + Sync {
    async fn authenticate(
        &self,
        parts: &axum::http::request::Parts,
        route: &RouteSpec,
    ) -> SiteAuthOutcome;
}

/// Decision returned by [`SiteAuth::authenticate`].
pub enum SiteAuthOutcome {
    /// Admit the request under the given identity. The route's
    /// `@scopes` are then checked against [`SiteAuthContext::scopes`];
    /// a route with no `@scopes` admits any allowed request.
    Allow(SiteAuthContext),
    /// Refuse the request. The embedder-shaped response (401/403/redirect/
    /// whatever) is returned to the client verbatim.
    Deny(Box<Response>),
}

/// Identity resolved by an embedder's [`SiteAuth`] hook.
#[derive(Clone, Debug, Default)]
pub struct SiteAuthContext {
    /// Tenant the credential is bound to. Threaded into the dispatch
    /// (`CallRequest::tenant_id`) so trust records, span attributes,
    /// and the `.harn` callee's `harness.tenant.id()` all see it.
    /// `None` admits the request tenant-less (public routes).
    pub tenant_id: Option<TenantId>,
    /// Scopes the credential carries. Checked against the route's
    /// `@scopes` at admission and unioned into the authenticated
    /// principal (via [`AuthRequest::granted_scopes`]) so the
    /// dispatch-level scope check agrees with the edge.
    pub scopes: BTreeSet<String>,
    /// Stable identifier for the authenticated subject (e.g. the
    /// embedder's API-key id or session subject). Surfaced read-only to
    /// the `.harn` callee as `harness.auth.subject()` for attribution and
    /// policy. `None` admits an anonymous/public request.
    pub subject: Option<String>,
    /// Auth scheme the embedder admitted the request under (e.g.
    /// `"apikey"`, `"oauth"`, `"session"`). Surfaced as
    /// `harness.auth.scheme()`. `None` when the embedder does not
    /// classify the credential scheme.
    pub scheme: Option<String>,
    /// Optional principal classification the embedder assigned (e.g.
    /// `"operator"` vs `"tenant"` vs `"worker"`). Surfaced as
    /// `harness.auth.kind()` so a `.harn` route policy can gate on
    /// allowed principal kinds. Generic — harn-serve never interprets it.
    pub kind: Option<String>,
    /// Opaque embedder context (e.g. the API-key record or session
    /// claims). Never interpreted by harn-serve; surfaced to the
    /// embedder's [`harn_vm::HostCallBridge`] for the duration of the
    /// dispatch via [`crate::current_auth_context`].
    pub context: Option<Value>,
}

impl SiteAuthContext {
    /// Project the embedder identity into the generic
    /// [`harn_vm::AuthPrincipal`] threaded onto the dispatch as the
    /// ambient `harness.auth` handle. Carries only identity facts —
    /// subject, scheme, granted scopes, and principal kind — never the
    /// opaque embedder `context` (that stays the host-call-bridge channel
    /// via [`crate::current_auth_context`]) and never the tenant (that is
    /// the single-sourced `harness.tenant` ambient).
    pub(crate) fn principal(&self) -> harn_vm::AuthPrincipal {
        harn_vm::AuthPrincipal {
            subject: self.subject.clone().unwrap_or_default(),
            scheme: self.scheme.clone().unwrap_or_default(),
            scopes: self.scopes.clone(),
            kind: self.kind.clone(),
        }
    }
}

impl SiteAuthOutcome {
    /// Convenience constructor: wrap an embedder response in the
    /// boxed `Deny` variant.
    pub fn deny(response: Response) -> Self {
        Self::Deny(Box::new(response))
    }
}

/// Embedder-supplied responder for `@stream` (#3213) and `@raw` (#3214)
/// routes.
///
/// The host-call bridge is synchronous request/response (JSON in, JSON
/// out), so a `.harn` handler can never *stream* a body, and its bodies
/// pay the JSON-envelope encoding both ways — wrong for SSE and wrong
/// for binary payloads. The seam: a route whose `.harn` export carries
/// the `@stream` or `@raw` marker (see [`crate::exports`]) never
/// dispatches into the VM. After routing and admission — the
/// [`SiteAuth`] hook plus the route's `@scopes` — the adapter calls the
/// provider installed with [`SiteServerConfig::with_stream_provider`],
/// and forwards whatever [`Response`] it returns to the client verbatim:
/// an [`axum::response::sse::Sse`] or `Body::from_stream` flows
/// unbuffered (keep-alive and client-disconnect propagation are axum's
/// — when the client goes away the response body stream is dropped,
/// cancelling the source), and a buffered binary body with its own
/// `Content-Type`/`Content-Disposition` arrives byte-exact, untouched
/// by the utf8-lossy JSON envelope.
///
/// The two markers differ only on the *request* body: `@stream` never
/// reads one (`body` is `None`), while `@raw` buffers it up to the
/// configured limit and hands the exact bytes over (`body` is `Some`)
/// — the channel for binary/multipart uploads.
///
/// The `.harn` function under a `@stream`/`@raw` route is a
/// declaration-only stub — it owns the route, its `@scopes`, and its
/// docs, while the body source/sink lives in embedder Rust (where the
/// event store / object store is).
#[async_trait]
pub trait SiteStreamProvider: Send + Sync {
    /// Answer one admitted request.
    ///
    /// * `route` — the matched function's declared [`RouteSpec`].
    /// * `auth` — the identity the [`SiteAuth`] hook resolved (tenant,
    ///   scopes, opaque context); `None` when no hook is installed.
    /// * `request` — the request head as the same JSON dict a `.harn`
    ///   handler would receive (`method`, `path`, `route`,
    ///   `path_params`/`params`, `query`, `headers`, `client_ip`,
    ///   `remote_addr`), minus any body — the raw bytes travel in
    ///   `body`, never through the JSON dict.
    /// * `body` — `None` on a `@stream` route (the body is never read);
    ///   `Some(bytes)` on a `@raw` route: the exact request payload,
    ///   buffered up to [`crate::DEFAULT_HTTP_BODY_LIMIT_BYTES`]
    ///   (larger requests are refused with a 413 before the provider is
    ///   consulted). Multipart parsing is the provider's job — the
    ///   `content-type` header (with its boundary) is in `request`.
    ///
    /// Errors are just responses: shape a 404/410/500 the same way a
    /// success is shaped, and return it.
    async fn open(
        &self,
        route: &RouteSpec,
        auth: Option<&SiteAuthContext>,
        request: Value,
        body: Option<Bytes>,
    ) -> Response;

    /// Answer one admitted `@ws` request by completing the WebSocket
    /// upgrade and driving the socket.
    ///
    /// This is the WebSocket sibling of [`open`](Self::open): a route
    /// whose `.harn` export carries the bare `@ws` marker (see
    /// [`crate::exports`]) never dispatches into the VM. After the same
    /// admission — the [`SiteAuth`] hook plus the route's `@scopes` —
    /// the adapter extracts the [`axum::extract::ws::WebSocketUpgrade`]
    /// while the raw connection's `OnUpgrade` handle is still present,
    /// then hands it here. The provider completes the handshake with
    /// [`WebSocketUpgrade::on_upgrade`] (or harn-serve's own WS helpers)
    /// and owns the socket for its lifetime; the `101 Switching
    /// Protocols` response it returns is forwarded to the client
    /// verbatim. Auth gates the upgrade *before* the connection is handed
    /// off, exactly as it gates a `@stream` response.
    ///
    /// * `route` — the matched function's declared [`RouteSpec`].
    /// * `auth` — the identity the [`SiteAuth`] hook resolved (tenant,
    ///   scopes, opaque context); `None` when no hook is installed.
    /// * `ws` — the extracted upgrade handle. Calling `on_upgrade`
    ///   yields the live socket; the returned [`Response`] is the 101.
    /// * `request` — the request head as the same JSON dict
    ///   [`open`](Self::open) receives, minus any body (a WebSocket
    ///   handshake carries none).
    ///
    /// The default implementation refuses with `426 Upgrade Required`,
    /// so a provider that only serves `@stream`/`@raw` routes keeps
    /// compiling and a misconfigured `@ws` route fails loudly rather
    /// than 101-ing into a socket nobody drives.
    async fn upgrade(
        &self,
        route: &RouteSpec,
        _auth: Option<&SiteAuthContext>,
        _ws: WebSocketUpgrade,
        _request: Value,
    ) -> Response {
        (
            StatusCode::UPGRADE_REQUIRED,
            Json(json!({
                "code": "ws_upgrade_unsupported",
                "message": format!(
                    "route {} {} is marked @ws but this stream provider does not implement \
                     SiteStreamProvider::upgrade",
                    route.method, route.path
                ),
            })),
        )
            .into_response()
    }
}

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
    /// CIDR ranges of reverse proxies whose `X-Forwarded-For` /
    /// `X-Real-IP` headers may be trusted to derive `req.client_ip`.
    /// Empty (the default) means the headers are ignored entirely and
    /// `client_ip` is the direct transport peer — the only spoof-proof
    /// choice when the server is not strictly behind a known proxy.
    pub trusted_proxies: Vec<IpNet>,
    /// Embedder auth hook consulted on every matched route. `None`
    /// (the default) keeps the historic behavior: no edge auth and
    /// tenant-less dispatch.
    pub auth: Option<Arc<dyn SiteAuth>>,
    /// Embedder responder for `@stream` / `@raw` / `@ws` routes.
    /// Required when the script declares any — building the router fails
    /// loudly otherwise, since the route would have no body source (or,
    /// for `@ws`, no upgrade sink).
    pub stream_provider: Option<Arc<dyn SiteStreamProvider>>,
}

impl SiteServerConfig {
    pub fn new(core: DispatchCore) -> Self {
        Self {
            core,
            transport: TransportConfig::default_enabled(),
            trusted_proxies: Vec::new(),
            auth: None,
            stream_provider: None,
        }
    }

    pub fn with_transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    pub fn with_trusted_proxies(mut self, trusted_proxies: Vec<IpNet>) -> Self {
        self.trusted_proxies = trusted_proxies;
        self
    }

    /// Install an embedder [`SiteAuth`] hook. Every matched route then
    /// authenticates through it before the body is read or anything is
    /// dispatched.
    pub fn with_auth(mut self, auth: Arc<dyn SiteAuth>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Install the embedder [`SiteStreamProvider`] that answers the
    /// script's `@stream` and `@raw` routes.
    pub fn with_stream_provider(mut self, provider: Arc<dyn SiteStreamProvider>) -> Self {
        self.stream_provider = Some(provider);
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
        let SiteServerConfig {
            core,
            transport,
            trusted_proxies,
            auth,
            stream_provider,
        } = self.config;
        let catalog = Arc::new(core.catalog().clone());
        let runtime = Arc::new(DispatchRuntime::start("SITE", Arc::new(core)));
        build_site_router(
            &catalog,
            runtime,
            &transport,
            trusted_proxies,
            auth,
            stream_provider,
        )
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

/// Resolved dispatch target for one mounted `(path, method)` pair: the
/// function to invoke plus the route metadata the auth hook and the
/// scope admission check need without re-consulting the catalog.
#[derive(Clone)]
struct SiteRoute {
    function: String,
    /// The function's declared [`RouteSpec`] (method may be `*`),
    /// handed to the embedder's [`SiteAuth`] hook.
    spec: RouteSpec,
    /// Method-agnostic `@scopes(...)` baseline declared on the function;
    /// empty means no scope requirement (public, as far as scopes are
    /// concerned). Required of every method; see [`SiteRoute::scopes_for`].
    required_scopes: BTreeSet<String>,
    /// Per-method `@scopes("GET read:x", ...)` extras, keyed by uppercased
    /// HTTP method. Unioned onto `required_scopes` for the matching method;
    /// empty for the common uniform route. See [`SiteRoute::scopes_for`].
    method_scopes: BTreeMap<String, BTreeSet<String>>,
    /// `@policy(...)` allowed principal kinds declared on the function.
    /// When non-empty, admission additionally requires the hook-resolved
    /// principal's `kind` to be in this set (checked after `@scopes`).
    /// Empty for routes without a `@policy(kinds:)` guard. See
    /// [`crate::exports::RoutePolicy`].
    allowed_kinds: BTreeSet<String>,
    /// `@stream` marker: after admission, hand the request head to the
    /// [`SiteStreamProvider`] instead of dispatching into the VM. The
    /// request body is never read.
    stream: bool,
    /// `@raw` marker: like `@stream`, the route is answered by the
    /// [`SiteStreamProvider`] after admission — but the request body is
    /// buffered (up to the configured limit) and handed to the provider
    /// as exact bytes.
    raw: bool,
    /// `@ws` marker: after admission, perform the WebSocket upgrade and
    /// hand the upgrade handle to [`SiteStreamProvider::upgrade`] instead
    /// of dispatching into the VM. May be combined with `stream` (one
    /// route, two transports): when both are set the adapter sniffs the
    /// request's upgrade headers — a genuine handshake takes the
    /// [`SiteStreamProvider::upgrade`] path, every other request falls
    /// through to the [`SiteStreamProvider::open`] (`stream`) path. Still
    /// mutually exclusive with `raw` (a handshake carries no body).
    ws: bool,
}

impl SiteRoute {
    /// The scopes this route requires of `method`: the method-agnostic
    /// `required_scopes` baseline, unioned with any per-method extras
    /// declared for that method. A method with no extras (the uniform
    /// case) resolves to the baseline alone — so a route that never used
    /// the per-method `@scopes` form behaves exactly as before. The method
    /// is matched on its uppercased name, the same key the parser stores.
    ///
    /// Returns a borrowed `Cow` to keep the uniform path allocation-free:
    /// only a genuinely per-method route (a non-empty `method_scopes`
    /// entry on top of a non-empty baseline) builds an owned union.
    fn scopes_for(&self, method: &Method) -> Cow<'_, BTreeSet<String>> {
        match self.method_scopes.get(method.as_str()) {
            None => Cow::Borrowed(&self.required_scopes),
            Some(extra) if extra.is_empty() => Cow::Borrowed(&self.required_scopes),
            Some(extra) if self.required_scopes.is_empty() => Cow::Borrowed(extra),
            Some(extra) => Cow::Owned(self.required_scopes.union(extra).cloned().collect()),
        }
    }
}

/// State shared by every site route handler: the dispatch executor plus
/// the method→function table for each mounted path.
#[derive(Clone)]
struct SiteState {
    runtime: Arc<DispatchRuntime>,
    /// `path → (method → route)`. `method` is uppercased; `"*"` is the
    /// any-method fallback consulted when no exact method matches.
    routes: Arc<BTreeMap<String, BTreeMap<String, SiteRoute>>>,
    /// Reverse-proxy CIDR ranges trusted for forwarded-header parsing;
    /// empty means `client_ip` is always the direct peer.
    trusted_proxies: Arc<Vec<IpNet>>,
    /// Embedder auth hook; `None` means no edge auth (historic
    /// behavior).
    auth: Option<Arc<dyn SiteAuth>>,
    /// Embedder stream provider answering `@stream` / `@raw` routes.
    /// Present whenever the catalog declares one (router construction
    /// enforces it).
    stream_provider: Option<Arc<dyn SiteStreamProvider>>,
}

impl SiteState {
    /// Resolve the handler for a `(path, method)` pair, honouring the
    /// `*` any-method fallback.
    fn resolve(&self, path: &str, method: &Method) -> Option<&SiteRoute> {
        let methods = self.routes.get(path)?;
        methods.get(method.as_str()).or_else(|| methods.get("*"))
    }
}

/// Group the catalog's routed functions into a `path → method → fn` table
/// and mount one `any(..)` handler per path. Distinct methods on the same
/// path coexist; a duplicate method+path is a configuration error.
fn build_site_router(
    catalog: &ExportCatalog,
    runtime: Arc<DispatchRuntime>,
    transport: &TransportConfig,
    trusted_proxies: Vec<IpNet>,
    auth: Option<Arc<dyn SiteAuth>>,
    stream_provider: Option<Arc<dyn SiteStreamProvider>>,
) -> Result<Router, String> {
    let mut routes: BTreeMap<String, BTreeMap<String, SiteRoute>> = BTreeMap::new();
    for function in catalog.functions.values() {
        let Some(spec @ RouteSpec { method, path }) = function.route.as_ref() else {
            continue;
        };
        // A `@stream` / `@raw` / `@ws` route is answered by the provider,
        // so it has no body source (or upgrade sink) without one;
        // refusing to start beats serving a route that can only 500.
        if (function.stream || function.raw || function.ws) && stream_provider.is_none() {
            let marker = if function.stream {
                "@stream"
            } else if function.raw {
                "@raw"
            } else {
                "@ws"
            };
            return Err(format!(
                "route {method} {path} (`{}`) is marked {marker} but no stream provider is \
                 configured; install one with SiteServerConfig::with_stream_provider(...)",
                function.name
            ));
        }
        let by_method = routes.entry(path.clone()).or_default();
        let entry = SiteRoute {
            function: function.name.clone(),
            spec: spec.clone(),
            required_scopes: function.required_scopes.clone(),
            method_scopes: function.method_scopes.clone(),
            allowed_kinds: function
                .policy
                .as_ref()
                .map(|policy| policy.allowed_kinds.clone())
                .unwrap_or_default(),
            stream: function.stream,
            raw: function.raw,
            ws: function.ws,
        };
        if let Some(existing) = by_method.insert(method.clone(), entry) {
            return Err(format!(
                "route conflict: {method} {path} is claimed by both `{}` and \
                 `{}`; give one of them a distinct @route(...)",
                existing.function, function.name
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
        trusted_proxies: Arc::new(trusted_proxies),
        auth,
        stream_provider,
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

/// Scope back-stop for provider routes (`@ws` / `@stream` / `@raw`).
///
/// These routes never build a `CallRequest`, so the dispatch-level
/// `AuthPolicy` scope check that back-stops a plain route cannot cover
/// them. With a `SiteAuth` hook installed, the admission check at the top
/// of [`site_dispatch`] already compared the method-resolved requirement
/// against the hook-granted scopes; this helper is the *no-hook* case —
/// no identity means no granted scopes, so any non-empty requirement
/// refuses, matching the allow-all default a plain route would hit at
/// dispatch. Returns `Some(403)` to short-circuit, or `None` to admit.
fn provider_scope_backstop(
    auth_context: Option<&SiteAuthContext>,
    required_scopes: &BTreeSet<String>,
    request_id: &str,
) -> Option<Response> {
    if auth_context.is_none() && !required_scopes.is_empty() {
        return Some(axum_response_from_dispatch_error(
            DispatchError::Forbidden {
                required: required_scopes.clone(),
                granted: BTreeSet::new(),
            },
            request_id,
        ));
    }
    None
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

    // The connect-info make service (see `serve_router_from_tcp`) stashes
    // the transport peer here; `oneshot`-driven tests insert it directly.
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);

    let method = parts.method.clone();
    let route_template = matched_path.unwrap_or_else(|| parts.uri.path().to_string());
    let Some(route) = state.resolve(&route_template, &method).cloned() else {
        return method_not_allowed(&state, &route_template);
    };
    let function = route.function.clone();
    let request_id = fresh_request_id();

    // Embedder auth runs on the request head, before the body is
    // buffered: an unauthenticated caller never costs a body read or a
    // dispatch. `Deny` short-circuits with the embedder's response
    // verbatim; `Allow` is then checked against the route's `@scopes`
    // (a route without `@scopes` has no scope requirement).
    // The scopes this route requires of *this* request's method: the
    // method-agnostic baseline unioned with any per-method `@scopes`
    // extras (uniform routes resolve to the baseline, allocation-free).
    let required_scopes = route.scopes_for(&method);
    let auth_context = match state.auth.as_ref() {
        None => None,
        Some(hook) => match hook.authenticate(&parts, &route.spec).await {
            SiteAuthOutcome::Deny(response) => return *response,
            SiteAuthOutcome::Allow(context) => {
                if !required_scopes.is_subset(&context.scopes) {
                    return axum_response_from_dispatch_error(
                        DispatchError::Forbidden {
                            required: required_scopes.into_owned(),
                            granted: context.scopes,
                        },
                        &request_id,
                    );
                }
                // `@policy(kinds: ...)` composes with `@scopes`: once the
                // scope floor is met, a route that declares allowed
                // principal kinds additionally requires the hook-resolved
                // principal's `kind` to be one of them. A request whose
                // principal carries no kind (the embedder did not classify
                // it) can never satisfy a non-empty allow-set, so it is
                // refused — fail-closed on the principal-class gate.
                if !route.allowed_kinds.is_empty()
                    && !context
                        .kind
                        .as_deref()
                        .is_some_and(|kind| route.allowed_kinds.contains(kind))
                {
                    return axum_response_from_dispatch_error(
                        DispatchError::ForbiddenPrincipalKind {
                            allowed: route.allowed_kinds.clone(),
                        },
                        &request_id,
                    );
                }
                Some(context)
            }
        },
    };

    // `@ws` routes hand off to the embedder's provider after the *same*
    // admission as `@stream`/`@raw` — auth gates the upgrade before the
    // connection is handed off — but instead of producing a response
    // body, the adapter extracts the WebSocket upgrade (while hyper's
    // `OnUpgrade` extension is still on `parts`) and the provider drives
    // the socket. The 101 it returns is forwarded verbatim.
    //
    // A route may carry `@ws` *and* `@stream` together — one route, two
    // transports (the gateway `/acp` carve-out): a genuine WebSocket
    // handshake upgrades through `provider.upgrade(...)`, while every
    // other request falls through to the `@stream` `provider.open(...)`
    // path below. The branch is picked by sniffing the request's
    // `Upgrade`/`Connection` headers on the head we already hold — before
    // any extractor or the body is consumed — so neither branch races the
    // other for the request. A `@ws`-only route keeps the historic
    // behavior: a non-upgrade request is refused by axum's extractor with
    // the correct 4xx (there is no stream path to fall through to).
    if route.ws && (is_websocket_upgrade(&parts.headers) || !route.stream) {
        // Same admission back-stop as `@stream`/`@raw`, resolved per
        // method (see `provider_scope_backstop`).
        if let Some(response) =
            provider_scope_backstop(auth_context.as_ref(), &required_scopes, &request_id)
        {
            return response;
        }
        let Some(provider) = state.stream_provider.as_ref() else {
            // Unreachable: router construction refuses a @ws route
            // without a provider. Shape a loud 500 rather than panic.
            return axum_response_from_dispatch_error(
                DispatchError::Execution(format!(
                    "provider route {route_template} has no stream provider"
                )),
                &request_id,
            );
        };
        // Extract the upgrade while the `OnUpgrade` extension is still
        // present on `parts`. On a `@ws`-only route a non-WebSocket
        // request (missing the upgrade headers / key) is refused here by
        // axum's extractor with the correct 4xx — never a panic, never a
        // stray 101. On a combined `@ws @stream` route we only reach this
        // when the header sniff already saw a genuine handshake.
        let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(upgrade) => upgrade,
            Err(rejection) => return rejection.into_response(),
        };
        // A WebSocket handshake carries no body, so none is built.
        let request = build_request_value(
            &method,
            &parts.uri,
            &route_template,
            raw_params.as_ref(),
            &query,
            &parts.headers,
            &[],
            peer,
            &state.trusted_proxies,
        );
        return provider
            .upgrade(&route.spec, auth_context.as_ref(), upgrade, request)
            .await;
    }

    // `@stream` / `@raw` routes hand off to the embedder's provider
    // right after admission, with no VM dispatch — the provider's
    // Response is forwarded verbatim, so an SSE/stream body flows
    // unbuffered and a binary body arrives byte-exact. The only
    // difference between the markers is the request body: `@stream`
    // never reads one, `@raw` buffers it and hands the exact bytes over.
    if route.stream || route.raw {
        // A provider route never builds a `CallRequest`, so the
        // dispatch-level `AuthPolicy` scope check cannot back-stop it the
        // way it does for plain routes (see `provider_scope_backstop`).
        if let Some(response) =
            provider_scope_backstop(auth_context.as_ref(), &required_scopes, &request_id)
        {
            return response;
        }
        let Some(provider) = state.stream_provider.as_ref() else {
            // Unreachable: router construction refuses a @stream/@raw
            // route without a provider. Shape a loud 500 rather than
            // panic.
            return axum_response_from_dispatch_error(
                DispatchError::Execution(format!(
                    "provider route {route_template} has no stream provider"
                )),
                &request_id,
            );
        };
        // Body buffering happens after admission: an unauthenticated
        // caller never costs a body read here either.
        let raw_body = if route.raw {
            match buffer_request_body(body).await {
                Ok(bytes) => Some(bytes),
                Err(response) => return response,
            }
        } else {
            None
        };
        let request = build_request_value(
            &method,
            &parts.uri,
            &route_template,
            raw_params.as_ref(),
            &query,
            &parts.headers,
            &[],
            peer,
            &state.trusted_proxies,
        );
        return provider
            .open(&route.spec, auth_context.as_ref(), request, raw_body)
            .await;
    }

    let wants_upgrade = is_websocket_upgrade(&parts.headers);

    // A WebSocket handshake carries no body; reading it would block on a
    // socket that never sends one. Every other method buffers up to the
    // configured limit (the `DefaultBodyLimit` layer rejects larger).
    let body_bytes = if wants_upgrade {
        Bytes::new()
    } else {
        match buffer_request_body(body).await {
            Ok(bytes) => bytes,
            Err(response) => return response,
        }
    };

    let req_value = build_request_value(
        &method,
        &parts.uri,
        &route_template,
        raw_params.as_ref(),
        &query,
        &parts.headers,
        &body_bytes,
        peer,
        &state.trusted_proxies,
    );
    let mut auth = AuthRequest::from_http(
        &method,
        parts.uri.path(),
        body_bytes.to_vec(),
        &parts.headers,
    );
    // The hook-resolved scopes feed the dispatch-level scope check too,
    // so a configured `AuthPolicy` (or the allow-all default) agrees
    // with the admission decision made above instead of re-denying a
    // route the embedder explicitly allowed.
    if let Some(context) = auth_context.as_ref() {
        auth.granted_scopes = context.scopes.clone();
    }

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
        agent_event_sink: None,
        actor_chain: None,
        actor_chain_hop: None,
        progress: None,
        tenant_id: auth_context
            .as_ref()
            .and_then(|context| context.tenant_id.clone()),
        request_id: Some(request_id.clone()),
        auth_context: auth_context
            .as_ref()
            .and_then(|context| context.context.clone()),
        auth_principal: auth_context.as_ref().map(SiteAuthContext::principal),
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
            return upgrade_websocket(&mut parts, state.runtime, spec, auth_context).await;
        }
    }

    axum_response_from_call(response, &request_id)
}

/// Buffer a request body up to [`DEFAULT_HTTP_BODY_LIMIT_BYTES`],
/// shaping the canonical 413 envelope when it is larger (the
/// `DefaultBodyLimit` layer caps the stream; `to_bytes` surfaces the
/// overflow as an error).
async fn buffer_request_body(body: axum::body::Body) -> Result<Bytes, Response> {
    to_bytes(body, DEFAULT_HTTP_BODY_LIMIT_BYTES)
        .await
        .map_err(|_| {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "code": "request_body_too_large",
                    "message": format!(
                        "request body exceeds the {DEFAULT_HTTP_BODY_LIMIT_BYTES}-byte limit"
                    ),
                })),
            )
                .into_response()
        })
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
    auth_context: Option<SiteAuthContext>,
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
        let auth_context = auth_context.clone();
        async move {
            drive_ws_session(session, runtime, on_message, auth_context).await;
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
///
/// `auth_context` is the identity the [`SiteAuth`] hook resolved for
/// the upgrade request; every frame dispatched over the socket carries
/// the same tenant / scopes / embedder context, since the frames belong
/// to the connection that was admitted.
async fn drive_ws_session(
    session: WsSession,
    runtime: Arc<DispatchRuntime>,
    on_message: Option<String>,
    auth_context: Option<SiteAuthContext>,
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
        let auth = AuthRequest {
            granted_scopes: auth_context
                .as_ref()
                .map(|context| context.scopes.clone())
                .unwrap_or_default(),
            ..AuthRequest::default()
        };
        let call = CallRequest {
            adapter: SITE_ADAPTER.to_string(),
            function: handler.clone(),
            arguments: CallArguments::Positional(vec![message_value]),
            auth,
            caller: SITE_ADAPTER.to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: auth_context
                .as_ref()
                .and_then(|context| context.tenant_id.clone()),
            request_id: Some(fresh_request_id()),
            auth_context: auth_context
                .as_ref()
                .and_then(|context| context.context.clone()),
            auth_principal: auth_context.as_ref().map(SiteAuthContext::principal),
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
/// embedded server and a hosted site, with strict text-or-base64 body
/// fields so binary payloads never pass through UTF-8 replacement.
#[allow(clippy::too_many_arguments)]
fn build_request_value(
    method: &Method,
    uri: &axum::http::Uri,
    route_template: &str,
    raw_params: Option<&RawPathParams>,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
    peer: Option<SocketAddr>,
    trusted_proxies: &[IpNet],
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
    match std::str::from_utf8(body) {
        Ok(text) => {
            request.insert("body".into(), Value::String(text.to_string()));
            request.insert("body_kind".into(), Value::String("text".to_string()));
        }
        Err(_) => {
            request.insert("body".into(), Value::Null);
            request.insert("body_kind".into(), Value::String("base64".to_string()));
        }
    }
    request.insert(
        "body_base64".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(body)),
    );
    request.insert("content_length".into(), Value::from(body.len()));
    request.insert(
        "client_ip".into(),
        resolve_client_ip(peer.map(|addr| addr.ip()), headers, trusted_proxies)
            .map(|ip| Value::String(ip.to_string()))
            .unwrap_or(Value::Null),
    );
    request.insert(
        "remote_addr".into(),
        peer.map(|addr| Value::String(addr.to_string()))
            .unwrap_or(Value::Null),
    );
    Value::Object(request)
}

/// Resolve the originating client IP.
///
/// The `X-Forwarded-For` / `X-Real-IP` headers are attacker-controlled
/// unless every hop between the client and this process is a proxy we
/// trust to rewrite them. So the headers are honoured only when
/// `trusted_proxies` is non-empty *and* the direct transport peer is one
/// of those proxies; otherwise the peer IP is authoritative.
///
/// When trusted, the client is the rightmost `X-Forwarded-For` entry that
/// is *not* itself a trusted proxy (walking right-to-left peels off our
/// own proxy hops). If every entry is trusted, the leftmost is the
/// originator. `X-Real-IP` is consulted only as a single-value fallback
/// when `X-Forwarded-For` yields nothing usable.
fn resolve_client_ip(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    trusted_proxies: &[IpNet],
) -> Option<IpAddr> {
    let peer = peer?;
    let is_trusted = |ip: &IpAddr| trusted_proxies.iter().any(|net| net.contains(ip));
    if trusted_proxies.is_empty() || !is_trusted(&peer) {
        return Some(peer);
    }

    let forwarded: Vec<IpAddr> = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
                .collect()
        })
        .unwrap_or_default();
    if let Some(client) = forwarded.iter().rev().find(|ip| !is_trusted(ip)) {
        return Some(*client);
    }
    if let Some(originator) = forwarded.first() {
        return Some(*originator);
    }

    let real_ip = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok());
    Some(real_ip.unwrap_or(peer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(cidrs: &[&str]) -> Vec<IpNet> {
        cidrs.iter().map(|c| c.parse().unwrap()).collect()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn no_peer_resolves_to_none() {
        assert_eq!(
            resolve_client_ip(None, &headers(&[]), &nets(&["10.0.0.0/8"])),
            None
        );
    }

    #[test]
    fn untrusted_config_returns_peer_ignoring_headers() {
        let got = resolve_client_ip(
            Some(ip("203.0.113.1")),
            &headers(&[("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "5.6.7.8")]),
            &[],
        );
        assert_eq!(got, Some(ip("203.0.113.1")));
    }

    #[test]
    fn real_ip_is_the_fallback_when_no_forwarded_for() {
        let got = resolve_client_ip(
            Some(ip("10.0.0.5")),
            &headers(&[("x-real-ip", "9.9.9.9")]),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(got, Some(ip("9.9.9.9")));
    }

    #[test]
    fn all_trusted_chain_falls_back_to_leftmost_originator() {
        // Every hop is internal: the originator is the leftmost entry.
        let got = resolve_client_ip(
            Some(ip("10.0.0.5")),
            &headers(&[("x-forwarded-for", "10.0.0.1, 10.0.0.2")]),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(got, Some(ip("10.0.0.1")));
    }

    #[test]
    fn ipv6_proxy_and_client_are_supported() {
        let got = resolve_client_ip(
            Some(ip("2001:db8::1")),
            &headers(&[("x-forwarded-for", "2606:4700::1234")]),
            &nets(&["2001:db8::/32"]),
        );
        assert_eq!(got, Some(ip("2606:4700::1234")));
    }

    #[test]
    fn garbage_forwarded_entries_are_skipped() {
        let got = resolve_client_ip(
            Some(ip("10.0.0.5")),
            &headers(&[("x-forwarded-for", "not-an-ip, 1.2.3.4, also-bad")]),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(got, Some(ip("1.2.3.4")));
    }
}
