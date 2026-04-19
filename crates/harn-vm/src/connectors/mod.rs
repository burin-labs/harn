//! Connector traits and shared helpers for inbound event-source providers.
//!
//! This lands in `harn-vm` for now because the current dependency surface
//! (`EventLog`, `SecretProvider`, `TriggerEvent`) already lives here. If the
//! connector ecosystem grows enough to justify extraction, the module can be
//! split into a dedicated crate later without changing the high-level contract.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use tokio::sync::Mutex as AsyncMutex;

use crate::event_log::AnyEventLog;
use crate::secrets::SecretProvider;
use crate::triggers::{ProviderId, TenantId, TriggerEvent};

pub mod cron;
pub mod hmac;
#[cfg(test)]
pub(crate) mod test_util;

pub use cron::{CatchupMode, CronConnector};
pub use hmac::{
    HmacSignatureStyle, DEFAULT_GITHUB_SIGNATURE_HEADER, DEFAULT_STANDARD_WEBHOOKS_ID_HEADER,
    DEFAULT_STANDARD_WEBHOOKS_SIGNATURE_HEADER, DEFAULT_STANDARD_WEBHOOKS_TIMESTAMP_HEADER,
    DEFAULT_STRIPE_SIGNATURE_HEADER, SIGNATURE_VERIFY_AUDIT_TOPIC,
};

/// Shared owned handle to a connector instance registered with the runtime.
pub type ConnectorHandle = Arc<AsyncMutex<Box<dyn Connector>>>;

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

    /// Verify + normalize a provider-native inbound request into `TriggerEvent`.
    fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError>;

    /// Payload schema surfaced to future trigger-type narrowing.
    fn payload_schema(&self) -> ProviderPayloadSchema;

    /// Outbound API wrapper exposed to handlers.
    fn client(&self) -> Arc<dyn ConnectorClient>;
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

/// Placeholder inbox index until the durable trigger inbox lands.
#[derive(Clone, Debug, Default)]
pub struct InboxIndex;

/// Placeholder metrics surface for connector-local counters and timings.
#[derive(Clone, Debug, Default)]
pub struct MetricsRegistry;

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
            config: JsonValue::Null,
        }
    }
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
            received_at: OffsetDateTime::now_utc(),
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
    last_refill: Instant,
}

impl TokenBucket {
    fn full(config: RateLimitConfig) -> Self {
        Self {
            tokens: config.capacity as f64,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, config: RateLimitConfig, now: Instant) {
        let interval = config.refill_interval.as_secs_f64().max(f64::EPSILON);
        let rate = config.refill_tokens.max(1) as f64 / interval;
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(config.capacity.max(1) as f64);
        self.last_refill = now;
    }

    fn try_acquire(&mut self, config: RateLimitConfig, now: Instant) -> bool {
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
        bucket.try_acquire(self.config, Instant::now())
    }

    pub async fn acquire(&self, provider: &ProviderId, key: &str) {
        loop {
            let wait = {
                let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
                let bucket = buckets
                    .entry((provider.as_str().to_string(), key.to_string()))
                    .or_insert_with(|| TokenBucket::full(self.config));
                if bucket.try_acquire(self.config, Instant::now()) {
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
#[derive(Default)]
pub struct ConnectorRegistry {
    connectors: BTreeMap<ProviderId, ConnectorHandle>,
}

impl ConnectorRegistry {
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
        let mut registry = ConnectorRegistry::default();
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
        let mut registry = ConnectorRegistry::default();
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
}
