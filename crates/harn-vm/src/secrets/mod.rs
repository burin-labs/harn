use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

mod env;
mod keyring;

pub use env::EnvSecretProvider;
pub use keyring::{KeyringSecretProvider, NativeKeyring, NativeKeyringError};

pub const DEFAULT_SECRET_PROVIDER_CHAIN: &str = "env,keyring";
pub const SECRET_PROVIDER_CHAIN_ENV: &str = "HARN_SECRET_PROVIDERS";
pub const SECRET_REF_SCHEME: &str = "harn-secret://";
pub const SECRET_REF_CHAIN_NAMESPACE: &str = "harn.provider_auth";
pub const CONNECTOR_OAUTH_TOKEN_SECRET_NAME: &str = "oauth-token";
pub const CONNECTOR_ACCESS_TOKEN_SECRET_NAME: &str = "access-token";
pub const CONNECTOR_REFRESH_TOKEN_SECRET_NAME: &str = "refresh-token";
const RUNTIME_PROVENANCE_SECRET_NAMESPACE: &str = "provenance";
const SCOPED_RUNTIME_PROVENANCE_SECRET_NAMESPACE: &str = "harn.provenance";

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

pub fn parse_secret_ref(raw: &str) -> Result<Option<SecretId>, SecretError> {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix(SECRET_REF_SCHEME) else {
        return Ok(None);
    };
    let (base, version) = match rest.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().map_err(|_| {
                SecretError::InvalidInput(format!(
                    "invalid secret reference version in '{trimmed}'"
                ))
            })?;
            (base, SecretVersion::Exact(version))
        }
        None => (rest, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/').ok_or_else(|| {
        SecretError::InvalidInput(format!(
            "invalid secret reference '{trimmed}': expected {SECRET_REF_SCHEME}<namespace>/<name>"
        ))
    })?;
    if namespace.trim().is_empty() || name.trim().is_empty() {
        return Err(SecretError::InvalidInput(format!(
            "invalid secret reference '{trimmed}': namespace and name must be non-empty"
        )));
    }
    Ok(Some(
        SecretId::new(namespace.trim(), name.trim()).with_version(version),
    ))
}

pub fn parse_secret_id(raw: &str) -> Result<SecretId, SecretError> {
    if let Some(id) = parse_secret_ref(raw)? {
        return Ok(id);
    }
    parse_secret_id_body(raw.trim(), raw)
}

fn parse_secret_id_body(body: &str, original: &str) -> Result<SecretId, SecretError> {
    let (base, version) = match body.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().map_err(|_| {
                SecretError::InvalidInput(format!("invalid secret id version in '{original}'"))
            })?;
            (base, SecretVersion::Exact(version))
        }
        None => (body, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/').ok_or_else(|| {
        SecretError::InvalidInput(format!(
            "invalid secret id '{original}': expected <namespace>/<name>"
        ))
    })?;
    if namespace.trim().is_empty() || name.trim().is_empty() {
        return Err(SecretError::InvalidInput(format!(
            "invalid secret id '{original}': namespace and name must be non-empty"
        )));
    }
    Ok(SecretId::new(namespace.trim(), name.trim()).with_version(version))
}

pub fn connector_oauth_token_id(provider: &str) -> SecretId {
    SecretId::new(provider, CONNECTOR_OAUTH_TOKEN_SECRET_NAME)
}

pub fn connector_access_token_id(provider: &str) -> SecretId {
    SecretId::new(provider, CONNECTOR_ACCESS_TOKEN_SECRET_NAME)
}

pub fn connector_refresh_token_id(provider: &str) -> SecretId {
    SecretId::new(provider, CONNECTOR_REFRESH_TOKEN_SECRET_NAME)
}

pub fn resolve_secret_ref_to_string(raw: &str) -> Result<Option<String>, SecretError> {
    let Some(id) = parse_secret_ref(raw)? else {
        return Ok(None);
    };
    let chain = configured_default_chain(SECRET_REF_CHAIN_NAMESPACE)?;
    let secret = futures::executor::block_on(chain.get(&id))?;
    let rendered = secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|error| {
                SecretError::InvalidInput(format!(
                    "secret reference '{id}' resolved to non-UTF-8 bytes: {error}"
                ))
            })
    })?;
    Ok(Some(rendered))
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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretScope {
    Tenant { id: Option<String> },
    Workspace { id: String },
    System,
    Custom { kind: String, id: Option<String> },
}

impl Default for SecretScope {
    fn default() -> Self {
        Self::Tenant { id: None }
    }
}

impl SecretScope {
    pub fn tenant(id: Option<String>) -> Self {
        Self::Tenant { id }
    }

    pub fn workspace(id: impl Into<String>) -> Self {
        Self::Workspace { id: id.into() }
    }

    pub fn system() -> Self {
        Self::System
    }

    pub fn custom(kind: impl Into<String>, id: Option<String>) -> Self {
        Self::Custom {
            kind: kind.into(),
            id,
        }
    }

