//! Connector traits and shared helpers for inbound event-source providers.
//!
//! This lands in `harn-vm` for now because the current dependency surface
//! (`EventLog`, `SecretProvider`, `TriggerEvent`) already lives here. If the
//! connector ecosystem grows enough to justify extraction, the module can be
//! split into a dedicated crate later without changing the high-level contract.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use tokio::sync::Mutex as AsyncMutex;

use crate::event_log::AnyEventLog;
use crate::secrets::SecretProvider;
use crate::triggers::test_util::clock::{self, ClockInstant};
use crate::triggers::{
    registered_provider_metadata, InboxIndex, ProviderId, ProviderMetadata,
    ProviderRuntimeMetadata, TenantId, TriggerEvent,
};

pub mod a2a_push;
pub mod cron;
pub mod effect_policy;
pub mod github;
pub mod harn_module;
pub mod hmac;
pub mod linear;
pub mod notion;
pub mod slack;
pub mod stream;
#[cfg(test)]
pub(crate) mod test_util;
pub mod testkit;
pub mod webhook;

pub use a2a_push::A2aPushConnector;
pub use cron::{CatchupMode, CronConnector};
pub use effect_policy::{
    connector_export_denied_builtin_reason, connector_export_effect_class,
    default_connector_export_policy, ConnectorExportEffectClass, HarnConnectorEffectPolicies,
};
pub use github::GitHubConnector;
pub use harn_module::{
    load_contract as load_harn_connector_contract, HarnConnector, HarnConnectorContract,
};
pub use hmac::{
    verify_hmac_authorization, HmacSignatureStyle, DEFAULT_CANONICAL_AUTHORIZATION_HEADER,
    DEFAULT_CANONICAL_HMAC_SCHEME, DEFAULT_GITHUB_SIGNATURE_HEADER,
    DEFAULT_LINEAR_SIGNATURE_HEADER, DEFAULT_NOTION_SIGNATURE_HEADER,
    DEFAULT_SLACK_SIGNATURE_HEADER, DEFAULT_SLACK_TIMESTAMP_HEADER,
    DEFAULT_STANDARD_WEBHOOKS_ID_HEADER, DEFAULT_STANDARD_WEBHOOKS_SIGNATURE_HEADER,
    DEFAULT_STANDARD_WEBHOOKS_TIMESTAMP_HEADER, DEFAULT_STRIPE_SIGNATURE_HEADER,
    SIGNATURE_VERIFY_AUDIT_TOPIC,
};
pub use linear::LinearConnector;
pub use notion::{
    load_pending_webhook_handshakes, NotionConnector, PersistedNotionWebhookHandshake,
};
pub use slack::SlackConnector;
pub use stream::StreamConnector;
use webhook::WebhookProviderProfile;
pub use webhook::{GenericWebhookConnector, WebhookSignatureVariant};

const OUTBOUND_CONNECTOR_HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(30);

pub(crate) fn outbound_http_client(user_agent: &'static str) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(OUTBOUND_CONNECTOR_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if crate::egress::redirect_url_allowed(
                "connector_redirect",
                attempt.url().as_str(),
            ) {
                attempt.follow()
            } else {
                attempt.error("egress policy blocked redirect target")
            }
        }))
        .build()
        .expect("connector HTTP client configuration should be valid")
}

/// Shared owned handle to a connector instance registered with the runtime.
pub type ConnectorHandle = Arc<AsyncMutex<Box<dyn Connector>>>;

thread_local! {
    static ACTIVE_CONNECTOR_CLIENTS: RefCell<BTreeMap<String, Arc<dyn ConnectorClient>>> =
        RefCell::new(BTreeMap::new());
}

/// Provider implementation contract for inbound connectors.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Stable provider id such as `github`, `slack`, or `webhook`.
    fn provider_id(&self) -> &ProviderId;

    /// Trigger kinds this connector supports (`webhook`, `poll`, `stream`, ...).
    fn kinds(&self) -> &[TriggerKind];

    /// Called once per connector instance at orchestrator startup.
    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError>;

    /// Activate the bindings relevant to this connector instance.
    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError>;

    /// Stop connector-owned background work and flush any connector-local state.
    async fn shutdown(&self, _deadline: StdDuration) -> Result<(), ConnectorError> {
        Ok(())
    }

    /// Verify + normalize a provider-native inbound request into `TriggerEvent`.
    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError>;

    /// Verify + normalize a provider-native inbound request into the richer
    /// connector result contract used by ack-first webhook adapters.
    async fn normalize_inbound_result(
        &self,
        raw: RawInbound,
    ) -> Result<ConnectorNormalizeResult, ConnectorError> {
        self.normalize_inbound(raw)
            .await
            .map(ConnectorNormalizeResult::event)
    }

    /// Payload schema surfaced to future trigger-type narrowing.
    fn payload_schema(&self) -> ProviderPayloadSchema;

    /// Outbound API wrapper exposed to handlers.
    fn client(&self) -> Arc<dyn ConnectorClient>;
}

/// Provider-supplied HTTP response returned before or instead of trigger dispatch.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: JsonValue,
}

impl ConnectorHttpResponse {
    pub fn new(status: u16, headers: BTreeMap<String, String>, body: JsonValue) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }
}

/// Normalized inbound result accepted by the runtime connector adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectorNormalizeResult {
    Event(Box<TriggerEvent>),
    Batch(Vec<TriggerEvent>),
    ImmediateResponse {
        response: ConnectorHttpResponse,
        events: Vec<TriggerEvent>,
    },
    Reject(ConnectorHttpResponse),
}

impl ConnectorNormalizeResult {
    pub fn event(event: TriggerEvent) -> Self {
        Self::Event(Box::new(event))
    }

