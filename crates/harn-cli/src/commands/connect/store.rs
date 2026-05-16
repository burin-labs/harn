use std::path::PathBuf;

use serde_json::{json, Value as JsonValue};

use crate::cli::ConnectApiKeyArgs;
use harn_vm::secrets::{KeyringSecretProvider, SecretBytes, SecretId, SecretProvider};

use super::workspace::{resolve_manifest_path, secret_namespace_for};
use super::{
    ConnectIndex, ConnectIndexEntry, StoredConnectorToken, CONNECT_INDEX_NAME,
    CONNECT_INDEX_NAMESPACE,
};

pub(super) async fn run_connect_api_key(args: &ConnectApiKeyArgs) -> Result<(), String> {
    let secret_id = parse_secret_id(&args.secret_id).ok_or_else(|| {
        format!(
            "invalid secret id `{}`; expected namespace/name",
            args.secret_id
        )
    })?;
    let value = match (args.value.as_ref(), args.value_file.as_ref()) {
        (Some(value), None) => value.as_bytes().to_vec(),
        (None, Some(path)) => std::fs::read(path)
            .map_err(|error| format!("failed to read API key file {}: {error}", path.display()))?,
        (None, None) => rpassword::prompt_password("API key: ")
            .map_err(|error| format!("failed to read API key: {error}"))?
            .into_bytes(),
        _ => unreachable!("clap enforces API key value conflicts"),
    };
    let provider = connect_secret_provider()?;
    provider
        .put(&secret_id, SecretBytes::from(value))
        .await
        .map_err(|error| format!("failed to store {secret_id}: {error}"))?;
    upsert_index_entry(
        &provider,
        ConnectIndexEntry {
            provider: args.connector.clone(),
            kind: "api-key".to_string(),
            secret_id: secret_id.to_string(),
            expires_at_unix: None,
            scopes: args.scopes.clone(),
            connected_at_unix: current_unix_timestamp(),
            last_used_at_unix: None,
        },
    )
    .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "provider": args.connector,
                "kind": "api-key",
                "secret_id": secret_id.to_string(),
                "scopes": args.scopes,
            }))
            .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Stored API key for {} as {}.", args.connector, secret_id);
    }
    Ok(())
}

pub(super) async fn run_connect_list(json_output: bool) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let mut index = load_connect_index(&provider).await?;
    index
        .providers
        .sort_by(|left, right| left.provider.cmp(&right.provider));
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&index)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else if index.providers.is_empty() {
        println!("No connector OAuth tokens stored in this workspace keyring.");
    } else {
        for entry in &index.providers {
            println!(
                "{}\t{}\t{}\texpires={}\tlast_used={}",
                entry.provider,
                entry.kind,
                entry.secret_id,
                entry
                    .expires_at_unix
                    .map(format_expiry)
                    .unwrap_or_else(|| "unknown".to_string()),
                entry
                    .last_used_at_unix
                    .map(format_expiry)
                    .unwrap_or_else(|| "never".to_string())
            );
        }
    }
    Ok(())
}

pub(super) async fn run_connect_revoke(
    provider_name: &str,
    json_output: bool,
) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let indexed_secret = load_connect_index(&provider).await.ok().and_then(|index| {
        index
            .providers
            .into_iter()
            .find(|entry| entry.provider == provider_name)
            .and_then(|entry| parse_secret_id(&entry.secret_id))
    });
    for id in connector_secret_ids(provider_name) {
        provider
            .delete(&id)
            .await
            .map_err(|error| format!("failed to delete {id}: {error}"))?;
    }
    if let Some(id) = indexed_secret {
        provider
            .delete(&id)
            .await
            .map_err(|error| format!("failed to delete {id}: {error}"))?;
    }
    remove_index_entry(&provider, provider_name).await?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "provider": provider_name,
                "revoked": true,
            }))
            .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Revoked stored connector credentials for {provider_name}.");
    }
    Ok(())
}

pub(super) fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, harn_vm::secrets::SecretVersion::Exact(version))
        }
        None => (trimmed, harn_vm::secrets::SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(harn_vm::secrets::SecretId::new(namespace, name).with_version(version))
}

pub(super) fn connect_secret_provider() -> Result<KeyringSecretProvider, String> {
    let manifest_dir = resolve_manifest_path(None)
        .map(|(_, dir)| dir)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Ok(KeyringSecretProvider::new(secret_namespace_for(
        &manifest_dir,
    )))
}

