use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Extension, OriginalUri, Query};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::secrets::{SecretId, SecretProvider, SecretVersion};
use tracing::Instrument as _;

use crate::commands::orchestrator::origin_guard::{enforce_allowed_origin, OriginAllowList};
use crate::commands::orchestrator::tls::{ServerRuntime, TlsFiles};
use crate::package::{CollectedManifestTrigger, TriggerKind};

const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
const ADMIN_RELOAD_PATH: &str = "/admin/reload";
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const REQUEST_ENTERED_FILE_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_ENTERED_FILE";
const REQUEST_RELEASE_FILE_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_RELEASE_FILE";
const API_KEYS_ENV: &str = "HARN_ORCHESTRATOR_API_KEYS";
const HMAC_SECRET_ENV: &str = "HARN_ORCHESTRATOR_HMAC_SECRET";
const AUTH_TIMESTAMP_WINDOW_SECS: i64 = 5 * 60;
const ACP_PATH: &str = "/acp";
const ACP_TOPIC_PREFIX: &str = "acp.session";
const ACP_PING_INTERVAL: Duration = Duration::from_secs(30);
const ACP_PONG_TIMEOUT: Duration = Duration::from_secs(10);
const INGEST_GLOBAL_CAPACITY_ENV: &str = "HARN_ORCHESTRATOR_INGEST_GLOBAL_CAPACITY";
const INGEST_PER_SOURCE_CAPACITY_ENV: &str = "HARN_ORCHESTRATOR_INGEST_PER_SOURCE_CAPACITY";
const INGEST_REFILL_PER_SEC_ENV: &str = "HARN_ORCHESTRATOR_INGEST_REFILL_PER_SEC";
const DEFAULT_INGEST_GLOBAL_CAPACITY: u32 = 4096;
const DEFAULT_INGEST_PER_SOURCE_CAPACITY: u32 = 1024;
const DEFAULT_INGEST_REFILL_PER_SEC: u32 = 1024;
const ACP_RETAINED_SESSION_SECS_ENV: &str = "HARN_ACP_WS_RETAIN_SECS";
const ACP_DEFAULT_RETAINED_SESSION_SECS: u64 = 5 * 60;
const ACP_REPLAY_BUFFER_LIMIT: usize = 4096;

#[derive(Clone)]
pub(crate) struct ListenerConfig {
    pub(crate) bind: std::net::SocketAddr,
    pub(crate) tls: Option<TlsFiles>,
    pub(crate) event_log: Arc<AnyEventLog>,
    pub(crate) secrets: Arc<dyn SecretProvider>,
    pub(crate) allowed_origins: OriginAllowList,
    pub(crate) max_body_bytes: usize,
    pub(crate) metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pub(crate) admin_reload: Option<AdminReloadHandle>,
    pub(crate) mcp_router: Option<Router>,
    pub(crate) routes: Vec<RouteConfig>,
    pub(crate) tenant_store: Option<Arc<harn_vm::TenantStore>>,
}

impl ListenerConfig {
    pub(crate) fn max_body_bytes_or_default(max_body_bytes: Option<usize>) -> usize {
        max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES)
    }
}

pub(crate) struct ListenerRuntime {
    server: ServerRuntime,
    routes: Arc<RouteRegistry>,
    readiness: Arc<ListenerReadiness>,
}

#[derive(Default)]
struct ListenerReadiness {
    ready: AtomicBool,
}

