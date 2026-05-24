use base64::Engine;
use harn_vm::mcp_auth::{
    determine_token_endpoint_auth_method, discover_mcp_oauth, dynamic_client_registration_body,
    ensure_pkce_s256_supported, select_client_registration_mode,
    validate_authorization_response_issuer, validate_issuer_binding,
    validate_token_endpoint_auth_method, OAuthClientRegistrationMode,
    OAuthClientRegistrationOptions,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use url::Url;

use crate::cli::{ConnectGenericArgs, ConnectLinearArgs, ConnectOAuthArgs};
use crate::package::{self, ProviderOAuthManifest};

use super::callback::{bind_loopback_listener, wait_for_oauth_response};
use super::store::{
    connector_token_summary, current_unix_timestamp, format_expiry, load_connector_token,
    save_connector_token,
};
use super::{
    DynamicClientRegistrationResponse, OAuthConnectRequest, OAuthProviderDefaults,
    OAuthServerMetadata, StoredConnectorToken, TokenResponse,
};

pub(super) async fn run_connect_named_oauth(
    provider: &str,
    args: &ConnectOAuthArgs,
) -> Result<(), String> {
    let defaults = oauth_provider_defaults(provider)
        .ok_or_else(|| format!("no OAuth defaults registered for provider '{provider}'"))?;
    run_oauth_connect(OAuthConnectRequest {
        provider: provider.to_string(),
        resource: args
            .resource
            .clone()
            .unwrap_or_else(|| defaults.default_resource.to_string()),
        authorization_endpoint: Some(
            args.auth_url
                .clone()
                .unwrap_or_else(|| defaults.authorization_endpoint.to_string()),
        ),
        token_endpoint: Some(
            args.token_url
                .clone()
                .unwrap_or_else(|| defaults.token_endpoint.to_string()),
        ),
        registration_endpoint: None,
        client_id: args.client_id.clone(),
        client_secret: args.client_secret.clone(),
        scopes: args
            .scope
            .clone()
            .or_else(|| defaults.default_scope.map(str::to_string)),
        redirect_uri: args.redirect_uri.clone(),
        token_auth_method: args
            .token_auth_method
            .clone()
            .or_else(|| Some(defaults.token_auth_method.to_string())),
        no_open: args.no_open,
        json: args.json,
    })
    .await
}

pub(super) async fn run_connect_linear_oauth(args: &ConnectLinearArgs) -> Result<(), String> {
    run_connect_named_oauth(
        "linear",
        &ConnectOAuthArgs {
            client_id: args.client_id.clone(),
            client_secret: args.client_secret.clone(),
            scope: args.scope.clone(),
            resource: args.resource.clone(),
            auth_url: args.auth_url.clone(),
            token_url: args.token_url.clone(),
            token_auth_method: args.token_auth_method.clone(),
            redirect_uri: args.redirect_uri.clone(),
            no_open: args.no_open,
            json: args.json,
        },
    )
    .await
}

pub(super) async fn run_connect_generic(args: &ConnectGenericArgs) -> Result<(), String> {
    run_oauth_connect(OAuthConnectRequest {
        provider: args.provider.clone(),
        resource: args
            .oauth
            .resource
            .clone()
            .unwrap_or_else(|| args.url.clone()),
        authorization_endpoint: args.oauth.auth_url.clone(),
        token_endpoint: args.oauth.token_url.clone(),
        registration_endpoint: None,
        client_id: args.oauth.client_id.clone(),
        client_secret: args.oauth.client_secret.clone(),
        scopes: args.oauth.scope.clone(),
        redirect_uri: args.oauth.redirect_uri.clone(),
        token_auth_method: args.oauth.token_auth_method.clone(),
        no_open: args.oauth.no_open,
        json: args.oauth.json,
    })
    .await
}

pub(super) async fn run_connect_registered_provider(
    provider: &str,
    args: &ConnectOAuthArgs,
) -> Result<(), String> {
    if let Some(metadata) = registered_provider_oauth(provider)? {
        return run_oauth_connect(oauth_request_from_provider_metadata(
            provider, args, &metadata,
        )?)
        .await;
    }
    if oauth_provider_defaults(provider).is_some() {
        return run_connect_named_oauth(provider, args).await;
    }
    Err(format!(
        "provider '{provider}' is not registered with OAuth metadata; add `oauth = {{ resource = \"...\" }}` to its [[providers]] entry or use `harn connect generic {provider} <url>`"
    ))
}

pub(super) fn registered_provider_oauth(
    provider: &str,
) -> Result<Option<ProviderOAuthManifest>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let extensions = package::try_load_runtime_extensions(&cwd)?;
    Ok(extensions
        .provider_connectors
        .into_iter()
        .find(|entry| entry.id.as_str() == provider)
        .and_then(|entry| entry.oauth))
}

