use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

pub use crate::http::{HttpMockCallSnapshot, HttpMockResponse};
pub use crate::triggers::test_util::clock::{
    active_mock_clock, install_override as install_clock_override, instant_now, now_ms, now_utc,
    ClockInstant, ClockOverrideGuard, MockClock,
};

use crate::connectors::{
    ConnectorCtx, MetricsRegistry, RateLimiterFactory, RawInbound, TriggerBinding,
};
use crate::event_log::{AnyEventLog, MemoryEventLog};
use crate::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider, SecretVersion,
};
use crate::triggers::{InboxIndex, ProviderId, TenantId};

#[derive(Clone, Debug)]
pub struct MemorySecretProvider {
    provider: String,
    inner: Arc<Mutex<BTreeMap<(String, String), VersionedSecret>>>,
}

#[derive(Clone, Debug, Default)]
struct VersionedSecret {
    latest: Option<u64>,
    versions: BTreeMap<u64, Vec<u8>>,
}

impl MemorySecretProvider {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn empty() -> Self {
        Self::new("connector-testkit")
    }

    pub fn with_secret(mut self, id: SecretId, value: impl AsRef<[u8]>) -> Self {
        self.insert(id, value);
        self
    }

    pub fn with_scoped_secret(
        self,
        namespace: impl Into<String>,
        tenant_id: impl AsRef<str>,
        binding_id: impl AsRef<str>,
        name: impl AsRef<str>,
        value: impl AsRef<[u8]>,
    ) -> Self {
        let id = scoped_secret_id(namespace, tenant_id, binding_id, name);
        self.with_secret(id, value)
    }

    pub fn insert(&mut self, id: SecretId, value: impl AsRef<[u8]>) {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        insert_secret(&mut inner, id, value.as_ref().to_vec());
    }

    pub fn insert_scoped(
        &mut self,
        namespace: impl Into<String>,
        tenant_id: impl AsRef<str>,
        binding_id: impl AsRef<str>,
        name: impl AsRef<str>,
        value: impl AsRef<[u8]>,
    ) -> SecretId {
        let id = scoped_secret_id(namespace, tenant_id, binding_id, name);
        self.insert(id.clone(), value);
        id
    }

    pub fn snapshot(&self) -> Vec<SecretMeta> {
        let inner = self.inner.lock().expect("memory secret provider poisoned");
        inner
            .iter()
            .filter_map(|((namespace, name), secret)| {
                let latest = secret.latest?;
                Some(SecretMeta {
                    id: SecretId::new(namespace.clone(), name.clone())
                        .with_version(SecretVersion::Exact(latest)),
                    provider: self.provider.clone(),
                })
            })
            .collect()
    }
}

#[async_trait]
impl SecretProvider for MemorySecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        let inner = self.inner.lock().expect("memory secret provider poisoned");
        let secret = inner
            .get(&(id.namespace.clone(), id.name.clone()))
            .ok_or_else(|| SecretError::NotFound {
                provider: self.provider.clone(),
                id: id.clone(),
            })?;
        let version = match id.version {
            SecretVersion::Latest => secret.latest,
            SecretVersion::Exact(version) => Some(version),
        }
        .ok_or_else(|| SecretError::NotFound {
            provider: self.provider.clone(),
            id: id.clone(),
        })?;
        secret
            .versions
            .get(&version)
            .cloned()
            .map(SecretBytes::from)
            .ok_or_else(|| SecretError::NotFound {
                provider: self.provider.clone(),
                id: id.clone(),
            })
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        let value = value.with_exposed(|bytes| bytes.to_vec());
        insert_secret(&mut inner, id.clone(), value);
        Ok(())
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        let key = (id.namespace.clone(), id.name.clone());
        let secret = inner.entry(key).or_default();
        let from_version = secret.latest;
        let to_version = from_version.unwrap_or(0) + 1;
        let value = from_version
            .and_then(|version| secret.versions.get(&version).cloned())
            .unwrap_or_default();
        secret.versions.insert(to_version, value);
        secret.latest = Some(to_version);
        Ok(RotationHandle {
            provider: self.provider.clone(),
            id: SecretId::new(id.namespace.clone(), id.name.clone())
                .with_version(SecretVersion::Exact(to_version)),
            from_version,
            to_version: Some(to_version),
        })
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        let inner = self.inner.lock().expect("memory secret provider poisoned");
        Ok(inner
            .iter()
            .filter(|((namespace, name), _)| {
                namespace == &prefix.namespace && name.starts_with(&prefix.name)
            })
            .filter_map(|((namespace, name), secret)| {
                let latest = secret.latest?;
                Some(SecretMeta {
                    id: SecretId::new(namespace.clone(), name.clone())
                        .with_version(SecretVersion::Exact(latest)),
                    provider: self.provider.clone(),
                })
            })
            .collect())
    }

    fn namespace(&self) -> &str {
        &self.provider
    }

    fn supports_versions(&self) -> bool {
        true
    }
}