impl ListenerReadiness {
    fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    fn mark_not_ready(&self) {
        self.ready.store(false, Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

pub(crate) struct AdminReloadRequest {
    pub(crate) source: String,
    pub(crate) response_tx: Option<oneshot::Sender<Result<JsonValue, String>>>,
}

#[derive(Clone)]
pub(crate) struct AdminReloadHandle {
    tx: mpsc::UnboundedSender<AdminReloadRequest>,
}

impl AdminReloadHandle {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<AdminReloadRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub(crate) fn trigger(&self, source: impl Into<String>) -> Result<(), String> {
        self.tx
            .send(AdminReloadRequest {
                source: source.into(),
                response_tx: None,
            })
            .map_err(|_| "reload channel is closed".to_string())
    }

    pub(crate) async fn request(&self, source: impl Into<String>) -> Result<JsonValue, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(AdminReloadRequest {
                source: source.into(),
                response_tx: Some(tx),
            })
            .map_err(|_| "reload channel is closed".to_string())?;
        rx.await
            .map_err(|_| "reload response channel closed".to_string())?
    }
}

impl ListenerRuntime {
    pub(crate) async fn start(config: ListenerConfig) -> Result<Self, String> {
        let pending_topic =
            Topic::new(PENDING_TOPIC).map_err(|error| format!("invalid pending topic: {error}"))?;
        let inbox_metrics = Arc::new(harn_vm::MetricsRegistry::default());
        let inbox = Arc::new(
            harn_vm::InboxIndex::new(config.event_log.clone(), inbox_metrics)
                .await
                .map_err(|error| format!("failed to initialize inbox index: {error}"))?,
        );
        let requires_auth = config
            .routes
            .iter()
            .any(|route| route.auth_mode.requires_credentials());
        let auth = Arc::new(ListenerAuth::from_env(requires_auth)?);
        let request_gate = TestRequestGate {
            entered_file: test_file_from_env(REQUEST_ENTERED_FILE_ENV),
            release_file: test_file_from_env(REQUEST_RELEASE_FILE_ENV),
        };
        let origin_state = Arc::new(config.allowed_origins.clone());
        let admin_state = config.admin_reload.clone().map(|reload| {
            Arc::new(AdminReloadState {
                event_log: config.event_log.clone(),
                auth: auth.clone(),
                reload,
            })
        });
        let acp_hub = AcpWebSocketHub::new(
            config.event_log.clone(),
            acp_retained_session_duration_from_env(),
        );
        let acp_hub_sweeper = acp_hub.clone();
        tokio::spawn(async move {
            acp_hub_sweeper.run_expiry_sweeper().await;
        });
        let acp_state = Arc::new(AcpWebSocketState {
            event_log: config.event_log.clone(),
            auth: auth.clone(),
            pipeline: None,
            hub: acp_hub,
        });
        let routes = Arc::new(RouteRegistry::new(
            config.routes,
            config.event_log.clone(),
            inbox,
            config.secrets.clone(),
            config.metrics_registry.clone(),
            auth.clone(),
            pending_topic.clone(),
            request_gate,
            config.tenant_store.clone(),
        )?);
        let readiness = Arc::new(ListenerReadiness::default());
        let mut app = Router::new()
            .route(
                "/health",
                get(|| async move { (StatusCode::OK, "ok").into_response() }),
            )
            .route(
                "/healthz",
                get(|| async move { (StatusCode::OK, "ok").into_response() }),
            )
            .route(
                "/readyz",
                get(readyz_endpoint).layer(Extension(readiness.clone())),
            )
            .route(
                "/metrics",
                get(metrics_endpoint).layer(Extension(config.metrics_registry.clone())),
            );
        app = app.route(
            ACP_PATH,
            get(acp_websocket_endpoint).layer(Extension(acp_state)),
        );
        if let Some(admin_state) = admin_state {
            app = app.route(
                ADMIN_RELOAD_PATH,
                post(admin_reload_endpoint).layer(Extension(admin_state)),
            );
        }
        if let Some(mcp_router) = config.mcp_router {
            app = app.merge(mcp_router);
        }
        let app = app.route(
            "/{*path}",
            post(ingest_trigger).layer(Extension(routes.clone())),
        );

        let app = app
            .layer(DefaultBodyLimit::max(config.max_body_bytes))
            .layer(middleware::from_fn_with_state(
                origin_state.clone(),
                enforce_allowed_origin,
            ));

        let server = ServerRuntime::start(config.bind, app, config.tls.as_ref()).await?;
        Ok(Self {
            server,
            routes,
            readiness,
        })
    }

    pub(crate) fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    pub(crate) fn scheme(&self) -> &'static str {
        if self.server.tls_enabled() {
            "https"
        } else {
            "http"
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("{}://{}", self.scheme(), self.local_addr())
    }

    pub(crate) fn mark_ready(&self) {
        self.readiness.mark_ready();
    }

    pub(crate) fn mark_not_ready(&self) {
        self.readiness.mark_not_ready();
    }

    pub(crate) fn trigger_metrics(&self) -> BTreeMap<String, TriggerMetricSnapshot> {
        self.routes.snapshot_metrics()
    }

    pub(crate) fn reload_routes(&self, routes: Vec<RouteConfig>) -> Result<(), String> {
        self.routes.reload(routes)
    }

    pub(crate) async fn shutdown(
        self,
        timeout: Duration,
    ) -> Result<BTreeMap<String, TriggerMetricSnapshot>, String> {
        let Self { server, routes, .. } = self;
        server.shutdown(timeout).await?;
        Ok(routes.snapshot_metrics())
    }
}

#[derive(Clone)]
pub(crate) struct RouteConfig {
    pub(crate) trigger_id: String,
    pub(crate) binding_version: u32,
    pub(crate) provider: harn_vm::ProviderId,
    pub(crate) path: String,
    pub(crate) auth_mode: AuthMode,
    pub(crate) signature_mode: SignatureMode,
    pub(crate) signing_secret: Option<SecretId>,
    pub(crate) dedupe_key_template: Option<String>,
    pub(crate) dedupe_retention_days: u32,
    pub(crate) connector_ingress: bool,
    pub(crate) connector: Option<harn_vm::connectors::ConnectorHandle>,
}

impl RouteConfig {
    fn dedupe_ttl(&self) -> Duration {
        Duration::from_secs(u64::from(self.dedupe_retention_days.max(1)) * 24 * 60 * 60)
    }

    pub(crate) fn from_trigger(
        trigger: &CollectedManifestTrigger,
        binding_version: u32,
    ) -> Result<Option<Self>, String> {
        match trigger.config.kind {
            TriggerKind::Webhook => {
                let provider = trigger.config.provider.clone();
                let signature_mode = match provider.as_str() {
                    "github" => SignatureMode::GitHub,
                    "linear" => SignatureMode::Unsigned,
                    "webhook" => SignatureMode::Standard,
                    "slack" => SignatureMode::Unsigned,
                    "notion" => SignatureMode::Unsigned,
                    other => match harn_vm::provider_metadata(other) {
                        Some(metadata)
                            if matches!(
                                metadata.runtime,
                                harn_vm::ProviderRuntimeMetadata::Placeholder
                            ) =>
                        {
                            SignatureMode::Unsigned
                        }
                        _ => {
                            return Err(format!(
                                "HTTP listener does not yet support webhook provider '{other}' on this branch"
                            ))
                        }
                    },
                };
                Ok(Some(Self {
                    trigger_id: trigger.config.id.clone(),
                    binding_version,
                    provider,
                    path: trigger_path(trigger)?,
                    auth_mode: AuthMode::Public,
                    signature_mode,
                    signing_secret: parse_secret_id(
                        trigger
                            .config
                            .secrets
                            .get("signing_secret")
                            .map(String::as_str),
                    ),
                    dedupe_key_template: trigger.config.dedupe_key.clone(),
                    dedupe_retention_days: trigger.config.retry.retention_days,
                    connector_ingress: false,
                    connector: None,
                }))
            }
            TriggerKind::A2aPush => {
                let connector_ingress = a2a_push_connector_configured(trigger);
                Ok(Some(Self {
                    trigger_id: trigger.config.id.clone(),
                    binding_version,
                    provider: harn_vm::ProviderId::from("a2a-push"),
                    path: trigger_path(trigger)?,
                    auth_mode: if connector_ingress {
                        AuthMode::Public
                    } else {
                        AuthMode::BearerOrHmac
                    },
                    signature_mode: SignatureMode::Unsigned,
                    signing_secret: None,
                    dedupe_key_template: trigger.config.dedupe_key.clone(),
                    dedupe_retention_days: trigger.config.retry.retention_days,
                    connector_ingress,
                    connector: None,
                }))
            }
            TriggerKind::Stream => {
                if !trigger.config.kind_specific.contains_key("path") {
                    return Ok(None);
                }
                Ok(Some(Self {
                    trigger_id: trigger.config.id.clone(),
                    binding_version,
                    provider: trigger.config.provider.clone(),
                    path: trigger_path(trigger)?,
                    auth_mode: AuthMode::Public,
                    signature_mode: SignatureMode::Unsigned,
                    signing_secret: None,
                    dedupe_key_template: trigger.config.dedupe_key.clone(),
                    dedupe_retention_days: trigger.config.retry.retention_days,
                    connector_ingress: true,
                    connector: None,
                }))
            }
            _ => Ok(None),
        }
    }
}

fn a2a_push_connector_configured(trigger: &CollectedManifestTrigger) -> bool {
    if !matches!(trigger.config.kind, TriggerKind::A2aPush) {
        return false;
    }
    let config = &trigger.config.kind_specific;
    if config
        .get("a2a_push")
        .and_then(toml::Value::as_table)
        .is_some_and(|table| !table.is_empty())
    {
        return true;
    }
    [
        "expected_iss",
        "expected_aud",
        "jwks_url",
        "auth_scheme",
        "expected_token",
        "token",
    ]
    .iter()
    .any(|field| config.contains_key(*field))
}

#[derive(Clone)]
struct RouteContext {
    route: RouteConfig,
    event_log: Arc<AnyEventLog>,
    inbox: Arc<harn_vm::InboxIndex>,
    secrets: Arc<dyn SecretProvider>,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    ingest_backpressure: IngestBackpressure,
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_gate: TestRequestGate,
    tenant_store: Option<Arc<harn_vm::TenantStore>>,
    metrics: Arc<RouteRuntimeMetrics>,
}

#[derive(Clone)]
struct ResolvedRoute {
    context: Arc<RouteContext>,
    path_tenant_id: Option<String>,
}

#[derive(Clone)]
struct TenantRequestScope {
    scope: harn_vm::TenantScope,
    credential_authenticated: bool,
}

#[derive(Clone, Default)]
struct TestRequestGate {
    entered_file: Option<PathBuf>,
    release_file: Option<PathBuf>,
}

#[derive(Clone)]
struct AdminReloadState {
    event_log: Arc<AnyEventLog>,
    auth: Arc<ListenerAuth>,
    reload: AdminReloadHandle,
}

#[derive(Clone)]
struct AcpWebSocketState {
    event_log: Arc<AnyEventLog>,
    auth: Arc<ListenerAuth>,
    pipeline: Option<String>,
    hub: Arc<AcpWebSocketHub>,
}

struct AcpWebSocketHub {
    state: Mutex<AcpWebSocketHubState>,
    event_log: Arc<AnyEventLog>,
    retention: Duration,
}

#[derive(Default)]
struct AcpWebSocketHubState {
    workers_by_id: BTreeMap<String, Arc<AcpWorker>>,
    workers_by_session: BTreeMap<String, Arc<AcpWorker>>,
}

struct AcpWorker {
    id: String,
    request_tx: Mutex<Option<mpsc::UnboundedSender<JsonValue>>>,
    socket_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    active_connection_id: Mutex<Option<String>>,
    sessions: Mutex<BTreeSet<String>>,
    replay_buffer: Mutex<VecDeque<AcpReplayEvent>>,
    next_event_id: AtomicU64,
    detached_at: Mutex<Option<Instant>>,
    event_log: Arc<AnyEventLog>,
    hub: Weak<AcpWebSocketHub>,
}

#[derive(Clone)]
struct AcpReplayEvent {
    id: u64,
    line: String,
    session_id: Option<String>,
}

#[derive(Debug)]
enum AcpAttachError {
    NotFound,
    AlreadyAttached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthMode {
    Public,
    BearerOrHmac,
}

impl AuthMode {
    fn requires_credentials(self) -> bool {
        !matches!(self, Self::Public)
    }
}

struct RouteRegistry {
    routes_by_path: RwLock<BTreeMap<String, Arc<RouteContext>>>,
    metrics_by_trigger_id: Mutex<BTreeMap<String, Arc<RouteRuntimeMetrics>>>,
    ingest_backpressure: IngestBackpressure,
    event_log: Arc<AnyEventLog>,
    inbox: Arc<harn_vm::InboxIndex>,
    secrets: Arc<dyn SecretProvider>,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_gate: TestRequestGate,
    tenant_store: Option<Arc<harn_vm::TenantStore>>,
}

impl RouteRegistry {
    #[allow(clippy::too_many_arguments)]
    fn new(
        routes: Vec<RouteConfig>,
        event_log: Arc<AnyEventLog>,
        inbox: Arc<harn_vm::InboxIndex>,
        secrets: Arc<dyn SecretProvider>,
        metrics_registry: Arc<harn_vm::MetricsRegistry>,
        auth: Arc<ListenerAuth>,
        pending_topic: Topic,
        request_gate: TestRequestGate,
        tenant_store: Option<Arc<harn_vm::TenantStore>>,
    ) -> Result<Self, String> {
        let registry = Self {
            routes_by_path: RwLock::new(BTreeMap::new()),
            metrics_by_trigger_id: Mutex::new(BTreeMap::new()),
            ingest_backpressure: IngestBackpressure::from_env(),
            event_log,
            inbox,
            secrets,
            metrics_registry,
            auth,
            pending_topic,
            request_gate,
            tenant_store,
        };
        registry.reload(routes)?;
        Ok(registry)
    }

    fn reload(&self, routes: Vec<RouteConfig>) -> Result<(), String> {
        validate_unique_route_paths(&routes)?;
        let mut next_routes = BTreeMap::new();
        let mut metrics_by_trigger_id = self
            .metrics_by_trigger_id
            .lock()
            .expect("route metrics poisoned");
        for route in routes {
            let metrics = metrics_by_trigger_id
                .entry(route.trigger_id.clone())
                .or_insert_with(|| Arc::new(RouteRuntimeMetrics::default()))
                .clone();
            next_routes.insert(
                route.path.clone(),
                Arc::new(RouteContext {
                    route,
                    event_log: self.event_log.clone(),
                    inbox: self.inbox.clone(),
                    secrets: self.secrets.clone(),
                    metrics_registry: self.metrics_registry.clone(),
                    ingest_backpressure: self.ingest_backpressure.clone(),
                    auth: self.auth.clone(),
                    pending_topic: self.pending_topic.clone(),
                    request_gate: self.request_gate.clone(),
                    tenant_store: self.tenant_store.clone(),
                    metrics,
                }),
            );
        }
        *self.routes_by_path.write().expect("route table poisoned") = next_routes;
        Ok(())
    }

    fn resolve(&self, path: &str) -> Option<ResolvedRoute> {
        let routes = self.routes_by_path.read().expect("route table poisoned");
        if let Some(context) = routes.get(path).cloned() {
            return Some(ResolvedRoute {
                context,
                path_tenant_id: None,
            });
        }
        let (tenant_id, route_path) = tenant_path_prefix(path)?;
        routes
            .get(&route_path)
            .cloned()
            .map(|context| ResolvedRoute {
                context,
                path_tenant_id: Some(tenant_id),
            })
    }

    fn snapshot_metrics(&self) -> BTreeMap<String, TriggerMetricSnapshot> {
        self.metrics_by_trigger_id
            .lock()
            .expect("route metrics poisoned")
            .iter()
            .map(|(trigger_id, metrics)| (trigger_id.clone(), metrics.snapshot()))
            .collect()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SignatureMode {
    GitHub,
    Standard,
    Unsigned,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TriggerMetricSnapshot {
    pub(crate) received: u64,
    pub(crate) dispatched: u64,
    pub(crate) failed: u64,
    pub(crate) in_flight: u64,
}

#[derive(Default)]
struct RouteRuntimeMetrics {
    received: AtomicU64,
    dispatched: AtomicU64,
    failed: AtomicU64,
    in_flight: AtomicU64,
}

#[derive(Clone, Debug)]
struct IngestBackpressure {
    config: IngestBackpressureConfig,
    state: Arc<Mutex<IngestBackpressureState>>,
}

#[derive(Clone, Copy, Debug)]
struct IngestBackpressureConfig {
    global_capacity: u32,
    per_source_capacity: u32,
    refill_per_sec: u32,
}

#[derive(Debug)]
struct IngestBackpressureState {
    global: IngestBucket,
    sources: BTreeMap<String, IngestBucket>,
}

#[derive(Clone, Debug)]
struct IngestBucket {
    tokens: f64,
    last_refill: Instant,
}

impl IngestBackpressure {
    fn from_env() -> Self {
        let config = IngestBackpressureConfig {
            global_capacity: read_u32_env(
                INGEST_GLOBAL_CAPACITY_ENV,
                DEFAULT_INGEST_GLOBAL_CAPACITY,
            ),
            per_source_capacity: read_u32_env(
                INGEST_PER_SOURCE_CAPACITY_ENV,
                DEFAULT_INGEST_PER_SOURCE_CAPACITY,
            ),
            refill_per_sec: read_u32_env(INGEST_REFILL_PER_SEC_ENV, DEFAULT_INGEST_REFILL_PER_SEC),
        };
        Self::new(config)
    }

    fn new(config: IngestBackpressureConfig) -> Self {
        let config = IngestBackpressureConfig {
            global_capacity: config.global_capacity.max(1),
            per_source_capacity: config.per_source_capacity.max(1),
            refill_per_sec: config.refill_per_sec.max(1),
        };
        let now = Instant::now();
        Self {
            config,
            state: Arc::new(Mutex::new(IngestBackpressureState {
                global: IngestBucket::full(config.global_capacity, now),
                sources: BTreeMap::new(),
            })),
        }
    }

    fn try_acquire_with_limit(
        &self,
        source: &str,
        per_minute_limit: Option<u32>,
    ) -> Result<(), Duration> {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .expect("ingest backpressure mutex poisoned");
        let source_capacity = per_minute_limit
            .unwrap_or(self.config.per_source_capacity)
            .max(1);
        let source_refill_per_sec = per_minute_limit
            .map(|limit| (limit / 60).max(1))
            .unwrap_or(self.config.refill_per_sec);

        state
            .global
            .refill(self.config.global_capacity, self.config.refill_per_sec, now);
        let (source_tokens, source_retry_after) = {
            let source_bucket = state
                .sources
                .entry(source.to_string())
                .or_insert_with(|| IngestBucket::full(source_capacity, now));
            source_bucket.refill(source_capacity, source_refill_per_sec, now);
            (
                source_bucket.tokens,
                source_bucket.retry_after(source_refill_per_sec),
            )
        };

        if state.global.tokens >= 1.0 && source_tokens >= 1.0 {
            state.global.tokens -= 1.0;
            if let Some(source_bucket) = state.sources.get_mut(source) {
                source_bucket.tokens -= 1.0;
            }
            Ok(())
        } else {
            Err(std::cmp::max(
                state.global.retry_after(self.config.refill_per_sec),
                source_retry_after,
            ))
        }
    }
}

impl IngestBucket {
    fn full(capacity: u32, now: Instant) -> Self {
        Self {
            tokens: capacity.max(1) as f64,
            last_refill: now,
        }
    }

    fn refill(&mut self, capacity: u32, refill_per_sec: u32, now: Instant) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens =
            (self.tokens + elapsed * refill_per_sec.max(1) as f64).min(capacity.max(1) as f64);
        self.last_refill = now;
    }

    fn retry_after(&self, refill_per_sec: u32) -> Duration {
        if self.tokens >= 1.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(((1.0 - self.tokens) / refill_per_sec.max(1) as f64).max(0.001))
    }
}

impl RouteRuntimeMetrics {
    fn snapshot(&self) -> TriggerMetricSnapshot {
        TriggerMetricSnapshot {
            received: self.received.load(Ordering::Relaxed),
            dispatched: self.dispatched.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
        }
    }
}

fn finalize_response(context: &RouteContext, mut response: Response) -> Response {
    if context.route.provider.as_str() == "slack" {
        if response.status().is_success() {
            context.metrics_registry.record_slack_delivery_success();
        } else {
            context.metrics_registry.record_slack_delivery_failure();
            if response.status().is_client_error()
                && response.status() != StatusCode::TOO_MANY_REQUESTS
            {
                response.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-slack-no-retry"),
                    axum::http::HeaderValue::from_static("1"),
                );
            }
        }
    }
    response
}

fn validate_unique_route_paths(routes: &[RouteConfig]) -> Result<(), String> {
    let mut seen_paths = BTreeSet::new();
    for route in routes {
        if !seen_paths.insert(route.path.clone()) {
            return Err(format!(
                "trigger route '{}' is configured more than once",
                route.path
            ));
        }
    }
    Ok(())
}

fn tenant_path_prefix(path: &str) -> Option<(String, String)> {
    for prefix in ["/hooks/tenant/", "/tenant/"] {
        let Some(rest) = path.strip_prefix(prefix) else {
            continue;
        };
        let (tenant_id, route_tail) = rest.split_once('/')?;
        if tenant_id.is_empty() {
            return None;
        }
        return Some((tenant_id.to_string(), format!("/{route_tail}")));
    }
    None
}

async fn resolve_tenant_request(
    context: &RouteContext,
    path_tenant_id: Option<&str>,
    headers: &BTreeMap<String, String>,
) -> Result<Option<TenantRequestScope>, Response> {
    let Some(store) = context.tenant_store.as_ref() else {
        return Ok(None);
    };
    let credential_key = tenant_api_key_from_headers(headers);
    let credential_scope = match credential_key {
        Some(key) => match store.resolve_api_key(key) {
            Ok(scope) => Some(scope),
            Err(harn_vm::TenantResolutionError::Suspended(id)) => {
                return Err(tenant_denial_response(
                    context,
                    Some(id.0),
                    path_tenant_id.map(ToString::to_string),
                    "tenant_suspended",
                    HttpError::payment_required("tenant is suspended"),
                )
                .await);
            }
            Err(harn_vm::TenantResolutionError::Unknown) => {
                return Err(tenant_denial_response(
                    context,
                    None,
                    path_tenant_id.map(ToString::to_string),
                    "unknown_api_key",
                    HttpError::forbidden("unknown tenant API key"),
                )
                .await);
            }
        },
        None => None,
    };

    let path_scope = match path_tenant_id {
        Some(id) => match store.get(id) {
            Some(record) if record.status == harn_vm::TenantStatus::Active => {
                Some(record.scope.clone())
            }
            Some(record) => {
                return Err(tenant_denial_response(
                    context,
                    Some(record.scope.id.0.clone()),
                    Some(id.to_string()),
                    "tenant_suspended",
                    HttpError::payment_required("tenant is suspended"),
                )
                .await);
            }
            None => {
                return Err(tenant_denial_response(
                    context,
                    credential_scope.as_ref().map(|scope| scope.id.0.clone()),
                    Some(id.to_string()),
                    "unknown_path_tenant",
                    HttpError::forbidden("unknown tenant"),
                )
                .await);
            }
        },
        None => None,
    };

    if let (Some(credential_scope), Some(path_scope)) = (&credential_scope, &path_scope) {
        if credential_scope.id != path_scope.id {
            return Err(tenant_denial_response(
                context,
                Some(credential_scope.id.0.clone()),
                Some(path_scope.id.0.clone()),
                "cross_tenant_attempt",
                HttpError::forbidden("API key is not valid for requested tenant"),
            )
            .await);
        }
    }

    let scope = credential_scope.or(path_scope);
    let Some(scope) = scope else {
        return Err(tenant_denial_response(
            context,
            None,
            None,
            "tenant_required",
            HttpError::forbidden("tenant is required"),
        )
        .await);
    };

    Ok(Some(TenantRequestScope {
        credential_authenticated: credential_key.is_some(),
        scope,
    }))
}

fn tenant_api_key_from_headers(headers: &BTreeMap<String, String>) -> Option<&str> {
    if let Some(api_key) = header_value(headers, "x-api-key") {
        return Some(api_key.trim()).filter(|value| !value.is_empty());
    }
    let authorization = header_value(headers, "authorization")?;
    let (scheme, value) = authorization.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        Some(value.trim()).filter(|value| !value.is_empty())
    } else {
        None
    }
}

async fn tenant_denial_response(
    context: &RouteContext,
    tenant_id: Option<String>,
    attempted_tenant_id: Option<String>,
    reason: &str,
    error: HttpError,
) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert("reason".to_string(), reason.to_string());
    if let Some(tenant_id) = tenant_id.as_ref() {
        headers.insert("tenant_id".to_string(), tenant_id.clone());
    }
    if let Some(attempted_tenant_id) = attempted_tenant_id.as_ref() {
        headers.insert(
            "attempted_tenant_id".to_string(),
            attempted_tenant_id.clone(),
        );
    }
    headers.insert("trigger_id".to_string(), context.route.trigger_id.clone());
    let payload = json!({
        "reason": reason,
        "tenant_id": tenant_id,
        "attempted_tenant_id": attempted_tenant_id,
        "trigger_id": context.route.trigger_id,
    });
    if let Ok(topic) = Topic::new("orchestrator.tenant.audit") {
        let _ = context
            .event_log
            .append(
                &topic,
                LogEvent::new("tenant_access_denied", payload).with_headers(headers),
            )
            .await;
    }
    error.into_response()
}

async fn ingest_trigger(
    Extension(routes): Extension<Arc<RouteRegistry>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    let Some(resolved) = routes.resolve(uri.path()) else {
        return (StatusCode::NOT_FOUND, "trigger route not configured").into_response();
    };
    let context = resolved.context;

    context.metrics.received.fetch_add(1, Ordering::Relaxed);
    context.metrics.in_flight.fetch_add(1, Ordering::Relaxed);
    context
        .metrics_registry
        .record_trigger_received(&context.route.trigger_id, context.route.provider.as_str());
    context.metrics_registry.set_trigger_inflight(
        &context.route.trigger_id,
        context.metrics.in_flight.load(Ordering::Relaxed),
    );
    let request_started = Instant::now();
    let accepted_at_ms = current_unix_ms();
    let body_size_bytes = body.len();
    let normalized_headers = normalize_headers(&headers);
    let tenant_scope = match resolve_tenant_request(
        &context,
        resolved.path_tenant_id.as_deref(),
        &normalized_headers,
    )
    .await
    {
        Ok(scope) => scope,
        Err(response) => {
            context.metrics.failed.fetch_add(1, Ordering::Relaxed);
            context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            context.metrics_registry.set_trigger_inflight(
                &context.route.trigger_id,
                context.metrics.in_flight.load(Ordering::Relaxed),
            );
            context.metrics_registry.record_http_request(
                &context.route.path,
                method.as_str(),
                response.status().as_u16(),
                request_started.elapsed(),
                body_size_bytes,
            );
            return finalize_response(&context, response);
        }
    };
    let ingest_source = tenant_scope
        .as_ref()
        .map(|tenant| format!("tenant:{}", tenant.scope.id.0))
        .unwrap_or_else(|| context.route.provider.as_str().to_string());
    let tenant_ingest_per_minute = tenant_scope
        .as_ref()
        .and_then(|tenant| tenant.scope.budget.ingest_per_minute);

    if let Err(retry_after) = context
        .ingest_backpressure
        .try_acquire_with_limit(&ingest_source, tenant_ingest_per_minute)
    {
        context.metrics.failed.fetch_add(1, Ordering::Relaxed);
        context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
        context.metrics_registry.set_trigger_inflight(
            &context.route.trigger_id,
            context.metrics.in_flight.load(Ordering::Relaxed),
        );
        context
            .metrics_registry
            .record_backpressure_event("ingest", "reject");
        let mut response = (StatusCode::SERVICE_UNAVAILABLE, "ingest saturated").into_response();
        let retry_after_secs = retry_after.as_secs().max(1).to_string();
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&retry_after_secs)
                .unwrap_or_else(|_| HeaderValue::from_static("1")),
        );
        let response = finalize_response(&context, response);
        context.metrics_registry.record_http_request(
            &context.route.path,
            method.as_str(),
            response.status().as_u16(),
            request_started.elapsed(),
            body_size_bytes,
        );
        return response;
    }
    context
        .metrics_registry
        .record_backpressure_event("ingest", "admit");

