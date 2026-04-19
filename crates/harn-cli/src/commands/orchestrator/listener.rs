use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, OriginalUri, Query};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::secrets::{SecretId, SecretProvider, SecretVersion};

use crate::commands::orchestrator::origin_guard::{enforce_allowed_origin, OriginAllowList};
use crate::commands::orchestrator::tls::{ServerRuntime, TlsFiles};
use crate::package::{CollectedManifestTrigger, TriggerKind};

const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const REQUEST_DELAY_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_DELAY_MS";
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

impl ListenerRuntime {
    pub(crate) async fn start(config: ListenerConfig) -> Result<Self, String> {
        let pending_topic =
            Topic::new(PENDING_TOPIC).map_err(|error| format!("invalid pending topic: {error}"))?;
        let requires_auth = config
            .routes
            .iter()
            .any(|route| route.auth_mode.requires_credentials());
        let auth = Arc::new(ListenerAuth::from_env(requires_auth)?);
        let request_delay = std::env::var(REQUEST_DELAY_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_millis);
        let origin_state = Arc::new(config.allowed_origins.clone());
        let routes = Arc::new(RouteRegistry::new(
            config.routes,
            config.event_log.clone(),
            config.secrets.clone(),
            auth.clone(),
            pending_topic.clone(),
            request_delay,
        )?);
        let app = Router::new()
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
}

impl RouteConfig {
    pub(crate) fn from_trigger(
        trigger: &CollectedManifestTrigger,
        binding_version: u32,
    ) -> Result<Option<Self>, String> {
        match trigger.config.kind {
            TriggerKind::Webhook => {
                let provider = trigger.config.provider.clone();
                let signature_mode = match provider.as_str() {
                    "github" => SignatureMode::GitHub,
                    "webhook" => SignatureMode::Standard,
                    other => {
                        return Err(format!(
                            "HTTP listener does not yet support webhook provider '{other}' on this branch"
                        ))
                    }
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
                }))
            }
            TriggerKind::A2aPush => Ok(Some(Self {
                trigger_id: trigger.config.id.clone(),
                binding_version,
                provider: harn_vm::ProviderId::from("a2a-push"),
                path: trigger_path(trigger)?,
                auth_mode: AuthMode::BearerOrHmac,
                signature_mode: SignatureMode::Unsigned,
                signing_secret: None,
            })),
            _ => Ok(None),
        }
    }
}

#[derive(Clone)]
struct RouteContext {
    route: RouteConfig,
    event_log: Arc<AnyEventLog>,
    secrets: Arc<dyn SecretProvider>,
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_delay: Option<Duration>,
    metrics: Arc<RouteRuntimeMetrics>,
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
    secrets: Arc<dyn SecretProvider>,
    auth: Arc<ListenerAuth>,
    pending_topic: Topic,
    request_delay: Option<Duration>,
}