pub(super) async fn save_connector_token(token: &StoredConnectorToken) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let token_payload = serde_json::to_vec(token)
        .map_err(|error| format!("failed to encode connector token: {error}"))?;
    provider
        .put(
            &connector_oauth_token_id(&token.provider),
            SecretBytes::from(token_payload),
        )
        .await
        .map_err(|error| format!("failed to store connector OAuth token: {error}"))?;
    provider
        .put(
            &SecretId::new(token.provider.clone(), "access-token"),
            SecretBytes::from(token.access_token.clone()),
        )
        .await
        .map_err(|error| format!("failed to store connector access token: {error}"))?;
    if let Some(refresh_token) = token.refresh_token.as_ref() {
        provider
            .put(
                &SecretId::new(token.provider.clone(), "refresh-token"),
                SecretBytes::from(refresh_token.clone()),
            )
            .await
            .map_err(|error| format!("failed to store connector refresh token: {error}"))?;
    }
    upsert_index_entry(
        &provider,
        ConnectIndexEntry {
            provider: token.provider.clone(),
            kind: "oauth".to_string(),
            secret_id: format!("{}/access-token", token.provider),
            expires_at_unix: token.expires_at_unix,
            scopes: token.scopes.clone(),
            connected_at_unix: token.connected_at_unix,
            last_used_at_unix: token.last_used_at_unix,
        },
    )
    .await
}

pub(super) async fn load_connector_token(
    provider_name: &str,
) -> Result<StoredConnectorToken, String> {
    let provider = connect_secret_provider()?;
    let secret = provider
        .get(&connector_oauth_token_id(provider_name))
        .await
        .map_err(|error| {
            format!("failed to load connector OAuth token for {provider_name}: {error}")
        })?;
    secret
        .with_exposed(|bytes| serde_json::from_slice::<StoredConnectorToken>(bytes))
        .map_err(|error| {
            format!("stored connector OAuth token for {provider_name} was invalid JSON: {error}")
        })
}

pub(super) fn connector_oauth_token_id(provider: &str) -> SecretId {
    SecretId::new(provider.to_string(), "oauth-token")
}

pub(super) fn connector_secret_ids(provider: &str) -> Vec<SecretId> {
    vec![
        SecretId::new(provider.to_string(), "oauth-token"),
        SecretId::new(provider.to_string(), "access-token"),
        SecretId::new(provider.to_string(), "refresh-token"),
    ]
}

pub(super) async fn load_connect_index(
    provider: &KeyringSecretProvider,
) -> Result<ConnectIndex, String> {
    let secret = match provider.get(&connect_index_id()).await {
        Ok(secret) => secret,
        Err(harn_vm::secrets::SecretError::NotFound { .. }) => {
            return Ok(ConnectIndex::default());
        }
        Err(error) => return Err(format!("failed to read connector index: {error}")),
    };
    secret
        .with_exposed(|bytes| serde_json::from_slice::<ConnectIndex>(bytes))
        .map_err(|error| format!("connector index was invalid JSON: {error}"))
}

pub(super) async fn save_connect_index(
    provider: &KeyringSecretProvider,
    index: &ConnectIndex,
) -> Result<(), String> {
    let payload = serde_json::to_vec(index)
        .map_err(|error| format!("failed to encode connector index: {error}"))?;
    provider
        .put(&connect_index_id(), SecretBytes::from(payload))
        .await
        .map_err(|error| format!("failed to store connector index: {error}"))
}

pub(super) async fn upsert_index_entry(
    provider: &KeyringSecretProvider,
    entry: ConnectIndexEntry,
) -> Result<(), String> {
    let mut index = load_connect_index(provider).await?;
    index
        .providers
        .retain(|item| item.provider != entry.provider);
    index.providers.push(entry);
    save_connect_index(provider, &index).await
}

pub(super) async fn remove_index_entry(
    provider: &KeyringSecretProvider,
    provider_name: &str,
) -> Result<(), String> {
    let mut index = load_connect_index(provider).await?;
    index
        .providers
        .retain(|item| item.provider != provider_name);
    save_connect_index(provider, &index).await
}

pub(super) fn connect_index_id() -> SecretId {
    SecretId::new(CONNECT_INDEX_NAMESPACE, CONNECT_INDEX_NAME)
}

pub(super) fn connector_token_summary(token: &StoredConnectorToken) -> JsonValue {
    json!({
        "provider": token.provider,
        "secret_id": format!("{}/access-token", token.provider),
        "expires_at_unix": token.expires_at_unix,
        "scopes": token.scopes,
        "connected_at_unix": token.connected_at_unix,
        "last_used_at_unix": token.last_used_at_unix,
        "resource": token.resource,
    })
}

pub(super) fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(super) fn format_expiry(unix: i64) -> String {
    unix.to_string()
}