    let trace_id = harn_vm::TraceId::new();
    let span = tracing::info_span!(
        "ingest",
        trigger_id = %context.route.trigger_id,
        binding_version = context.route.binding_version,
        trace_id = %trace_id.0
    );
    let _ = harn_vm::observability::otel::set_span_parent(&span, &trace_id, None);
    let mut span_context_headers = BTreeMap::new();
    let _ = harn_vm::observability::otel::inject_current_context_headers(
        &span,
        &mut span_context_headers,
    );

    async move {
        if let Err(error) = authorize_request(
            &context,
            tenant_scope.as_ref(),
            method.as_str(),
            uri.path(),
            &normalized_headers,
            body.as_ref(),
        )
        .await
        {
            context.metrics.failed.fetch_add(1, Ordering::Relaxed);
            context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            context.metrics_registry.set_trigger_inflight(
                &context.route.trigger_id,
                context.metrics.in_flight.load(Ordering::Relaxed),
            );
            let response = finalize_response(&context, error.into_response());
            context.metrics_registry.record_http_request(
                &context.route.path,
                method.as_str(),
                response.status().as_u16(),
                request_started.elapsed(),
                body_size_bytes,
            );
            return response;
        }

        if let Some(path) = context.request_gate.entered_file.as_ref() {
            if let Err(error) = mark_test_file(path).await {
                context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                context.metrics_registry.set_trigger_inflight(
                    &context.route.trigger_id,
                    context.metrics.in_flight.load(Ordering::Relaxed),
                );
                let response = finalize_response(
                    &context,
                    (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
                );
                context.metrics_registry.record_http_request(
                    &context.route.path,
                    method.as_str(),
                    response.status().as_u16(),
                    request_started.elapsed(),
                    body_size_bytes,
                );
                return response;
            }
        }
        if let Some(path) = context.request_gate.release_file.as_ref() {
            wait_for_test_release_file(path).await;
        }

        let result = normalize_request(
            &context,
            &normalized_headers,
            &query,
            body.as_ref(),
            trace_id,
            tenant_scope.as_ref().map(|tenant| &tenant.scope),
        )
        .await;
        let ingress_timing = IngressLifecycleTiming {
            accepted_at_ms,
            normalized_at_ms: current_unix_ms(),
            accepted_to_normalized: request_started.elapsed(),
        };
        let response = match result {
            Ok(NormalizedRequest::Events(events)) => {
                match enqueue_normalized_events(
                    &context,
                    events,
                    &span_context_headers,
                    ingress_timing,
                )
                .await
                {
                    Ok(summary) => {
                        context
                            .metrics
                            .dispatched
                            .fetch_add(summary.accepted as u64, Ordering::Relaxed);
                        context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                        context.metrics_registry.set_trigger_inflight(
                            &context.route.trigger_id,
                            context.metrics.in_flight.load(Ordering::Relaxed),
                        );
                        enqueue_summary_response(&context, summary)
                    }
                    Err(error) => {
                        context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                        context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                        context.metrics_registry.set_trigger_inflight(
                            &context.route.trigger_id,
                            context.metrics.in_flight.load(Ordering::Relaxed),
                        );
                        error.into_response()
                    }
                }
            }
            Ok(NormalizedRequest::Immediate { response, events }) => {
                match enqueue_normalized_events(
                    &context,
                    events,
                    &span_context_headers,
                    ingress_timing,
                )
                .await
                {
                    Ok(summary) => {
                        context.metrics.dispatched.fetch_add(
                            std::cmp::max(summary.accepted, 1) as u64,
                            Ordering::Relaxed,
                        );
                        context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                        context.metrics_registry.set_trigger_inflight(
                            &context.route.trigger_id,
                            context.metrics.in_flight.load(Ordering::Relaxed),
                        );
                        response
                    }
                    Err(error) => {
                        context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                        context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                        context.metrics_registry.set_trigger_inflight(
                            &context.route.trigger_id,
                            context.metrics.in_flight.load(Ordering::Relaxed),
                        );
                        error.into_response()
                    }
                }
            }
            Ok(NormalizedRequest::Rejected(response)) => {
                context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                context.metrics_registry.set_trigger_inflight(
                    &context.route.trigger_id,
                    context.metrics.in_flight.load(Ordering::Relaxed),
                );
                response
            }
            Err(error) => {
                context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                context.metrics_registry.set_trigger_inflight(
                    &context.route.trigger_id,
                    context.metrics.in_flight.load(Ordering::Relaxed),
                );
                error.into_response()
            }
        };
        let response = finalize_response(&context, response);
        context.metrics_registry.record_http_request(
            &context.route.path,
            method.as_str(),
            response.status().as_u16(),
            request_started.elapsed(),
            body_size_bytes,
        );
        response
    }
    .instrument(span)
    .await
}

async fn metrics_endpoint(
    Extension(metrics): Extension<Arc<harn_vm::MetricsRegistry>>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render_prometheus(),
    )
}

async fn readyz_endpoint(Extension(readiness): Extension<Arc<ListenerReadiness>>) -> Response {
    if readiness.is_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response()
    }
}

async fn admin_reload_endpoint(
    Extension(state): Extension<Arc<AdminReloadState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let normalized_headers = normalize_headers(&headers);
    if state
        .auth
        .authorize(
            state.event_log.as_ref(),
            method.as_str(),
            uri.path(),
            &normalized_headers,
            &body,
        )
        .await
        .is_err()
    {
        return HttpError::unauthorized("auth failed").into_response();
    }
    let source = serde_json::from_slice::<JsonValue>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("source")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "admin_api".to_string());
    match state.reload.request(source.clone()).await {
        Ok(summary) => (
            StatusCode::OK,
            axum::Json(json!({
                "status": "ok",
                "source": source,
                "summary": summary,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "status": "error",
                "source": source,
                "error": error,
            })),
        )
            .into_response(),
    }
}