impl RouteRegistry {
    fn new(
        routes: Vec<RouteConfig>,
        event_log: Arc<AnyEventLog>,
        secrets: Arc<dyn SecretProvider>,
        auth: Arc<ListenerAuth>,
        pending_topic: Topic,
        request_delay: Option<Duration>,
    ) -> Result<Self, String> {
        let registry = Self {
            routes_by_path: RwLock::new(BTreeMap::new()),
            metrics_by_trigger_id: Mutex::new(BTreeMap::new()),
            event_log,
            secrets,
            auth,
            pending_topic,
            request_delay,
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
                    secrets: self.secrets.clone(),
                    auth: self.auth.clone(),
                    pending_topic: self.pending_topic.clone(),
                    request_delay: self.request_delay,
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
        return error.into_response();
    }

    if let Some(delay) = context.request_delay {
        tokio::time::sleep(delay).await;
    }

    let result = normalize_request(&context, &normalized_headers, &query, body.as_ref()).await;
    let response = match result {
        Ok(event) => {
            let payload = json!({
                "trigger_id": context.route.trigger_id,
                "binding_version": context.route.binding_version,
                "event": event,
            });
            match context
                .event_log
                .append(
                    &context.pending_topic,
                    LogEvent::new("trigger_event", payload),
                )
                .await
            {
                Ok(event_id) => {
                    context.metrics.dispatched.fetch_add(1, Ordering::Relaxed);
                    context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "status": "accepted",
                            "event_id": event_id,
                            "trigger_id": context.route.trigger_id,
                        })),
                    )
                        .into_response()
                }
                Err(error) => {
                    context.metrics.failed.fetch_add(1, Ordering::Relaxed);
                    context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to append trigger event to pending log: {error}"),
                    )
                        .into_response()
                }
            }
        }
        Err(error) => {
            context.metrics.failed.fetch_add(1, Ordering::Relaxed);
            context.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            error.into_response()
        }
    };

    response
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
    _query: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<harn_vm::TriggerEvent, HttpError> {
    let received_at = OffsetDateTime::now_utc();
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
    let dedupe_key = dedupe_key(
        context.route.signature_mode,
        normalized_headers,
        &normalized_body,
        body,
    );
    let provider_payload = harn_vm::ProviderPayload::normalize(
        &provider,
        &provider_kind,
        normalized_headers,
        normalized_body,
    )
    .map_err(|error| HttpError::unprocessable(error.to_string()))?;

    Ok(harn_vm::TriggerEvent {
        id: harn_vm::TriggerEventId::new(),
        provider,
        kind: trigger_kind,
        received_at,
        occurred_at: infer_occurred_at(&provider_payload),
        dedupe_key,
        trace_id: harn_vm::TraceId::new(),
        tenant_id: None,
        headers: harn_vm::redact_headers(
            normalized_headers,
            &harn_vm::HeaderRedactionPolicy::default(),
        ),
        provider_payload,
        signature_status,
    })
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
struct ListenerAuth {
    api_keys: Vec<String>,
    hmac_secret: Option<String>,
}

impl ListenerAuth {
    fn from_env(required: bool) -> Result<Self, String> {
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

    async fn authorize(
        &self,
        event_log: &AnyEventLog,
        method: &str,
        path: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<(), ()> {
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

    fn matches_api_key(&self, candidate: &str) -> bool {
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
    signature_mode: SignatureMode,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
    raw_body: &[u8],
) -> String {
    match signature_mode {
        SignatureMode::GitHub => header_value(headers, "x-github-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        SignatureMode::Standard => header_value(headers, "webhook-id")
            .map(ToString::to_string)
            .or_else(|| {
                body.get("id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        SignatureMode::Unsigned => header_value(headers, "x-a2a-delivery")
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
        harn_vm::triggers::event::GitHubEventPayload::Other(common) => &common.raw,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::event_log::{
        install_default_for_base_dir, reset_active_event_log, EventLog, Topic,
    };
    use harn_vm::{
        ProviderId, TriggerBindingSource, TriggerBindingSpec, TriggerHandlerSpec,
        TriggerRetryConfig,
    };
    use tempfile::tempdir;

    static REQUEST_DELAY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn manifest_binding_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "a2a-push".to_string(),
            provider: ProviderId::from("a2a-push"),
            handler: TriggerHandlerSpec::Worker {
                queue: "triage".to_string(),
            },
            when: None,
            retry: TriggerRetryConfig::default(),
            match_events: vec!["a2a.task.received".to_string()],
            dedupe_key: None,
            dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
            filter: None,
            daily_cost_usd: None,
            max_concurrent: None,
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
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn reload_swaps_routes_without_losing_inflight_request() {
        let _env_guard = REQUEST_DELAY_LOCK.lock().expect("request delay lock");
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

        std::env::set_var(REQUEST_DELAY_ENV, "200");
        let listener = ListenerRuntime::start(ListenerConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            tls: None,
            event_log: log.clone(),
            secrets: Arc::new(harn_vm::secrets::EnvSecretProvider::new(
                "harn/listener-test",
            )),
            allowed_origins: OriginAllowList::wildcard(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
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

        tokio::time::sleep(Duration::from_millis(50)).await;
        harn_vm::install_manifest_triggers(vec![manifest_binding_spec(
            "incoming-review-task",
            "v2",
        )])
        .await
        .expect("install v2 binding");
        listener
            .reload_routes(vec![route("/a2a/v2", 2)])
            .expect("reload listener routes");

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

        listener.shutdown().await.expect("shutdown listener");
        std::env::remove_var(REQUEST_DELAY_ENV);
        reset_active_event_log();
        harn_vm::clear_trigger_registry();
    }
}
