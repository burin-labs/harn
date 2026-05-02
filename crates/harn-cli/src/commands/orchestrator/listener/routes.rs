mod ingest;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Extension, OriginalUri, Query};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tracing::Instrument as _;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::secrets::{SecretId, SecretProvider, SecretVersion};

use self::ingest::{
    authorize_request, current_unix_ms, enqueue_normalized_events, enqueue_summary_response,
    header_value, normalize_request, IngressLifecycleTiming, NormalizedRequest,
};
use crate::commands::orchestrator::errors::OrchestratorError;
use crate::package::{CollectedManifestTrigger, TriggerKind};

pub(super) const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
pub(super) const API_KEYS_ENV: &str = "HARN_ORCHESTRATOR_API_KEYS";
pub(super) const HMAC_SECRET_ENV: &str = "HARN_ORCHESTRATOR_HMAC_SECRET";
const AUTH_TIMESTAMP_WINDOW_SECS: i64 = 5 * 60;
pub(super) const INGEST_GLOBAL_CAPACITY_ENV: &str = "HARN_ORCHESTRATOR_INGEST_GLOBAL_CAPACITY";
pub(super) const INGEST_PER_SOURCE_CAPACITY_ENV: &str =
    "HARN_ORCHESTRATOR_INGEST_PER_SOURCE_CAPACITY";
pub(super) const INGEST_REFILL_PER_SEC_ENV: &str = "HARN_ORCHESTRATOR_INGEST_REFILL_PER_SEC";
const DEFAULT_INGEST_GLOBAL_CAPACITY: u32 = 4096;
const DEFAULT_INGEST_PER_SOURCE_CAPACITY: u32 = 1024;
const DEFAULT_INGEST_REFILL_PER_SEC: u32 = 1024;

pub(super) use self::ingest::{normalize_headers, HttpError};

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
    ) -> Result<Option<Self>, OrchestratorError> {
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
                            ).into())
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
pub(super) struct TestRequestGate {
    pub(super) entered_file: Option<PathBuf>,
    pub(super) release_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthMode {
    Public,
    BearerOrHmac,
}

impl AuthMode {
    pub(super) fn requires_credentials(self) -> bool {
        !matches!(self, Self::Public)
    }
}

pub(super) struct RouteRegistry {
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
    pub(super) fn new(
        routes: Vec<RouteConfig>,
        event_log: Arc<AnyEventLog>,
        inbox: Arc<harn_vm::InboxIndex>,
        secrets: Arc<dyn SecretProvider>,
        metrics_registry: Arc<harn_vm::MetricsRegistry>,
        auth: Arc<ListenerAuth>,
        pending_topic: Topic,
        request_gate: TestRequestGate,
        tenant_store: Option<Arc<harn_vm::TenantStore>>,
    ) -> Result<Self, OrchestratorError> {
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

    pub(super) fn reload(&self, routes: Vec<RouteConfig>) -> Result<(), OrchestratorError> {
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

    pub(super) fn snapshot_metrics(&self) -> BTreeMap<String, TriggerMetricSnapshot> {
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

fn validate_unique_route_paths(routes: &[RouteConfig]) -> Result<(), OrchestratorError> {
    let mut seen_paths = BTreeSet::new();
    for route in routes {
        if !seen_paths.insert(route.path.clone()) {
            return Err(format!(
                "trigger route '{}' is configured more than once",
                route.path
            )
            .into());
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

pub(super) async fn ingest_trigger(
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
                    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
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

fn trigger_path(trigger: &CollectedManifestTrigger) -> Result<String, OrchestratorError> {
    let path = trigger
        .config
        .kind_specific
        .get("path")
        .and_then(JsonValueExt::as_toml_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("/triggers/{}", trigger.config.id));
    if !path.starts_with('/') {
        return Err(format!("trigger '{}' path must start with '/'", trigger.config.id).into());
    }
    Ok(path)
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
    pub(crate) fn from_env(required: bool) -> Result<Self, OrchestratorError> {
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
            ).into());
        }
        if required && hmac_secret.is_none() {
            return Err(format!(
                "{HMAC_SECRET_ENV} must be set when a2a-push routes are configured"
            )
            .into());
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

trait JsonValueExt {
    fn as_toml_str(&self) -> Option<&str>;
}

impl JsonValueExt for toml::Value {
    fn as_toml_str(&self) -> Option<&str> {
        self.as_str()
    }
}

pub(super) fn test_file_from_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(super) async fn wait_for_test_release_file(path: &Path) {
    while tokio::fs::metadata(path).await.is_err() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn mark_test_file(path: &Path) -> Result<(), OrchestratorError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    tokio::fs::write(path, b"1").await.map_err(|error| {
        OrchestratorError::Listener(format!("failed to write {}: {error}", path.display()))
    })
}

fn read_u32_env(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
