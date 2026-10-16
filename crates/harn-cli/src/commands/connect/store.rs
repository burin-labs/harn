use std::collections::BTreeSet;

use serde_json::{json, Value as JsonValue};

use crate::{cli::ConnectApiKeyArgs, package};
use harn_vm::secrets::{
    configured_secret_chain, configured_secret_namespace, ChainSecretProvider, SecretAuditContext,
    SecretBytes, SecretChainPlan, SecretDeleteRequest, SecretId, SecretProvider, SecretScope,
    SECRET_PROVIDER_CHAIN_ENV,
};

use super::{
    ConnectIndex, ConnectIndexEntry, StoredConnectorToken, CONNECT_INDEX_NAME,
    CONNECT_INDEX_NAMESPACE,
};

/// Read a credential from the environment variable `name`, for every
/// `harn connect` option that names one (`--from-env`,
/// `--client-secret-from-env`).
///
/// Errors name the variable and the failure category only. `VarError`'s own
/// `Display` embeds a non-Unicode value verbatim, which would echo the secret
/// into the terminal and any captured log.
pub(super) fn read_named_env_secret(name: &str, what: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Err(format!(
            "failed to read {what} from environment variable {name}: it is not set"
        )),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!(
            "failed to read {what} from environment variable {name}: its value is not valid Unicode"
        )),
    }
}

