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
use std::sync::{Arc, Mutex};
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

pub mod cron;
pub mod github;
pub mod hmac;
pub mod slack;
#[cfg(test)]
pub(crate) mod test_util;
pub mod webhook;

pub use cron::{CatchupMode, CronConnector};
pub use github::GitHubConnector;
pub use hmac::{
    verify_hmac_authorization, HmacSignatureStyle, DEFAULT_CANONICAL_AUTHORIZATION_HEADER,
    DEFAULT_CANONICAL_HMAC_SCHEME, DEFAULT_GITHUB_SIGNATURE_HEADER, DEFAULT_SLACK_SIGNATURE_HEADER,
    DEFAULT_SLACK_TIMESTAMP_HEADER, DEFAULT_STANDARD_WEBHOOKS_ID_HEADER,
    DEFAULT_STANDARD_WEBHOOKS_SIGNATURE_HEADER, DEFAULT_STANDARD_WEBHOOKS_TIMESTAMP_HEADER,
    DEFAULT_STRIPE_SIGNATURE_HEADER, SIGNATURE_VERIFY_AUDIT_TOPIC,
};
pub use slack::SlackConnector;
use webhook::WebhookProviderProfile;
pub use webhook::{GenericWebhookConnector, WebhookSignatureVariant};

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
    fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError>;

    /// Payload schema surfaced to future trigger-type narrowing.
    fn payload_schema(&self) -> ProviderPayloadSchema;

    /// Outbound API wrapper exposed to handlers.
    fn client(&self) -> Arc<dyn ConnectorClient>;
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
}

impl MetricsRegistry {
    pub fn snapshot(&self) -> ConnectorMetricsSnapshot {
        ConnectorMetricsSnapshot {
            inbox_claims_written: self.inbox_claims_written.load(Ordering::Relaxed),
            inbox_duplicates_rejected: self.inbox_duplicates_rejected.load(Ordering::Relaxed),
            inbox_fast_path_hits: self.inbox_fast_path_hits.load(Ordering::Relaxed),
            inbox_durable_hits: self.inbox_durable_hits.load(Ordering::Relaxed),
            inbox_expired_entries: self.inbox_expired_entries.load(Ordering::Relaxed),
            inbox_active_entries: self.inbox_active_entries.load(Ordering::Relaxed),
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
    if provider.provider == "slack" {
        return Box::new(SlackConnector::new());
    }
    match &provider.runtime {
        ProviderRuntimeMetadata::Builtin {
            connector,
            default_signature_variant,
        } => match connector.as_str() {
            "cron" => Box::new(CronConnector::new()),
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

    fn normalize_inbound(&self, _raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
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

        fn normalize_inbound(&self, _raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
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
}