pub(super) fn oauth_request_from_provider_metadata(
    provider: &str,
    args: &ConnectOAuthArgs,
    metadata: &ProviderOAuthManifest,
) -> Result<OAuthConnectRequest, String> {
    let resource = args
        .resource
        .clone()
        .or_else(|| metadata.resource.clone())
        .ok_or_else(|| {
            format!(
                "registered provider '{provider}' OAuth metadata must include `resource`, or pass --resource"
            )
        })?;
    Ok(OAuthConnectRequest {
        provider: provider.to_string(),
        resource,
        authorization_endpoint: args
            .auth_url
            .clone()
            .or_else(|| metadata.authorization_endpoint.clone()),
        token_endpoint: args
            .token_url
            .clone()
            .or_else(|| metadata.token_endpoint.clone()),
        registration_endpoint: metadata.registration_endpoint.clone(),
        client_id: args
            .client_id
            .clone()
            .or_else(|| metadata.client_id.clone()),
        client_secret: args
            .client_secret
            .clone()
            .or_else(|| metadata.client_secret.clone()),
        scopes: args.scope.clone().or_else(|| metadata.scopes.clone()),
        redirect_uri: args.redirect_uri.clone(),
        token_auth_method: args
            .token_auth_method
            .clone()
            .or_else(|| metadata.token_endpoint_auth_method.clone()),
        no_open: args.no_open,
        json: args.json,
    })
}

pub(super) fn oauth_provider_defaults(provider: &str) -> Option<OAuthProviderDefaults> {
    match provider {
        "slack" => Some(OAuthProviderDefaults {
            authorization_endpoint: "https://slack.com/oauth/v2/authorize",
            token_endpoint: "https://slack.com/api/oauth.v2.access",
            token_auth_method: "client_secret_post",
            default_resource: "https://slack.com/",
            default_scope: None,
        }),
        "linear" => Some(OAuthProviderDefaults {
            authorization_endpoint: "https://linear.app/oauth/authorize",
            token_endpoint: "https://api.linear.app/oauth/token",
            token_auth_method: "client_secret_post",
            default_resource: "https://api.linear.app/",
            default_scope: None,
        }),
        "notion" => Some(OAuthProviderDefaults {
            authorization_endpoint: "https://api.notion.com/v1/oauth/authorize",
            token_endpoint: "https://api.notion.com/v1/oauth/token",
            token_auth_method: "client_secret_basic",
            default_resource: "https://api.notion.com/",
            default_scope: None,
        }),
        _ => None,
    }
}

