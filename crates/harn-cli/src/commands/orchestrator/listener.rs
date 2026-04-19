use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::secrets::{SecretId, SecretProvider, SecretVersion};

use crate::commands::orchestrator::origin_guard::{enforce_allowed_origin, OriginAllowList};
use crate::commands::orchestrator::tls::{ServerRuntime, TlsFiles};
use crate::package::{CollectedManifestTrigger, TriggerKind};

const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const REQUEST_DELAY_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_DELAY_MS";

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
    metrics: Arc<BTreeMap<String, Arc<RouteRuntimeMetrics>>>,
}

impl ListenerRuntime {
    pub(crate) async fn start(config: ListenerConfig) -> Result<Self, String> {
        let pending_topic =
            Topic::new(PENDING_TOPIC).map_err(|error| format!("invalid pending topic: {error}"))?;
        let request_delay = std::env::var(REQUEST_DELAY_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_millis);
        let origin_state = Arc::new(config.allowed_origins.clone());
        let mut route_metrics = BTreeMap::new();
        let mut app = Router::new()
            .route(
                "/healthz",
                get(|| async move { (StatusCode::OK, "ok").into_response() }),
            )
            .route(
                "/readyz",
                get(|| async move { (StatusCode::OK, "ready").into_response() }),
            );

        let mut seen_paths = BTreeSet::new();
        for route in &config.routes {
            if !seen_paths.insert(route.path.clone()) {
                return Err(format!(
                    "trigger route '{}' is configured more than once",
                    route.path
                ));
            }
            let context = Arc::new(RouteContext {
                route: route.clone(),
                event_log: config.event_log.clone(),
                secrets: config.secrets.clone(),
                pending_topic: pending_topic.clone(),
                request_delay,
                metrics: Arc::new(RouteRuntimeMetrics::default()),
            });
            route_metrics.insert(route.trigger_id.clone(), context.metrics.clone());
            app = app.route(
                &route.path,
                post(ingest_trigger).layer(Extension(context.clone())),
            );
        }

        let app = app
            .layer(DefaultBodyLimit::max(config.max_body_bytes))
            .layer(middleware::from_fn_with_state(
                origin_state.clone(),
                enforce_allowed_origin,
            ))
            .with_state(origin_state);

        let server = ServerRuntime::start(config.bind, app, config.tls.as_ref()).await?;
        Ok(Self {
            server,
            metrics: Arc::new(route_metrics),
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

    pub(crate) fn trigger_metrics(&self) -> BTreeMap<String, TriggerMetricSnapshot> {
        snapshot_metrics(self.metrics.as_ref())
    }

    pub(crate) async fn shutdown(self) -> Result<BTreeMap<String, TriggerMetricSnapshot>, String> {
        let Self { server, metrics } = self;
        server.shutdown().await?;
        Ok(snapshot_metrics(metrics.as_ref()))
    }
}

#[derive(Clone)]
pub(crate) struct RouteConfig {
    pub(crate) trigger_id: String,
    pub(crate) binding_version: u32,
    pub(crate) provider: harn_vm::ProviderId,
    pub(crate) path: String,
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
    pending_topic: Topic,
    request_delay: Option<Duration>,
    metrics: Arc<RouteRuntimeMetrics>,
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

fn snapshot_metrics(
    metrics: &BTreeMap<String, Arc<RouteRuntimeMetrics>>,
) -> BTreeMap<String, TriggerMetricSnapshot> {
    metrics
        .iter()
        .map(|(trigger_id, metrics)| (trigger_id.clone(), metrics.snapshot()))
        .collect()
}

async fn ingest_trigger(
    Extension(context): Extension<Arc<RouteContext>>,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    context.metrics.received.fetch_add(1, Ordering::Relaxed);
    context.metrics.in_flight.fetch_add(1, Ordering::Relaxed);

    if let Some(delay) = context.request_delay {
        tokio::time::sleep(delay).await;
    }

    let result = normalize_request(&context, &headers, &query, body.as_ref()).await;

    match result {
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
    }
}

async fn normalize_request(
    context: &RouteContext,
    headers: &HeaderMap,
    _query: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<harn_vm::TriggerEvent, HttpError> {
    let received_at = OffsetDateTime::now_utc();
    let normalized_headers = normalize_headers(headers);
    let normalized_body = normalize_body(body, &normalized_headers);
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
                &normalized_headers,
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
                &normalized_headers,
                &secret,
                Some(time::Duration::minutes(5)),
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
    };

    let provider_kind = provider_event_kind(&provider, &normalized_headers, &normalized_body);
    let trigger_kind = trigger_event_kind(&provider, &normalized_headers, &normalized_body);
    let dedupe_key = dedupe_key(
        context.route.signature_mode,
        &normalized_headers,
        &normalized_body,
        body,
    );
    let provider_payload = harn_vm::ProviderPayload::normalize(
        &provider,
        &provider_kind,
        &normalized_headers,
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
            &normalized_headers,
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
        harn_vm::triggers::event::KnownProviderPayload::GitHub(payload) => &payload.raw,
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