fn insert_secret(
    inner: &mut BTreeMap<(String, String), VersionedSecret>,
    id: SecretId,
    value: Vec<u8>,
) {
    let secret = inner.entry((id.namespace, id.name)).or_default();
    let version = match id.version {
        SecretVersion::Latest => secret.latest.unwrap_or(0) + 1,
        SecretVersion::Exact(version) => version,
    };
    secret.versions.insert(version, value);
    secret.latest = Some(secret.latest.map_or(version, |latest| latest.max(version)));
}

pub fn scoped_secret_id(
    namespace: impl Into<String>,
    tenant_id: impl AsRef<str>,
    binding_id: impl AsRef<str>,
    name: impl AsRef<str>,
) -> SecretId {
    SecretId::new(
        namespace,
        format!(
            "tenants/{}/bindings/{}/{}",
            tenant_id.as_ref(),
            binding_id.as_ref(),
            name.as_ref()
        ),
    )
}

#[derive(Clone)]
pub struct ConnectorTestkit {
    pub clock: Arc<MockClock>,
    pub event_log: Arc<AnyEventLog>,
    pub inbox: Arc<InboxIndex>,
    pub metrics: Arc<MetricsRegistry>,
    pub rate_limiter: Arc<RateLimiterFactory>,
    pub secrets: Arc<MemorySecretProvider>,
}

impl ConnectorTestkit {
    pub async fn new(start: OffsetDateTime) -> Self {
        Self::with_secrets(start, MemorySecretProvider::empty()).await
    }

    pub async fn with_secrets(start: OffsetDateTime, secrets: MemorySecretProvider) -> Self {
        let clock = MockClock::new(start);
        let event_log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
        let metrics = Arc::new(MetricsRegistry::default());
        let inbox = Arc::new(
            InboxIndex::new(event_log.clone(), metrics.clone())
                .await
                .expect("connector testkit inbox should initialize"),
        );
        Self {
            clock,
            event_log,
            inbox,
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::default()),
            secrets: Arc::new(secrets),
        }
    }

    pub fn ctx(&self) -> ConnectorCtx {
        ConnectorCtx {
            event_log: self.event_log.clone(),
            secrets: self.secrets.clone(),
            inbox: self.inbox.clone(),
            metrics: self.metrics.clone(),
            rate_limiter: self.rate_limiter.clone(),
        }
    }

    pub fn install_clock(&self) -> ClockOverrideGuard {
        install_clock_override(self.clock.clone())
    }
}

#[derive(Debug)]
pub struct TempPackageWorkspace {
    root: PathBuf,
}

impl TempPackageWorkspace {
    pub fn new(prefix: impl AsRef<str>) -> io::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "{}-{}",
            prefix.as_ref().trim_matches('-'),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn write_file(
        &self,
        relative: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> io::Result<PathBuf> {
        let path = self.root.join(relative.as_ref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        Ok(path)
    }

    pub fn write_harn_package(&self, name: &str) -> io::Result<PathBuf> {
        self.write_file(
            "Harn.toml",
            format!("[package]\nname = \"{}\"\nversion = \"0.0.0-test\"\n", name),
        )
    }

    pub fn write_cargo_package(&self, name: &str) -> io::Result<PathBuf> {
        self.write_file(
            "Cargo.toml",
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
                name
            ),
        )
    }

    pub fn write_npm_package(&self, name: &str) -> io::Result<PathBuf> {
        self.write_file(
            "package.json",
            format!("{{\"name\":\"{}\",\"version\":\"0.0.0-test\"}}\n", name),
        )
    }
}

impl Drop for TempPackageWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub struct HttpMockGuard;

impl HttpMockGuard {
    pub fn new() -> Self {
        crate::http::reset_http_state();
        Self
    }

    pub fn push(
        &self,
        method: impl Into<String>,
        url_pattern: impl Into<String>,
        responses: Vec<HttpMockResponse>,
    ) {
        crate::http::push_http_mock(method, url_pattern, responses);
    }

