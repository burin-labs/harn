use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Extension, OriginalUri, Query};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
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
    pub(crate) routes: Vec<RouteConfig>,
}

impl ListenerConfig {
    pub(crate) fn max_body_bytes_or_default(max_body_bytes: Option<usize>) -> usize {
        max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES)
    }
}

pub(crate) struct ListenerRuntime {
    server: ServerRuntime,
    routes: Arc<RouteRegistry>,
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
        let routes = Arc::new(RouteRegistry::new(
            config.routes,
            config.event_log.clone(),
            inbox,
            config.secrets.clone(),
            config.metrics_registry.clone(),
            auth.clone(),
            pending_topic.clone(),
            request_gate,
        )?);
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
                get(|| async move { (StatusCode::OK, "ready").into_response() }),
            )
            .route(
                "/metrics",
                get(metrics_endpoint).layer(Extension(config.metrics_registry.clone())),
            );
        if let Some(admin_state) = admin_state {
            app = app.route(
                ADMIN_RELOAD_PATH,
                post(admin_reload_endpoint).layer(Extension(admin_state)),
            );
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
            ))
            .with_state(origin_state);

        let server = ServerRuntime::start(config.bind, app, config.tls.as_ref()).await?;
        Ok(Self { server, routes })
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
        let Self { server, routes } = self;
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
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_gate: TestRequestGate,
    metrics: Arc<RouteRuntimeMetrics>,
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
    event_log: Arc<AnyEventLog>,
    inbox: Arc<harn_vm::InboxIndex>,
    secrets: Arc<dyn SecretProvider>,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_gate: TestRequestGate,
}

impl RouteRegistry {
    fn new(
        routes: Vec<RouteConfig>,
        event_log: Arc<AnyEventLog>,
        inbox: Arc<harn_vm::InboxIndex>,
        secrets: Arc<dyn SecretProvider>,
        metrics_registry: Arc<harn_vm::MetricsRegistry>,
        auth: Arc<ListenerAuth>,
        pending_topic: Topic,
        request_gate: TestRequestGate,
    ) -> Result<Self, String> {
        let registry = Self {
            routes_by_path: RwLock::new(BTreeMap::new()),
            metrics_by_trigger_id: Mutex::new(BTreeMap::new()),
            event_log,
            inbox,
            secrets,
            metrics_registry,
            auth,
            pending_topic,
            request_gate,
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
                    auth: self.auth.clone(),
                    pending_topic: self.pending_topic.clone(),
                    request_gate: self.request_gate.clone(),
                    metrics,
                }),
            );
        }
        *self.routes_by_path.write().expect("route table poisoned") = next_routes;
        Ok(())
    }

    fn resolve(&self, path: &str) -> Option<Arc<RouteContext>> {
        self.routes_by_path
            .read()
            .expect("route table poisoned")
            .get(path)
            .cloned()
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

async fn ingest_trigger(
    Extension(routes): Extension<Arc<RouteRegistry>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    let Some(context) = routes.resolve(uri.path()) else {
        return (StatusCode::NOT_FOUND, "trigger route not configured").into_response();
    };

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
        let normalized_headers = normalize_headers(&headers);
        if let Err(error) = authorize_request(
            &context,
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

async fn authorize_request(
    context: &RouteContext,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<(), HttpError> {
    match context.route.auth_mode {
        AuthMode::Public => Ok(()),
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
        });
        let result = connector
            .lock()
            .await
            .normalize_inbound_result(raw)
            .await
            .map_err(HttpError::from_connector)?;
        return connector_normalize_result_to_request(result, trace_id);
    }

    let normalized_body = normalize_body(body, normalized_headers);
    let provider = context.route.provider.clone();

    let signature_status = match context.route.signature_mode {
        SignatureMode::Unsigned => harn_vm::SignatureStatus::Unsigned,
        SignatureMode::GitHub => {
            let secret = load_secret(context, context.route.signing_secret.as_ref()).await?;
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
            let secret = load_secret(context, context.route.signing_secret.as_ref()).await?;
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
        tenant_id: None,
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
            Ok(NormalizedRequest::Events(vec![event]))
        }
        harn_vm::ConnectorNormalizeResult::Batch(mut events) => {
            set_trace_id(&mut events, trace_id);
            Ok(NormalizedRequest::Events(events))
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse {
            response,
            mut events,
        } => {
            set_trace_id(&mut events, trace_id);
            Ok(NormalizedRequest::Immediate {
                response: connector_http_response_to_response(response)?,
                events,
            })
        }
        harn_vm::ConnectorNormalizeResult::Reject(response) => Ok(NormalizedRequest::Rejected(
            connector_http_response_to_response(response)?,
        )),
    }
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
                    .append(&context.pending_topic, log_event)
                    .instrument(queue_span)
                    .await
                    .map_err(|error| {
                        HttpError::internal(format!(
                            "failed to append trigger event to pending log: {error}"
                        ))
                    })?;
                context.metrics_registry.record_event_log_append(
                    context.pending_topic.as_str(),
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
    secret_id: Option<&SecretId>,
) -> Result<String, HttpError> {
    let secret_id = secret_id.ok_or_else(|| {
        HttpError::internal(format!(
            "trigger '{}' requires a signing secret",
            context.route.trigger_id
        ))
    })?;
    let secret = context
        .secrets
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

fn current_unix_ms() -> i64 {
    unix_ms(OffsetDateTime::now_utc())
}

fn unix_ms(timestamp: OffsetDateTime) -> i64 {
    (timestamp.unix_timestamp_nanos() / 1_000_000) as i64
}

fn duration_between_ms(later_ms: i64, earlier_ms: i64) -> Duration {
    Duration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
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
    use tempfile::tempdir;

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
            routes: vec![route("/a2a/v1", 1)],
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
            routes: vec![webhook_route("/hooks/github")],
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
            routes: vec![webhook_route("/hooks/github")],
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
            routes: vec![route],
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