pub(super) async fn run_oauth_connect(mut request: OAuthConnectRequest) -> Result<(), String> {
    let discovery = if request.authorization_endpoint.is_none() || request.token_endpoint.is_none()
    {
        Some(discover_oauth_server(&request.resource).await?)
    } else {
        None
    };
    if let Some(discovery) = discovery.as_ref() {
        ensure_pkce_support(&discovery.metadata)?;
        if request.scopes.is_none() && !discovery.scopes.is_empty() {
            request.scopes = Some(discovery.scopes.join(" "));
        }
    }

    let authorization_endpoint = request
        .authorization_endpoint
        .clone()
        .or_else(|| {
            discovery
                .as_ref()
                .map(|discovery| discovery.metadata.authorization_endpoint.clone())
        })
        .ok_or_else(|| "OAuth authorization endpoint is required".to_string())?;
    let token_endpoint = request
        .token_endpoint
        .clone()
        .or_else(|| {
            discovery
                .as_ref()
                .map(|discovery| discovery.metadata.token_endpoint.clone())
        })
        .ok_or_else(|| "OAuth token endpoint is required".to_string())?;
    let registration_endpoint = request.registration_endpoint.clone().or_else(|| {
        discovery
            .as_ref()
            .and_then(|discovery| discovery.metadata.registration_endpoint.clone())
    });

    let (listener, redirect_uri) = bind_loopback_listener(&request.redirect_uri)?;
    request.redirect_uri = redirect_uri.clone();
    let (client_id, client_secret, token_auth_method) = resolve_oauth_client(
        &request,
        discovery.as_ref(),
        registration_endpoint.as_deref(),
    )
    .await?;
    let (code_verifier, code_challenge) = generate_pkce_pair();
    let state = random_hex(16);
    let auth_url = build_authorization_url(
        &authorization_endpoint,
        &client_id,
        &redirect_uri,
        &state,
        &code_challenge,
        &request.resource,
        request.scopes.as_deref(),
    )?;

    println!("Provider: {}", request.provider);
    println!("Redirect URI: {redirect_uri}");
    println!("Opening browser for OAuth authorization...");
    if request.no_open || webbrowser::open(auth_url.as_str()).is_err() {
        println!("Open this URL manually:\n{auth_url}");
    }

    let callback = wait_for_oauth_response(listener, &redirect_uri, &state)?;
    if let Some(discovery) = discovery.as_ref() {
        validate_authorization_response_issuer(&discovery.metadata, callback.issuer.as_deref())?;
    }
    let token = exchange_authorization_code(
        &token_endpoint,
        AuthorizationCodeExchange {
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            token_auth_method: &token_auth_method,
            redirect_uri: &redirect_uri,
            resource: &request.resource,
            scopes: request.scopes.as_deref(),
            code: &callback.code,
            code_verifier: &code_verifier,
        },
    )
    .await?;

    let stored = StoredConnectorToken {
        provider: request.provider.clone(),
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_unix: token
            .expires_in
            .map(|seconds| current_unix_timestamp().saturating_add(seconds)),
        token_endpoint,
        client_id,
        client_secret,
        token_endpoint_auth_method: token_auth_method,
        issuer: discovery.as_ref().map(|discovery| discovery.issuer.clone()),
        resource: request.resource.clone(),
        scopes: request.scopes.clone(),
        connected_at_unix: current_unix_timestamp(),
        last_used_at_unix: None,
    };
    save_connector_token(&stored).await?;

    if request.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&connector_token_summary(&stored))
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!(
            "OAuth token stored for {} as {}/access-token.",
            stored.provider, stored.provider
        );
        println!(
            "Expires: {}",
            stored
                .expires_at_unix
                .map(format_expiry)
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    Ok(())
}