impl AcpWebSocketHub {
    fn new(event_log: Arc<AnyEventLog>, retention: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AcpWebSocketHubState::default()),
            event_log,
            retention,
        })
    }

    fn spawn_worker(self: &Arc<Self>, pipeline: Option<String>) -> Result<Arc<AcpWorker>, String> {
        let worker_id = uuid::Uuid::new_v4().to_string();
        let (to_acp_tx, to_acp_rx) = mpsc::unbounded_channel::<JsonValue>();
        let (from_acp_tx, mut from_acp_rx) = mpsc::unbounded_channel::<String>();
        let worker = Arc::new(AcpWorker {
            id: worker_id.clone(),
            request_tx: Mutex::new(Some(to_acp_tx)),
            socket_tx: Mutex::new(None),
            active_connection_id: Mutex::new(None),
            sessions: Mutex::new(BTreeSet::new()),
            replay_buffer: Mutex::new(VecDeque::new()),
            next_event_id: AtomicU64::new(1),
            detached_at: Mutex::new(None),
            event_log: self.event_log.clone(),
            hub: Arc::downgrade(self),
        });
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_id
            .insert(worker_id.clone(), worker.clone());

        let worker_for_output = worker.clone();
        tokio::spawn(async move {
            while let Some(line) = from_acp_rx.recv().await {
                worker_for_output.handle_output(line).await;
            }
        });

        let worker_name = worker_id.clone();
        std::thread::Builder::new()
            .name(format!("harn-acp-ws-{worker_name}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("[harn] failed to start ACP WebSocket worker: {error}");
                        return;
                    }
                };
                runtime.block_on(crate::acp::run_acp_channel_server(
                    pipeline,
                    to_acp_rx,
                    from_acp_tx,
                ));
            })
            .map_err(|error| format!("worker spawn failed: {error}"))?;

        Ok(worker)
    }

    fn register_session(&self, session_id: String, worker: &Arc<AcpWorker>) {
        worker
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.clone());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .workers_by_session
            .entry(session_id)
            .or_insert_with(|| worker.clone());
    }

    fn attach(
        &self,
        session_id: &str,
        connection_id: &str,
        socket_tx: mpsc::UnboundedSender<String>,
        last_acked_event_id: u64,
    ) -> Result<Arc<AcpWorker>, AcpAttachError> {
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_session
            .get(session_id)
            .cloned()
            .ok_or(AcpAttachError::NotFound)?;
        worker.attach(connection_id, socket_tx, last_acked_event_id)?;
        Ok(worker)
    }

    fn remove_worker(&self, worker: &Arc<AcpWorker>) {
        let sessions = worker.session_ids();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.workers_by_id.remove(&worker.id);
        for session_id in sessions {
            if state
                .workers_by_session
                .get(&session_id)
                .is_some_and(|mapped| Arc::ptr_eq(mapped, worker))
            {
                state.workers_by_session.remove(&session_id);
            }
        }
        worker.shutdown();
    }

    async fn run_expiry_sweeper(self: Arc<Self>) {
        let sweep_interval = self
            .retention
            .min(Duration::from_secs(15))
            .max(Duration::from_secs(1));
        let mut interval = tokio::time::interval(sweep_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let expired: Vec<Arc<AcpWorker>> = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state
                    .workers_by_id
                    .values()
                    .filter(|worker| worker.is_expired(self.retention))
                    .cloned()
                    .collect()
            };
            for worker in expired {
                let sessions = worker.session_ids();
                self.remove_worker(&worker);
                append_acp_event(
                    &self.event_log,
                    &worker.id,
                    "session_worker_expired",
                    json!({
                        "worker_id": worker.id,
                        "session_ids": sessions,
                        "retention_ms": self.retention.as_millis(),
                    }),
                )
                .await;
            }
        }
    }
}

impl AcpWorker {
    fn attach(
        &self,
        connection_id: &str,
        socket_tx: mpsc::UnboundedSender<String>,
        last_acked_event_id: u64,
    ) -> Result<(), AcpAttachError> {
        {
            let mut active = self
                .active_connection_id
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if active
                .as_deref()
                .is_some_and(|active_connection_id| active_connection_id != connection_id)
            {
                return Err(AcpAttachError::AlreadyAttached);
            }
            *active = Some(connection_id.to_string());
        }
        *self.socket_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(socket_tx.clone());
        *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.replay_since(last_acked_event_id, socket_tx);
        Ok(())
    }

    fn detach(&self, connection_id: &str) {
        let mut active = self
            .active_connection_id
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if active.as_deref() != Some(connection_id) {
            return;
        }
        *active = None;
        *self.socket_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn send_request(&self, value: JsonValue) -> Result<(), ()> {
        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or(())?
            .send(value)
            .map_err(|_| ())
    }

    fn shutdown(&self) {
        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.socket_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    fn is_expired(&self, retention: Duration) -> bool {
        self.detached_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|detached_at| detached_at.elapsed() >= retention)
    }

    fn session_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    async fn handle_output(self: &Arc<Self>, line: String) {
        let session_id = session_id_from_acp_message(&line).or_else(|| {
            let sessions = self.session_ids();
            (sessions.len() == 1).then(|| sessions[0].clone())
        });
        if let Some(session_id) = session_id.clone() {
            if let Some(hub) = self.hub.upgrade() {
                hub.register_session(session_id, self);
            }
        }

        let event_id = self.next_event_id.fetch_add(1, Ordering::SeqCst);
        let annotated = annotate_acp_line(&line, event_id, session_id.as_deref(), false);
        {
            let mut replay_buffer = self.replay_buffer.lock().unwrap_or_else(|e| e.into_inner());
            replay_buffer.push_back(AcpReplayEvent {
                id: event_id,
                line: annotated.clone(),
                session_id: session_id.clone(),
            });
            while replay_buffer.len() > ACP_REPLAY_BUFFER_LIMIT {
                replay_buffer.pop_front();
            }
        }

        let topic_id = session_id.as_deref().unwrap_or(&self.id);
        append_acp_event(
            &self.event_log,
            topic_id,
            "message_sent",
            acp_replay_log_payload(&annotated, event_id, session_id.as_deref()),
        )
        .await;

        let socket_tx = self
            .socket_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(socket_tx) = socket_tx {
            let _ = socket_tx.send(annotated);
        }
    }

    fn replay_since(&self, last_acked_event_id: u64, socket_tx: mpsc::UnboundedSender<String>) {
        let events: Vec<AcpReplayEvent> = self
            .replay_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|event| event.id > last_acked_event_id)
            .cloned()
            .collect();
        for event in events {
            let replayed =
                annotate_acp_line(&event.line, event.id, event.session_id.as_deref(), true);
            let _ = socket_tx.send(replayed);
        }
    }
}