    pub fn into_events(self) -> Vec<TriggerEvent> {
        match self {
            Self::Event(event) => vec![*event],
            Self::Batch(events) | Self::ImmediateResponse { events, .. } => events,
            Self::Reject(_) => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PostNormalizeOutcome {
    Ready(Box<TriggerEvent>),
    DuplicateDropped,
}

pub async fn postprocess_normalized_event(
    inbox: &InboxIndex,
    binding_id: &str,
    dedupe_enabled: bool,
    dedupe_ttl: StdDuration,
    mut event: TriggerEvent,
) -> Result<PostNormalizeOutcome, ConnectorError> {
    if dedupe_enabled && !event.dedupe_claimed() {
        if !inbox
            .insert_if_new(binding_id, &event.dedupe_key, dedupe_ttl)
            .await?
        {
            return Ok(PostNormalizeOutcome::DuplicateDropped);
        }
        event.mark_dedupe_claimed();
    }

    Ok(PostNormalizeOutcome::Ready(Box::new(event)))
}

/// Outbound provider client interface used by connector-backed stdlib modules.
#[async_trait]
pub trait ConnectorClient: Send + Sync {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError>;
}

/// Minimal outbound client errors shared by connector implementations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    MethodNotFound(String),
    InvalidArgs(String),
    RateLimited(String),
    Transport(String),
    EgressBlocked(crate::egress::EgressBlocked),
    Other(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MethodNotFound(message)
            | Self::InvalidArgs(message)
            | Self::RateLimited(message)
            | Self::Transport(message)
            | Self::Other(message) => message.fmt(f),
            Self::EgressBlocked(blocked) => blocked.fmt(f),
        }
    }
}

impl std::error::Error for ClientError {}

/// Shared connector-layer errors.
#[derive(Debug)]
pub enum ConnectorError {
    DuplicateProvider(String),
    DuplicateDelivery(String),
    UnknownProvider(String),
    MissingHeader(String),
    InvalidHeader {
        name: String,
        detail: String,
    },
    InvalidSignature(String),
    TimestampOutOfWindow {
        timestamp: OffsetDateTime,
        now: OffsetDateTime,
        window: time::Duration,
    },
    Json(String),
    Secret(String),
    EventLog(String),
    HarnRuntime(String),
    Client(ClientError),
    Unsupported(String),
    Activation(String),
}

impl ConnectorError {
    pub fn invalid_signature(message: impl Into<String>) -> Self {
        Self::InvalidSignature(message.into())
    }
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateProvider(provider) => {
                write!(f, "connector provider `{provider}` is already registered")
            }
            Self::DuplicateDelivery(message) => message.fmt(f),
            Self::UnknownProvider(provider) => {
                write!(f, "connector provider `{provider}` is not registered")
            }
            Self::MissingHeader(header) => write!(f, "missing required header `{header}`"),
            Self::InvalidHeader { name, detail } => {
                write!(f, "invalid header `{name}`: {detail}")
            }
            Self::InvalidSignature(message)
            | Self::Json(message)
            | Self::Secret(message)
            | Self::EventLog(message)
            | Self::HarnRuntime(message)
            | Self::Unsupported(message)
            | Self::Activation(message) => message.fmt(f),
            Self::TimestampOutOfWindow {
                timestamp,
                now,
                window,
            } => write!(
                f,
                "timestamp {timestamp} is outside the allowed verification window of {window} around {now}"
            ),
            Self::Client(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConnectorError {}

impl From<crate::event_log::LogError> for ConnectorError {
    fn from(value: crate::event_log::LogError) -> Self {
        Self::EventLog(value.to_string())
    }
}

impl From<crate::secrets::SecretError> for ConnectorError {
    fn from(value: crate::secrets::SecretError) -> Self {
        Self::Secret(value.to_string())
    }
}

impl From<serde_json::Error> for ConnectorError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

impl From<ClientError> for ConnectorError {
    fn from(value: ClientError) -> Self {
        Self::Client(value)
    }
}

/// Startup context shared with connector instances.
#[derive(Clone)]
pub struct ConnectorCtx {
    pub event_log: Arc<AnyEventLog>,
    pub secrets: Arc<dyn SecretProvider>,
    pub inbox: Arc<InboxIndex>,
    pub metrics: Arc<MetricsRegistry>,
    pub rate_limiter: Arc<RateLimiterFactory>,
}

/// Snapshot of connector-local metrics surfaced for tests and diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectorMetricsSnapshot {
    pub inbox_claims_written: u64,
    pub inbox_duplicates_rejected: u64,
    pub inbox_fast_path_hits: u64,
    pub inbox_durable_hits: u64,
    pub inbox_expired_entries: u64,
    pub inbox_active_entries: u64,
    pub linear_timestamp_rejections_total: u64,
    pub dispatch_succeeded_total: u64,
    pub dispatch_failed_total: u64,
    pub retry_scheduled_total: u64,
    pub slack_delivery_success_total: u64,
    pub slack_delivery_failure_total: u64,
}

type MetricLabels = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq)]
struct HistogramMetric {
    buckets: BTreeMap<String, u64>,
    count: u64,
    sum: f64,
}

static ACTIVE_METRICS_REGISTRY: OnceLock<Mutex<Option<Arc<MetricsRegistry>>>> = OnceLock::new();

pub fn install_active_metrics_registry(metrics: Arc<MetricsRegistry>) {
    let slot = ACTIVE_METRICS_REGISTRY.get_or_init(|| Mutex::new(None));
    *slot.lock().expect("active metrics registry poisoned") = Some(metrics);
}

pub fn clear_active_metrics_registry() {
    if let Some(slot) = ACTIVE_METRICS_REGISTRY.get() {
        *slot.lock().expect("active metrics registry poisoned") = None;
    }
}

pub fn active_metrics_registry() -> Option<Arc<MetricsRegistry>> {
    ACTIVE_METRICS_REGISTRY.get().and_then(|slot| {
        slot.lock()
            .expect("active metrics registry poisoned")
            .clone()
    })
}

/// Shared metrics surface for connector-local counters and timings.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    inbox_claims_written: AtomicU64,
    inbox_duplicates_rejected: AtomicU64,
    inbox_fast_path_hits: AtomicU64,
    inbox_durable_hits: AtomicU64,
    inbox_expired_entries: AtomicU64,
    inbox_active_entries: AtomicU64,
    linear_timestamp_rejections_total: AtomicU64,
    dispatch_succeeded_total: AtomicU64,
    dispatch_failed_total: AtomicU64,
    retry_scheduled_total: AtomicU64,
    slack_delivery_success_total: AtomicU64,
    slack_delivery_failure_total: AtomicU64,
    custom_counters: Mutex<BTreeMap<String, u64>>,
    counters: Mutex<BTreeMap<(String, MetricLabels), f64>>,
    gauges: Mutex<BTreeMap<(String, MetricLabels), f64>>,
    histograms: Mutex<BTreeMap<(String, MetricLabels), HistogramMetric>>,
    pending_trigger_events: Mutex<BTreeMap<MetricLabels, BTreeMap<String, i64>>>,
}

