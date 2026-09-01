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
    #[error("credential store verification failed: {0}")]
    Verification(&'static str),
}

/// A stable reason that the operating-system credential store cannot service
/// requests in the current process. Callers may use this to distinguish an
/// unavailable desktop session from an operational keyring failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeKeyringUnavailable {
    /// No platform adapter was linked into this product build.
    AdapterMissing,
    /// The platform store exists but cannot be accessed by this process.
    StorageInaccessible,
    /// The operation requires desktop interaction that this process cannot show.
    InteractionRequired,
}

impl NativeKeyringUnavailable {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdapterMissing => "adapter_missing",
            Self::StorageInaccessible => "storage_inaccessible",
            Self::InteractionRequired => "interaction_required",
        }
    }
}

impl fmt::Display for NativeKeyringUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl NativeKeyringError {
    /// Classify errors that mean the native store is unavailable to this
    /// process. Other platform failures remain operational errors.
    pub fn unavailable_reason(&self) -> Option<NativeKeyringUnavailable> {
        match self {
            Self::Keyring(KeyringError::NoDefaultStore) => {
                Some(NativeKeyringUnavailable::AdapterMissing)
            }
            Self::Keyring(KeyringError::NoStorageAccess(_)) => {
                Some(NativeKeyringUnavailable::StorageInaccessible)
            }
            #[cfg(all(feature = "native-keyring", target_os = "macos"))]
            Self::Keyring(KeyringError::PlatformFailure(error))
                if error
                    .downcast_ref::<security_framework::base::Error>()
                    .is_some_and(|error| error.code() == -25308) =>
            {
                // errSecInteractionNotAllowed: common for SSH/headless agents
                // whose login keychain cannot present an unlock prompt.
                Some(NativeKeyringUnavailable::InteractionRequired)
            }
            _ => None,
        }
    }
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

    /// Whether an entry exists for `user`, without reading its secret.
    ///
    /// [`Self::list`] searches item *attributes*; it never asks the platform
    /// for item data. On macOS that is the whole difference: reading data
    /// raises a Keychain access dialog for any binary not on the item's ACL,
    /// while enumerating attributes raises none. A caller that only needs to
    /// know whether a credential exists pays nothing here.
    pub fn contains(&self, user: &str) -> Result<bool, NativeKeyringError> {
        Ok(self.list()?.iter().any(|entry| entry == user))
    }

    pub fn healthcheck(&self) -> Result<String, NativeKeyringError> {
        let user = format!("__harn_probe__:{}", uuid::Uuid::now_v7().simple());
        self.healthcheck_with_user(&user)
    }