pub(super) async fn resolve_oauth_client(
    request: &OAuthConnectRequest,
    discovery: Option<&OAuthDiscoveryResult>,
    registration_endpoint: Option<&str>,
) -> Result<(String, Option<String>, String), String> {
    let registration_mode = discovery
        .map(|discovery| {
            select_client_registration_mode(
                &discovery.metadata,
                OAuthClientRegistrationOptions {
                    client_id: request.client_id.as_deref(),
                    client_secret: request.client_secret.as_deref(),
                    client_id_metadata_document_url: request.client_id.as_deref(),
                },
            )
        })
        .unwrap_or_else(|| {
            if request.client_id.is_some() {
                OAuthClientRegistrationMode::PreRegistered
            } else if registration_endpoint.is_some() {
                OAuthClientRegistrationMode::DynamicClientRegistration
            } else {
                OAuthClientRegistrationMode::Manual
            }
        });
    if let Some(client_id) = request.client_id.clone() {
        let token_auth_method = request
            .token_auth_method
            .clone()
            .or_else(|| {
                discovery.as_ref().and_then(|discovery| {
                    determine_token_auth_method(&discovery.metadata, request.client_secret.as_ref())
                        .ok()
                })
            })
            .unwrap_or_else(|| {
                if request.client_secret.is_some() {
                    "client_secret_post".to_string()
                } else {
                    "none".to_string()
                }
            });
        validate_token_auth_method(&token_auth_method)?;
        return Ok((client_id, request.client_secret.clone(), token_auth_method));
    }

    if registration_mode != OAuthClientRegistrationMode::DynamicClientRegistration {
        return Err(
            "No client_id available. Supply --client-id, use a Client ID Metadata Document URL as --client-id when supported, or use a server that supports dynamic client registration.".to_string()
        );
    }
    let registration_endpoint = registration_endpoint.ok_or_else(|| {
        "No client_id available. Supply --client-id or use a server that supports dynamic client registration.".to_string()
    })?;
    let registration = dynamic_client_registration(
        registration_endpoint,
        &request.redirect_uri,
        request.scopes.as_deref(),
    )
    .await?;
    let auth_method = request
        .token_auth_method
        .clone()
        .or(registration.token_endpoint_auth_method)
        .unwrap_or_else(|| "none".to_string());
    validate_token_auth_method(&auth_method)?;
    Ok((
        registration.client_id,
        registration.client_secret,
        auth_method,
    ))
}

pub(super) async fn run_connect_refresh(
    provider_name: &str,
    json_output: bool,
) -> Result<(), String> {
    let mut stored = load_connector_token(provider_name).await?;
    let refresh_token = stored.refresh_token.clone().ok_or_else(|| {
        format!("stored connector token for {provider_name} does not include a refresh token")
    })?;
    let mut token_endpoint = stored.token_endpoint.clone();
    if let Some(stored_issuer) = stored.issuer.as_deref() {
        let discovery = discover_oauth_server(&stored.resource).await?;
        validate_issuer_binding(stored_issuer, &discovery.issuer)?;
        token_endpoint = discovery.metadata.token_endpoint;
    }
    let refreshed = request_token(
        &reqwest::Client::new(),
        &token_endpoint,
        &stored.token_endpoint_auth_method,
        &stored.client_id,
        stored.client_secret.as_deref(),
        &[
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token),
            ("client_id", stored.client_id.clone()),
            ("resource", stored.resource.clone()),
        ],
    )
    .await?;
    stored.access_token = refreshed.access_token;
    stored.refresh_token = refreshed.refresh_token.or(stored.refresh_token);
    stored.expires_at_unix = refreshed
        .expires_in
        .map(|seconds| current_unix_timestamp().saturating_add(seconds));
    stored.token_endpoint = token_endpoint;
    stored.last_used_at_unix = Some(current_unix_timestamp());
    save_connector_token(&stored).await?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&connector_token_summary(&stored))
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Refreshed OAuth token for {provider_name}.");
    }
    Ok(())
}

pub(super) async fn discover_oauth_server(resource: &str) -> Result<OAuthDiscoveryResult, String> {
    let client = reqwest::Client::new();
    let discovery = discover_mcp_oauth(&client, resource)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OAuthDiscoveryResult {
        metadata: discovery.authorization_server_metadata,
        issuer: discovery.authorization_server_issuer,
        scopes: discovery.scopes,
    })
}

pub(super) fn ensure_pkce_support(metadata: &OAuthServerMetadata) -> Result<(), String> {
    ensure_pkce_s256_supported(metadata)
}

