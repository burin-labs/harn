use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use crate::event_log::{
    AnyEventLog, CompactReport, ConsumerId, EventId, EventLog, EventLogDescription, LogError,
    LogEvent, Topic,
};
use crate::orchestration::CapabilityPolicy;
use crate::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
};
use crate::TenantId;

pub const TENANT_REGISTRY_DIR: &str = "tenants";
pub const TENANT_REGISTRY_FILE: &str = "registry.json";
pub const TENANT_SECRET_NAMESPACE_PREFIX: &str = "harn.tenant.";
pub const TENANT_EVENT_TOPIC_PREFIX: &str = "tenant.";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiKeyId(pub String);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TenantBudget {
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
    pub ingest_per_minute: Option<u32>,
    pub event_log_size_bytes: u64,
    pub in_flight_dispatches: u32,
    pub dlq_entries: u32,
}

impl Default for TenantBudget {
    fn default() -> Self {
        Self {
            daily_cost_usd: None,
            hourly_cost_usd: None,
            ingest_per_minute: None,
            event_log_size_bytes: 10 * 1024 * 1024 * 1024,
            in_flight_dispatches: 100,
            dlq_entries: 10_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TenantScope {
    pub id: TenantId,
    pub state_root: PathBuf,
    pub secret_namespace: String,
    pub event_log_topic_prefix: String,
    pub capability_ceiling: CapabilityPolicy,
    pub budget: TenantBudget,
    pub api_key_ids: Vec<ApiKeyId>,
}

impl TenantScope {
    pub fn new(id: TenantId, orchestrator_state_root: impl AsRef<Path>) -> Result<Self, String> {
        validate_tenant_id(&id.0)?;
        let state_root = orchestrator_state_root
            .as_ref()
            .join(TENANT_REGISTRY_DIR)
            .join(&id.0);
        Ok(Self {
            secret_namespace: tenant_secret_namespace(&id),
            event_log_topic_prefix: tenant_event_topic_prefix(&id),
            id,
            state_root,
            capability_ceiling: CapabilityPolicy::default(),
            budget: TenantBudget::default(),
            api_key_ids: Vec::new(),
        })
    }

    pub fn topic(&self, topic: &Topic) -> Result<Topic, LogError> {
        tenant_topic(&self.id, topic)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantStatus {
    Active,
    Suspended,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TenantApiKeyRecord {
    pub id: ApiKeyId,
    pub hash_sha256: String,
    pub prefix: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TenantRecord {
    pub scope: TenantScope,
    pub status: TenantStatus,
    pub created_at: String,
    pub suspended_at: Option<String>,
    pub api_keys: Vec<TenantApiKeyRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TenantRegistrySnapshot {
    pub tenants: Vec<TenantRecord>,
}

#[derive(Clone, Debug)]
pub struct TenantStore {
    state_dir: PathBuf,
    tenants: BTreeMap<String, TenantRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TenantResolutionError {
    Unknown,
    Suspended(TenantId),
}

impl TenantStore {
    pub fn load(state_dir: impl AsRef<Path>) -> Result<Self, String> {
        let state_dir = state_dir.as_ref().to_path_buf();
        let path = registry_path(&state_dir);
        if !path.is_file() {
            return Ok(Self {
                state_dir,
                tenants: BTreeMap::new(),
            });
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let snapshot: TenantRegistrySnapshot = serde_json::from_str(&content)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        let tenants = snapshot
            .tenants
            .into_iter()
            .map(|record| (record.scope.id.0.clone(), record))
            .collect();
        Ok(Self { state_dir, tenants })
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = self.state_dir.join(TENANT_REGISTRY_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
        let snapshot = TenantRegistrySnapshot {
            tenants: self.list().to_vec(),
        };
        let encoded = serde_json::to_string_pretty(&snapshot).map_err(|error| error.to_string())?;
        let path = registry_path(&self.state_dir);
        write_file_replace(&path, encoded.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", path.display()))
    }

    pub fn create_tenant(
        &mut self,
        id: impl Into<String>,
        budget: TenantBudget,
    ) -> Result<(TenantRecord, String), String> {
        let id = id.into();
        validate_tenant_id(&id)?;
        if self.tenants.contains_key(&id) {
            return Err(format!("tenant '{id}' already exists"));
        }
        let api_key = generate_api_key(&id);
        let api_key_id = ApiKeyId(format!("key_{}", uuid::Uuid::now_v7()));
        let created_at = now_rfc3339();
        let mut scope = TenantScope::new(TenantId::new(id.clone()), &self.state_dir)?;
        scope.budget = budget;
        scope.api_key_ids.push(api_key_id.clone());
        std::fs::create_dir_all(&scope.state_root).map_err(|error| {
            format!(
                "failed to create tenant state dir {}: {error}",
                scope.state_root.display()
            )
        })?;
        let record = TenantRecord {
            scope,
            status: TenantStatus::Active,
            created_at: created_at.clone(),
            suspended_at: None,
            api_keys: vec![TenantApiKeyRecord {
                id: api_key_id,
                hash_sha256: api_key_hash(&api_key),
                prefix: api_key_prefix(&api_key),
                created_at,
            }],
        };
        self.tenants.insert(id, record.clone());
        self.save()?;
        Ok((record, api_key))
    }

    pub fn list(&self) -> Vec<TenantRecord> {
        self.tenants.values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<&TenantRecord> {
        self.tenants.get(id)
    }

    pub fn suspend(&mut self, id: &str) -> Result<TenantRecord, String> {
        let record = self
            .tenants
            .get_mut(id)
            .ok_or_else(|| format!("unknown tenant '{id}'"))?;
        record.status = TenantStatus::Suspended;
        record.suspended_at = Some(now_rfc3339());
        let record = record.clone();
        self.save()?;
        Ok(record)
    }

    pub fn delete(&mut self, id: &str) -> Result<TenantRecord, String> {
        let record = self
            .tenants
            .remove(id)
            .ok_or_else(|| format!("unknown tenant '{id}'"))?;
        if record.scope.state_root.exists() {
            std::fs::remove_dir_all(&record.scope.state_root).map_err(|error| {
                format!(
                    "failed to remove tenant state dir {}: {error}",
                    record.scope.state_root.display()
                )
            })?;
        }
        self.save()?;
        Ok(record)
    }

    pub fn resolve_api_key(&self, candidate: &str) -> Result<TenantScope, TenantResolutionError> {
        let candidate_hash = api_key_hash(candidate);
        for record in self.tenants.values() {
            let matched = record.api_keys.iter().any(|key| {
                key.hash_sha256
                    .as_bytes()
                    .ct_eq(candidate_hash.as_bytes())
                    .into()
            });
            if matched {
                return match record.status {
                    TenantStatus::Active => Ok(record.scope.clone()),
                    TenantStatus::Suspended => {
                        Err(TenantResolutionError::Suspended(record.scope.id.clone()))
                    }
                };
            }
        }
        Err(TenantResolutionError::Unknown)
    }
}

pub struct TenantEventLog {
    inner: Arc<AnyEventLog>,
    scope: TenantScope,
}

impl TenantEventLog {
    pub fn new(inner: Arc<AnyEventLog>, scope: TenantScope) -> Self {
        Self { inner, scope }
    }

    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }

    fn scoped_topic(&self, topic: &Topic) -> Result<Topic, LogError> {
        if topic.as_str().starts_with(TENANT_EVENT_TOPIC_PREFIX) {
            if topic
                .as_str()
                .starts_with(&self.scope.event_log_topic_prefix)
            {
                return Ok(topic.clone());
            }
            return Err(LogError::InvalidTopic(format!(
                "topic '{}' is outside tenant scope '{}'",
                topic.as_str(),
                self.scope.id.0
            )));
        }
        self.scope.topic(topic)
    }
}

impl EventLog for TenantEventLog {
    fn describe(&self) -> EventLogDescription {
        self.inner.describe()
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        self.inner.append(&self.scoped_topic(topic)?, event).await
    }

    async fn flush(&self) -> Result<(), LogError> {
        self.inner.flush().await
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        self.inner
            .read_range(&self.scoped_topic(topic)?, from, limit)
            .await
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        self.inner
            .clone()
            .subscribe(&self.scoped_topic(topic)?, from)
            .await
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        self.inner
            .ack(&self.scoped_topic(topic)?, consumer, up_to)
            .await
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        self.inner
            .consumer_cursor(&self.scoped_topic(topic)?, consumer)
            .await
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        self.inner.latest(&self.scoped_topic(topic)?).await
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        self.inner.compact(&self.scoped_topic(topic)?, before).await
    }
}

pub struct TenantSecretProvider {
    inner: Arc<dyn SecretProvider>,
    scope: TenantScope,
}

impl TenantSecretProvider {
    pub fn new(inner: Arc<dyn SecretProvider>, scope: TenantScope) -> Self {
        Self { inner, scope }
    }

    fn scoped_id(&self, id: &SecretId) -> Result<SecretId, SecretError> {
        if id.namespace == self.scope.secret_namespace {
            return Ok(id.clone());
        }
        if id.namespace.starts_with(TENANT_SECRET_NAMESPACE_PREFIX) {
            return Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            });
        }
        Ok(SecretId {
            namespace: self.scope.secret_namespace.clone(),
            name: id.name.clone(),
            version: id.version.clone(),
        })
    }
}

#[async_trait]
impl SecretProvider for TenantSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.inner.get(&self.scoped_id(id)?).await
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        self.inner.put(&self.scoped_id(id)?, value).await
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        self.inner.rotate(&self.scoped_id(id)?).await
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        self.inner.list(&self.scoped_id(prefix)?).await
    }

    fn namespace(&self) -> &str {
        &self.scope.secret_namespace
    }

    fn supports_versions(&self) -> bool {
        self.inner.supports_versions()
    }
}

pub fn tenant_event_topic_prefix(id: &TenantId) -> String {
    format!("{TENANT_EVENT_TOPIC_PREFIX}{}.", id.0)
}

pub fn tenant_secret_namespace(id: &TenantId) -> String {
    format!("{TENANT_SECRET_NAMESPACE_PREFIX}{}", id.0)
}

pub fn tenant_topic(id: &TenantId, topic: &Topic) -> Result<Topic, LogError> {
    validate_tenant_id(&id.0).map_err(LogError::InvalidTopic)?;
    let prefix = tenant_event_topic_prefix(id);
    if topic.as_str().starts_with(&prefix) {
        return Ok(topic.clone());
    }
    if topic.as_str().starts_with(TENANT_EVENT_TOPIC_PREFIX) {
        return Err(LogError::InvalidTopic(format!(
            "topic '{}' is outside tenant scope '{}'",
            topic.as_str(),
            id.0
        )));
    }
    Topic::new(format!("{prefix}{}", topic.as_str()))
}

pub fn validate_tenant_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("tenant id cannot be empty".to_string());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!(
            "tenant id '{id}' contains unsupported characters; use ASCII letters, numbers, '_' or '-'"
        ));
    }
    Ok(())
}

fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir
        .join(TENANT_REGISTRY_DIR)
        .join(TENANT_REGISTRY_FILE)
}

fn write_file_replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("registry"),
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&tmp_path, contents)?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    Ok(())
}

fn generate_api_key(id: &str) -> String {
    let random: [u8; 32] = rand::random();
    format!("harn_tenant_{id}_{}", hex::encode(random))
}

fn api_key_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn api_key_prefix(value: &str) -> String {
    value.chars().take(18).collect()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::event_log::{EventLog, MemoryEventLog};

    #[tokio::test]
    async fn tenant_event_log_enforces_topic_prefix() {
        let inner = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
        let scope =
            TenantScope::new(TenantId::new("tenant-a"), std::env::temp_dir()).expect("scope");
        let tenant_log = Arc::new(TenantEventLog::new(inner.clone(), scope));
        let base = Topic::new("trigger.outbox").unwrap();

        tenant_log
            .append(&base, LogEvent::new("ok", serde_json::json!({"n": 1})))
            .await
            .unwrap();

        let scoped = Topic::new("tenant.tenant-a.trigger.outbox").unwrap();
        assert_eq!(inner.read_range(&scoped, None, 10).await.unwrap().len(), 1);
        let other = Topic::new("tenant.tenant-b.trigger.outbox").unwrap();
        assert!(tenant_log
            .append(&other, LogEvent::new("bad", serde_json::json!({})))
            .await
            .is_err());
    }

    struct MemorySecretProvider {
        namespace: String,
        values: Mutex<BTreeMap<SecretId, SecretBytes>>,
    }

    #[async_trait]
    impl SecretProvider for MemorySecretProvider {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            self.values
                .lock()
                .expect("secret map")
                .get(id)
                .map(SecretBytes::reborrow)
                .ok_or_else(|| SecretError::NotFound {
                    provider: self.namespace.clone(),
                    id: id.clone(),
                })
        }

        async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
            self.values
                .lock()
                .expect("secret map")
                .insert(id.clone(), value);
            Ok(())
        }

        async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
            Err(SecretError::Unsupported {
                provider: self.namespace.clone(),
                operation: "rotate",
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            &self.namespace
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn tenant_secret_provider_rescopes_and_denies_cross_tenant_ids() {
        let inner = Arc::new(MemorySecretProvider {
            namespace: "global".to_string(),
            values: Mutex::new(BTreeMap::new()),
        });
        let scope =
            TenantScope::new(TenantId::new("tenant-a"), std::env::temp_dir()).expect("scope");
        let provider = TenantSecretProvider::new(inner.clone(), scope.clone());

        provider
            .put(
                &SecretId::new("github", "webhook"),
                SecretBytes::from("a-secret"),
            )
            .await
            .unwrap();

        let scoped_id = SecretId::new(scope.secret_namespace, "webhook");
        let value = inner.get(&scoped_id).await.unwrap();
        value.with_exposed(|bytes| assert_eq!(bytes, b"a-secret"));

        let cross = SecretId::new("harn.tenant.tenant-b", "webhook");
        assert!(provider.get(&cross).await.is_err());
    }

    #[test]
    fn tenant_store_save_replaces_registry_without_temp_leak() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = TenantStore::load(temp.path()).unwrap();
        store
            .create_tenant("tenant-a", TenantBudget::default())
            .unwrap();

        let registry = registry_path(temp.path());
        assert!(registry.is_file());
        let leaked_temp = std::fs::read_dir(registry.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!leaked_temp);
    }
}
