use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::StoredMcpToken;
use crate::secrets::{KeyringSecretProvider, SecretBytes, SecretError, SecretId, SecretProvider};

// CLI and ACP authorizations share one keyring namespace.
const KEYRING_SERVICE: &str = "dev.harn.mcp";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct OAuthTokenStoreKey {
    resource: String,
    issuer: String,
    client_id: String,
}

impl OAuthTokenStoreKey {
    pub(super) fn new(resource: &str, issuer: &str, client_id: &str) -> Self {
        Self {
            resource: resource.to_string(),
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
        }
    }

    pub(super) fn from_token(token: &StoredMcpToken) -> Self {
        Self::new(&token.resource, &token.issuer, &token.client_id)
    }

    pub(super) fn account(&self) -> String {
        token_store_account(&self.resource, &self.issuer, &self.client_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOAuthClientIndex {
    client_id: String,
}

#[async_trait]
pub(super) trait OAuthTokenStorage: Send + Sync {
    async fn load_token(&self, key: &OAuthTokenStoreKey) -> Result<Option<StoredMcpToken>, String>;
    async fn save_token(&self, token: &StoredMcpToken) -> Result<(), String>;
    async fn delete_token(&self, key: &OAuthTokenStoreKey) -> Result<(), String>;
    async fn load_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
    ) -> Result<Option<String>, String>;
    async fn save_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
        client_id: &str,
    ) -> Result<(), String>;
    async fn delete_active_client_id(&self, resource: &str, issuer: &str) -> Result<(), String>;
}

pub(super) struct KeyringOAuthTokenStorage {
    provider: KeyringSecretProvider,
}

impl Default for KeyringOAuthTokenStorage {
    fn default() -> Self {
        Self {
            provider: KeyringSecretProvider::new(KEYRING_SERVICE),
        }
    }
}

#[async_trait]
impl OAuthTokenStorage for KeyringOAuthTokenStorage {
    async fn load_token(&self, key: &OAuthTokenStoreKey) -> Result<Option<StoredMcpToken>, String> {
        let payload = match self.provider.get(&token_secret_id(key)).await {
            Ok(secret) => secret,
            Err(SecretError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(format!("Failed to read OAuth token from keyring: {error}")),
        };
        let token = payload
            .with_exposed(|bytes| serde_json::from_slice::<StoredMcpToken>(bytes))
            .map_err(|error| format!("Stored OAuth token was invalid JSON: {error}"))?;
        validate_token_store_binding(&token, key)?;
        Ok(Some(token))
    }

    async fn save_token(&self, token: &StoredMcpToken) -> Result<(), String> {
        let payload = serde_json::to_string(token)
            .map_err(|error| format!("Failed to serialize OAuth token: {error}"))?;
        self.provider
            .put(
                &token_secret_id(&OAuthTokenStoreKey::from_token(token)),
                SecretBytes::from(payload.into_bytes()),
            )
            .await
            .map_err(|error| format!("Failed to store OAuth token in keyring: {error}"))?;
        self.save_active_client_id(&token.resource, &token.issuer, &token.client_id)
            .await
    }

    async fn delete_token(&self, key: &OAuthTokenStoreKey) -> Result<(), String> {
        self.provider
            .delete(&token_secret_id(key))
            .await
            .map_err(|error| format!("Failed to delete OAuth token from keyring: {error}"))
    }

    async fn load_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
    ) -> Result<Option<String>, String> {
        let payload = match self
            .provider
            .get(&token_client_index_secret_id(resource, issuer))
            .await
        {
            Ok(secret) => secret,
            Err(SecretError::NotFound { .. }) => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Failed to read OAuth client index from keyring: {error}"
                ))
            }
        };
        let index = payload
            .with_exposed(|bytes| serde_json::from_slice::<StoredOAuthClientIndex>(bytes))
            .map_err(|error| format!("Stored OAuth client index was invalid JSON: {error}"))?;
        Ok(Some(index.client_id))
    }

    async fn save_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
        client_id: &str,
    ) -> Result<(), String> {
        let payload = serde_json::to_string(&StoredOAuthClientIndex {
            client_id: client_id.to_string(),
        })
        .map_err(|error| format!("Failed to serialize OAuth client index: {error}"))?;
        self.provider
            .put(
                &token_client_index_secret_id(resource, issuer),
                SecretBytes::from(payload.into_bytes()),
            )
            .await
            .map_err(|error| format!("Failed to store OAuth client index in keyring: {error}"))
    }

    async fn delete_active_client_id(&self, resource: &str, issuer: &str) -> Result<(), String> {
        self.provider
            .delete(&token_client_index_secret_id(resource, issuer))
            .await
            .map_err(|error| format!("Failed to delete OAuth client index from keyring: {error}"))
    }
}

pub(super) async fn load_stored_token_from_store<S: OAuthTokenStorage + ?Sized>(
    store: &S,
    resource: &str,
    issuer: &str,
    client_id_hint: Option<&str>,
) -> Result<Option<StoredMcpToken>, String> {
    let client_id = match client_id_hint {
        Some(client_id) => client_id.to_string(),
        None => match store.load_active_client_id(resource, issuer).await? {
            Some(client_id) => client_id,
            None => return Ok(None),
        },
    };
    store
        .load_token(&OAuthTokenStoreKey::new(resource, issuer, &client_id))
        .await
}

pub(super) async fn delete_stored_token_and_active_index<S: OAuthTokenStorage + ?Sized>(
    store: &S,
    key: &OAuthTokenStoreKey,
) -> Result<(), String> {
    store.delete_token(key).await?;
    if store
        .load_active_client_id(&key.resource, &key.issuer)
        .await?
        .as_deref()
        == Some(key.client_id.as_str())
    {
        store
            .delete_active_client_id(&key.resource, &key.issuer)
            .await?;
    }
    Ok(())
}

pub(super) fn token_store_account(resource: &str, issuer: &str, client_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    hasher.update([0]);
    hasher.update(resource.as_bytes());
    hasher.update([0]);
    hasher.update(client_id.as_bytes());
    let digest = hasher.finalize();
    format!(
        "mcp-token-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
}

fn token_client_index_account(resource: &str, issuer: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    hasher.update([0]);
    hasher.update(resource.as_bytes());
    let digest = hasher.finalize();
    format!(
        "mcp-client-index-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
}

fn token_secret_id(key: &OAuthTokenStoreKey) -> SecretId {
    SecretId::new("", key.account())
}

fn token_client_index_secret_id(resource: &str, issuer: &str) -> SecretId {
    SecretId::new("", token_client_index_account(resource, issuer))
}

fn validate_token_store_binding(
    token: &StoredMcpToken,
    key: &OAuthTokenStoreKey,
) -> Result<(), String> {
    if token.resource != key.resource
        || token.issuer != key.issuer
        || token.client_id != key.client_id
    {
        return Err("Stored OAuth token key does not match its token binding".to_string());
    }
    Ok(())
}