pub(super) async fn dynamic_client_registration(
    registration_endpoint: &str,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> Result<DynamicClientRegistrationResponse, String> {
    let client = reqwest::Client::new();
    let body = dynamic_client_registration_body("Harn CLI", [redirect_uri], scopes);
    let response = client
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("Dynamic client registration failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "Dynamic client registration failed: {status} {body}"
        ));
    }
    response
        .json::<DynamicClientRegistrationResponse>()
        .await
        .map_err(|error| format!("Invalid dynamic client registration response: {error}"))
}

pub(super) fn determine_token_auth_method(
    metadata: &OAuthServerMetadata,
    client_secret: Option<&String>,
) -> Result<String, String> {
    determine_token_endpoint_auth_method(metadata, client_secret.map(String::as_str))
}

pub(super) fn validate_token_auth_method(method: &str) -> Result<(), String> {
    validate_token_endpoint_auth_method(method)
}

pub(super) fn build_authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    resource: &str,
    scopes: Option<&str>,
) -> Result<Url, String> {
    let mut url = Url::parse(authorization_endpoint)
        .map_err(|error| format!("Invalid authorization endpoint: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("state", state);
        query.append_pair("code_challenge", code_challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("resource", resource);
        if let Some(scopes) = scopes {
            query.append_pair("scope", scopes);
        }
    }
    Ok(url)
}

pub(super) async fn exchange_authorization_code(
    token_endpoint: &str,
    request: AuthorizationCodeExchange<'_>,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", request.code.to_string()),
        ("redirect_uri", request.redirect_uri.to_string()),
        ("client_id", request.client_id.to_string()),
        ("code_verifier", request.code_verifier.to_string()),
        ("resource", request.resource.to_string()),
    ];
    if let Some(scopes) = request.scopes {
        form.push(("scope", scopes.to_string()));
    }
    request_token(
        &client,
        token_endpoint,
        request.token_auth_method,
        request.client_id,
        request.client_secret,
        &form,
    )
    .await
}

pub(super) struct AuthorizationCodeExchange<'a> {
    pub(super) client_id: &'a str,
    pub(super) client_secret: Option<&'a str>,
    pub(super) token_auth_method: &'a str,
    pub(super) redirect_uri: &'a str,
    pub(super) resource: &'a str,
    pub(super) scopes: Option<&'a str>,
    pub(super) code: &'a str,
    pub(super) code_verifier: &'a str,
}

pub(super) async fn request_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    token_auth_method: &str,
    client_id: &str,
    client_secret: Option<&str>,
    form: &[(&str, String)],
) -> Result<TokenResponse, String> {
    validate_token_auth_method(token_auth_method)?;
    let mut request = client.post(token_endpoint).form(form);
    match token_auth_method {
        "client_secret_basic" => {
            let client_secret = client_secret
                .ok_or_else(|| "Missing client secret for client_secret_basic".to_string())?;
            request = request.basic_auth(client_id, Some(client_secret));
        }
        "client_secret_post" => {
            let client_secret = client_secret
                .ok_or_else(|| "Missing client secret for client_secret_post".to_string())?;
            let mut extended = form.to_vec();
            extended.push(("client_secret", client_secret.to_string()));
            request = client.post(token_endpoint).form(&extended);
        }
        _ => {}
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("Token request failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Token request failed: {status} {body}"));
    }
    let token = response
        .json::<TokenResponse>()
        .await
        .map_err(|error| format!("Invalid token response: {error}"))?;
    if token.ok == Some(false) {
        return Err(format!(
            "Token request failed: {}",
            token
                .error
                .unwrap_or_else(|| "provider returned ok=false".to_string())
        ));
    }
    Ok(token)
}

pub(super) fn generate_pkce_pair() -> (String, String) {
    let verifier = random_hex(32);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

pub(super) fn random_hex(bytes: usize) -> String {
    let raw: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    raw.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) struct OAuthDiscoveryResult {
    pub(super) metadata: OAuthServerMetadata,
    pub(super) issuer: String,
    pub(super) scopes: Vec<String>,
}