fn acp_retained_session_duration_from_env() -> Duration {
    let seconds = std::env::var(ACP_RETAINED_SESSION_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(ACP_DEFAULT_RETAINED_SESSION_SECS);
    Duration::from_secs(seconds)
}

async fn acp_websocket_endpoint(
    Extension(state): Extension<Arc<AcpWebSocketState>>,
    ws: WebSocketUpgrade,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let normalized_headers = normalize_headers(&headers);
    if state.auth.has_credentials()
        && state
            .auth
            .authorize(
                state.event_log.as_ref(),
                method.as_str(),
                uri.path(),
                &normalized_headers,
                &[],
            )
            .await
            .is_err()
    {
        return HttpError::unauthorized("auth failed").into_response();
    }

    ws.on_upgrade(move |socket| run_acp_websocket(socket, state))
        .into_response()
}

async fn run_acp_websocket(socket: WebSocket, state: Arc<AcpWebSocketState>) {
    let connection_id = uuid::Uuid::new_v4().to_string();
    append_acp_event(
        &state.event_log,
        &connection_id,
        "connection_opened",
        json!({
            "transport": "websocket",
            "path": ACP_PATH,
        }),
    )
    .await;

    let (mut sender, mut receiver) = socket.split();
    let (socket_tx, mut socket_rx) = mpsc::unbounded_channel::<String>();
    let mut ping_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + ACP_PING_INTERVAL,
        ACP_PING_INTERVAL,
    );
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut liveness_interval = tokio::time::interval(Duration::from_secs(1));
    liveness_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_sent_at: Option<Instant> = None;
    let mut session_id: Option<String> = None;
    let mut worker: Option<Arc<AcpWorker>> = None;

    loop {
        tokio::select! {
            Some(line) = socket_rx.recv() => {
                if let Some(id) = session_id_from_acp_response(&line) {
                    session_id = Some(id.clone());
                    append_acp_event(
                        &state.event_log,
                        &connection_id,
                        "session_opened",
                        json!({"session_id": id}),
                    )
                    .await;
                }
                append_acp_event(
                    &state.event_log,
                    &connection_id,
                    "message_sent",
                    acp_message_log_payload(&line, session_id.as_deref()),
                )
                .await;
                if sender.send(WsMessage::Text(line.into())).await.is_err() {
                    break;
                }
            }
            frame = receiver.next() => {
                let Some(frame) = frame else {
                    break;
                };
                let Ok(frame) = frame else {
                    break;
                };
                match frame {
                    WsMessage::Text(text) => {
                        let line = text.to_string();
                        append_acp_event(
                            &state.event_log,
                            &connection_id,
                            "message_received",
                            acp_message_log_payload(&line, session_id.as_deref()),
                        )
                        .await;
                        match serde_json::from_str::<JsonValue>(&line) {
                            Ok(value) => {
                                if let Some(load_session_id) = session_load_session_id(&value) {
                                    match state.hub.attach(
                                        &load_session_id,
                                        &connection_id,
                                        socket_tx.clone(),
                                        last_acked_event_id(&value),
                                    ) {
                                        Ok(attached) => {
                                            session_id = Some(load_session_id);
                                            worker = Some(attached);
                                        }
                                        Err(AcpAttachError::AlreadyAttached) => {
                                            send_socket_jsonrpc_error(
                                                &socket_tx,
                                                value.get("id").unwrap_or(&JsonValue::Null),
                                                -32010,
                                                "ACP session is already attached to another WebSocket",
                                            );
                                            continue;
                                        }
                                        Err(AcpAttachError::NotFound) => {
                                            replay_persisted_acp_events(
                                                &state.event_log,
                                                &load_session_id,
                                                last_acked_event_id(&value),
                                                &socket_tx,
                                            )
                                            .await;
                                            if worker.is_none() {
                                                send_socket_jsonrpc_error(
                                                    &socket_tx,
                                                    value.get("id").unwrap_or(&JsonValue::Null),
                                                    -32004,
                                                    &format!("Session not found: {load_session_id}"),
                                                );
                                                continue;
                                            }
                                        }
                                    }
                                }
                                if worker.is_none() {
                                    match state.hub.spawn_worker(state.pipeline.clone()) {
                                        Ok(new_worker) => {
                                            new_worker.attach(
                                                &connection_id,
                                                socket_tx.clone(),
                                                0,
                                            )
                                            .expect("fresh ACP worker is unattached");
                                            worker = Some(new_worker);
                                        }
                                        Err(error) => {
                                            append_acp_event(
                                                &state.event_log,
                                                &connection_id,
                                                "connection_failed",
                                                json!({"reason": error}),
                                            )
                                            .await;
                                            break;
                                        }
                                    }
                                }
                                if worker
                                    .as_ref()
                                    .is_none_or(|worker| worker.send_request(value).is_err())
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                let response = harn_vm::jsonrpc::error_response(
                                    JsonValue::Null,
                                    -32700,
                                    &format!("Parse error: {error}"),
                                );
                                if let Ok(line) = serde_json::to_string(&response) {
                                    let _ = sender.send(WsMessage::Text(line.into())).await;
                                }
                            }
                        }
                    }
                    WsMessage::Binary(_) => {
                        let response = harn_vm::jsonrpc::error_response(
                            JsonValue::Null,
                            -32600,
                            "ACP WebSocket transport only accepts JSON-RPC text frames",
                        );
                        if let Ok(line) = serde_json::to_string(&response) {
                            let _ = sender.send(WsMessage::Text(line.into())).await;
                        }
                    }
                    WsMessage::Ping(payload) => {
                        if sender.send(WsMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    WsMessage::Pong(_) => {
                        ping_sent_at = None;
                    }
                    WsMessage::Close(_) => {
                        break;
                    }
                }
            }
            _ = ping_interval.tick() => {
                if ping_sent_at.is_none() {
                    ping_sent_at = Some(Instant::now());
                    if sender.send(WsMessage::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
            _ = liveness_interval.tick() => {
                if ping_sent_at.is_some_and(|sent| sent.elapsed() > ACP_PONG_TIMEOUT) {
                    let _ = sender.send(WsMessage::Close(None)).await;
                    append_acp_event(
                        &state.event_log,
                        &connection_id,
                        "connection_liveness_timeout",
                        json!({"timeout_ms": ACP_PONG_TIMEOUT.as_millis()}),
                    )
                    .await;
                    break;
                }
            }
        }
    }

    if let Some(worker) = worker.as_ref() {
        worker.detach(&connection_id);
    }
    append_acp_event(
        &state.event_log,
        &connection_id,
        "connection_closed",
        json!({
            "session_id": session_id,
            "retention_ms": state.hub.retention.as_millis(),
        }),
    )
    .await;
}

async fn append_acp_event(
    event_log: &Arc<AnyEventLog>,
    connection_id: &str,
    kind: &str,
    payload: JsonValue,
) {
    let Ok(topic) = Topic::new(format!("{ACP_TOPIC_PREFIX}.{connection_id}")) else {
        return;
    };
    let _ = event_log.append(&topic, LogEvent::new(kind, payload)).await;
}

async fn replay_persisted_acp_events(
    event_log: &Arc<AnyEventLog>,
    session_id: &str,
    last_acked_event_id: u64,
    socket_tx: &mpsc::UnboundedSender<String>,
) {
    let Ok(topic) = Topic::new(format!("{ACP_TOPIC_PREFIX}.{session_id}")) else {
        return;
    };
    let Ok(events) = event_log
        .read_range(&topic, None, ACP_REPLAY_BUFFER_LIMIT)
        .await
    else {
        return;
    };
    for (_, event) in events {
        if event.kind != "message_sent" {
            continue;
        }
        let Some(acp_event_id) = event
            .payload
            .get("acp_event_id")
            .and_then(JsonValue::as_u64)
        else {
            continue;
        };
        if acp_event_id <= last_acked_event_id {
            continue;
        }
        let Some(line) = event.payload.get("line").and_then(JsonValue::as_str) else {
            continue;
        };
        let replayed = annotate_acp_line(line, acp_event_id, Some(session_id), true);
        let _ = socket_tx.send(replayed);
    }
}

fn acp_replay_log_payload(line: &str, acp_event_id: u64, session_id: Option<&str>) -> JsonValue {
    let mut payload = acp_message_log_payload(line, session_id);
    payload["acp_event_id"] = json!(acp_event_id);
    payload["line"] = json!(line);
    payload
}

fn acp_message_log_payload(line: &str, session_id: Option<&str>) -> JsonValue {
    match serde_json::from_str::<JsonValue>(line) {
        Ok(value) => {
            let mut payload = json!({
                "method": value.get("method").and_then(JsonValue::as_str),
                "id": value.get("id").cloned(),
                "session_id": session_id,
            });
            if let Some(params_session_id) = value
                .get("params")
                .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                .and_then(JsonValue::as_str)
            {
                payload["session_id"] = json!(params_session_id);
            }
            if let Some(result_session_id) = value
                .get("result")
                .and_then(|result| result.get("sessionId").or_else(|| result.get("session_id")))
                .and_then(JsonValue::as_str)
            {
                payload["session_id"] = json!(result_session_id);
            }
            payload
        }
        Err(_) => json!({
            "malformed": true,
            "session_id": session_id,
        }),
    }
}

fn annotate_acp_line(
    line: &str,
    event_id: u64,
    session_id: Option<&str>,
    replayed: bool,
) -> String {
    let Ok(mut value) = serde_json::from_str::<JsonValue>(line) else {
        return line.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return line.to_string();
    };
    let harn_meta = object
        .entry("_harn")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(harn_meta) = harn_meta {
        harn_meta.insert("eventId".to_string(), json!(event_id));
        harn_meta.insert("replayed".to_string(), json!(replayed));
        if let Some(session_id) = session_id {
            harn_meta.insert("sessionId".to_string(), json!(session_id));
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_string())
}

fn session_load_session_id(value: &JsonValue) -> Option<String> {
    if value.get("method").and_then(JsonValue::as_str) != Some("session/load") {
        return None;
    }
    value
        .get("params")
        .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn last_acked_event_id(value: &JsonValue) -> u64 {
    value
        .get("params")
        .and_then(|params| {
            params
                .get("lastAckedEventId")
                .or_else(|| params.get("last_acked_event_id"))
                .or_else(|| params.get("lastEventId"))
        })
        .and_then(JsonValue::as_u64)
        .unwrap_or(0)
}

fn send_socket_jsonrpc_error(
    socket_tx: &mpsc::UnboundedSender<String>,
    id: &JsonValue,
    code: i64,
    message: &str,
) {
    let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
    if let Ok(line) = serde_json::to_string(&response) {
        let _ = socket_tx.send(line);
    }
}

fn session_id_from_acp_response(line: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(line)
        .ok()
        .and_then(|value| value.get("result").cloned())
        .and_then(|result| {
            result
                .get("sessionId")
                .or_else(|| result.get("session_id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
}

fn session_id_from_acp_message(line: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(line)
        .ok()
        .and_then(|value| {
            value
                .get("params")
                .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                .and_then(JsonValue::as_str)
                .or_else(|| {
                    value
                        .get("result")
                        .and_then(|result| {
                            result.get("sessionId").or_else(|| result.get("session_id"))
                        })
                        .and_then(JsonValue::as_str)
                })
                .or_else(|| {
                    value
                        .get("result")
                        .and_then(|result| result.get("session"))
                        .and_then(|session| {
                            session
                                .get("sessionId")
                                .or_else(|| session.get("session_id"))
                        })
                        .and_then(JsonValue::as_str)
                })
                .map(ToString::to_string)
        })
}

async fn authorize_request(
    context: &RouteContext,
    tenant_scope: Option<&TenantRequestScope>,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<(), HttpError> {
    match context.route.auth_mode {
        AuthMode::Public => Ok(()),
        AuthMode::BearerOrHmac
            if tenant_scope.is_some_and(|tenant| tenant.credential_authenticated) =>
        {
            Ok(())
        }
        AuthMode::BearerOrHmac => context
            .auth
            .authorize(context.event_log.as_ref(), method, path, headers, body)
            .await
            .map_err(|()| HttpError::unauthorized("auth failed")),
    }
}

async fn normalize_request(
    context: &RouteContext,
    normalized_headers: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
    body: &[u8],
    trace_id: harn_vm::TraceId,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<NormalizedRequest, HttpError> {
    let received_at = OffsetDateTime::now_utc();
    if let Some(connector) = context.route.connector.as_ref() {
        let mut raw = harn_vm::RawInbound::new("", normalized_headers.clone(), body.to_vec());
        raw.query = query.clone();
        raw.received_at = received_at;
        raw.metadata = json!({
            "binding_id": context.route.trigger_id,
            "binding_version": context.route.binding_version,
            "path": context.route.path,
            "tenant_id": tenant_scope.map(|tenant| tenant.id.0.as_str()),
        });
        let result = connector
            .lock()
            .await
            .normalize_inbound_result(raw)
            .await
            .map_err(HttpError::from_connector)?;
        return connector_normalize_result_to_request(result, trace_id, tenant_scope);
    }

    let normalized_body = normalize_body(body, normalized_headers);
    let provider = context.route.provider.clone();

    let signature_status = match context.route.signature_mode {
        SignatureMode::Unsigned => harn_vm::SignatureStatus::Unsigned,
        SignatureMode::GitHub => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::github(),
                body,
                normalized_headers,
                &secret,
                None,
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
        SignatureMode::Standard => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::standard_webhooks(),
                body,
                normalized_headers,
                &secret,
                Some(time::Duration::minutes(5)),
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
    };

    let provider_kind = provider_event_kind(&provider, normalized_headers, &normalized_body);
    let trigger_kind = trigger_event_kind(&provider, normalized_headers, &normalized_body);
    let dedupe_key = dedupe_key(&provider, normalized_headers, &normalized_body, body);
    let provider_payload = harn_vm::ProviderPayload::normalize(
        &provider,
        &provider_kind,
        normalized_headers,
        normalized_body,
    )
    .map_err(|error| HttpError::unprocessable(error.to_string()))?;

    Ok(NormalizedRequest::Events(vec![harn_vm::TriggerEvent {
        id: harn_vm::TriggerEventId::new(),
        provider,
        kind: trigger_kind,
        received_at,
        occurred_at: infer_occurred_at(&provider_payload),
        dedupe_key,
        trace_id,
        tenant_id: tenant_scope.map(|tenant| tenant.id.clone()),
        headers: harn_vm::redact_headers(
            normalized_headers,
            &harn_vm::HeaderRedactionPolicy::default(),
        ),
        batch: None,
        raw_body: Some(body.to_vec()),
        provider_payload,
        signature_status,
        dedupe_claimed: false,
    }]))
}

enum NormalizedRequest {
    Events(Vec<harn_vm::TriggerEvent>),
    Immediate {
        response: Response,
        events: Vec<harn_vm::TriggerEvent>,
    },
    Rejected(Response),
}

struct EnqueueSummary {
    accepted: usize,
    duplicates: usize,
    first_event_id: Option<String>,
}

#[derive(Clone, Copy)]
struct IngressLifecycleTiming {
    accepted_at_ms: i64,
    normalized_at_ms: i64,
    accepted_to_normalized: Duration,
}

fn connector_normalize_result_to_request(
    result: harn_vm::ConnectorNormalizeResult,
    trace_id: harn_vm::TraceId,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<NormalizedRequest, HttpError> {
    match result {
        harn_vm::ConnectorNormalizeResult::Event(event) => {
            let mut event = *event;
            if let Some(challenge) = slack_url_verification_challenge(&event) {
                return Ok(NormalizedRequest::Immediate {
                    response: (
                        StatusCode::OK,
                        [("content-type", "text/plain; charset=utf-8")],
                        challenge,
                    )
                        .into_response(),
                    events: Vec::new(),
                });
            }
            if let Some(response) = notion_subscription_verification_response(&event) {
                return Ok(NormalizedRequest::Immediate {
                    response,
                    events: Vec::new(),
                });
            }
            event.trace_id = trace_id;
            apply_tenant_scope(vec![event], tenant_scope).map(NormalizedRequest::Events)
        }
        harn_vm::ConnectorNormalizeResult::Batch(mut events) => {
            set_trace_id(&mut events, trace_id);
            apply_tenant_scope(events, tenant_scope).map(NormalizedRequest::Events)
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse {
            response,
            mut events,
        } => {
            set_trace_id(&mut events, trace_id);
            Ok(NormalizedRequest::Immediate {
                response: connector_http_response_to_response(response)?,
                events: apply_tenant_scope(events, tenant_scope)?,
            })
        }
        harn_vm::ConnectorNormalizeResult::Reject(response) => Ok(NormalizedRequest::Rejected(
            connector_http_response_to_response(response)?,
        )),
    }
}

fn apply_tenant_scope(
    mut events: Vec<harn_vm::TriggerEvent>,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<Vec<harn_vm::TriggerEvent>, HttpError> {
    let Some(tenant_scope) = tenant_scope else {
        return Ok(events);
    };
    for event in &mut events {
        match event.tenant_id.as_ref() {
            Some(existing) if existing != &tenant_scope.id => {
                return Err(HttpError::forbidden(format!(
                    "event tenant '{}' does not match request tenant '{}'",
                    existing.0, tenant_scope.id.0
                )));
            }
            Some(_) => {}
            None => event.tenant_id = Some(tenant_scope.id.clone()),
        }
    }
    Ok(events)
}

fn set_trace_id(events: &mut [harn_vm::TriggerEvent], trace_id: harn_vm::TraceId) {
    for event in events {
        event.trace_id = trace_id.clone();
    }
}

fn connector_http_response_to_response(
    response: harn_vm::ConnectorHttpResponse,
) -> Result<Response, HttpError> {
    let status = StatusCode::from_u16(response.status).map_err(|error| {
        HttpError::internal(format!(
            "connector returned invalid HTTP status {}: {error}",
            response.status
        ))
    })?;
    let mut builder = Response::builder().status(status);
    let has_content_type = response
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("content-type"));
    for (name, value) in response.headers {
        let name = axum::http::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            HttpError::internal(format!(
                "connector returned invalid response header name: {error}"
            ))
        })?;
        let value = axum::http::HeaderValue::from_str(&value).map_err(|error| {
            HttpError::internal(format!(
                "connector returned invalid response header value for '{}': {error}",
                name.as_str()
            ))
        })?;
        builder = builder.header(name, value);
    }

    let body = match response.body {
        JsonValue::Null => Body::empty(),
        JsonValue::String(value) => {
            if !has_content_type {
                builder = builder.header(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                );
            }
            Body::from(value)
        }
        value => {
            if !has_content_type {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            }
            let bytes = serde_json::to_vec(&value)
                .map_err(|error| HttpError::internal(error.to_string()))?;
            Body::from(bytes)
        }
    };

    builder
        .body(body)
        .map_err(|error| HttpError::internal(error.to_string()))
}

async fn enqueue_normalized_events(
    context: &RouteContext,
    events: Vec<harn_vm::TriggerEvent>,
    span_context_headers: &BTreeMap<String, String>,
    timing: IngressLifecycleTiming,
) -> Result<EnqueueSummary, HttpError> {
    let mut summary = EnqueueSummary {
        accepted: 0,
        duplicates: 0,
        first_event_id: None,
    };

    for event in events {
        let binding_key =
            listener_binding_key(&context.route.trigger_id, context.route.binding_version);
        context
            .metrics_registry
            .record_trigger_accepted_to_normalized(
                &context.route.trigger_id,
                &binding_key,
                event.provider.as_str(),
                event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                "normalized",
                timing.accepted_to_normalized,
            );
        let postprocess = harn_vm::postprocess_normalized_event(
            context.inbox.as_ref(),
            &context.route.trigger_id,
            context.route.dedupe_key_template.is_some(),
            context.route.dedupe_ttl(),
            event,
        )
        .await
        .map_err(HttpError::from_connector)?;
        match postprocess {
            harn_vm::PostNormalizeOutcome::DuplicateDropped => {
                summary.duplicates += 1;
                context
                    .metrics_registry
                    .record_trigger_deduped(&context.route.trigger_id, "inbox_duplicate");
            }
            harn_vm::PostNormalizeOutcome::Ready(event) => {
                let event = *event;
                let pending_topic = topic_for_event(&event, &context.pending_topic)
                    .map_err(|error| HttpError::internal(error.to_string()))?;
                let payload = json!({
                    "trigger_id": context.route.trigger_id,
                    "binding_version": context.route.binding_version,
                    "event": event,
                });
                let queue_span = tracing::info_span!(
                    "queue_append",
                    trigger_id = %context.route.trigger_id,
                    binding_version = context.route.binding_version,
                    event_id = %event.id.0,
                    trace_id = %event.trace_id.0
                );
                let _ = harn_vm::observability::otel::set_span_parent_from_headers(
                    &queue_span,
                    span_context_headers,
                    &event.trace_id,
                    None,
                );
                let mut pending_headers = BTreeMap::new();
                pending_headers.insert("trace_id".to_string(), event.trace_id.0.clone());
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_ACCEPTED_AT_MS_HEADER.to_string(),
                    timing.accepted_at_ms.to_string(),
                );
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_NORMALIZED_AT_MS_HEADER.to_string(),
                    timing.normalized_at_ms.to_string(),
                );
                pending_headers.insert("trigger_id".to_string(), context.route.trigger_id.clone());
                pending_headers.insert("binding_key".to_string(), binding_key.clone());
                pending_headers.insert("provider".to_string(), event.provider.as_str().to_string());
                if let Some(tenant_id) = event.tenant_id.as_ref() {
                    pending_headers.insert("tenant_id".to_string(), tenant_id.0.clone());
                }
                let _ = harn_vm::observability::otel::inject_current_context_headers(
                    &queue_span,
                    &mut pending_headers,
                );
                let append_started = Instant::now();
                let payload_size_bytes = serde_json::to_vec(&payload)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0);
                let mut log_event = LogEvent::new("trigger_event", payload);
                let queue_appended_at_ms = log_event.occurred_at_ms;
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_QUEUE_APPENDED_AT_MS_HEADER.to_string(),
                    queue_appended_at_ms.to_string(),
                );
                log_event.headers = pending_headers;
                let event_id = context
                    .event_log
                    .append(&pending_topic, log_event)
                    .instrument(queue_span)
                    .await
                    .map_err(|error| {
                        HttpError::internal(format!(
                            "failed to append trigger event to pending log: {error}"
                        ))
                    })?;
                context.metrics_registry.record_event_log_append(
                    pending_topic.as_str(),
                    append_started.elapsed(),
                    payload_size_bytes,
                );
                context
                    .metrics_registry
                    .record_trigger_accepted_to_queue_append(
                        &context.route.trigger_id,
                        &binding_key,
                        event.provider.as_str(),
                        event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                        "queued",
                        duration_between_ms(queue_appended_at_ms, timing.accepted_at_ms),
                    );
                context.metrics_registry.note_trigger_pending_event(
                    event.id.0.as_str(),
                    &context.route.trigger_id,
                    &binding_key,
                    event.provider.as_str(),
                    event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                    timing.accepted_at_ms,
                    queue_appended_at_ms,
                );
                tracing::info!(
                    component = "listener",
                    trace_id = %event.trace_id.0,
                    trigger_id = %context.route.trigger_id,
                    event_id = %event_id,
                    "trigger event accepted"
                );
                summary.accepted += 1;
                if summary.first_event_id.is_none() {
                    summary.first_event_id = Some(event_id.to_string());
                }
            }
        }
    }

    Ok(summary)
}

fn enqueue_summary_response(context: &RouteContext, summary: EnqueueSummary) -> Response {
    if summary.accepted == 0 && summary.duplicates > 0 {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "status": "duplicate_dropped",
                "trigger_id": context.route.trigger_id,
            })),
        )
            .into_response();
    }

    if summary.accepted == 1 && summary.duplicates == 0 {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "status": "accepted",
                "event_id": summary.first_event_id,
                "trigger_id": context.route.trigger_id,
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        axum::Json(json!({
            "status": "accepted",
            "events_accepted": summary.accepted,
            "duplicates_dropped": summary.duplicates,
            "trigger_id": context.route.trigger_id,
        })),
    )
        .into_response()
}

fn trigger_path(trigger: &CollectedManifestTrigger) -> Result<String, String> {
    let path = trigger
        .config
        .kind_specific
        .get("path")
        .and_then(JsonValueExt::as_toml_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("/triggers/{}", trigger.config.id));
    if !path.starts_with('/') {
        return Err(format!(
            "trigger '{}' path must start with '/'",
            trigger.config.id
        ));
    }
    Ok(path)
}

async fn load_secret(
    context: &RouteContext,
    tenant_scope: Option<&harn_vm::TenantScope>,
    secret_id: Option<&SecretId>,
) -> Result<String, HttpError> {
    let secret_id = secret_id.ok_or_else(|| {
        HttpError::internal(format!(
            "trigger '{}' requires a signing secret",
            context.route.trigger_id
        ))
    })?;
    let tenant_provider;
    let provider: &dyn SecretProvider = if let Some(scope) = tenant_scope {
        tenant_provider =
            harn_vm::TenantSecretProvider::new(context.secrets.clone(), scope.clone());
        &tenant_provider
    } else {
        context.secrets.as_ref()
    };
    let secret = provider
        .get(secret_id)
        .await
        .map_err(|error| HttpError::internal(error.to_string()))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                HttpError::internal(format!(
                    "secret '{}' is not valid UTF-8: {error}",
                    secret_id
                ))
            })
    })
}

fn parse_secret_id(raw: Option<&str>) -> Option<SecretId> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, SecretVersion::Exact(version))
        }
        None => (trimmed, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(SecretId::new(namespace, name).with_version(version))
}