pub(super) async fn run_connect_api_key(args: &ConnectApiKeyArgs) -> Result<(), String> {
    let secret_id = parse_secret_id(&args.secret_id).ok_or_else(|| {
        format!(
            "invalid secret id `{}`; expected namespace/name",
            args.secret_id
        )
    })?;
    let value = match (
        args.value.as_ref(),
        args.value_file.as_ref(),
        args.from_env.as_ref(),
    ) {
        (Some(value), None, None) => value.as_bytes().to_vec(),
        (None, Some(path), None) => std::fs::read(path)
            .map_err(|error| format!("failed to read API key file {}: {error}", path.display()))?,
        (None, None, Some(name)) => read_named_env_secret(name, "API key")?.into_bytes(),
        (None, None, None) => rpassword::prompt_password("API key: ")
            .map_err(|error| format!("failed to read API key: {error}"))?
            .into_bytes(),
        _ => unreachable!("clap enforces API key value conflicts"),
    };
    if value.is_empty() {
        return Err("API key must not be empty".to_string());
    }
    let environment_fallbacks =
        declared_credential_environment_names(&args.connector, &secret_id.to_string());
    let provider = connect_secret_writer()?;
    if let Err(error) = provider.put(&secret_id, SecretBytes::from(value)).await {
        return Err(format_store_failure(
            &secret_id.to_string(),
            &error.to_string(),
            &environment_fallbacks,
        ));
    }
    upsert_index_entry(
        &provider,
        ConnectIndexEntry {
            provider: args.connector.clone(),
            kind: "api-key".to_string(),
            secret_id: secret_id.to_string(),
            secret_ids: vec![secret_id.to_string()],
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

fn declared_credential_environment_names(connector: &str, secret_id: &str) -> Vec<String> {
    let Some(extensions) = std::env::current_dir()
        .ok()
        .and_then(|cwd| package::try_load_runtime_extensions(&cwd).ok())
    else {
        return Vec::new();
    };
    let Some(setup) = extensions
        .provider_connectors
        .iter()
        .find(|entry| entry.id.as_str() == connector)
        .and_then(|entry| entry.setup.as_ref())
    else {
        return Vec::new();
    };

    package::credential_environment_names_for_secret(&setup.credential_environment, secret_id)
}

pub(super) fn format_store_failure(
    secret_id: &str,
    error: &str,
    environment_names: &[String],
) -> String {
    let base = format!("failed to store {secret_id}: {error}");
    match environment_names {
        [] => base,
        [name] => format!(
            "{base}. For unattended setup, export {name} and keep it set when Harn runs; this connector declares it as the environment source for {secret_id}"
        ),
        names => format!(
            "{base}. For unattended setup, export one of {} and keep it set when Harn runs; this connector declares those names as environment sources for {secret_id}",
            names.join(", ")
        ),
    }
}

pub(super) async fn run_connect_list(json_output: bool) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let mut index = load_connect_index(&provider).await?;
    index
        .providers
        .sort_by(|left, right| left.provider.cmp(&right.provider));
    let store = connect_store_display();
    if json_output {
        let mut value = serde_json::to_value(&index)
            .map_err(|error| format!("failed to encode JSON output: {error}"))?;
        if let Some(object) = value.as_object_mut() {
            let plan = SecretChainPlan::configured();
            object.insert(
                "store".to_string(),
                json!({
                    "providers": plan.providers,
                    "excluded": plan.excluded,
                    "namespace": configured_secret_namespace(),
                }),
            );
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else if index.providers.is_empty() {
        println!("No connector OAuth tokens stored in secret providers {store}.");
    } else {
        println!("Connector credentials in secret providers {store}:");
        for entry in &index.providers {
            println!(
                "{}\t{}\t{}\texpires={}\tlast_used={}",
                entry.provider,
                entry.kind,
                display_secret_ids(entry),
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
    let provider = connect_secret_writer()?;
    let indexed_secret = load_connect_index(&provider).await.ok().and_then(|index| {
        index
            .providers
            .into_iter()
            .find(|entry| entry.provider == provider_name)
            .and_then(|entry| parse_secret_id(&entry.secret_id))
    });
    for id in connector_secret_ids(provider_name)
        .into_iter()
        .chain(indexed_secret)
    {
        delete_connect_secret(&provider, &id).await?;
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

pub(crate) fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
    harn_vm::secrets::parse_secret_id(raw).ok()
}

/// The one store `harn connect` reads: the configured provider chain, the same
/// one `harness.secrets`, connector dispatch, and std/oauth resolve through.
/// `HARN_SECRET_PROVIDERS` therefore selects the backend for connect and for
/// the runs that consume what it stored.
pub(crate) fn connect_secret_provider() -> Result<ChainSecretProvider, String> {
    configured_secret_chain()
        .map_err(|error| format!("failed to configure connector secret providers: {error}"))
}

/// The configured chain, refused when nothing in it persists. A chain of only
/// `env` would accept a credential into this process's environment and lose
/// it at exit, so `harn connect` says so instead of reporting success.
pub(crate) fn connect_secret_writer() -> Result<ChainSecretProvider, String> {
    let chain = connect_secret_provider()?;
    if chain
        .providers()
        .iter()
        .any(|provider| provider.persists_writes())
    {
        return Ok(chain);
    }
    Err(format!(
        "harn connect stores credentials in a persistent secret provider, but the configured chain ({}) has none; unset {SECRET_PROVIDER_CHAIN_ENV} or include keyring or file",
        SecretChainPlan::configured().display()
    ))
}

async fn delete_connect_secret(
    provider: &ChainSecretProvider,
    id: &SecretId,
) -> Result<(), String> {
    provider
        .delete_scoped(SecretDeleteRequest {
            id: id.clone(),
            scope: SecretScope::default(),
            audit: SecretAuditContext::default(),
        })
        .await
        .map_err(|error| format!("failed to delete {id}: {error}"))
}

pub(crate) async fn load_connect_secret_text(secret_id: &str) -> Result<String, String> {
    let id = parse_secret_id(secret_id)
        .ok_or_else(|| format!("invalid secret id `{secret_id}`; expected namespace/name"))?;
    let provider = connect_secret_provider()?;
    let secret = provider
        .get(&id)
        .await
        .map_err(|error| format!("failed to load {id}: {error}"))?;
    secret
        .with_exposed(|bytes| String::from_utf8(bytes.to_vec()))
        .map_err(|error| format!("secret {id} is not valid UTF-8: {error}"))
}

pub(super) async fn save_connector_token(token: &StoredConnectorToken) -> Result<(), String> {
    let provider = connect_secret_writer()?;
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
            &harn_vm::secrets::connector_access_token_id(&token.provider),
            SecretBytes::from(token.access_token.clone()),
        )
        .await
        .map_err(|error| format!("failed to store connector access token: {error}"))?;
    if let Some(refresh_token) = token.refresh_token.as_ref() {
        provider
            .put(
                &harn_vm::secrets::connector_refresh_token_id(&token.provider),
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
            secret_id: harn_vm::secrets::connector_access_token_id(&token.provider).to_string(),
            secret_ids: stored_oauth_secret_ids(token),
            expires_at_unix: token.expires_at_unix,
            scopes: token.scopes.clone(),
            connected_at_unix: token.connected_at_unix,
            last_used_at_unix: token.last_used_at_unix,
        },
    )
    .await
}

/// Where `harn connect` keeps a provider's OAuth record: the configured
/// provider chain shared with `harness.secrets` and std/oauth secrets storage,
/// so every surface reads and writes the same `<provider>/oauth-token` entry.
/// Names each provider and where it looks (environment variable, keyring
/// service and account, file path).
pub(super) fn connector_token_store_description(provider_name: &str) -> String {
    let id = connector_oauth_token_id(provider_name);
    let Ok(chain) = connect_secret_provider() else {
        return format!("secret {id}");
    };
    let located = chain
        .providers()
        .iter()
        .map(|provider| match provider.locator(&id) {
            Some(locator) => format!("{} ({locator})", provider.kind()),
            None => provider.kind().to_string(),
        })
        .collect::<Vec<_>>();
    format!("secret {id} in providers: {}", located.join(", "))
}

/// The configured chain, for `--list` output: `env -> keyring`, plus any
/// default provider the chain leaves out.
fn connect_store_display() -> String {
    format!(
        "{} (namespace {})",
        SecretChainPlan::configured().display(),
        configured_secret_namespace()
    )
}

pub(super) async fn load_connector_token(
    provider_name: &str,
) -> Result<StoredConnectorToken, String> {
    let provider = connect_secret_provider()?;
    let secret = provider
        .get(&connector_oauth_token_id(provider_name))
        .await
        .map_err(|error| {
            format!(
                "failed to load connector OAuth token for {provider_name} from {}: {error}",
                connector_token_store_description(provider_name)
            )
        })?;
    secret
        .with_exposed(|bytes| serde_json::from_slice::<StoredConnectorToken>(bytes))
        .map_err(|error| {
            format!("stored connector OAuth token for {provider_name} was invalid JSON: {error}")
        })
}

pub(super) fn connector_oauth_token_id(provider: &str) -> SecretId {
    harn_vm::secrets::connector_oauth_token_id(provider)
}

pub(super) fn connector_secret_ids(provider: &str) -> Vec<SecretId> {
    vec![
        harn_vm::secrets::connector_oauth_token_id(provider),
        harn_vm::secrets::connector_access_token_id(provider),
        harn_vm::secrets::connector_refresh_token_id(provider),
    ]
}

pub(super) async fn load_connect_index(
    provider: &dyn SecretProvider,
) -> Result<ConnectIndex, String> {
    let secret = match provider.get(&connect_index_id()).await {
        Ok(secret) => secret,
        Err(error) if secret_error_is_not_found(&error) => {
            return Ok(ConnectIndex::default());
        }
        Err(error) => return Err(format!("failed to read connector index: {error}")),
    };
    secret
        .with_exposed(|bytes| serde_json::from_slice::<ConnectIndex>(bytes))
        .map_err(|error| format!("connector index was invalid JSON: {error}"))
}

pub(super) fn secret_error_is_not_found(error: &harn_vm::secrets::SecretError) -> bool {
    error.is_not_found()
}

pub(super) async fn save_connect_index(
    provider: &ChainSecretProvider,
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
    provider: &ChainSecretProvider,
    mut entry: ConnectIndexEntry,
) -> Result<(), String> {
    let mut index = load_connect_index(provider).await?;
    if entry.secret_ids.is_empty() {
        entry.secret_ids.push(entry.secret_id.clone());
    }
    if let Some(existing) = index
        .providers
        .iter()
        .find(|item| item.provider == entry.provider)
        .cloned()
    {
        entry.secret_ids = merged_secret_ids(&existing, &entry);
        let access_token_id =
            harn_vm::secrets::connector_access_token_id(&entry.provider).to_string();
        if entry.secret_id != access_token_id
            && entry.secret_ids.iter().any(|id| id == &access_token_id)
        {
            entry.secret_id = access_token_id;
        }
        if entry.scopes.is_none() {
            entry.scopes = existing.scopes;
        }
        if entry.expires_at_unix.is_none() {
            entry.expires_at_unix = existing.expires_at_unix;
        }
        entry.connected_at_unix = existing.connected_at_unix.min(entry.connected_at_unix);
        entry.last_used_at_unix = entry.last_used_at_unix.or(existing.last_used_at_unix);
    }
    index
        .providers
        .retain(|item| item.provider != entry.provider);
    index.providers.push(entry);
    save_connect_index(provider, &index).await
}

pub(super) async fn remove_index_entry(
    provider: &ChainSecretProvider,
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
        "secret_id": harn_vm::secrets::connector_access_token_id(&token.provider).to_string(),
        "secret_ids": stored_oauth_secret_ids(token),
        "expires_at_unix": token.expires_at_unix,
        "scopes": token.scopes,
        "connected_at_unix": token.connected_at_unix,
        "last_used_at_unix": token.last_used_at_unix,
        "resource": token.resource,
        "issuer": token.issuer,
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

fn display_secret_ids(entry: &ConnectIndexEntry) -> String {
    let ids = normalized_secret_ids(entry);
    if ids.is_empty() {
        entry.secret_id.clone()
    } else {
        ids.join(",")
    }
}

fn stored_oauth_secret_ids(token: &StoredConnectorToken) -> Vec<String> {
    let mut ids = vec![
        harn_vm::secrets::connector_oauth_token_id(&token.provider).to_string(),
        harn_vm::secrets::connector_access_token_id(&token.provider).to_string(),
    ];
    if token.refresh_token.is_some() {
        ids.push(harn_vm::secrets::connector_refresh_token_id(&token.provider).to_string());
    }
    ids
}

fn merged_secret_ids(existing: &ConnectIndexEntry, next: &ConnectIndexEntry) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for id in normalized_secret_ids(existing)
        .into_iter()
        .chain(normalized_secret_ids(next))
    {
        ids.insert(id);
    }
    ids.into_iter().collect()
}

fn normalized_secret_ids(entry: &ConnectIndexEntry) -> Vec<String> {
    let mut ids = Vec::new();
    if !entry.secret_id.is_empty() {
        ids.push(entry.secret_id.clone());
    }
    ids.extend(entry.secret_ids.iter().filter(|id| !id.is_empty()).cloned());
    ids.sort();
    ids.dedup();
    ids
}