impl MetricsRegistry {
    const DURATION_BUCKETS: [f64; 9] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0];
    const TRIGGER_LATENCY_BUCKETS: [f64; 15] = [
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
    ];
    const SIZE_BUCKETS: [f64; 9] = [
        128.0, 512.0, 1024.0, 4096.0, 16384.0, 65536.0, 262144.0, 1048576.0, 10485760.0,
    ];

    pub fn snapshot(&self) -> ConnectorMetricsSnapshot {
        ConnectorMetricsSnapshot {
            inbox_claims_written: self.inbox_claims_written.load(Ordering::Relaxed),
            inbox_duplicates_rejected: self.inbox_duplicates_rejected.load(Ordering::Relaxed),
            inbox_fast_path_hits: self.inbox_fast_path_hits.load(Ordering::Relaxed),
            inbox_durable_hits: self.inbox_durable_hits.load(Ordering::Relaxed),
            inbox_expired_entries: self.inbox_expired_entries.load(Ordering::Relaxed),
            inbox_active_entries: self.inbox_active_entries.load(Ordering::Relaxed),
            linear_timestamp_rejections_total: self
                .linear_timestamp_rejections_total
                .load(Ordering::Relaxed),
            dispatch_succeeded_total: self.dispatch_succeeded_total.load(Ordering::Relaxed),
            dispatch_failed_total: self.dispatch_failed_total.load(Ordering::Relaxed),
            retry_scheduled_total: self.retry_scheduled_total.load(Ordering::Relaxed),
            slack_delivery_success_total: self.slack_delivery_success_total.load(Ordering::Relaxed),
            slack_delivery_failure_total: self.slack_delivery_failure_total.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_inbox_claim(&self) {
        self.inbox_claims_written.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_inbox_duplicate_fast_path(&self) {
        self.inbox_duplicates_rejected
            .fetch_add(1, Ordering::Relaxed);
        self.inbox_fast_path_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_inbox_duplicate_durable(&self) {
        self.inbox_duplicates_rejected
            .fetch_add(1, Ordering::Relaxed);
        self.inbox_durable_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_inbox_expired_entries(&self, count: u64) {
        if count > 0 {
            self.inbox_expired_entries
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_inbox_active_entries(&self, count: usize) {
        self.inbox_active_entries
            .store(count as u64, Ordering::Relaxed);
    }

    pub fn record_linear_timestamp_rejection(&self) {
        self.linear_timestamp_rejections_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dispatch_succeeded(&self) {
        self.dispatch_succeeded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dispatch_failed(&self) {
        self.dispatch_failed_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_retry_scheduled(&self) {
        self.retry_scheduled_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_slack_delivery_success(&self) {
        self.slack_delivery_success_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_slack_delivery_failure(&self) {
        self.slack_delivery_failure_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_custom_counter(&self, name: &str, amount: u64) {
        if amount == 0 {
            return;
        }
        let mut counters = self
            .custom_counters
            .lock()
            .expect("custom counters poisoned");
        *counters.entry(name.to_string()).or_default() += amount;
    }

    pub fn record_http_request(
        &self,
        endpoint: &str,
        method: &str,
        status: u16,
        duration: StdDuration,
        body_size_bytes: usize,
    ) {
        self.increment_counter(
            "harn_http_requests_total",
            labels([
                ("endpoint", endpoint),
                ("method", method),
                ("status", &status.to_string()),
            ]),
            1,
        );
        self.observe_histogram(
            "harn_http_request_duration_seconds",
            labels([("endpoint", endpoint)]),
            duration.as_secs_f64(),
            &Self::DURATION_BUCKETS,
        );
        self.observe_histogram(
            "harn_http_body_size_bytes",
            labels([("endpoint", endpoint)]),
            body_size_bytes as f64,
            &Self::SIZE_BUCKETS,
        );
    }

    pub fn record_trigger_received(&self, trigger_id: &str, provider: &str) {
        self.increment_counter(
            "harn_trigger_received_total",
            labels([("trigger_id", trigger_id), ("provider", provider)]),
            1,
        );
    }

    pub fn record_trigger_deduped(&self, trigger_id: &str, reason: &str) {
        self.increment_counter(
            "harn_trigger_deduped_total",
            labels([("trigger_id", trigger_id), ("reason", reason)]),
            1,
        );
    }

    pub fn record_trigger_predicate_evaluation(
        &self,
        trigger_id: &str,
        result: bool,
        cost_usd: f64,
    ) {
        self.increment_counter(
            "harn_trigger_predicate_evaluations_total",
            labels([
                ("trigger_id", trigger_id),
                ("result", if result { "true" } else { "false" }),
            ]),
            1,
        );
        self.observe_histogram(
            "harn_trigger_predicate_cost_usd",
            labels([("trigger_id", trigger_id)]),
            cost_usd.max(0.0),
            &[0.0, 0.001, 0.01, 0.05, 0.1, 1.0],
        );
    }

    pub fn record_trigger_dispatched(&self, trigger_id: &str, handler_kind: &str, outcome: &str) {
        self.increment_counter(
            "harn_trigger_dispatched_total",
            labels([
                ("trigger_id", trigger_id),
                ("handler_kind", handler_kind),
                ("outcome", outcome),
            ]),
            1,
        );
    }

    pub fn record_trigger_retry(&self, trigger_id: &str, attempt: u32) {
        self.increment_counter(
            "harn_trigger_retries_total",
            labels([
                ("trigger_id", trigger_id),
                ("attempt", &attempt.to_string()),
            ]),
            1,
        );
    }

    pub fn record_trigger_dlq(&self, trigger_id: &str, reason: &str) {
        self.increment_counter(
            "harn_trigger_dlq_total",
            labels([("trigger_id", trigger_id), ("reason", reason)]),
            1,
        );
    }

    pub fn record_trigger_accepted_to_normalized(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        duration: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_webhook_accepted_to_normalized_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            duration.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_accepted_to_queue_append(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        duration: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_webhook_accepted_to_queue_append_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            duration.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_queue_age_at_dispatch_admission(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        age: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_queue_age_at_dispatch_admission_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            age.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_queue_age_at_dispatch_start(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        age: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_queue_age_at_dispatch_start_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            age.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_dispatch_runtime(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        duration: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_dispatch_runtime_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            duration.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_retry_delay(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        duration: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_retry_delay_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            duration.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn record_trigger_accepted_to_dlq(
        &self,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        status: &str,
        duration: StdDuration,
    ) {
        self.observe_histogram(
            "harn_trigger_accepted_to_dlq_seconds",
            trigger_lifecycle_labels(trigger_id, binding_key, provider, tenant_id, status),
            duration.as_secs_f64(),
            &Self::TRIGGER_LATENCY_BUCKETS,
        );
    }

    pub fn note_trigger_pending_event(
        &self,
        event_id: &str,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        accepted_at_ms: i64,
        now_ms: i64,
    ) {
        let labels = trigger_pending_labels(trigger_id, binding_key, provider, tenant_id);
        {
            let mut pending = self
                .pending_trigger_events
                .lock()
                .expect("pending trigger events poisoned");
            pending
                .entry(labels.clone())
                .or_default()
                .insert(event_id.to_string(), accepted_at_ms);
        }
        self.refresh_oldest_pending_gauge(labels, now_ms);
    }

    pub fn clear_trigger_pending_event(
        &self,
        event_id: &str,
        trigger_id: &str,
        binding_key: &str,
        provider: &str,
        tenant_id: Option<&str>,
        now_ms: i64,
    ) {
        let labels = trigger_pending_labels(trigger_id, binding_key, provider, tenant_id);
        {
            let mut pending = self
                .pending_trigger_events
                .lock()
                .expect("pending trigger events poisoned");
            if let Some(events) = pending.get_mut(&labels) {
                events.remove(event_id);
                if events.is_empty() {
                    pending.remove(&labels);
                }
            }
        }
        self.refresh_oldest_pending_gauge(labels, now_ms);
    }

    pub fn set_trigger_inflight(&self, trigger_id: &str, count: u64) {
        self.set_gauge(
            "harn_trigger_inflight",
            labels([("trigger_id", trigger_id)]),
            count as f64,
        );
    }

    pub fn set_trigger_budget_cost_today(&self, trigger_id: &str, cost_usd: f64) {
        self.set_gauge(
            "harn_trigger_budget_cost_today_usd",
            labels([("trigger_id", trigger_id)]),
            cost_usd.max(0.0),
        );
    }

    pub fn record_trigger_budget_exhausted(&self, trigger_id: &str, strategy: &str) {
        self.increment_counter(
            "harn_trigger_budget_exhausted_total",
            labels([("trigger_id", trigger_id), ("strategy", strategy)]),
            1,
        );
    }

    pub fn record_backpressure_event(&self, dimension: &str, action: &str) {
        self.increment_counter(
            "harn_backpressure_events_total",
            labels([("dimension", dimension), ("action", action)]),
            1,
        );
    }

    pub fn record_event_log_append(
        &self,
        topic: &str,
        duration: StdDuration,
        payload_bytes: usize,
    ) {
        self.observe_histogram(
            "harn_event_log_append_duration_seconds",
            labels([("topic", topic)]),
            duration.as_secs_f64(),
            &Self::DURATION_BUCKETS,
        );
        self.set_gauge(
            "harn_event_log_topic_size_bytes",
            labels([("topic", topic)]),
            payload_bytes as f64,
        );
    }

    pub fn set_event_log_consumer_lag(&self, topic: &str, consumer: &str, lag: u64) {
        self.set_gauge(
            "harn_event_log_consumer_lag",
            labels([("topic", topic), ("consumer", consumer)]),
            lag as f64,
        );
    }

    pub fn record_a2a_hop(&self, target: &str, outcome: &str, duration: StdDuration) {
        self.increment_counter(
            "harn_a2a_hops_total",
            labels([("target", target), ("outcome", outcome)]),
            1,
        );
        self.observe_histogram(
            "harn_a2a_hop_duration_seconds",
            labels([("target", target)]),
            duration.as_secs_f64(),
            &Self::DURATION_BUCKETS,
        );
    }

    pub fn set_worker_queue_depth(&self, queue: &str, depth: u64) {
        self.set_gauge(
            "harn_worker_queue_depth",
            labels([("queue", queue)]),
            depth as f64,
        );
    }

    pub fn record_worker_queue_claim_age(&self, queue: &str, age_seconds: f64) {
        self.observe_histogram(
            "harn_worker_queue_claim_age_seconds",
            labels([("queue", queue)]),
            age_seconds.max(0.0),
            &Self::DURATION_BUCKETS,
        );
    }

    /// Increment the scheduler-selection counter for a particular fairness key.
    pub fn record_scheduler_selection(
        &self,
        queue: &str,
        fairness_dimension: &str,
        fairness_key: &str,
    ) {
        self.increment_counter(
            "harn_scheduler_selections_total",
            labels([
                ("queue", queue),
                ("fairness_dimension", fairness_dimension),
                ("fairness_key", fairness_key),
            ]),
            1,
        );
    }

    /// Increment the scheduler-deferred counter (queue had work but couldn't
    /// be selected because the key was at its concurrency cap).
    pub fn record_scheduler_deferral(
        &self,
        queue: &str,
        fairness_dimension: &str,
        fairness_key: &str,
    ) {
        self.increment_counter(
            "harn_scheduler_deferrals_total",
            labels([
                ("queue", queue),
                ("fairness_dimension", fairness_dimension),
                ("fairness_key", fairness_key),
            ]),
            1,
        );
    }

    /// Increment the scheduler starvation-promotion counter.
    pub fn record_scheduler_starvation_promotion(
        &self,
        queue: &str,
        fairness_dimension: &str,
        fairness_key: &str,
    ) {
        self.increment_counter(
            "harn_scheduler_starvation_promotions_total",
            labels([
                ("queue", queue),
                ("fairness_dimension", fairness_dimension),
                ("fairness_key", fairness_key),
            ]),
            1,
        );
    }

    /// Set the current scheduler deficit gauge for a fairness key.
    pub fn set_scheduler_deficit(
        &self,
        queue: &str,
        fairness_dimension: &str,
        fairness_key: &str,
        deficit: i64,
    ) {
        self.set_gauge(
            "harn_scheduler_deficit",
            labels([
                ("queue", queue),
                ("fairness_dimension", fairness_dimension),
                ("fairness_key", fairness_key),
            ]),
            deficit as f64,
        );
    }

    /// Set the oldest-eligible-job-age gauge for a fairness key (seconds).
    pub fn set_scheduler_oldest_eligible_age(
        &self,
        queue: &str,
        fairness_dimension: &str,
        fairness_key: &str,
        age_ms: u64,
    ) {
        self.set_gauge(
            "harn_scheduler_oldest_eligible_age_seconds",
            labels([
                ("queue", queue),
                ("fairness_dimension", fairness_dimension),
                ("fairness_key", fairness_key),
            ]),
            age_ms as f64 / 1000.0,
        );
    }

    pub fn set_orchestrator_pump_backlog(&self, topic: &str, count: u64) {
        self.set_gauge(
            "harn_orchestrator_pump_backlog",
            labels([("topic", topic)]),
            count as f64,
        );
    }

    pub fn set_orchestrator_pump_outstanding(&self, topic: &str, count: usize) {
        self.set_gauge(
            "harn_orchestrator_pump_outstanding",
            labels([("topic", topic)]),
            count as f64,
        );
    }

    pub fn record_orchestrator_pump_admission_delay(&self, topic: &str, duration: StdDuration) {
        self.observe_histogram(
            "harn_orchestrator_pump_admission_delay_seconds",
            labels([("topic", topic)]),
            duration.as_secs_f64(),
            &Self::DURATION_BUCKETS,
        );
    }

    pub fn record_llm_call(&self, provider: &str, model: &str, outcome: &str, cost_usd: f64) {
        self.increment_counter(
            "harn_llm_calls_total",
            labels([
                ("provider", provider),
                ("model", model),
                ("outcome", outcome),
            ]),
            1,
        );
        if cost_usd > 0.0 {
            self.increment_counter(
                "harn_llm_cost_usd_total",
                labels([("provider", provider), ("model", model)]),
                cost_usd,
            );
        } else {
            self.ensure_counter(
                "harn_llm_cost_usd_total",
                labels([("provider", provider), ("model", model)]),
            );
        }
    }

    pub fn record_llm_cache_hit(&self, provider: &str) {
        self.increment_counter(
            "harn_llm_cache_hits_total",
            labels([("provider", provider)]),
            1,
        );
    }

    pub fn render_prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let counters = [
            (
                "connector_linear_timestamp_rejections_total",
                snapshot.linear_timestamp_rejections_total,
            ),
            (
                "dispatch_succeeded_total",
                snapshot.dispatch_succeeded_total,
            ),
            ("dispatch_failed_total", snapshot.dispatch_failed_total),
            ("inbox_duplicates_total", snapshot.inbox_duplicates_rejected),
            ("retry_scheduled_total", snapshot.retry_scheduled_total),
            (
                "slack_events_delivery_success_total",
                snapshot.slack_delivery_success_total,
            ),
            (
                "slack_events_delivery_failure_total",
                snapshot.slack_delivery_failure_total,
            ),
        ];

        let mut rendered = String::new();
        for (name, value) in counters {
            rendered.push_str("# TYPE ");
            rendered.push_str(name);
            rendered.push_str(" counter\n");
            rendered.push_str(name);
            rendered.push(' ');
            rendered.push_str(&value.to_string());
            rendered.push('\n');
        }
        let custom_counters = self
            .custom_counters
            .lock()
            .expect("custom counters poisoned");
        for (name, value) in custom_counters.iter() {
            let metric_name = format!(
                "connector_custom_{}_total",
                name.chars()
                    .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' {
                        ch
                    } else {
                        '_'
                    })
                    .collect::<String>()
            );
            rendered.push_str("# TYPE ");
            rendered.push_str(&metric_name);
            rendered.push_str(" counter\n");
            rendered.push_str(&metric_name);
            rendered.push(' ');
            rendered.push_str(&value.to_string());
            rendered.push('\n');
        }
        rendered.push_str("# TYPE slack_events_auto_disable_min_success_ratio gauge\n");
        rendered.push_str("slack_events_auto_disable_min_success_ratio 0.05\n");
        rendered.push_str("# TYPE slack_events_auto_disable_min_events_per_hour gauge\n");
        rendered.push_str("slack_events_auto_disable_min_events_per_hour 1000\n");
        self.render_generic_metrics(&mut rendered);
        rendered
    }

    fn increment_counter(&self, name: &str, labels: MetricLabels, amount: impl Into<f64>) {
        let amount = amount.into();
        if amount <= 0.0 || !amount.is_finite() {
            return;
        }
        let mut counters = self.counters.lock().expect("metrics counters poisoned");
        *counters.entry((name.to_string(), labels)).or_default() += amount;
    }

    fn ensure_counter(&self, name: &str, labels: MetricLabels) {
        let mut counters = self.counters.lock().expect("metrics counters poisoned");
        counters.entry((name.to_string(), labels)).or_default();
    }

    fn set_gauge(&self, name: &str, labels: MetricLabels, value: f64) {
        let mut gauges = self.gauges.lock().expect("metrics gauges poisoned");
        gauges.insert((name.to_string(), labels), value);
    }

    fn observe_histogram(
        &self,
        name: &str,
        labels: MetricLabels,
        value: f64,
        bucket_bounds: &[f64],
    ) {
        if !value.is_finite() {
            return;
        }
        let mut histograms = self.histograms.lock().expect("metrics histograms poisoned");
        let histogram = histograms
            .entry((name.to_string(), labels))
            .or_insert_with(|| HistogramMetric {
                buckets: bucket_bounds
                    .iter()
                    .map(|bound| (prometheus_float(*bound), 0))
                    .chain(std::iter::once(("+Inf".to_string(), 0)))
                    .collect(),
                count: 0,
                sum: 0.0,
            });
        histogram.count += 1;
        histogram.sum += value;
        for bound in bucket_bounds {
            if value <= *bound {
                let key = prometheus_float(*bound);
                *histogram.buckets.entry(key).or_default() += 1;
            }
        }
        *histogram.buckets.entry("+Inf".to_string()).or_default() += 1;
    }

    fn refresh_oldest_pending_gauge(&self, labels: MetricLabels, now_ms: i64) {
        let oldest_accepted_at_ms = self
            .pending_trigger_events
            .lock()
            .expect("pending trigger events poisoned")
            .get(&labels)
            .and_then(|events| events.values().min().copied());
        let age_seconds = oldest_accepted_at_ms
            .map(|accepted_at_ms| millis_delta(now_ms, accepted_at_ms).as_secs_f64())
            .unwrap_or(0.0);
        self.set_gauge(
            "harn_trigger_oldest_pending_age_seconds",
            labels,
            age_seconds,
        );
    }

    fn render_generic_metrics(&self, rendered: &mut String) {
        let counters = self
            .counters
            .lock()
            .expect("metrics counters poisoned")
            .clone();
        let gauges = self.gauges.lock().expect("metrics gauges poisoned").clone();
        let histograms = self
            .histograms
            .lock()
            .expect("metrics histograms poisoned")
            .clone();

        for name in metric_family_names(MetricKind::Counter) {
            rendered.push_str("# TYPE ");
            rendered.push_str(name);
            rendered.push_str(" counter\n");
            for ((sample_name, labels), value) in counters.iter().filter(|((n, _), _)| n == name) {
                render_sample(rendered, sample_name, labels, *value);
            }
        }
        for name in metric_family_names(MetricKind::Gauge) {
            rendered.push_str("# TYPE ");
            rendered.push_str(name);
            rendered.push_str(" gauge\n");
            for ((sample_name, labels), value) in gauges.iter().filter(|((n, _), _)| n == name) {
                render_sample(rendered, sample_name, labels, *value);
            }
        }
        for name in metric_family_names(MetricKind::Histogram) {
            rendered.push_str("# TYPE ");
            rendered.push_str(name);
            rendered.push_str(" histogram\n");
            for ((sample_name, labels), histogram) in
                histograms.iter().filter(|((n, _), _)| n == name)
            {
                for (le, value) in &histogram.buckets {
                    let mut bucket_labels = labels.clone();
                    bucket_labels.insert("le".to_string(), le.clone());
                    render_sample(
                        rendered,
                        &format!("{sample_name}_bucket"),
                        &bucket_labels,
                        *value as f64,
                    );
                }
                render_sample(
                    rendered,
                    &format!("{sample_name}_sum"),
                    labels,
                    histogram.sum,
                );
                render_sample(
                    rendered,
                    &format!("{sample_name}_count"),
                    labels,
                    histogram.count as f64,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

fn metric_family_names(kind: MetricKind) -> &'static [&'static str] {
    match kind {
        MetricKind::Counter => &[
            "harn_http_requests_total",
            "harn_trigger_received_total",
            "harn_trigger_deduped_total",
            "harn_trigger_predicate_evaluations_total",
            "harn_trigger_dispatched_total",
            "harn_trigger_retries_total",
            "harn_trigger_dlq_total",
            "harn_trigger_budget_exhausted_total",
            "harn_backpressure_events_total",
            "harn_a2a_hops_total",
            "harn_llm_calls_total",
            "harn_llm_cost_usd_total",
            "harn_llm_cache_hits_total",
            "harn_scheduler_selections_total",
            "harn_scheduler_deferrals_total",
            "harn_scheduler_starvation_promotions_total",
        ],
        MetricKind::Gauge => &[
            "harn_trigger_inflight",
            "harn_event_log_topic_size_bytes",
            "harn_event_log_consumer_lag",
            "harn_trigger_budget_cost_today_usd",
            "harn_worker_queue_depth",
            "harn_orchestrator_pump_backlog",
            "harn_orchestrator_pump_outstanding",
            "harn_trigger_oldest_pending_age_seconds",
            "harn_scheduler_deficit",
            "harn_scheduler_oldest_eligible_age_seconds",
        ],
        MetricKind::Histogram => &[
            "harn_http_request_duration_seconds",
            "harn_http_body_size_bytes",
            "harn_trigger_predicate_cost_usd",
            "harn_event_log_append_duration_seconds",
            "harn_a2a_hop_duration_seconds",
            "harn_worker_queue_claim_age_seconds",
            "harn_orchestrator_pump_admission_delay_seconds",
            "harn_trigger_webhook_accepted_to_normalized_seconds",
            "harn_trigger_webhook_accepted_to_queue_append_seconds",
            "harn_trigger_queue_age_at_dispatch_admission_seconds",
            "harn_trigger_queue_age_at_dispatch_start_seconds",
            "harn_trigger_dispatch_runtime_seconds",
            "harn_trigger_retry_delay_seconds",
            "harn_trigger_accepted_to_dlq_seconds",
        ],
    }
}

fn labels<const N: usize>(pairs: [(&str, &str); N]) -> MetricLabels {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn trigger_lifecycle_labels(
    trigger_id: &str,
    binding_key: &str,
    provider: &str,
    tenant_id: Option<&str>,
    status: &str,
) -> MetricLabels {
    labels([
        ("binding_key", binding_key),
        ("provider", provider),
        ("status", status),
        ("tenant_id", tenant_label(tenant_id)),
        ("trigger_id", trigger_id),
    ])
}

fn trigger_pending_labels(
    trigger_id: &str,
    binding_key: &str,
    provider: &str,
    tenant_id: Option<&str>,
) -> MetricLabels {
    labels([
        ("binding_key", binding_key),
        ("provider", provider),
        ("tenant_id", tenant_label(tenant_id)),
        ("trigger_id", trigger_id),
    ])
}

fn tenant_label(tenant_id: Option<&str>) -> &str {
    tenant_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("none")
}

fn millis_delta(later_ms: i64, earlier_ms: i64) -> StdDuration {
    StdDuration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
}

fn render_sample(rendered: &mut String, name: &str, labels: &MetricLabels, value: f64) {
    rendered.push_str(name);
    if !labels.is_empty() {
        rendered.push('{');
        for (index, (label, label_value)) in labels.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(label);
            rendered.push_str("=\"");
            rendered.push_str(&escape_label_value(label_value));
            rendered.push('"');
        }
        rendered.push('}');
    }
    rendered.push(' ');
    rendered.push_str(&prometheus_float(value));
    rendered.push('\n');
}

fn escape_label_value(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

fn prometheus_float(value: f64) -> String {
    if value.is_infinite() && value.is_sign_positive() {
        return "+Inf".to_string();
    }
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        let rendered = format!("{value:.6}");
        rendered
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Provider payload schema metadata exposed by a connector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderPayloadSchema {
    pub harn_schema_name: String,
    #[serde(default)]
    pub json_schema: JsonValue,
}

impl ProviderPayloadSchema {
    pub fn new(harn_schema_name: impl Into<String>, json_schema: JsonValue) -> Self {
        Self {
            harn_schema_name: harn_schema_name.into(),
            json_schema,
        }
    }

    pub fn named(harn_schema_name: impl Into<String>) -> Self {
        Self::new(harn_schema_name, JsonValue::Null)
    }
}

impl Default for ProviderPayloadSchema {
    fn default() -> Self {
        Self::named("raw")
    }
}

/// High-level transport kind a connector supports.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TriggerKind(String);

impl TriggerKind {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for TriggerKind {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for TriggerKind {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Future trigger manifest binding routed to a connector activation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerBinding {
    pub provider: ProviderId,
    pub kind: TriggerKind,
    pub binding_id: String,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default = "default_dedupe_retention_days")]
    pub dedupe_retention_days: u32,
    #[serde(default)]
    pub config: JsonValue,
}

impl TriggerBinding {
    pub fn new(
        provider: ProviderId,
        kind: impl Into<TriggerKind>,
        binding_id: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            kind: kind.into(),
            binding_id: binding_id.into(),
            dedupe_key: None,
            dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
            config: JsonValue::Null,
        }
    }
}

fn default_dedupe_retention_days() -> u32 {
    crate::triggers::DEFAULT_INBOX_RETENTION_DAYS
}

/// Small in-memory trigger-binding registry used to fan bindings into connectors.
#[derive(Clone, Debug, Default)]
pub struct TriggerRegistry {
    bindings: BTreeMap<ProviderId, Vec<TriggerBinding>>,
}

impl TriggerRegistry {
    pub fn register(&mut self, binding: TriggerBinding) {
        self.bindings
            .entry(binding.provider.clone())
            .or_default()
            .push(binding);
    }

    pub fn bindings(&self) -> &BTreeMap<ProviderId, Vec<TriggerBinding>> {
        &self.bindings
    }

    pub fn bindings_for(&self, provider: &ProviderId) -> &[TriggerBinding] {
        self.bindings
            .get(provider)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Metadata returned from connector activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationHandle {
    pub provider: ProviderId,
    pub binding_count: usize,
}

impl ActivationHandle {
    pub fn new(provider: ProviderId, binding_count: usize) -> Self {
        Self {
            provider,
            binding_count,
        }
    }
}

/// Provider-native inbound request payload preserved as raw bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInbound {
    pub kind: String,
    pub headers: BTreeMap<String, String>,
    pub query: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub received_at: OffsetDateTime,
    pub occurred_at: Option<OffsetDateTime>,
    pub tenant_id: Option<TenantId>,
    pub metadata: JsonValue,
}

impl RawInbound {
    pub fn new(kind: impl Into<String>, headers: BTreeMap<String, String>, body: Vec<u8>) -> Self {
        Self {
            kind: kind.into(),
            headers,
            query: BTreeMap::new(),
            body,
            received_at: clock::now_utc(),
            occurred_at: None,
            tenant_id: None,
            metadata: JsonValue::Null,
        }
    }

    pub fn json_body(&self) -> Result<JsonValue, ConnectorError> {
        Ok(serde_json::from_slice(&self.body)?)
    }
}

/// Token-bucket configuration shared across connector clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub capacity: u32,
    pub refill_tokens: u32,
    pub refill_interval: StdDuration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            capacity: 60,
            refill_tokens: 1,
            refill_interval: StdDuration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: ClockInstant,
}

impl TokenBucket {
    fn full(config: RateLimitConfig) -> Self {
        Self {
            tokens: config.capacity as f64,
            last_refill: clock::instant_now(),
        }
    }

    fn refill(&mut self, config: RateLimitConfig, now: ClockInstant) {
        let interval = config.refill_interval.as_secs_f64().max(f64::EPSILON);
        let rate = config.refill_tokens.max(1) as f64 / interval;
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(config.capacity.max(1) as f64);
        self.last_refill = now;
    }

    fn try_acquire(&mut self, config: RateLimitConfig, now: ClockInstant) -> bool {
        self.refill(config, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn wait_duration(&self, config: RateLimitConfig) -> StdDuration {
        if self.tokens >= 1.0 {
            return StdDuration::ZERO;
        }
        let interval = config.refill_interval.as_secs_f64().max(f64::EPSILON);
        let rate = config.refill_tokens.max(1) as f64 / interval;
        let missing = (1.0 - self.tokens).max(0.0);
        StdDuration::from_secs_f64((missing / rate).max(0.001))
    }
}

/// Shared per-provider, per-key token bucket factory for outbound connector clients.
#[derive(Debug)]
pub struct RateLimiterFactory {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<(String, String), TokenBucket>>,
}

impl RateLimiterFactory {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    pub fn scoped(&self, provider: &ProviderId, key: impl Into<String>) -> ScopedRateLimiter<'_> {
        ScopedRateLimiter {
            factory: self,
            provider: provider.clone(),
            key: key.into(),
        }
    }

    pub fn try_acquire(&self, provider: &ProviderId, key: &str) -> bool {
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = buckets
            .entry((provider.as_str().to_string(), key.to_string()))
            .or_insert_with(|| TokenBucket::full(self.config));
        bucket.try_acquire(self.config, clock::instant_now())
    }

    pub async fn acquire(&self, provider: &ProviderId, key: &str) {
        loop {
            let wait = {
                let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
                let bucket = buckets
                    .entry((provider.as_str().to_string(), key.to_string()))
                    .or_insert_with(|| TokenBucket::full(self.config));
                if bucket.try_acquire(self.config, clock::instant_now()) {
                    return;
                }
                bucket.wait_duration(self.config)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

impl Default for RateLimiterFactory {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

/// Borrowed view onto a single provider/key rate-limit scope.
#[derive(Clone, Debug)]
pub struct ScopedRateLimiter<'a> {
    factory: &'a RateLimiterFactory,
    provider: ProviderId,
    key: String,
}

impl<'a> ScopedRateLimiter<'a> {
    pub fn try_acquire(&self) -> bool {
        self.factory.try_acquire(&self.provider, &self.key)
    }

    pub async fn acquire(&self) {
        self.factory.acquire(&self.provider, &self.key).await;
    }
}

/// Runtime connector registry keyed by provider id.
pub struct ConnectorRegistry {
    connectors: BTreeMap<ProviderId, ConnectorHandle>,
}

impl ConnectorRegistry {
    pub fn empty() -> Self {
        Self {
            connectors: BTreeMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::empty();
        for provider in registered_provider_metadata() {
            registry
                .register(default_connector_for_provider(&provider))
                .expect("default connector registration should not fail");
        }
        registry
    }

    pub fn register(&mut self, connector: Box<dyn Connector>) -> Result<(), ConnectorError> {
        let provider = connector.provider_id().clone();
        if self.connectors.contains_key(&provider) {
            return Err(ConnectorError::DuplicateProvider(provider.0));
        }
        self.connectors
            .insert(provider, Arc::new(AsyncMutex::new(connector)));
        Ok(())
    }

    pub fn get(&self, id: &ProviderId) -> Option<ConnectorHandle> {
        self.connectors.get(id).cloned()
    }

    pub fn remove(&mut self, id: &ProviderId) -> Option<ConnectorHandle> {
        self.connectors.remove(id)
    }

    pub fn list(&self) -> Vec<ProviderId> {
        self.connectors.keys().cloned().collect()
    }

    pub async fn init_all(&self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        for connector in self.connectors.values() {
            connector.lock().await.init(ctx.clone()).await?;
        }
        Ok(())
    }

    pub async fn client_map(&self) -> BTreeMap<ProviderId, Arc<dyn ConnectorClient>> {
        let mut clients = BTreeMap::new();
        for (provider, connector) in &self.connectors {
            let client = connector.lock().await.client();
            clients.insert(provider.clone(), client);
        }
        clients
    }

    pub async fn activate_all(
        &self,
        registry: &TriggerRegistry,
    ) -> Result<Vec<ActivationHandle>, ConnectorError> {
        let mut handles = Vec::new();
        for (provider, connector) in &self.connectors {
            let bindings = registry.bindings_for(provider);
            if bindings.is_empty() {
                continue;
            }
            let connector = connector.lock().await;
            handles.push(connector.activate(bindings).await?);
        }
        Ok(handles)
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

fn default_connector_for_provider(provider: &ProviderMetadata) -> Box<dyn Connector> {
    // The provider catalog on main registers `github` with
    // ProviderRuntimeMetadata::Builtin { connector: "webhook", ... } so that
    // before a native connector existed the catalog auto-wired a
    // GenericWebhookConnector. Now that #170 lands a first-class
    // GitHubConnector (inbound HMAC + GitHub App outbound), we short-circuit
    // provider_id "github" here and return the native connector instead of a
    // webhook-backed fallback. This keeps manifests that say
    // `provider = "github"` pointed at the new connector without requiring
    // users to switch to a distinct provider_id.
    if provider.provider == "github" {
        return Box::new(GitHubConnector::new());
    }
    if provider.provider == "linear" {
        return Box::new(LinearConnector::new());
    }
    if provider.provider == "slack" {
        return Box::new(SlackConnector::new());
    }
    if provider.provider == "notion" {
        return Box::new(NotionConnector::new());
    }
    if provider.provider == "a2a-push" {
        return Box::new(A2aPushConnector::new());
    }
    match &provider.runtime {
        ProviderRuntimeMetadata::Builtin {
            connector,
            default_signature_variant,
        } => match connector.as_str() {
            "cron" => Box::new(CronConnector::new()),
            "stream" => Box::new(StreamConnector::new(
                ProviderId::from(provider.provider.clone()),
                provider.schema_name.clone(),
            )),
            "webhook" => {
                let variant = WebhookSignatureVariant::parse(default_signature_variant.as_deref())
                    .expect("catalog webhook signature variant must be valid");
                Box::new(GenericWebhookConnector::with_profile(
                    WebhookProviderProfile::new(
                        ProviderId::from(provider.provider.clone()),
                        provider.schema_name.clone(),
                        variant,
                    ),
                ))
            }
            _ => Box::new(PlaceholderConnector::from_metadata(provider)),
        },
        ProviderRuntimeMetadata::Placeholder => {
            Box::new(PlaceholderConnector::from_metadata(provider))
        }
    }
}

struct PlaceholderConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    schema_name: String,
}

impl PlaceholderConnector {
    fn from_metadata(metadata: &ProviderMetadata) -> Self {
        Self {
            provider_id: ProviderId::from(metadata.provider.clone()),
            kinds: metadata
                .kinds
                .iter()
                .cloned()
                .map(TriggerKind::from)
                .collect(),
            schema_name: metadata.schema_name.clone(),
        }
    }
}

struct PlaceholderClient;

#[async_trait]
impl ConnectorClient for PlaceholderClient {
    async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
        Err(ClientError::Other(format!(
            "connector client method '{method}' is not implemented for this provider"
        )))
    }
}

#[async_trait]
impl Connector for PlaceholderConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, _ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(&self, _raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        Err(ConnectorError::Unsupported(format!(
            "provider '{}' is cataloged but does not have a concrete inbound connector yet",
            self.provider_id.as_str()
        )))
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named(self.schema_name.clone())
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        Arc::new(PlaceholderClient)
    }
}

pub fn install_active_connector_clients(clients: BTreeMap<ProviderId, Arc<dyn ConnectorClient>>) {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| {
        *slot.borrow_mut() = clients
            .into_iter()
            .map(|(provider, client)| (provider.as_str().to_string(), client))
            .collect();
    });
}

pub fn active_connector_client(provider: &str) -> Option<Arc<dyn ConnectorClient>> {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| slot.borrow().get(provider).cloned())
}

pub fn clear_active_connector_clients() {
    ACTIVE_CONNECTOR_CLIENTS.with(|slot| slot.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    struct NoopClient;

    #[async_trait]
    impl ConnectorClient for NoopClient {
        async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
            Ok(json!({ "method": method }))
        }
    }

    struct FakeConnector {
        provider_id: ProviderId,
        kinds: Vec<TriggerKind>,
        activate_calls: Arc<AtomicUsize>,
    }

    impl FakeConnector {
        fn new(provider_id: &str, activate_calls: Arc<AtomicUsize>) -> Self {
            Self {
                provider_id: ProviderId::from(provider_id),
                kinds: vec![TriggerKind::from("webhook")],
                activate_calls,
            }
        }
    }

    #[async_trait]
    impl Connector for FakeConnector {
        fn provider_id(&self) -> &ProviderId {
            &self.provider_id
        }

        fn kinds(&self) -> &[TriggerKind] {
            &self.kinds
        }

        async fn init(&mut self, _ctx: ConnectorCtx) -> Result<(), ConnectorError> {
            Ok(())
        }

        async fn activate(
            &self,
            bindings: &[TriggerBinding],
        ) -> Result<ActivationHandle, ConnectorError> {
            self.activate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ActivationHandle::new(
                self.provider_id.clone(),
                bindings.len(),
            ))
        }

        async fn normalize_inbound(
            &self,
            _raw: RawInbound,
        ) -> Result<TriggerEvent, ConnectorError> {
            Err(ConnectorError::Unsupported(
                "not needed for registry tests".to_string(),
            ))
        }

        fn payload_schema(&self) -> ProviderPayloadSchema {
            ProviderPayloadSchema::named("FakePayload")
        }

        fn client(&self) -> Arc<dyn ConnectorClient> {
            Arc::new(NoopClient)
        }
    }

    #[tokio::test]
    async fn connector_registry_rejects_duplicate_providers() {
        let activate_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ConnectorRegistry::empty();
        registry
            .register(Box::new(FakeConnector::new(
                "github",
                activate_calls.clone(),
            )))
            .unwrap();

        let error = registry
            .register(Box::new(FakeConnector::new("github", activate_calls)))
            .unwrap_err();
        assert!(matches!(
            error,
            ConnectorError::DuplicateProvider(provider) if provider == "github"
        ));
    }

    #[tokio::test]
    async fn connector_registry_activates_only_bound_connectors() {
        let github_calls = Arc::new(AtomicUsize::new(0));
        let slack_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ConnectorRegistry::empty();
        registry
            .register(Box::new(FakeConnector::new("github", github_calls.clone())))
            .unwrap();
        registry
            .register(Box::new(FakeConnector::new("slack", slack_calls.clone())))
            .unwrap();

        let mut trigger_registry = TriggerRegistry::default();
        trigger_registry.register(TriggerBinding::new(
            ProviderId::from("github"),
            "webhook",
            "github.push",
        ));
        trigger_registry.register(TriggerBinding::new(
            ProviderId::from("github"),
            "webhook",
            "github.installation",
        ));

        let handles = registry.activate_all(&trigger_registry).await.unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].provider.as_str(), "github");
        assert_eq!(handles[0].binding_count, 2);
        assert_eq!(github_calls.load(Ordering::SeqCst), 1);
        assert_eq!(slack_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rate_limiter_scopes_tokens_by_provider_and_key() {
        let factory = RateLimiterFactory::new(RateLimitConfig {
            capacity: 1,
            refill_tokens: 1,
            refill_interval: StdDuration::from_secs(60),
        });

        assert!(factory.try_acquire(&ProviderId::from("github"), "org:1"));
        assert!(!factory.try_acquire(&ProviderId::from("github"), "org:1"));
        assert!(factory.try_acquire(&ProviderId::from("github"), "org:2"));
        assert!(factory.try_acquire(&ProviderId::from("slack"), "org:1"));
    }

    #[test]
    fn raw_inbound_json_body_preserves_raw_bytes() {
        let raw = RawInbound::new(
            "push",
            BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            br#"{"ok":true}"#.to_vec(),
        );

        assert_eq!(raw.json_body().unwrap(), json!({ "ok": true }));
    }

    #[test]
    fn connector_registry_lists_catalog_providers() {
        let registry = ConnectorRegistry::default();
        let providers = registry.list();
        assert!(providers.contains(&ProviderId::from("cron")));
        assert!(providers.contains(&ProviderId::from("github")));
        assert!(providers.contains(&ProviderId::from("webhook")));
    }

    #[test]
    fn metrics_registry_exports_orchestrator_metric_families() {
        let metrics = MetricsRegistry::default();
        metrics.record_http_request(
            "/triggers/github",
            "POST",
            200,
            StdDuration::from_millis(25),
            512,
        );
        metrics.record_trigger_received("github-new-issue", "github");
        metrics.record_trigger_deduped("github-new-issue", "inbox_duplicate");
        metrics.record_trigger_predicate_evaluation("github-new-issue", true, 0.002);
        metrics.record_trigger_dispatched("github-new-issue", "local", "succeeded");
        metrics.record_trigger_retry("github-new-issue", 2);
        metrics.record_trigger_dlq("github-new-issue", "retry_exhausted");
        metrics.set_trigger_inflight("github-new-issue", 0);
        metrics.record_event_log_append(
            "orchestrator.triggers.pending",
            StdDuration::from_millis(1),
            2048,
        );
        metrics.set_event_log_consumer_lag("orchestrator.triggers.pending", "orchestrator-pump", 0);
        metrics.set_trigger_budget_cost_today("github-new-issue", 0.002);
        metrics.record_trigger_budget_exhausted("github-new-issue", "daily_budget_exceeded");
        metrics.record_a2a_hop("agent.example", "succeeded", StdDuration::from_millis(10));
        metrics.set_worker_queue_depth("triage", 1);
        metrics.record_worker_queue_claim_age("triage", 3.0);
        metrics.set_orchestrator_pump_backlog("trigger.inbox.envelopes", 2);
        metrics.set_orchestrator_pump_outstanding("trigger.inbox.envelopes", 1);
        metrics.record_orchestrator_pump_admission_delay(
            "trigger.inbox.envelopes",
            StdDuration::from_millis(50),
        );
        metrics.record_trigger_accepted_to_normalized(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "normalized",
            StdDuration::from_millis(25),
        );
        metrics.record_trigger_accepted_to_queue_append(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "queued",
            StdDuration::from_millis(40),
        );
        metrics.record_trigger_queue_age_at_dispatch_admission(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "admitted",
            StdDuration::from_millis(75),
        );
        metrics.record_trigger_queue_age_at_dispatch_start(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "started",
            StdDuration::from_millis(125),
        );
        metrics.record_trigger_dispatch_runtime(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "succeeded",
            StdDuration::from_millis(250),
        );
        metrics.record_trigger_retry_delay(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "scheduled",
            StdDuration::from_secs(2),
        );
        metrics.record_trigger_accepted_to_dlq(
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            "retry_exhausted",
            StdDuration::from_secs(45),
        );
        metrics.record_backpressure_event("ingest", "reject");
        metrics.note_trigger_pending_event(
            "evt-1",
            "github-new-issue",
            "github-new-issue@v7",
            "github",
            Some("tenant-a"),
            1_000,
            4_000,
        );
        metrics.record_llm_call("mock", "mock", "succeeded", 0.01);
        metrics.record_llm_cache_hit("mock");

        let rendered = metrics.render_prometheus();
        for needle in [
            "harn_http_requests_total{endpoint=\"/triggers/github\",method=\"POST\",status=\"200\"} 1",
            "harn_http_request_duration_seconds_bucket{endpoint=\"/triggers/github\",le=\"0.05\"} 1",
            "harn_http_body_size_bytes_bucket{endpoint=\"/triggers/github\",le=\"512\"} 1",
            "harn_trigger_received_total{provider=\"github\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_deduped_total{reason=\"inbox_duplicate\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_predicate_evaluations_total{result=\"true\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_predicate_cost_usd_bucket{le=\"0.01\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_dispatched_total{handler_kind=\"local\",outcome=\"succeeded\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_retries_total{attempt=\"2\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_dlq_total{reason=\"retry_exhausted\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_inflight{trigger_id=\"github-new-issue\"} 0",
            "harn_event_log_append_duration_seconds_bucket{le=\"0.005\",topic=\"orchestrator.triggers.pending\"} 1",
            "harn_event_log_topic_size_bytes{topic=\"orchestrator.triggers.pending\"} 2048",
            "harn_event_log_consumer_lag{consumer=\"orchestrator-pump\",topic=\"orchestrator.triggers.pending\"} 0",
            "harn_trigger_budget_cost_today_usd{trigger_id=\"github-new-issue\"} 0.002",
            "harn_trigger_budget_exhausted_total{strategy=\"daily_budget_exceeded\",trigger_id=\"github-new-issue\"} 1",
            "harn_backpressure_events_total{action=\"reject\",dimension=\"ingest\"} 1",
            "harn_a2a_hops_total{outcome=\"succeeded\",target=\"agent.example\"} 1",
            "harn_a2a_hop_duration_seconds_bucket{le=\"0.01\",target=\"agent.example\"} 1",
            "harn_worker_queue_depth{queue=\"triage\"} 1",
            "harn_worker_queue_claim_age_seconds_bucket{le=\"5\",queue=\"triage\"} 1",
            "harn_orchestrator_pump_backlog{topic=\"trigger.inbox.envelopes\"} 2",
            "harn_orchestrator_pump_outstanding{topic=\"trigger.inbox.envelopes\"} 1",
            "harn_orchestrator_pump_admission_delay_seconds_bucket{le=\"0.05\",topic=\"trigger.inbox.envelopes\"} 1",
            "harn_trigger_webhook_accepted_to_normalized_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"0.025\",provider=\"github\",status=\"normalized\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_webhook_accepted_to_queue_append_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"0.05\",provider=\"github\",status=\"queued\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_queue_age_at_dispatch_admission_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"0.1\",provider=\"github\",status=\"admitted\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_queue_age_at_dispatch_start_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"0.25\",provider=\"github\",status=\"started\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_dispatch_runtime_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"0.25\",provider=\"github\",status=\"succeeded\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_retry_delay_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"2.5\",provider=\"github\",status=\"scheduled\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_accepted_to_dlq_seconds_bucket{binding_key=\"github-new-issue@v7\",le=\"60\",provider=\"github\",status=\"retry_exhausted\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 1",
            "harn_trigger_oldest_pending_age_seconds{binding_key=\"github-new-issue@v7\",provider=\"github\",tenant_id=\"tenant-a\",trigger_id=\"github-new-issue\"} 3",
            "harn_llm_calls_total{model=\"mock\",outcome=\"succeeded\",provider=\"mock\"} 1",
            "harn_llm_cost_usd_total{model=\"mock\",provider=\"mock\"} 0.01",
            "harn_llm_cache_hits_total{provider=\"mock\"} 1",
        ] {
            assert!(rendered.contains(needle), "missing {needle}\n{rendered}");
        }
    }
}
