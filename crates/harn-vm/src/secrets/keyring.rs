use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    emit_secret_access_event, RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta,
    SecretProvider,
};

#[derive(Debug)]
pub struct KeyringSecretProvider {
    namespace: String,
    entries: Mutex<HashMap<String, Arc<::keyring::Entry>>>,
}

impl KeyringSecretProvider {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn service(&self) -> &str {
        &self.namespace
    }

    pub async fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        let entry = self.entry(id)?;
        match entry.delete_credential() {
            Ok(()) | Err(::keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError::Backend {
                provider: "keyring".to_string(),
                message: format!("failed to delete keyring credential: {error}"),
            }),
        }
    }

    pub fn healthcheck(&self) -> Result<String, SecretError> {
        let probe = SecretId::new("", "__harn_probe__");
        let entry = self.entry(&probe)?;
        match entry.get_secret() {
            Ok(_) | Err(::keyring::Error::NoEntry) => {
                Ok(format!("service '{}' reachable", self.namespace))
            }
            Err(error) => Err(SecretError::Backend {
                provider: "keyring".to_string(),
                message: format!("failed to access keyring backend: {error}"),
            }),
        }
    }

    fn entry(&self, id: &SecretId) -> Result<Arc<::keyring::Entry>, SecretError> {
        let account = account_name(id);
        let mut entries = self.entries.lock().expect("keyring cache poisoned");
        if let Some(entry) = entries.get(&account) {
            return Ok(entry.clone());
        }

        let entry = Arc::new(
            ::keyring::Entry::new(self.service(), &account).map_err(|error| {
                SecretError::Backend {
                    provider: "keyring".to_string(),
                    message: format!("failed to create keyring entry: {error}"),
                }
            })?,
        );
        entries.insert(account, entry.clone());
        Ok(entry)
    }
}

#[async_trait]
impl SecretProvider for KeyringSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        let entry = self.entry(id)?;
        match entry.get_secret() {
            Ok(bytes) => {
                emit_secret_access_event("keyring", id);
                Ok(SecretBytes::from(bytes))
            }
            Err(::keyring::Error::NoEntry) => Err(SecretError::NotFound {
                provider: "keyring".to_string(),
                id: id.clone(),
            }),
            Err(error) => Err(SecretError::Backend {
                provider: "keyring".to_string(),
                message: format!("failed to read keyring secret: {error}"),
            }),
        }
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        let entry = self.entry(id)?;
        value.with_exposed(|bytes| {
            entry
                .set_secret(bytes)
                .map_err(|error| SecretError::Backend {
                    provider: "keyring".to_string(),
                    message: format!("failed to store keyring secret: {error}"),
                })
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: "keyring".to_string(),
            operation: "rotate",
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Err(SecretError::Unsupported {
            provider: "keyring".to_string(),
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
