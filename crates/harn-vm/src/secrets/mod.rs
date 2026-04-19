use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

mod env;
mod keyring;

pub use env::EnvSecretProvider;
pub use keyring::KeyringSecretProvider;

pub const DEFAULT_SECRET_PROVIDER_CHAIN: &str = "env,keyring";
pub const SECRET_PROVIDER_CHAIN_ENV: &str = "HARN_SECRET_PROVIDERS";

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum SecretVersion {
    #[default]
    Latest,
    Exact(u64),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SecretId {
    pub namespace: String,
    pub name: String,
    #[serde(default)]
    pub version: SecretVersion,
}

impl SecretId {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            version: SecretVersion::Latest,
        }
    }

    pub fn with_version(mut self, version: SecretVersion) -> Self {
        self.version = version;
        self
    }
}

impl fmt::Display for SecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.namespace.is_empty() {
            write!(f, "{}", self.name)?;
        } else {
            write!(f, "{}/{}", self.namespace, self.name)?;
        }
        match self.version {
            SecretVersion::Latest => Ok(()),
            SecretVersion::Exact(version) => write!(f, "@{version}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub id: SecretId,
    pub provider: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RotationHandle {
    pub provider: String,
    pub id: SecretId,
    pub from_version: Option<u64>,
    pub to_version: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretError {
    NotFound {
        provider: String,
        id: SecretId,
    },
    Unsupported {
        provider: String,
        operation: &'static str,
    },
    Backend {
        provider: String,
        message: String,
    },
    InvalidConfig(String),
    NoProviders {
        namespace: String,
    },
    All(Vec<SecretError>),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { provider, id } => {
                write!(f, "{provider}: secret '{id}' not found")
            }
            Self::Unsupported {
                provider,
                operation,
            } => write!(f, "{provider}: operation '{operation}' is unsupported"),
            Self::Backend { provider, message } => write!(f, "{provider}: {message}"),
            Self::InvalidConfig(message) => write!(f, "{message}"),
            Self::NoProviders { namespace } => {
                write!(
                    f,
                    "no secret providers configured for namespace '{namespace}'"
                )
            }
            Self::All(errors) => {
                let rendered = errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                write!(f, "all secret providers failed: {rendered}")
            }
        }
    }
}

impl std::error::Error for SecretError {}

#[derive(Default)]
struct SecretBuffer {
    bytes: Vec<u8>,
    #[cfg(test)]
    drop_probe: Option<std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>>,
}

impl SecretBuffer {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            #[cfg(test)]
            drop_probe: None,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    fn attach_drop_probe(&mut self, probe: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>) {
        self.drop_probe = Some(probe);
    }
}

impl std::ops::Deref for SecretBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl Zeroize for SecretBuffer {
    fn zeroize(&mut self) {
        self.bytes.zeroize();
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(probe) = &self.drop_probe {
            *probe.lock().expect("drop probe poisoned") = Some(self.bytes.clone());
        }
    }
}

pub struct SecretBytes(Zeroizing<SecretBuffer>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(SecretBuffer::new(bytes)))
    }

    pub fn len(&self) -> usize {
        self.0.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.as_slice().is_empty()
    }

    pub fn with_exposed<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(self.0.as_slice())
    }

    pub fn reborrow(&self) -> Self {
        self.with_exposed(|bytes| Self::new(bytes.to_vec()))
    }

    #[cfg(test)]
    pub(crate) fn attach_drop_probe(
        &mut self,
        probe: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    ) {
        self.0.attach_drop_probe(probe);
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes {{ redacted: {} bytes }}", self.len())
    }
}

impl Serialize for SecretBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("<redacted:{} bytes>", self.len()))
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(value: Vec<u8>) -> Self {
        Self::new(value)
    }
}

impl From<String> for SecretBytes {
    fn from(value: String) -> Self {
        Self::new(value.into_bytes())
    }
}

impl From<&str> for SecretBytes {
    fn from(value: &str) -> Self {
        Self::new(value.as_bytes().to_vec())
    }
}

impl From<&[u8]> for SecretBytes {
    fn from(value: &[u8]) -> Self {
        Self::new(value.to_vec())
    }
}

#[async_trait]
pub trait SecretProvider: Send + Sync {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError>;
    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError>;
    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError>;
    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError>;

    fn namespace(&self) -> &str;
    fn supports_versions(&self) -> bool;
}

pub struct ChainSecretProvider {
    namespace: String,
    providers: Vec<Arc<dyn SecretProvider>>,
}