#[derive(Clone, Default)]
pub(crate) struct ListenerAuth {
    api_keys: Vec<String>,
    hmac_secret: Option<String>,
}

impl ListenerAuth {
    pub(crate) fn from_env(required: bool) -> Result<Self, String> {
        let api_keys = std::env::var(API_KEYS_ENV)
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|segment| !segment.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let hmac_secret = std::env::var(HMAC_SECRET_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        if required && api_keys.is_empty() {
            return Err(format!(
                "{API_KEYS_ENV} must contain at least one API key when a2a-push routes are configured"
            ));
        }
        if required && hmac_secret.is_none() {
            return Err(format!(
                "{HMAC_SECRET_ENV} must be set when a2a-push routes are configured"
            ));
        }

        Ok(Self {
            api_keys,
            hmac_secret,
        })
    }

    pub(crate) fn has_api_keys(&self) -> bool {
        !self.api_keys.is_empty()
    }

    pub(crate) fn has_credentials(&self) -> bool {
        self.has_api_keys() || self.hmac_secret.is_some()
    }

    pub(crate) async fn authorize(
        &self,
        event_log: &AnyEventLog,
        method: &str,
        path: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<(), ()> {
        if let Some(api_key) = header_value(headers, "x-api-key") {
            if self.matches_api_key(api_key.trim()) {
                return Ok(());
            }
            return Err(());
        }

        let authorization = header_value(headers, "authorization").ok_or(())?;
        let Some((scheme, value)) = authorization.split_once(' ') else {
            return Err(());
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(());
        }

        if scheme.eq_ignore_ascii_case("Bearer") {
            if self.matches_api_key(value) {
                return Ok(());
            }
            return Err(());
        }

        if scheme.eq_ignore_ascii_case(harn_vm::connectors::DEFAULT_CANONICAL_HMAC_SCHEME) {
            let Some(secret) = self.hmac_secret.as_deref() else {
                return Err(());
            };
            return harn_vm::connectors::verify_hmac_authorization(
                event_log,
                &harn_vm::ProviderId::from("orchestrator"),
                method,
                path,
                body,
                headers,
                secret,
                time::Duration::seconds(AUTH_TIMESTAMP_WINDOW_SECS),
                OffsetDateTime::now_utc(),
            )
            .await
            .map_err(|_| ());
        }

        Err(())
    }

    pub(crate) fn matches_api_key(&self, candidate: &str) -> bool {
        self.api_keys
            .iter()
            .any(|key| key.as_bytes().ct_eq(candidate.as_bytes()).into())
    }
}

fn normalize_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            normalized.insert(name.as_str().to_string(), value.to_string());
        }
    }

    for (raw, canonical) in [
        ("content-type", "Content-Type"),
        ("content-length", "Content-Length"),
        ("origin", "Origin"),
        ("x-github-event", "X-GitHub-Event"),
        ("x-github-delivery", "X-GitHub-Delivery"),
        ("x-hub-signature-256", "X-Hub-Signature-256"),
        ("linear-signature", "Linear-Signature"),
        ("linear-delivery", "Linear-Delivery"),
        ("linear-event", "Linear-Event"),
        ("x-slack-signature", "X-Slack-Signature"),
        ("x-slack-request-timestamp", "X-Slack-Request-Timestamp"),
        ("x-slack-retry-num", "X-Slack-Retry-Num"),
        ("x-slack-retry-reason", "X-Slack-Retry-Reason"),
        ("x-notion-signature", "X-Notion-Signature"),
        ("request-id", "request-id"),
        ("x-request-id", "x-request-id"),
        ("webhook-id", "webhook-id"),
        ("webhook-signature", "webhook-signature"),
        ("webhook-timestamp", "webhook-timestamp"),
        ("x-a2a-delivery", "X-A2A-Delivery"),
    ] {
        if let Some(value) = header_value(&normalized, raw) {
            let value = value.to_string();
            normalized.entry(canonical.to_string()).or_insert(value);
        }
    }

    normalized
}

fn normalize_body(body: &[u8], headers: &BTreeMap<String, String>) -> JsonValue {
    let content_type = header_value(headers, "content-type").unwrap_or_default();
    if content_type.contains("json") {
        if let Ok(value) = serde_json::from_slice(body) {
            return value;
        }
    }
    use base64::Engine;

    let raw_base64 = base64::engine::general_purpose::STANDARD.encode(body);
    serde_json::from_slice(body).unwrap_or_else(|_| {
        json!({
            "raw_base64": raw_base64,
            "raw_utf8": std::str::from_utf8(body).ok(),
        })
    })
}

fn provider_event_kind(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
) -> String {
    match provider.as_str() {
        "github" => header_value(headers, "x-github-event")
            .map(ToString::to_string)
            .unwrap_or_else(|| "webhook".to_string()),
        "a2a-push" => "push".to_string(),
        _ => body
            .get("type")
            .and_then(JsonValue::as_str)
            .or_else(|| body.get("event").and_then(JsonValue::as_str))
            .unwrap_or("webhook")
            .to_string(),
    }
}

fn trigger_event_kind(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
) -> String {
    if provider.as_str() == "github" {
        let event = header_value(headers, "x-github-event").unwrap_or("webhook");
        if let Some(action) = body.get("action").and_then(JsonValue::as_str) {
            return format!("{event}.{action}");
        }
        return event.to_string();
    }
    provider_event_kind(provider, headers, body)
}