    pub fn calls(&self) -> Vec<HttpMockCallSnapshot> {
        crate::http::http_mock_calls_snapshot()
    }
}

impl Default for HttpMockGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HttpMockGuard {
    fn drop(&mut self) {
        crate::http::reset_http_state();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MockStreamEvent {
    Json(JsonValue),
    Bytes(Vec<u8>),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct MockStreamHandle {
    tx: mpsc::UnboundedSender<MockStreamEvent>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
pub struct MockStreamReader {
    rx: mpsc::UnboundedReceiver<MockStreamEvent>,
    cancelled: Arc<AtomicBool>,
}

pub fn mock_stream() -> (MockStreamHandle, MockStreamReader) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    (
        MockStreamHandle {
            tx,
            cancelled: cancelled.clone(),
        },
        MockStreamReader { rx, cancelled },
    )
}

impl MockStreamHandle {
    pub fn send_json(
        &self,
        value: JsonValue,
    ) -> Result<(), mpsc::error::SendError<MockStreamEvent>> {
        self.tx.send(MockStreamEvent::Json(value))
    }

    pub fn send_bytes(
        &self,
        value: impl Into<Vec<u8>>,
    ) -> Result<(), mpsc::error::SendError<MockStreamEvent>> {
        self.tx.send(MockStreamEvent::Bytes(value.into()))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.tx.send(MockStreamEvent::Cancelled);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl MockStreamReader {
    pub async fn next(&mut self) -> Option<MockStreamEvent> {
        self.rx.recv().await
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebhookFixture {
    pub raw: RawInbound,
    pub body: Vec<u8>,
}

impl WebhookFixture {
    pub fn with_binding(mut self, binding: &TriggerBinding) -> Self {
        self.raw.metadata = json!({
            "binding_id": binding.binding_id,
            "binding_version": 1,
        });
        self
    }

    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.raw.tenant_id = Some(TenantId(tenant_id.into()));
        self
    }
}

pub fn github_ping_fixture(secret: &str, received_at: OffsetDateTime) -> WebhookFixture {
    let body = br#"{"zen":"Keep it logically awesome.","hook_id":42}"#.to_vec();
    let mut raw = RawInbound::new(
        "webhook",
        BTreeMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            ("x-github-event".to_string(), "ping".to_string()),
            ("x-github-delivery".to_string(), "delivery-1".to_string()),
            (
                "x-hub-signature-256".to_string(),
                format!("sha256={}", hmac_sha256_hex(secret.as_bytes(), &body)),
            ),
        ]),
        body.clone(),
    );
    raw.received_at = received_at;
    WebhookFixture { raw, body }
}

pub fn slack_message_fixture(
    secret: &str,
    timestamp: i64,
    received_at: OffsetDateTime,
) -> WebhookFixture {
    let body = br#"{"type":"event_callback","event_id":"Ev1","team_id":"T1","event":{"type":"message","channel_type":"channel","channel":"C1","user":"U1","text":"hello","event_ts":"1710000000.000100"}}"#.to_vec();
    let signed = format!("v0:{timestamp}:{}", String::from_utf8_lossy(&body));
    let mut raw = RawInbound::new(
        "webhook",
        BTreeMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            (
                "x-slack-request-timestamp".to_string(),
                timestamp.to_string(),
            ),
            (
                "x-slack-signature".to_string(),
                format!(
                    "v0={}",
                    hmac_sha256_hex(secret.as_bytes(), signed.as_bytes())
                ),
            ),
        ]),
        body.clone(),
    );
    raw.received_at = received_at;
    WebhookFixture { raw, body }
}

pub fn linear_issue_update_fixture(secret: &str, received_at: OffsetDateTime) -> WebhookFixture {
    let body = br#"{"type":"Issue","action":"update","createdAt":"2026-04-19T00:00:00Z","data":{"id":"issue-1","identifier":"ENG-1","title":"connector"},"updatedFrom":{"title":"old"}}"#.to_vec();
    let mut raw = RawInbound::new(
        "webhook",
        BTreeMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            (
                "linear-signature".to_string(),
                hmac_sha256_hex(secret.as_bytes(), &body),
            ),
        ]),
        body.clone(),
    );
    raw.received_at = received_at;
    WebhookFixture { raw, body }
}

pub fn notion_page_content_updated_fixture(
    secret: &str,
    received_at: OffsetDateTime,
) -> WebhookFixture {
    let body = br#"{"id":"evt_1","type":"page.content_updated","workspace_id":"ws_1","subscription_id":"sub_1","integration_id":"int_1","entity":{"id":"page_1","type":"page"},"api_version":"2022-06-28"}"#.to_vec();
    let mut raw = RawInbound::new(
        "webhook",
        BTreeMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            (
                "x-notion-signature".to_string(),
                format!("sha256={}", hmac_sha256_hex(secret.as_bytes(), &body)),
            ),
            ("request-id".to_string(), "req_1".to_string()),
        ]),
        body.clone(),
    );
    raw.received_at = received_at;
    WebhookFixture { raw, body }
}

pub fn webhook_binding(
    provider: impl Into<String>,
    binding_id: impl Into<String>,
    signing_secret: Option<SecretId>,
) -> TriggerBinding {
    let provider = provider.into();
    let mut binding =
        TriggerBinding::new(ProviderId::from(provider.clone()), "webhook", binding_id);
    let mut secrets = serde_json::Map::new();
    if let Some(secret) = signing_secret {
        secrets.insert(
            "signing_secret".to_string(),
            JsonValue::String(secret.to_string()),
        );
    }
    binding.config = json!({
        "path": format!("/hooks/{provider}"),
        "match": {"events": ["*"]},
        "secrets": secrets,
    });
    binding
}