impl ChainSecretProvider {
    pub fn new(namespace: impl Into<String>, providers: Vec<Arc<dyn SecretProvider>>) -> Self {
        Self {
            namespace: namespace.into(),
            providers,
        }
    }

    pub fn providers(&self) -> &[Arc<dyn SecretProvider>] {
        &self.providers
    }
}

#[async_trait]
impl SecretProvider for ChainSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        if self.providers.is_empty() {
            return Err(SecretError::NoProviders {
                namespace: self.namespace.clone(),
            });
        }

        let mut errors = Vec::new();
        for provider in &self.providers {
            match provider.get(id).await {
                Ok(secret) => return Ok(secret),
                Err(error) => errors.push(error),
            }
        }

        Err(SecretError::All(errors))
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        if self.providers.is_empty() {
            return Err(SecretError::NoProviders {
                namespace: self.namespace.clone(),
            });
        }

        let mut last_value = Some(value);
        let mut errors = Vec::new();
        for (index, provider) in self.providers.iter().enumerate() {
            let attempt_value = if index + 1 == self.providers.len() {
                last_value
                    .take()
                    .expect("final secret write attempt missing value")
            } else {
                last_value
                    .as_ref()
                    .expect("intermediate secret write attempt missing value")
                    .reborrow()
            };
            match provider.put(id, attempt_value).await {
                Ok(()) => return Ok(()),
                Err(error) => errors.push(error),
            }
        }

        Err(SecretError::All(errors))
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        if self.providers.is_empty() {
            return Err(SecretError::NoProviders {
                namespace: self.namespace.clone(),
            });
        }

        let mut errors = Vec::new();
        for provider in &self.providers {
            match provider.rotate(id).await {
                Ok(handle) => return Ok(handle),
                Err(error) => errors.push(error),
            }
        }

        Err(SecretError::All(errors))
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        if self.providers.is_empty() {
            return Err(SecretError::NoProviders {
                namespace: self.namespace.clone(),
            });
        }

        let mut errors = Vec::new();
        let mut merged = BTreeMap::<SecretId, SecretMeta>::new();
        for provider in &self.providers {
            match provider.list(prefix).await {
                Ok(items) => {
                    for item in items {
                        merged.entry(item.id.clone()).or_insert(item);
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        if merged.is_empty() && !errors.is_empty() {
            return Err(SecretError::All(errors));
        }

        Ok(merged.into_values().collect())
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.supports_versions())
    }
}

pub fn configured_default_chain(
    namespace: impl Into<String>,
) -> Result<ChainSecretProvider, SecretError> {
    let namespace = namespace.into();
    let configured = std::env::var(SECRET_PROVIDER_CHAIN_ENV)
        .unwrap_or_else(|_| DEFAULT_SECRET_PROVIDER_CHAIN.to_string());
    let mut providers: Vec<Arc<dyn SecretProvider>> = Vec::new();

    for raw_name in configured.split(',') {
        let provider_name = raw_name.trim();
        if provider_name.is_empty() {
            continue;
        }
        match provider_name {
            "env" => providers.push(Arc::new(EnvSecretProvider::new(namespace.clone()))),
            "keyring" => providers.push(Arc::new(KeyringSecretProvider::new(namespace.clone()))),
            other => {
                return Err(SecretError::InvalidConfig(format!(
                    "unsupported secret provider '{other}' in {SECRET_PROVIDER_CHAIN_ENV}; expected a comma-separated list of env,keyring"
                )))
            }
        }
    }

    Ok(ChainSecretProvider::new(namespace, providers))
}

pub(crate) fn emit_secret_access_event(provider: &str, id: &SecretId) {
    #[derive(Serialize)]
    struct SecretAccessEvent<'a> {
        topic: &'a str,
        provider: &'a str,
        id: &'a SecretId,
        caller_span_id: Option<u64>,
        mutation_session_id: Option<String>,
        timestamp: String,
    }

    let event = SecretAccessEvent {
        topic: "audit.secret_access",
        provider,
        id,
        caller_span_id: crate::tracing::current_span_id(),
        mutation_session_id: crate::orchestration::current_mutation_session()
            .map(|session| session.session_id),
        timestamp: crate::orchestration::now_rfc3339(),
    };
    let metadata = serde_json::to_value(event)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .map(|object| object.into_iter().collect::<BTreeMap<_, _>>())
        .unwrap_or_default();
    crate::events::log_info_meta("secret.audit", "secret accessed", metadata);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, Once};

    use async_trait::async_trait;

    use super::*;

    fn install_mock_keyring() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            ::keyring::set_default_credential_builder(::keyring::mock::default_credential_builder());
        });
    }

    struct FakeProvider {
        namespace: String,
        result: Mutex<Vec<Result<SecretBytes, SecretError>>>,
    }

    impl FakeProvider {
        fn new(
            namespace: impl Into<String>,
            result: Vec<Result<SecretBytes, SecretError>>,
        ) -> Self {
            Self {
                namespace: namespace.into(),
                result: Mutex::new(result),
            }
        }
    }

    #[async_trait]
    impl SecretProvider for FakeProvider {
        async fn get(&self, _id: &SecretId) -> Result<SecretBytes, SecretError> {
            self.result
                .lock()
                .expect("fake provider poisoned")
                .remove(0)
        }

        async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Err(SecretError::Unsupported {
                provider: self.namespace.clone(),
                operation: "put",
            })
        }

        async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
            Err(SecretError::Unsupported {
                provider: self.namespace.clone(),
                operation: "rotate",
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Err(SecretError::Unsupported {
                provider: self.namespace.clone(),
                operation: "list",
            })
        }

        fn namespace(&self) -> &str {
            &self.namespace
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    #[test]
    fn secret_bytes_debug_is_redacted() {
        let secret = SecretBytes::from("abcd");
        assert_eq!(format!("{secret:?}"), "SecretBytes { redacted: 4 bytes }");
    }

    #[test]
    fn secret_bytes_zeroes_on_drop() {
        let probe = Arc::new(Mutex::new(None));
        let mut secret = SecretBytes::from("super-secret");
        secret.attach_drop_probe(probe.clone());
        drop(secret);

        let dropped = probe
            .lock()
            .expect("drop probe poisoned")
            .clone()
            .expect("probe should capture bytes");
        assert!(dropped.iter().all(|byte| *byte == 0));
    }

    #[tokio::test]
    async fn chain_secret_provider_falls_through_to_next_hit() {
        let id = SecretId::new("harn.test", "api-key");
        let first = Arc::new(FakeProvider::new(
            "first",
            vec![Err(SecretError::NotFound {
                provider: "first".to_string(),
                id: id.clone(),
            })],
        ));
        let second = Arc::new(FakeProvider::new(
            "second",
            vec![Ok(SecretBytes::from("value"))],
        ));
        let chain = ChainSecretProvider::new("harn/test", vec![first, second]);

        let secret = chain.get(&id).await.expect("chain should resolve");
        let exposed = secret.with_exposed(|bytes| bytes.to_vec());
        assert_eq!(exposed, b"value");
    }

    #[tokio::test]
    async fn chain_secret_provider_returns_all_errors_when_everything_fails() {
        let id = SecretId::new("harn.test", "missing");
        let first = Arc::new(FakeProvider::new(
            "first",
            vec![Err(SecretError::NotFound {
                provider: "first".to_string(),
                id: id.clone(),
            })],
        ));
        let second = Arc::new(FakeProvider::new(
            "second",
            vec![Err(SecretError::Backend {
                provider: "second".to_string(),
                message: "boom".to_string(),
            })],
        ));
        let chain = ChainSecretProvider::new("harn/test", vec![first, second]);

        let error = chain.get(&id).await.expect_err("chain should fail");
        match error {
            SecretError::All(errors) => {
                assert_eq!(errors.len(), 2);
                assert!(matches!(errors[0], SecretError::NotFound { .. }));
                assert!(matches!(errors[1], SecretError::Backend { .. }));
            }
            other => panic!("expected aggregated errors, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn keyring_provider_round_trips_and_zeroes_on_drop() {
        install_mock_keyring();

        let provider = KeyringSecretProvider::new("harn.test");
        let id = SecretId::new("", format!("mock-{}", uuid::Uuid::now_v7()));
        provider
            .put(&id, SecretBytes::from("round-trip-secret"))
            .await
            .expect("mock keyring write should succeed");

        let probe = Arc::new(Mutex::new(None));
        let mut secret = provider
            .get(&id)
            .await
            .expect("mock keyring read should succeed");
        assert_eq!(
            secret.with_exposed(|bytes| bytes.to_vec()),
            b"round-trip-secret"
        );
        secret.attach_drop_probe(probe.clone());
        drop(secret);

        let dropped = probe
            .lock()
            .expect("drop probe poisoned")
            .clone()
            .expect("probe should capture bytes");
        assert!(dropped.iter().all(|byte| *byte == 0));

        provider
            .delete(&id)
            .await
            .expect("mock keyring delete should succeed");
    }
}
