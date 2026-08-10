use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    emit_secret_access_event, RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta,
    SecretProvider, SecretVersion,
};

/// A process-local secret provider whose values are zeroized when dropped.
///
/// Hosts use this for session credentials that must not be written to a native
/// keyring or projected into the process environment. Clones share one store.
#[derive(Clone, Debug)]
pub struct MemorySecretProvider {
    provider: String,
    inner: Arc<Mutex<BTreeMap<(String, String), VersionedSecret>>>,
}

#[derive(Debug, Default)]
struct VersionedSecret {
    latest: Option<u64>,
    versions: BTreeMap<u64, SecretBytes>,
}

impl MemorySecretProvider {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn empty() -> Self {
        // Preserve the long-standing connector testkit label for callers that
        // used this constructor through its former re-export path.
        Self::new("connector-testkit")
    }

    pub fn with_secret(mut self, id: SecretId, value: impl AsRef<[u8]>) -> Self {
        self.insert(id, value);
        self
    }

    pub fn insert(&mut self, id: SecretId, value: impl AsRef<[u8]>) {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        insert_secret(&mut inner, id, SecretBytes::from(value.as_ref()));
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
            .ok_or_else(|| not_found(&self.provider, id))?;
        let version = match id.version {
            SecretVersion::Latest => secret.latest,
            SecretVersion::Exact(version) => Some(version),
        }
        .ok_or_else(|| not_found(&self.provider, id))?;
        let value = secret
            .versions
            .get(&version)
            .map(SecretBytes::reborrow)
            .ok_or_else(|| not_found(&self.provider, id))?;
        emit_secret_access_event("memory", id);
        Ok(value)
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        insert_secret(&mut inner, id.clone(), value);
        Ok(())
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        let mut inner = self.inner.lock().expect("memory secret provider poisoned");
        let secret = inner
            .entry((id.namespace.clone(), id.name.clone()))
            .or_default();
        let from_version = secret.latest;
        let to_version = from_version.unwrap_or(0) + 1;
        let value = from_version
            .and_then(|version| secret.versions.get(&version).map(SecretBytes::reborrow))
            .unwrap_or_else(|| SecretBytes::from(Vec::new()));
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
        Ok(self
            .snapshot()
            .into_iter()
            .filter(|meta| {
                meta.id.namespace == prefix.namespace && meta.id.name.starts_with(&prefix.name)
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
    value: SecretBytes,
) {
    let secret = inner.entry((id.namespace, id.name)).or_default();
    let version = match id.version {
        SecretVersion::Latest => secret.latest.unwrap_or(0) + 1,
        SecretVersion::Exact(version) => version,
    };
    secret.versions.insert(version, value);
    secret.latest = Some(secret.latest.map_or(version, |latest| latest.max(version)));
}

fn not_found(provider: &str, id: &SecretId) -> SecretError {
    SecretError::NotFound {
        provider: provider.to_string(),
        id: id.clone(),
    }
}