fn hmac_sha256_hex(secret: &[u8], data: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut key = if secret.len() > BLOCK_SIZE {
        Sha256::digest(secret).to_vec()
    } else {
        secret.to_vec()
    };
    key.resize(BLOCK_SIZE, 0);
    let mut outer = vec![0x5c; BLOCK_SIZE];
    let mut inner = vec![0x36; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        outer[i] ^= key[i];
        inner[i] ^= key[i];
    }
    let mut inner_hash = Sha256::new();
    inner_hash.update(&inner);
    inner_hash.update(data);
    let inner_result = inner_hash.finalize();
    let mut outer_hash = Sha256::new();
    outer_hash.update(&outer);
    outer_hash.update(inner_result);
    hex::encode(outer_hash.finalize())
}

pub async fn advance_until<F>(
    clock: &MockClock,
    timeout: StdDuration,
    tick: StdDuration,
    mut predicate: F,
) -> bool
where
    F: FnMut() -> bool,
{
    let mut elapsed = StdDuration::ZERO;
    while elapsed <= timeout {
        if predicate() {
            return true;
        }
        clock.advance_std(tick).await;
        elapsed += tick;
    }
    predicate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretProvider;

    fn parse_ts(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).unwrap()
    }

    #[tokio::test]
    async fn memory_secret_provider_scopes_and_versions_secrets() {
        let mut provider = MemorySecretProvider::new("test");
        let scoped = provider.insert_scoped("github", "tenant-a", "binding-a", "token", "v1");
        provider
            .put(&scoped, SecretBytes::from("v2"))
            .await
            .expect("put latest");

        let latest = provider.get(&scoped).await.expect("latest");
        assert_eq!(latest.with_exposed(|bytes| bytes.to_vec()), b"v2".to_vec());
        let first = provider
            .get(&scoped.clone().with_version(SecretVersion::Exact(1)))
            .await
            .expect("v1");
        assert_eq!(first.with_exposed(|bytes| bytes.to_vec()), b"v1".to_vec());
        assert!(provider
            .get(&scoped_secret_id(
                "github",
                "tenant-b",
                "binding-a",
                "token"
            ))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn connector_testkit_controls_clock_and_deadlines() {
        let kit = ConnectorTestkit::new(parse_ts("2026-04-19T00:00:00Z")).await;
        let _guard = kit.install_clock();
        let mut fired = false;
        assert!(
            !advance_until(
                &kit.clock,
                StdDuration::from_millis(20),
                StdDuration::from_millis(10),
                || fired,
            )
            .await
        );
        fired = true;
        assert!(
            advance_until(
                &kit.clock,
                StdDuration::from_millis(20),
                StdDuration::from_millis(10),
                || fired,
            )
            .await
        );
        assert_eq!(instant_now().as_millis(), 30);
    }

    #[tokio::test]
    async fn mock_stream_cancels_reader_without_wall_clock_sleep() {
        let (handle, mut reader) = mock_stream();
        handle.send_json(json!({"event": "one"})).expect("send");
        assert_eq!(
            reader.next().await,
            Some(MockStreamEvent::Json(json!({"event": "one"})))
        );
        handle.cancel();
        assert_eq!(reader.next().await, Some(MockStreamEvent::Cancelled));
        assert!(reader.is_cancelled());
    }

    #[test]
    fn temp_workspace_writes_package_markers() {
        let workspace = TempPackageWorkspace::new("harn-testkit").expect("workspace");
        workspace.write_harn_package("demo").expect("harn package");
        workspace
            .write_cargo_package("demo")
            .expect("cargo package");
        workspace.write_npm_package("demo").expect("npm package");
        assert!(workspace.path().join("Harn.toml").exists());
        assert!(workspace.path().join("Cargo.toml").exists());
        assert!(workspace.path().join("package.json").exists());
    }

    #[test]
    fn webhook_fixtures_include_provider_signatures() {
        let received_at = parse_ts("2026-04-19T00:00:00Z");
        let github = github_ping_fixture("topsecret", received_at);
        assert!(github.raw.headers["x-hub-signature-256"].starts_with("sha256="));
        let slack = slack_message_fixture("topsecret", received_at.unix_timestamp(), received_at);
        assert!(slack.raw.headers["x-slack-signature"].starts_with("v0="));
        let linear = linear_issue_update_fixture("topsecret", received_at);
        assert_eq!(linear.raw.headers["linear-signature"].len(), 64);
        let notion = notion_page_content_updated_fixture("topsecret", received_at);
        assert!(notion.raw.headers["x-notion-signature"].starts_with("sha256="));
    }
}