    pub fn namespace(&self) -> String {
        match self {
            Self::Tenant { id: Some(id) } if !id.is_empty() => format!("harn.tenant.{id}"),
            Self::Tenant { .. } => "harn.tenant".to_string(),
            Self::Workspace { id } => format!("harn.workspace.{id}"),
            Self::System => "harn.system".to_string(),
            Self::Custom { kind, id: Some(id) } if !id.is_empty() => {
                format!("harn.{kind}.{id}")
            }
            Self::Custom { kind, .. } => format!("harn.{kind}"),
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Self::Tenant { .. } => "tenant",
            Self::Workspace { .. } => "workspace",
            Self::System => "system",
            Self::Custom { kind, .. } => kind.as_str(),
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Tenant { id } | Self::Custom { id, .. } => id.as_deref(),
            Self::Workspace { id } => Some(id.as_str()),
            Self::System => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretWriteOptions {
    pub ttl: Option<Duration>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretRotationOptions {
    pub grace: Option<Duration>,
    pub ttl: Option<Duration>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretAuditContext {
    pub request_id: Option<String>,
    pub actor_subject: Option<String>,
    pub actor_kind: Option<String>,
}

#[derive(Debug)]
pub struct SecretReadRequest {
    pub id: SecretId,
    pub scope: SecretScope,
    pub audit: SecretAuditContext,
}

#[derive(Debug)]
pub struct SecretDeleteRequest {
    pub id: SecretId,
    pub scope: SecretScope,
    pub audit: SecretAuditContext,
}

#[derive(Debug)]
pub struct SecretWriteRequest {
    pub id: SecretId,
    pub scope: SecretScope,
    pub value: SecretBytes,
    pub options: SecretWriteOptions,
    pub audit: SecretAuditContext,
}

#[derive(Debug)]
pub struct SecretRotateRequest {
    pub id: SecretId,
    pub scope: SecretScope,
    pub value: SecretBytes,
    pub options: SecretRotationOptions,
    pub audit: SecretAuditContext,
}

#[derive(Debug)]
pub struct SecretLeaseRequest {
    pub id: SecretId,
    pub scope: SecretScope,
    pub duration: Duration,
    pub audit: SecretAuditContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretWriteReceipt {
    pub provider: String,
    pub id: SecretId,
    pub scope: SecretScope,
    pub version: Option<u64>,
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretRotationReceipt {
    pub provider: String,
    pub id: SecretId,
    pub scope: SecretScope,
    pub from_version: Option<u64>,
    pub to_version: Option<u64>,
    pub grace_until_unix_ms: Option<i64>,
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Debug)]
pub struct SecretLeaseGrant {
    pub provider: String,
    pub id: SecretId,
    pub scope: SecretScope,
    pub lease_id: String,
    pub value: SecretBytes,
    pub expires_at_unix_ms: i64,
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
    AccessDenied {
        operation: String,
        id: SecretId,
        message: String,
    },
    InvalidConfig(String),
    InvalidInput(String),
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
            Self::AccessDenied {
                operation,
                id,
                message,
            } => write!(f, "secret {operation} denied for '{id}': {message}"),
            Self::InvalidConfig(message) => write!(f, "{message}"),
            Self::InvalidInput(message) => write!(f, "{message}"),
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

    async fn read_scoped(&self, request: SecretReadRequest) -> Result<SecretBytes, SecretError> {
        ensure_scoped_secret_access_allowed("read", &request.id)?;
        self.get(&request.id).await
    }

    async fn write_scoped(
        &self,
        request: SecretWriteRequest,
    ) -> Result<SecretWriteReceipt, SecretError> {
        ensure_scoped_secret_access_allowed("write", &request.id)?;
        if request.options.ttl.is_some() {
            return Err(SecretError::Unsupported {
                provider: self.namespace().to_string(),
                operation: "write_ttl",
            });
        }
        self.put(&request.id, request.value).await?;
        Ok(SecretWriteReceipt {
            provider: self.namespace().to_string(),
            id: request.id,
            scope: request.scope,
            version: None,
            expires_at_unix_ms: None,
        })
    }

    async fn delete_scoped(&self, request: SecretDeleteRequest) -> Result<(), SecretError> {
        ensure_scoped_secret_access_allowed("delete", &request.id)?;
        let _ = request;
        Err(SecretError::Unsupported {
            provider: self.namespace().to_string(),
            operation: "delete",
        })
    }

    async fn rotate_scoped(
        &self,
        request: SecretRotateRequest,
    ) -> Result<SecretRotationReceipt, SecretError> {
        ensure_scoped_secret_access_allowed("rotate", &request.id)?;
        let _ = request;
        Err(SecretError::Unsupported {
            provider: self.namespace().to_string(),
            operation: "rotate_to",
        })
    }

    async fn lease_scoped(
        &self,
        request: SecretLeaseRequest,
    ) -> Result<SecretLeaseGrant, SecretError> {
        ensure_scoped_secret_access_allowed("lease", &request.id)?;
        let _ = request;
        Err(SecretError::Unsupported {
            provider: self.namespace().to_string(),
            operation: "lease",
        })
    }

    fn namespace(&self) -> &str;
    fn supports_versions(&self) -> bool;
}

pub fn ensure_scoped_secret_access_allowed(
    operation: impl Into<String>,
    id: &SecretId,
) -> Result<(), SecretError> {
    if is_runtime_reserved_secret_namespace(&id.namespace) {
        return Err(SecretError::AccessDenied {
            operation: operation.into(),
            id: id.clone(),
            message: format!(
                "namespace `{}` is reserved for Harn runtime provenance signing and is not accessible through agent-scoped secret APIs",
                id.namespace
            ),
        });
    }
    Ok(())
}

pub fn is_runtime_reserved_secret_namespace(namespace: &str) -> bool {
    let namespace = namespace.trim_matches('.');
    namespace == RUNTIME_PROVENANCE_SECRET_NAMESPACE
        || namespace == SCOPED_RUNTIME_PROVENANCE_SECRET_NAMESPACE
        || namespace
            .strip_prefix(SCOPED_RUNTIME_PROVENANCE_SECRET_NAMESPACE)
            .is_some_and(|suffix| suffix.starts_with('.'))
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

    async fn delete_scoped(&self, request: SecretDeleteRequest) -> Result<(), SecretError> {
        ensure_scoped_secret_access_allowed("delete", &request.id)?;
        if self.providers.is_empty() {
            return Err(SecretError::NoProviders {
                namespace: self.namespace.clone(),
            });
        }

        // Delete from every backend that supports it so a stale copy in one
        // provider can't resurrect a credential the caller asked to revoke.
        // A `NotFound` counts as success — the secret is already gone there.
        let mut errors = Vec::new();
        let mut any_ok = false;
        for provider in &self.providers {
            match provider
                .delete_scoped(SecretDeleteRequest {
                    id: request.id.clone(),
                    scope: request.scope.clone(),
                    audit: request.audit.clone(),
                })
                .await
            {
                Ok(()) | Err(SecretError::NotFound { .. }) => any_ok = true,
                Err(error) => errors.push(error),
            }
        }

        if any_ok {
            Ok(())
        } else {
            Err(SecretError::All(errors))
        }
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
        timestamp: crate::orchestration::now_unix_seconds_text(),
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;

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
    fn parse_secret_ref_accepts_namespace_name_and_version() {
        let id = parse_secret_ref("harn-secret://provider/anthropic-api-key@7")
            .expect("parse should succeed")
            .expect("secret ref should be detected");
        assert_eq!(id.namespace, "provider");
        assert_eq!(id.name, "anthropic-api-key");
        assert_eq!(id.version, SecretVersion::Exact(7));
    }

    #[test]
    fn parse_secret_ref_ignores_non_refs_and_rejects_malformed_refs() {
        assert!(parse_secret_ref("plain-api-key")
            .expect("non-ref should be accepted")
            .is_none());
        assert!(parse_secret_ref("harn-secret://missing-name")
            .expect_err("missing slash should fail")
            .to_string()
            .contains("invalid secret reference"));
    }

    #[test]
    fn parse_secret_id_accepts_canonical_and_ref_forms() {
        let canonical = parse_secret_id("google_workspace/access-token@2").expect("canonical id");
        assert_eq!(canonical.namespace, "google_workspace");
        assert_eq!(canonical.name, "access-token");
        assert_eq!(canonical.version, SecretVersion::Exact(2));

        let reference =
            parse_secret_id("harn-secret://google_workspace/refresh-token").expect("ref id");
        assert_eq!(reference, connector_refresh_token_id("google_workspace"));

        assert_eq!(
            connector_oauth_token_id("google_workspace").name,
            CONNECTOR_OAUTH_TOKEN_SECRET_NAME
        );
        assert_eq!(
            connector_access_token_id("google_workspace").name,
            CONNECTOR_ACCESS_TOKEN_SECRET_NAME
        );
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
    async fn scoped_secret_access_denies_runtime_reserved_namespaces() {
        let chain = ChainSecretProvider::new(
            "harn/test",
            vec![Arc::new(FakeProvider::new("unused", Vec::new()))],
        );

        for namespace in ["provenance", "harn.provenance", "harn.provenance.agent"] {
            let id = SecretId::new(namespace, "harn-cli.ed25519.seed");
            let error = chain
                .read_scoped(SecretReadRequest {
                    id: id.clone(),
                    scope: SecretScope::custom("provenance", None),
                    audit: SecretAuditContext::default(),
                })
                .await
                .expect_err("reserved namespace should be denied before backend access");
            match error {
                SecretError::AccessDenied {
                    operation,
                    id: denied_id,
                    message,
                } => {
                    assert_eq!(operation, "read");
                    assert_eq!(denied_id, id);
                    assert!(message.contains("reserved for Harn runtime provenance signing"));
                }
                other => panic!("expected access-denied error, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn keyring_provider_round_trips_and_zeroes_on_drop() {
        let provider = KeyringSecretProvider::with_store(
            "harn.test",
            keyring_core::mock::Store::new().unwrap(),
        );
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
