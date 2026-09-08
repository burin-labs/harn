use harn_vm::secrets::{KeyringSecretProvider, SecretId, SecretProvider};

use super::{OAuthConnectRequest, DEFAULT_OAUTH_REDIRECT_URI};

/// Registration metadata that may be recovered from a pre-namespace OAuth
/// record. Token material and the old client secret are deliberately absent
/// from this type, so deserialization cannot carry them into a new request.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub(super) struct LegacyOAuthRegistration {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default, alias = "scope")]
    scopes: Option<String>,
    #[serde(default, alias = "authorization_url", alias = "auth_url")]
    authorization_endpoint: Option<String>,
    #[serde(default, alias = "token_url")]
    token_endpoint: Option<String>,
    #[serde(default, alias = "token_auth_method")]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

fn legacy_secret_namespace() -> Result<String, String> {
    let (_, manifest_dir) = super::workspace::resolve_manifest_path(None)?;
    let leaf = manifest_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    Ok(format!("harn/{leaf}"))
}

pub(super) async fn load_legacy_oauth_registration(
    provider_name: &str,
) -> Result<Option<LegacyOAuthRegistration>, String> {
    let provider = KeyringSecretProvider::new(legacy_secret_namespace()?);
    let id = harn_vm::secrets::connector_oauth_token_id(provider_name);
    // Migration is opportunistic. A machine without a usable OS keyring must
    // still reach the provider's normal dynamic-registration path.
    match provider.contains(&id).await {
        Ok(true) => {}
        Ok(false) | Err(_) => return Ok(None),
    }
    load_legacy_oauth_registration_from(&provider, &id).await
}

pub(super) async fn load_legacy_oauth_registration_from(
    provider: &dyn SecretProvider,
    id: &SecretId,
) -> Result<Option<LegacyOAuthRegistration>, String> {
    let secret = match provider.get(id).await {
        Ok(secret) => secret,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(format!("failed to read legacy OAuth registration: {error}")),
    };
    let registration = secret
        .with_exposed(|bytes| serde_json::from_slice::<LegacyOAuthRegistration>(bytes))
        .map_err(|error| format!("legacy OAuth registration was invalid JSON: {error}"))?;
    if registration.client_id.is_none()
        && registration.authorization_endpoint.is_none()
        && registration.token_endpoint.is_none()
        && registration.scopes.is_none()
        && registration.token_endpoint_auth_method.is_none()
        && registration.redirect_uri.is_none()
        && registration.resource.is_none()
    {
        return Ok(None);
    }
    Ok(Some(registration))
}

pub(super) fn oauth_request_with_legacy_registration(
    mut request: OAuthConnectRequest,
    registration: LegacyOAuthRegistration,
) -> OAuthConnectRequest {
    request.client_id = request.client_id.or(registration.client_id);
    request.authorization_endpoint = request
        .authorization_endpoint
        .or(registration.authorization_endpoint);
    request.token_endpoint = request.token_endpoint.or(registration.token_endpoint);
    request.scopes = request.scopes.or(registration.scopes);
    request.token_auth_method = request
        .token_auth_method
        .or(registration.token_endpoint_auth_method);
    if request.redirect_uri == DEFAULT_OAUTH_REDIRECT_URI {
        request.redirect_uri = registration
            .redirect_uri
            .unwrap_or_else(|| DEFAULT_OAUTH_REDIRECT_URI.to_string());
    }
    if request.resource.trim().is_empty() {
        request.resource = registration.resource.unwrap_or_default();
    }
    request
}

pub(super) fn migrated_oauth_client_secret_required(request: &OAuthConnectRequest) -> bool {
    request.client_id.is_some()
        && request.client_secret.is_none()
        && matches!(
            request.token_auth_method.as_deref(),
            Some("client_secret_basic" | "client_secret_post")
        )
}