    fn healthcheck_with_user(&self, user: &str) -> Result<String, NativeKeyringError> {
        const PROBE_VALUE: &[u8] = b"harn-keyring-healthcheck";

        self.set(user, PROBE_VALUE)?;
        let read_result = self.get(user);
        let delete_result = self.delete(user);

        if !delete_result? {
            return Err(NativeKeyringError::Verification(
                "stored probe could not be deleted",
            ));
        }
        match read_result? {
            Some(value) if value == PROBE_VALUE => Ok(format!(
                "service '{}' passed write, read, and delete checks",
                self.service
            )),
            Some(_) => Err(NativeKeyringError::Verification(
                "read returned a different value",
            )),
            None => Err(NativeKeyringError::Verification(
                "stored probe could not be read",
            )),
        }
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
    #[cfg(all(feature = "native-keyring", target_os = "macos"))]
    {
        let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
        return Ok(store);
    }
    #[cfg(all(feature = "native-keyring", target_os = "ios"))]
    {
        let store: Arc<CredentialStore> = apple_native_keyring_store::protected::Store::new()?;
        return Ok(store);
    }
    #[cfg(all(feature = "native-keyring", target_os = "windows"))]
    {
        let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
        return Ok(store);
    }
    #[cfg(all(
        feature = "native-keyring",
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

    /// Presence from the attribute-only search rather than an item read, so
    /// asking "does this credential exist" costs no Keychain access dialog.
    ///
    /// No `audit.secret_access` event is emitted: nothing read a secret. The
    /// event records value access, and firing it here would report reads that
    /// did not happen.
    async fn contains(&self, id: &SecretId) -> Result<bool, SecretError> {
        self.keyring
            .contains(&account_name(id))
            .map_err(|error| backend_error("read", error))
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

    #[test]
    fn healthcheck_proves_write_read_delete_and_leaves_no_probe() {
        let keyring = NativeKeyring::with_store(
            "harn.healthcheck-test",
            keyring_core::mock::Store::new().unwrap(),
        );

        let detail = keyring
            .healthcheck_with_user("__harn_probe__:test")
            .expect("writable mock keyring");

        assert!(detail.contains("passed write, read, and delete checks"));
        assert_eq!(keyring.get("__harn_probe__:test").unwrap(), None);
    }

    /// Presence must not read the secret.
    ///
    /// The falsifier is the point: the stored credential is rigged so that any
    /// *data* access fails. `contains` still answers, because it searches item
    /// attributes. Implement it as `get(...).is_ok()` and this test fails —
    /// which is exactly the regression that made one `connect status` raise a
    /// Keychain access dialog per stored secret (#7749).
    #[tokio::test]
    async fn presence_is_answered_without_reading_the_secret() {
        let store = keyring_core::mock::Store::new().unwrap();
        let credential_store: Arc<CredentialStore> = store;
        let keyring =
            NativeKeyring::with_store("harn.presence-test", Arc::clone(&credential_store));
        keyring.set_string("alpha/token", "super-secret").unwrap();

        // Rig the stored item so reading its data fails. On a real macOS
        // keychain this is the ACL dialog; here it is a hard error, which is
        // the observable stand-in.
        credential_store
            .build("harn.presence-test", "alpha/token", None)
            .unwrap()
            .as_any()
            .downcast_ref::<keyring_core::mock::Cred>()
            .unwrap()
            .set_error(KeyringError::Invalid(
                "reading this item's data is not allowed".to_string(),
                "value read attempted".to_string(),
            ));

        let provider = KeyringSecretProvider::with_store("harn.presence-test", credential_store);
        let present = SecretId::new("alpha", "token");

        assert!(
            provider.contains(&present).await.unwrap(),
            "presence must come from the attribute search, not a value read"
        );
        // Guard against passing for the wrong reason: the value read really is
        // broken, so a `get`-based implementation could not have returned true.
        assert!(provider.get(&present).await.is_err());
        assert!(!provider
            .contains(&SecretId::new("alpha", "absent"))
            .await
            .unwrap());
    }

    /// The control for the case above: the trait's default `contains` really
    /// is value-based.
    ///
    /// Together the two are the falsifier, without anyone having to edit the
    /// override and re-run. This one shows a provider that implements only
    /// `get` propagates a read failure out of `contains`; the keyring case
    /// shows the keyring provider answers `true` for an item whose `get`
    /// fails. No value-reading implementation can do both.
    #[tokio::test]
    async fn the_default_presence_implementation_reads_the_value() {
        struct UnreadableProvider;

        #[async_trait]
        impl SecretProvider for UnreadableProvider {
            async fn get(&self, _id: &SecretId) -> Result<SecretBytes, SecretError> {
                Err(SecretError::Backend {
                    provider: "fixture".to_string(),
                    message: "value read attempted".to_string(),
                })
            }

            async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
                unimplemented!("presence control never writes")
            }

            async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
                unimplemented!("presence control never rotates")
            }

            async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
                unimplemented!("presence control never lists")
            }

            fn namespace(&self) -> &str {
                "fixture"
            }

            fn supports_versions(&self) -> bool {
                false
            }
        }

        let error = UnreadableProvider
            .contains(&SecretId::new("alpha", "token"))
            .await
            .expect_err("the default presence check reads the value, so a read failure surfaces");
        assert!(error.to_string().contains("value read attempted"));
    }

    #[test]
    fn healthcheck_rejects_a_store_that_is_reachable_but_not_writable() {
        let store = keyring_core::mock::Store::new().unwrap();
        let credential_store: Arc<CredentialStore> = store;
        let entry = credential_store
            .build("harn.healthcheck-read-only", "__harn_probe__:test", None)
            .unwrap();
        entry
            .as_any()
            .downcast_ref::<keyring_core::mock::Cred>()
            .unwrap()
            .set_error(KeyringError::Invalid(
                "mock read-only store".to_string(),
                "write denied".to_string(),
            ));
        let keyring = NativeKeyring::with_store("harn.healthcheck-read-only", credential_store);

        let error = keyring
            .healthcheck_with_user("__harn_probe__:test")
            .expect_err("read-only store must fail the healthcheck");

        assert!(error.to_string().contains("mock read-only store"));
    }
}