fn dedupe_key(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
    raw_body: &[u8],
) -> String {
    match provider.as_str() {
        "github" => header_value(headers, "x-github-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        "webhook" => header_value(headers, "webhook-id")
            .map(ToString::to_string)
            .or_else(|| {
                body.get("id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        _ => header_value(headers, "x-a2a-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
    }
}

fn infer_occurred_at(payload: &harn_vm::ProviderPayload) -> Option<OffsetDateTime> {
    let harn_vm::ProviderPayload::Known(payload) = payload else {
        return None;
    };
    let raw = match payload {
        harn_vm::triggers::event::KnownProviderPayload::GitHub(payload) => github_raw(payload),
        harn_vm::triggers::event::KnownProviderPayload::Slack(payload) => slack_raw(payload),
        harn_vm::triggers::event::KnownProviderPayload::Webhook(payload) => &payload.raw,
        harn_vm::triggers::event::KnownProviderPayload::A2aPush(payload) => &payload.raw,
        _ => return None,
    };
    raw.get("timestamp")
        .and_then(JsonValue::as_str)
        .and_then(|value| {
            OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
}

fn github_raw(payload: &harn_vm::triggers::event::GitHubEventPayload) -> &JsonValue {
    match payload {
        harn_vm::triggers::event::GitHubEventPayload::Issues(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::PullRequest(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::IssueComment(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::PullRequestReview(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::Push(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::WorkflowRun(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::DeploymentStatus(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::CheckRun(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::Other(common) => &common.raw,
    }
}

fn slack_raw(payload: &harn_vm::triggers::event::SlackEventPayload) -> &JsonValue {
    match payload {
        harn_vm::triggers::event::SlackEventPayload::Message(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AppMention(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::ReactionAdded(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AppHomeOpened(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AssistantThreadStarted(inner) => {
            &inner.common.raw
        }
        harn_vm::triggers::event::SlackEventPayload::Other(common) => &common.raw,
    }
}

fn slack_url_verification_challenge(event: &harn_vm::TriggerEvent) -> Option<String> {
    let harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Slack(
        payload,
    )) = &event.provider_payload
    else {
        return None;
    };
    if event.kind != "url_verification" {
        return None;
    }
    slack_raw(payload)
        .get("challenge")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn notion_subscription_verification_response(event: &harn_vm::TriggerEvent) -> Option<Response> {
    let harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Notion(
        payload,
    )) = &event.provider_payload
    else {
        return None;
    };
    if event.kind != "subscription.verification" {
        return None;
    }
    Some(
        (
            StatusCode::OK,
            axum::Json(json!({
                "status": "handshake_captured",
                "verification_token": payload.verification_token,
            })),
        )
            .into_response(),
    )
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256:{encoded}")
}

trait JsonValueExt {
    fn as_toml_str(&self) -> Option<&str>;
}

impl JsonValueExt for toml::Value {
    fn as_toml_str(&self) -> Option<&str> {
        self.as_str()
    }
}

struct HttpError {
    status: StatusCode,
    message: String,
}

impl HttpError {
    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn payment_required(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYMENT_REQUIRED,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    fn from_connector(error: harn_vm::ConnectorError) -> Self {
        match error {
            harn_vm::ConnectorError::MissingHeader(_)
            | harn_vm::ConnectorError::InvalidHeader { .. }
            | harn_vm::ConnectorError::InvalidSignature(_)
            | harn_vm::ConnectorError::TimestampOutOfWindow { .. } => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            harn_vm::ConnectorError::Unsupported(_) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: error.to_string(),
            },
            _ => Self::internal(error.to_string()),
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        (self.status, self.message).into_response()
    }
}

fn test_file_from_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

async fn wait_for_test_release_file(path: &Path) {
    while tokio::fs::metadata(path).await.is_err() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn mark_test_file(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    tokio::fs::write(path, b"1")
        .await
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn listener_binding_key(trigger_id: &str, binding_version: u32) -> String {
    format!("{trigger_id}@v{binding_version}")
}

fn topic_for_event(
    event: &harn_vm::TriggerEvent,
    topic: &Topic,
) -> Result<Topic, harn_vm::event_log::LogError> {
    match event.tenant_id.as_ref() {
        Some(tenant_id) => harn_vm::tenant_topic(tenant_id, topic),
        None => Ok(topic.clone()),
    }
}

fn current_unix_ms() -> i64 {
    unix_ms(OffsetDateTime::now_utc())
}

fn unix_ms(timestamp: OffsetDateTime) -> i64 {
    (timestamp.unix_timestamp_nanos() / 1_000_000) as i64
}

fn duration_between_ms(later_ms: i64, earlier_ms: i64) -> Duration {
    Duration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
}

fn read_u32_env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
// Tests below hold the shared `lock_harn_state` guard across `.await`
// points; the guard is dropped when each `#[tokio::test]` future resolves
// so this is safe in practice, matching the pattern already in
// `mcp/serve.rs`.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use harn_vm::event_log::{
        install_default_for_base_dir, reset_active_event_log, EventLog, Topic,
    };
    use harn_vm::secrets::{
        RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
    };
    use harn_vm::{
        ProviderId, TriggerBindingSource, TriggerBindingSpec, TriggerHandlerSpec,
        TriggerRetryConfig,
    };
    use sha2::{Digest, Sha256};
    use tempfile::{tempdir, TempDir};

    use crate::tests::common::harn_state_lock::lock_harn_state;

    fn manifest_binding_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "a2a-push".to_string(),
            provider: ProviderId::from("a2a-push"),
            autonomy_tier: harn_vm::AutonomyTier::ActAuto,
            handler: TriggerHandlerSpec::Worker {
                queue: "triage".to_string(),
            },
            dispatch_priority: harn_vm::WorkerQueuePriority::Normal,
            when: None,
            when_budget: None,
            retry: TriggerRetryConfig::default(),
            match_events: vec!["a2a.task.received".to_string()],
            dedupe_key: None,
            dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
            filter: None,
            daily_cost_usd: None,
            hourly_cost_usd: None,
            max_autonomous_decisions_per_hour: None,
            max_autonomous_decisions_per_day: None,
            on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::False,
            max_concurrent: None,
            flow_control: harn_vm::TriggerFlowControlConfig::default(),
            manifest_path: None,
            package_name: Some("listener-test".to_string()),
            definition_fingerprint: fingerprint.to_string(),
        }
    }

    fn route(path: &str, version: u32) -> RouteConfig {
        RouteConfig {
            trigger_id: "incoming-review-task".to_string(),
            binding_version: version,
            provider: ProviderId::from("a2a-push"),
            path: path.to_string(),
            auth_mode: AuthMode::Public,
            signature_mode: SignatureMode::Unsigned,
            signing_secret: None,
            dedupe_key_template: None,
            dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
            connector_ingress: false,
            connector: None,
        }
    }

    #[derive(Clone)]
    struct StaticSecretProvider {
        secret_id: SecretId,
        secret: String,
    }

    #[async_trait::async_trait]
    impl SecretProvider for StaticSecretProvider {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            if id == &self.secret_id {
                Ok(SecretBytes::from(self.secret.clone()))
            } else {
                Err(SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: id.clone(),
                })
            }
        }

        async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Ok(())
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            Ok(RotationHandle {
                provider: self.namespace().to_string(),
                id: id.clone(),
                from_version: None,
                to_version: None,
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            "listener-test"
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    fn webhook_route(path: &str) -> RouteConfig {
        RouteConfig {
            trigger_id: "github-webhook".to_string(),
            binding_version: 1,
            provider: ProviderId::from("github"),
            path: path.to_string(),
            auth_mode: AuthMode::Public,
            signature_mode: SignatureMode::GitHub,
            signing_secret: Some(SecretId::new("github", "test-signing-secret")),
            dedupe_key_template: Some("event.dedupe_key".to_string()),
            dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
            connector_ingress: false,
            connector: None,
        }
    }

    fn github_signature(secret: &str, body: &[u8]) -> String {
        const BLOCK: usize = 64;
        let mut key = secret.as_bytes().to_vec();
        if key.len() > BLOCK {
            key = Sha256::digest(&key).to_vec();
        }
        key.resize(BLOCK, 0);
        let mut inner_pad = vec![0x36; BLOCK];
        let mut outer_pad = vec![0x5c; BLOCK];
        for i in 0..BLOCK {
            inner_pad[i] ^= key[i];
            outer_pad[i] ^= key[i];
        }
        let mut inner = Sha256::new();
        inner.update(&inner_pad);
        inner.update(body);
        let inner_digest = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(&outer_pad);
        outer.update(inner_digest);
        let digest = outer.finalize();
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("sha256={encoded}")
    }

    fn authorized_acp_request(
        addr: std::net::SocketAddr,
    ) -> tokio_tungstenite::tungstenite::http::Request<()> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = format!("ws://{addr}{ACP_PATH}")
            .into_client_request()
            .expect("client request");
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            "Bearer ws-test-key".parse().expect("authorization header"),
        );
        request
    }

    async fn next_acp_text(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> JsonValue {
        loop {
            let message = socket
                .next()
                .await
                .expect("websocket message")
                .expect("websocket ok");
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                return serde_json::from_str(&text).expect("json-rpc text");
            }
        }
    }

    async fn acp_request(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: u64,
        method: &str,
        params: JsonValue,
    ) -> JsonValue {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send acp request");
        loop {
            let message = next_acp_text(socket).await;
            if message.get("method").is_some() && message.get("id").is_some() {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .expect("send host response");
                continue;
            }
            if message.get("id").and_then(JsonValue::as_u64) == Some(id) {
                return message;
            }
        }
    }

    async fn new_acp_session(addr: std::net::SocketAddr) -> String {
        let (mut socket, _) = tokio_tungstenite::connect_async(authorized_acp_request(addr))
            .await
            .expect("connect acp websocket");
        let response = acp_request(&mut socket, 1, "session/new", json!({})).await;
        response["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_string()
    }

    async fn send_acp_request(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: u64,
        method: &str,
        params: JsonValue,
    ) {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send acp request");
    }

    async fn send_acp_response(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: u64,
        result: JsonValue,
    ) {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send acp response");
    }

    async fn start_acp_test_listener() -> (ListenerRuntime, Arc<AnyEventLog>, TempDir) {
        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(harn_vm::secrets::EnvSecretProvider::new(
                "harn/listener-test",
            )),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: Vec::new(),
            tenant_store: None,
        })
        .await
        .expect("start listener");
        (listener, log, dir)
    }

    async fn pending_events(log: &Arc<AnyEventLog>) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
        log.read_range(&Topic::new(PENDING_TOPIC).expect("pending topic"), None, 16)
            .await
            .expect("read pending events")
    }

    async fn claim_events(log: &Arc<AnyEventLog>) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
        log.read_range(
            &Topic::new(harn_vm::TRIGGER_INBOX_CLAIMS_TOPIC).expect("claims topic"),
            None,
            16,
        )
        .await
        .expect("read claim events")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readyz_tracks_listener_readiness_gate() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        let (listener, _log, _dir) = start_acp_test_listener().await;
        let url = format!("{}/readyz", listener.url());
        let client = reqwest::Client::new();

        let response = client.get(&url).send().await.expect("readyz before ready");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        listener.mark_ready();
        let response = client.get(&url).send().await.expect("readyz after ready");
        assert_eq!(response.status(), StatusCode::OK);

        listener.mark_not_ready();
        let response = client
            .get(&url)
            .send()
            .await
            .expect("readyz after not ready");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_websocket_requires_configured_bearer_auth() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(API_KEYS_ENV, "ws-test-key");
        std::env::remove_var(HMAC_SECRET_ENV);

        let (listener, _log, _dir) = start_acp_test_listener().await;

        let unauthorized =
            tokio_tungstenite::connect_async(format!("ws://{}{}", listener.local_addr(), ACP_PATH))
                .await;
        assert!(unauthorized.is_err(), "missing bearer should fail upgrade");

        let (mut socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("authorized connect");
        let response = acp_request(&mut socket, 1, "initialize", json!({})).await;
        assert_eq!(response["result"]["agentInfo"]["name"], "harn");
        assert!(
            response["result"]["agentCapabilities"]["sessionCapabilities"]
                .get("load")
                .is_some()
        );

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(API_KEYS_ENV);
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_websocket_parallel_clients_get_distinct_sessions_and_can_load_active_session() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(API_KEYS_ENV, "ws-test-key");
        std::env::remove_var(HMAC_SECRET_ENV);

        let (listener, _log, _dir) = start_acp_test_listener().await;

        let (first, second) = tokio::join!(
            new_acp_session(listener.local_addr()),
            new_acp_session(listener.local_addr())
        );
        assert_ne!(first, second);

        let (mut socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("authorized connect");
        let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
        let session_id = created["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_string();
        let loaded = acp_request(
            &mut socket,
            2,
            "session/load",
            json!({"sessionId": session_id}),
        )
        .await;
        assert_eq!(
            loaded["result"]["session"]["sessionId"],
            created["result"]["sessionId"]
        );
        let prompted = acp_request(
            &mut socket,
            3,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "println(\"websocket prompt\")"}],
            }),
        )
        .await;
        assert_eq!(prompted["result"]["stopReason"], "completed");

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(API_KEYS_ENV);
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_websocket_rejects_duplicate_attach_to_live_session() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(API_KEYS_ENV, "ws-test-key");
        std::env::remove_var(HMAC_SECRET_ENV);

        let (listener, _log, _dir) = start_acp_test_listener().await;
        let (mut first_socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("first connect");
        let created = acp_request(&mut first_socket, 1, "session/new", json!({})).await;
        let session_id = created["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_string();

        let (mut second_socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("second connect");
        let loaded = acp_request(
            &mut second_socket,
            2,
            "session/load",
            json!({"sessionId": session_id}),
        )
        .await;
        assert_eq!(loaded["error"]["code"], json!(-32010));

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(API_KEYS_ENV);
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_websocket_reconnect_replays_pending_host_request_and_completes_prompt() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(API_KEYS_ENV, "ws-test-key");
        std::env::remove_var(HMAC_SECRET_ENV);

        let (listener, _log, _dir) = start_acp_test_listener().await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("connect");
        let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
        let session_id = created["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_string();

        send_acp_request(
            &mut socket,
            2,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "println(\"reconnect\")"}],
            }),
        )
        .await;
        let host_request = loop {
            let message = next_acp_text(&mut socket).await;
            if message.get("method").is_some() && message.get("id").is_some() {
                break message;
            }
        };
        let host_request_id = host_request["id"].as_u64().expect("host request id");
        let replay_from = host_request["_harn"]["eventId"]
            .as_u64()
            .expect("host request event id")
            .saturating_sub(1);
        socket.close(None).await.expect("close first socket");
        drop(socket);
        tokio::time::sleep(Duration::from_millis(250)).await;

        let (mut reconnected, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("reconnect");
        send_acp_request(
            &mut reconnected,
            3,
            "session/load",
            json!({
                "sessionId": session_id,
                "lastAckedEventId": replay_from,
            }),
        )
        .await;

        let mut saw_replayed_host_request = false;
        let mut saw_load_response = false;
        let mut saw_prompt_response = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !(saw_replayed_host_request && saw_load_response && saw_prompt_response) {
                let message = next_acp_text(&mut reconnected).await;
                if message.get("method").is_some() && message.get("id").is_some() {
                    if message.get("id").and_then(JsonValue::as_u64) == Some(host_request_id) {
                        assert_eq!(message["_harn"]["replayed"], json!(true));
                        saw_replayed_host_request = true;
                    }
                    let id = message["id"].as_u64().expect("host request id");
                    send_acp_response(&mut reconnected, id, json!({})).await;
                } else if message.get("id").and_then(JsonValue::as_u64) == Some(3) {
                    assert_eq!(message["result"]["session"]["sessionId"], json!(session_id));
                    saw_load_response = true;
                } else if message.get("id").and_then(JsonValue::as_u64) == Some(2) {
                    assert_eq!(message["result"]["stopReason"], json!("completed"));
                    saw_prompt_response = true;
                }
            }
        })
        .await
        .expect("reconnect flow completed");

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(API_KEYS_ENV);
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_websocket_replays_serialized_events_after_worker_expiry() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(API_KEYS_ENV, "ws-test-key");
        std::env::remove_var(HMAC_SECRET_ENV);
        std::env::set_var(ACP_RETAINED_SESSION_SECS_ENV, "1");

        let (listener, _log, _dir) = start_acp_test_listener().await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("connect");
        let created = acp_request(&mut socket, 1, "session/new", json!({})).await;
        let session_id = created["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_string();
        let replay_from = created["_harn"]["eventId"]
            .as_u64()
            .expect("created event id")
            .saturating_sub(1);
        socket.close(None).await.expect("close socket");
        drop(socket);
        tokio::time::sleep(Duration::from_millis(2_500)).await;

        let (mut reconnected, _) =
            tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
                .await
                .expect("reconnect");
        send_acp_request(
            &mut reconnected,
            4,
            "session/load",
            json!({
                "sessionId": session_id,
                "lastAckedEventId": replay_from,
            }),
        )
        .await;

        let mut saw_persisted_replay = false;
        let mut saw_expired_session_error = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !(saw_persisted_replay && saw_expired_session_error) {
                let message = next_acp_text(&mut reconnected).await;
                if message["_harn"]["replayed"] == json!(true) {
                    assert_eq!(message["result"]["sessionId"], json!(session_id));
                    saw_persisted_replay = true;
                }
                if message.get("id").and_then(JsonValue::as_u64) == Some(4) {
                    assert_eq!(message["error"]["code"], json!(-32004));
                    saw_expired_session_error = true;
                }
            }
        })
        .await
        .expect("expired replay flow completed");

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(API_KEYS_ENV);
        std::env::remove_var(ACP_RETAINED_SESSION_SECS_ENV);
        reset_active_event_log();
    }

    fn unix_now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_millis() as i64
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn reload_swaps_routes_without_losing_inflight_request() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        harn_vm::clear_trigger_registry();

        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        harn_vm::install_manifest_triggers(vec![manifest_binding_spec(
            "incoming-review-task",
            "v1",
        )])
        .await
        .expect("install v1 binding");

        let request_entered_path = dir.path().join("request-entered");
        let request_release_path = dir.path().join("request-release");
        std::env::set_var(REQUEST_ENTERED_FILE_ENV, &request_entered_path);
        std::env::set_var(REQUEST_RELEASE_FILE_ENV, &request_release_path);
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(harn_vm::secrets::EnvSecretProvider::new(
                "harn/listener-test",
            )),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![route("/a2a/v1", 1)],
            tenant_store: None,
        })
        .await
        .expect("start listener");

        let client = reqwest::Client::new();
        let first_url = format!("http://{}/a2a/v1", listener.local_addr());
        let second_url = format!("http://{}/a2a/v2", listener.local_addr());

        let first_request = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .post(first_url)
                    .json(&json!({"task_id": "task-1", "sender": "alpha"}))
                    .send()
                    .await
                    .expect("first request")
                    .status()
            })
        };

        wait_for_test_release_file(&request_entered_path).await;
        harn_vm::install_manifest_triggers(vec![manifest_binding_spec(
            "incoming-review-task",
            "v2",
        )])
        .await
        .expect("install v2 binding");
        listener
            .reload_routes(vec![route("/a2a/v2", 2)])
            .expect("reload listener routes");
        tokio::fs::write(&request_release_path, b"release")
            .await
            .expect("release first request");

        assert_eq!(
            first_request.await.expect("join first request"),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(&second_url)
                .json(&json!({"task_id": "task-2", "sender": "beta"}))
                .send()
                .await
                .expect("second request")
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("http://{}/a2a/v1", listener.local_addr()))
                .json(&json!({"task_id": "task-old", "sender": "gamma"}))
                .send()
                .await
                .expect("old route request")
                .status(),
            StatusCode::NOT_FOUND
        );

        let pending_topic = Topic::new(PENDING_TOPIC).expect("pending topic");
        let events = log
            .read_range(&pending_topic, None, 16)
            .await
            .expect("read pending events");
        let versions: Vec<u64> = events
            .iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("binding_version")
                    .and_then(JsonValue::as_u64)
            })
            .collect();
        let task_ids: Vec<String> = events
            .iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("event")
                    .and_then(|value| value.get("provider_payload"))
                    .and_then(|value| value.get("task_id"))
                    .and_then(JsonValue::as_str)
                    .map(|value| value.to_string())
            })
            .collect();
        assert_eq!(versions, vec![1, 2]);
        assert_eq!(task_ids, vec!["task-1".to_string(), "task-2".to_string()]);

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(REQUEST_ENTERED_FILE_ENV);
        std::env::remove_var(REQUEST_RELEASE_FILE_ENV);
        reset_active_event_log();
        harn_vm::clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webhook_first_delivery_is_appended() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
        })
        .await
        .expect("start listener");

        let body = br#"{"action":"opened","issue":{"number":1}}"#;
        let response = reqwest::Client::new()
            .post(format!("http://{}/hooks/github", listener.local_addr()))
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-1")
            .header("X-Hub-Signature-256", github_signature("topsecret", body))
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send webhook");

        assert_eq!(response.status(), StatusCode::OK);
        let payload: JsonValue = response.json().await.expect("response json");
        assert_eq!(
            payload.get("status"),
            Some(&JsonValue::String("accepted".to_string()))
        );

        let events = pending_events(&log).await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]
                .1
                .payload
                .get("event")
                .and_then(|value| value.get("dedupe_key"))
                .and_then(JsonValue::as_str),
            Some("delivery-1")
        );

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webhook_ingest_saturation_returns_retry_after() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        std::env::set_var(INGEST_PER_SOURCE_CAPACITY_ENV, "1");
        std::env::set_var(INGEST_GLOBAL_CAPACITY_ENV, "100");
        std::env::set_var(INGEST_REFILL_PER_SEC_ENV, "1");

        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        let metrics = Arc::new(harn_vm::MetricsRegistry::default());
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: metrics.clone(),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
        })
        .await
        .expect("start listener");

        let body = br#"{"action":"opened","issue":{"number":1}}"#;
        let signature = github_signature("topsecret", body);
        let client = reqwest::Client::new();
        let url = format!("http://{}/hooks/github", listener.local_addr());

        let first = client
            .post(&url)
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-1")
            .header("X-Hub-Signature-256", &signature)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send first webhook");
        assert_eq!(first.status(), StatusCode::OK);

        let saturated = client
            .post(&url)
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-2")
            .header("X-Hub-Signature-256", &signature)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send saturated webhook");
        assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            saturated
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );

        let events = pending_events(&log).await;
        assert_eq!(events.len(), 1);
        assert!(metrics
            .render_prometheus()
            .contains("harn_backpressure_events_total{action=\"reject\",dimension=\"ingest\"} 1"));

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        std::env::remove_var(INGEST_PER_SOURCE_CAPACITY_ENV);
        std::env::remove_var(INGEST_GLOBAL_CAPACITY_ENV);
        std::env::remove_var(INGEST_REFILL_PER_SEC_ENV);
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webhook_duplicate_delivery_is_dropped() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![webhook_route("/hooks/github")],
            tenant_store: None,
        })
        .await
        .expect("start listener");

        let body = br#"{"action":"opened","issue":{"number":1}}"#;
        let signature = github_signature("topsecret", body);
        let client = reqwest::Client::new();
        let url = format!("http://{}/hooks/github", listener.local_addr());

        let first = client
            .post(&url)
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-1")
            .header("X-Hub-Signature-256", &signature)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send first webhook");
        assert_eq!(first.status(), StatusCode::OK);

        let duplicate = client
            .post(&url)
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-1")
            .header("X-Hub-Signature-256", &signature)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send duplicate webhook");

        assert_eq!(duplicate.status(), StatusCode::OK);
        let payload: JsonValue = duplicate.json().await.expect("duplicate response json");
        assert_eq!(
            payload.get("status"),
            Some(&JsonValue::String("duplicate_dropped".to_string()))
        );

        let events = pending_events(&log).await;
        assert_eq!(events.len(), 1);

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webhook_dedupe_claim_uses_route_retention_days() {
        let _guard = lock_harn_state();
        reset_active_event_log();
        let dir = tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        let mut route = webhook_route("/hooks/github");
        route.dedupe_retention_days = 3;
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(StaticSecretProvider {
                secret_id: SecretId::new("github", "test-signing-secret"),
                secret: "topsecret".to_string(),
            }),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            metrics_registry: Arc::new(harn_vm::MetricsRegistry::default()),
            admin_reload: None,
            mcp_router: None,
            routes: vec![route],
            tenant_store: None,
        })
        .await
        .expect("start listener");

        let body = br#"{"action":"opened","issue":{"number":1}}"#;
        let before_ms = unix_now_ms();
        let response = reqwest::Client::new()
            .post(format!("http://{}/hooks/github", listener.local_addr()))
            .header("X-GitHub-Event", "issues")
            .header("X-GitHub-Delivery", "delivery-ttl")
            .header("X-Hub-Signature-256", github_signature("topsecret", body))
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("send webhook");
        let after_ms = unix_now_ms();

        assert_eq!(response.status(), StatusCode::OK);
        let claims = claim_events(&log).await;
        assert_eq!(claims.len(), 1);
        let claim = &claims[0].1.payload;
        assert_eq!(
            claim.get("binding_id").and_then(JsonValue::as_str),
            Some("github-webhook")
        );
        assert_eq!(
            claim.get("dedupe_key").and_then(JsonValue::as_str),
            Some("delivery-ttl")
        );
        let expires_at_ms = claim
            .get("expires_at_ms")
            .and_then(JsonValue::as_i64)
            .expect("claim expires_at_ms");
        let ttl_ms = 3 * 24 * 60 * 60 * 1000;
        assert!(
            (before_ms + ttl_ms..=after_ms + ttl_ms).contains(&expires_at_ms),
            "expires_at_ms {expires_at_ms} should use 3 day route retention"
        );

        listener
            .shutdown(Duration::from_secs(5))
            .await
            .expect("shutdown listener");
        reset_active_event_log();
    }
}
