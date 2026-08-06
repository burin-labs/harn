use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use keyring_core::{CredentialStore, Entry, Error as KeyringError};

use super::{
    emit_secret_access_event, ensure_scoped_secret_access_allowed, RotationHandle, SecretBytes,
    SecretDeleteRequest, SecretError, SecretId, SecretMeta, SecretProvider,
};

static PLATFORM_STORE: OnceLock<Arc<CredentialStore>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum NativeKeyringError {
    #[error(transparent)]
    Keyring(#[from] KeyringError),
    #[error("credential contains invalid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

/// Cross-platform access to the operating system's native credential store.
///
/// The keyring ecosystem owns the platform mappings and secure-storage API
/// calls. Harn only supplies the stable `(service, user)` namespace used by its
/// runtime and host capability.
pub struct NativeKeyring {
    service: String,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    store: Option<Arc<CredentialStore>>,
}

impl fmt::Debug for NativeKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeKeyring")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl NativeKeyring {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            entries: Mutex::new(HashMap::new()),
            store: None,
        }
    }

    #[cfg(test)]
    fn with_store(service: impl Into<String>, store: Arc<CredentialStore>) -> Self {
        Self {
            service: service.into(),
            entries: Mutex::new(HashMap::new()),
            store: Some(store),
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn get(&self, user: &str) -> Result<Option<Vec<u8>>, NativeKeyringError> {
        match self.entry(user)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn get_string(&self, user: &str) -> Result<Option<String>, NativeKeyringError> {
        self.get(user)?
            .map(String::from_utf8)
            .transpose()
            .map_err(Into::into)
    }

    pub fn set(&self, user: &str, secret: &[u8]) -> Result<(), NativeKeyringError> {
        self.entry(user)?.set_secret(secret).map_err(Into::into)
    }

    pub fn set_string(&self, user: &str, secret: &str) -> Result<(), NativeKeyringError> {
        self.set(user, secret.as_bytes())
    }

    pub fn delete(&self, user: &str) -> Result<bool, NativeKeyringError> {
        match self.entry(user)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn list(&self) -> Result<Vec<String>, NativeKeyringError> {
        let store = self.store()?;
        #[cfg(target_os = "windows")]
        let pattern = format!(r"\.{}$", regex::escape(&self.service));
        #[cfg(target_os = "windows")]
        let spec = HashMap::from([("pattern", pattern.as_str())]);
        #[cfg(not(target_os = "windows"))]
        let spec = HashMap::from([("service", self.service.as_str())]);
        let mut users = store
            .search(&spec)?
            .into_iter()
            .filter_map(|entry| entry.get_specifiers())
            .filter_map(|(service, user)| (service == self.service).then_some(user))
            .collect::<Vec<_>>();
        users.sort();
        users.dedup();
        Ok(users)
    }

    pub fn healthcheck(&self) -> Result<String, NativeKeyringError> {
        let _ = self.get("__harn_probe__")?;
        Ok(format!("service '{}' reachable", self.service))
    }

    fn entry(&self, user: &str) -> Result<Arc<Entry>, NativeKeyringError> {
        let mut entries = self.entries.lock().expect("keyring cache poisoned");
        if let Some(entry) = entries.get(user) {
            return Ok(entry.clone());
        }
        let entry = Arc::new(self.store()?.build(self.service(), user, None)?);
        entries.insert(user.to_string(), entry.clone());
        Ok(entry)
    }

    fn store(&self) -> Result<Arc<CredentialStore>, NativeKeyringError> {
        if let Some(store) = &self.store {
            return Ok(store.clone());
        }
        if let Some(store) = PLATFORM_STORE.get() {
            return Ok(store.clone());
        }
        let store = platform_store()?;
        let _ = PLATFORM_STORE.set(store.clone());
        Ok(PLATFORM_STORE.get().cloned().unwrap_or(store))
    }
}

fn platform_store() -> Result<Arc<CredentialStore>, NativeKeyringError> {
    #[cfg(target_os = "macos")]
    {
        let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
        return Ok(store);
    }
    #[cfg(target_os = "ios")]
    {
        let store: Arc<CredentialStore> = apple_native_keyring_store::protected::Store::new()?;
        return Ok(store);
    }
    #[cfg(target_os = "windows")]
    {
        let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
        return Ok(store);
    }
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    {
        let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
        return Ok(store);
    }
    #[allow(unreachable_code)]
    Err(KeyringError::NoDefaultStore.into())
}

#[derive(Debug)]
pub struct KeyringSecretProvider {
    keyring: NativeKeyring,
}

impl KeyringSecretProvider {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            keyring: NativeKeyring::new(namespace),
        }
    }

    #[cfg(test)]
    pub(super) fn with_store(namespace: impl Into<String>, store: Arc<CredentialStore>) -> Self {
        Self {
            keyring: NativeKeyring::with_store(namespace, store),
        }
    }

    pub fn service(&self) -> &str {
        self.keyring.service()
    }

    pub async fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        self.keyring
            .delete(&account_name(id))
            .map(|_| ())
            .map_err(|error| backend_error("delete", error))
    }

    pub fn healthcheck(&self) -> Result<String, SecretError> {
        self.keyring
            .healthcheck()
            .map_err(|error| backend_error("access", error))
    }
}

#[async_trait]
impl SecretProvider for KeyringSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        match self
            .keyring
            .get(&account_name(id))
            .map_err(|error| backend_error("read", error))?
        {
            Some(bytes) => {
                emit_secret_access_event("keyring", id);
                Ok(SecretBytes::from(bytes))
            }
            None => Err(SecretError::NotFound {
                provider: "keyring".to_string(),
                id: id.clone(),
            }),
        }
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        value.with_exposed(|bytes| {
            self.keyring
                .set(&account_name(id), bytes)
                .map_err(|error| backend_error("store", error))
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: "keyring".to_string(),
            operation: "rotate",
        })
    }

    async fn delete_scoped(&self, request: SecretDeleteRequest) -> Result<(), SecretError> {
        ensure_scoped_secret_access_allowed("delete", &request.id)?;
        self.delete(&request.id).await
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Err(SecretError::Unsupported {
            provider: "keyring".to_string(),
            operation: "list",
        })
    }

    fn namespace(&self) -> &str {
        self.service()
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

fn backend_error(operation: &str, error: NativeKeyringError) -> SecretError {
    SecretError::Backend {
        provider: "keyring".to_string(),
        message: format!("failed to {operation} keyring credential: {error}"),
    }
}

fn account_name(id: &SecretId) -> String {
    let mut account = String::new();
    if !id.namespace.is_empty() {
        account.push_str(&sanitize_component(&id.namespace));
        account.push('/');
    }
    account.push_str(&sanitize_component(&id.name));
    match id.version {
        super::SecretVersion::Latest => {}
        super::SecretVersion::Exact(version) => {
            account.push('#');
            account.push('v');
            account.push_str(&version.to_string());
        }
    }
    account
}

fn sanitize_component(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "_".to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_keyring_round_trips_and_lists_service_users() {
        let keyring = NativeKeyring::with_store(
            "harn.native-test",
            keyring_core::mock::Store::new().unwrap(),
        );
        keyring.set_string("alpha", "one").unwrap();
        keyring.set_string("beta", "two").unwrap();

        assert_eq!(keyring.get_string("alpha").unwrap().as_deref(), Some("one"));
        assert_eq!(keyring.list().unwrap(), vec!["alpha", "beta"]);
        assert!(keyring.delete("alpha").unwrap());
        assert!(!keyring.delete("alpha").unwrap());
    }
}
