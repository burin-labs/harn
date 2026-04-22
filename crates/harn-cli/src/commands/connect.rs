use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use url::Url;

use crate::cli::{
    ConnectArgs, ConnectCommand, ConnectGenericArgs, ConnectGithubArgs, ConnectLinearArgs,
    ConnectOAuthArgs,
};
use crate::package;
use harn_vm::secrets::{KeyringSecretProvider, SecretBytes, SecretId, SecretProvider};

const MANIFEST: &str = "harn.toml";
const DEFAULT_LINEAR_API_BASE_URL: &str = "https://api.linear.app/graphql";
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECT_INDEX_NAMESPACE: &str = "connect";
const CONNECT_INDEX_NAME: &str = "index";

#[derive(Clone, Debug)]
struct OAuthProviderDefaults {
    authorization_endpoint: &'static str,
    token_endpoint: &'static str,
    token_auth_method: &'static str,
    default_resource: &'static str,
    default_scope: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct OAuthConnectRequest {
    provider: String,
    resource: String,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    registration_endpoint: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scopes: Option<String>,
    redirect_uri: String,
    token_auth_method: Option<String>,
    no_open: bool,
    json: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConnectorToken {
    provider: String,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at_unix: Option<i64>,
    token_endpoint: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    token_endpoint_auth_method: String,
    resource: String,
    #[serde(default)]
    scopes: Option<String>,
    connected_at_unix: i64,
    #[serde(default)]
    last_used_at_unix: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ConnectIndex {
    #[serde(default)]
    providers: Vec<ConnectIndexEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ConnectIndexEntry {
    provider: String,
    kind: String,
    secret_id: String,
    #[serde(default)]
    expires_at_unix: Option<i64>,
    #[serde(default)]
    scopes: Option<String>,
    connected_at_unix: i64,
    #[serde(default)]
    last_used_at_unix: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
struct OAuthProtectedResource {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct OAuthServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct DynamicClientRegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    _extra: serde_json::Map<String, JsonValue>,
}

pub(crate) async fn run_connect(args: ConnectArgs) {
    if let Err(error) = run_connect_inner(args).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run_connect_inner(args: ConnectArgs) -> Result<(), String> {
    let actions = args.list as u8
        + args.revoke.is_some() as u8
        + args.refresh.is_some() as u8
        + (!args.generic.is_empty()) as u8
        + args.command.is_some() as u8;
    if actions != 1 {
        return Err(
            "choose exactly one connect action: a provider subcommand, --generic, --list, --revoke, or --refresh"
                .to_string(),
        );
    }

    if args.list {
        return run_connect_list(args.json).await;
    }
    if let Some(provider) = args.revoke {
        return run_connect_revoke(&provider, args.json).await;
    }
    if let Some(provider) = args.refresh {
        return run_connect_refresh(&provider, args.json).await;
    }
    if !args.generic.is_empty() {
        if args.generic.len() != 2 {
            return Err("--generic expects <provider> <url>".to_string());
        }
        return run_connect_generic(&ConnectGenericArgs {
            provider: args.generic[0].clone(),
            url: args.generic[1].clone(),
            oauth: ConnectOAuthArgs {
                client_id: None,
                client_secret: None,
                scope: None,
                resource: None,
                auth_url: None,
                token_url: None,
                token_auth_method: None,
                redirect_uri: "http://127.0.0.1:0/oauth/callback".to_string(),
                no_open: false,
                json: args.json,
            },
        })
        .await;
    }

    match args.command.expect("validated one command action") {
        ConnectCommand::Github(args) => run_connect_github(&args).await,
        ConnectCommand::Linear(args) if args.url.is_some() => run_connect_linear(&args).await,
        ConnectCommand::Linear(args) => run_connect_linear_oauth(&args).await,
        ConnectCommand::Slack(args) => run_connect_named_oauth("slack", &args).await,
        ConnectCommand::Notion(args) => run_connect_named_oauth("notion", &args).await,
        ConnectCommand::Generic(args) => run_connect_generic(&args).await,
    }
}

async fn run_connect_named_oauth(provider: &str, args: &ConnectOAuthArgs) -> Result<(), String> {
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

async fn run_connect_linear_oauth(args: &ConnectLinearArgs) -> Result<(), String> {
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

async fn run_connect_generic(args: &ConnectGenericArgs) -> Result<(), String> {
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

fn oauth_provider_defaults(provider: &str) -> Option<OAuthProviderDefaults> {
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

async fn run_connect_linear(args: &ConnectLinearArgs) -> Result<(), String> {
    let url = args.url.as_deref().ok_or_else(|| {
        "`harn connect linear` webhook registration requires --url; omit --url for OAuth setup"
            .to_string()
    })?;
    if !args.all_public_teams && args.team_id.is_none() {
        return Err(
            "`harn connect linear` requires either --team-id or --all-public-teams".to_string(),
        );
    }

    let (manifest_path, manifest_dir) = resolve_manifest_path(args.config.as_deref())?;
    let extensions = package::load_runtime_extensions(&manifest_dir);
    let triggers: Vec<_> = extensions
        .triggers
        .into_iter()
        .filter(|trigger| trigger.provider.as_str() == "linear")
        .collect();
    if triggers.is_empty() {
        return Err(format!(
            "no Linear triggers found in {}",
            manifest_path.display()
        ));
    }

    let resource_types = derive_linear_resource_types(&triggers)?;
    let token = resolve_linear_auth(args, &manifest_dir).await?;
    let label = args.label.clone().unwrap_or_else(|| {
        let package = extensions
            .root_manifest
            .as_ref()
            .and_then(|manifest| manifest.package.as_ref())
            .and_then(|package| package.name.clone())
            .unwrap_or_else(|| {
                manifest_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
        format!("Harn ({package})")
    });

    let input = if let Some(team_id) = args.team_id.as_ref() {
        json!({
            "url": url,
            "teamId": team_id,
            "label": label,
            "resourceTypes": resource_types,
        })
    } else {
        json!({
            "url": url,
            "allPublicTeams": true,
            "label": label,
            "resourceTypes": resource_types,
        })
    };

    let response = reqwest::Client::new()
        .post(
            args.api_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_LINEAR_API_BASE_URL.to_string()),
        )
        .header("Content-Type", "application/json")
        .header("Authorization", token)
        .json(&json!({
            "query": "mutation RegisterWebhook($input: WebhookCreateInput!) { webhookCreate(input: $input) { success webhook { id enabled url } } }",
            "variables": { "input": input },
            "operationName": "RegisterWebhook",
        }))
        .send()
        .await
        .map_err(|error| format!("failed to call Linear GraphQL API: {error}"))?;

    let status = response.status();
    let payload = response
        .json::<JsonValue>()
        .await
        .map_err(|error| format!("failed to decode Linear GraphQL response: {error}"))?;
    if !status.is_success() {
        return Err(format_linear_graphql_error(status.as_u16(), &payload));
    }
    if payload.get("errors").is_some() {
        return Err(format_linear_graphql_error(status.as_u16(), &payload));
    }
    let result = payload
        .get("data")
        .and_then(|value| value.get("webhookCreate"))
        .ok_or_else(|| "Linear GraphQL response missing data.webhookCreate".to_string())?;
    if !result
        .get("success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return Err("Linear webhookCreate returned success = false".to_string());
    }

    let rendered = json!({
        "manifest": manifest_path.display().to_string(),
        "url": url,
        "resource_types": resource_types,
        "webhook": result.get("webhook").cloned().unwrap_or(JsonValue::Null),
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        let webhook = rendered.get("webhook").unwrap_or(&JsonValue::Null);
        println!(
            "Registered Linear webhook {} for {}",
            webhook
                .get("id")
                .and_then(JsonValue::as_str)
                .unwrap_or("<unknown>"),
            url
        );
        println!(
            "Enabled: {}",
            webhook
                .get("enabled")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false)
        );
        println!(
            "Resource types: {}",
            rendered["resource_types"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("Manifest: {}", manifest_path.display());
    }

    Ok(())
}

async fn run_connect_github(args: &ConnectGithubArgs) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    let state = random_hex(16);
    let installation_id = match args.installation_id.clone() {
        Some(id) => id,
        None => {
            let install_url = github_install_url(args, &state)?;
            let (listener, redirect_uri) = bind_loopback_listener(&args.redirect_uri)?;
            println!("Opening browser for GitHub App installation...");
            println!("Callback listener: {redirect_uri}");
            if args.no_open || webbrowser::open(install_url.as_str()).is_err() {
                println!("Open this URL manually:\n{install_url}");
            }
            wait_for_github_installation(listener, &redirect_uri, Some(&state))?
        }
    };

    let mut stored = Vec::new();
    let metadata = json!({
        "provider": "github",
        "app_slug": args.app_slug,
        "app_id": args.app_id,
        "installation_id": installation_id,
        "connected_at_unix": current_unix_timestamp(),
    });
    let metadata_id = SecretId::new("github", format!("installation-{installation_id}"));
    provider
        .put(
            &metadata_id,
            SecretBytes::from(
                serde_json::to_vec(&metadata)
                    .map_err(|error| format!("failed to encode GitHub metadata: {error}"))?,
            ),
        )
        .await
        .map_err(|error| format!("failed to store {metadata_id}: {error}"))?;
    stored.push(metadata_id.to_string());

    if let Some(private_key_file) = args.private_key_file.as_ref() {
        let app_id = args
            .app_id
            .as_ref()
            .ok_or_else(|| "--app-id is required with --private-key-file".to_string())?;
        let private_key = std::fs::read(private_key_file).map_err(|error| {
            format!(
                "failed to read private key file {}: {error}",
                private_key_file.display()
            )
        })?;
        let key_id = SecretId::new("github", format!("app-{app_id}/private-key"));
        provider
            .put(&key_id, SecretBytes::from(private_key))
            .await
            .map_err(|error| format!("failed to store {key_id}: {error}"))?;
        stored.push(key_id.to_string());
    }

    if args.webhook_secret.is_some() || args.webhook_secret_file.is_some() {
        let secret = match (
            args.webhook_secret.as_ref(),
            args.webhook_secret_file.as_ref(),
        ) {
            (Some(value), None) => value.as_bytes().to_vec(),
            (None, Some(path)) => std::fs::read(path).map_err(|error| {
                format!(
                    "failed to read webhook secret file {}: {error}",
                    path.display()
                )
            })?,
            _ => unreachable!("clap enforces webhook secret conflicts"),
        };
        let secret_id = SecretId::new("github", "webhook-secret");
        provider
            .put(&secret_id, SecretBytes::from(secret))
            .await
            .map_err(|error| format!("failed to store {secret_id}: {error}"))?;
        stored.push(secret_id.to_string());
    }

    upsert_index_entry(
        &provider,
        ConnectIndexEntry {
            provider: "github".to_string(),
            kind: "github-app".to_string(),
            secret_id: format!("github/installation-{installation_id}"),
            expires_at_unix: None,
            scopes: None,
            connected_at_unix: current_unix_timestamp(),
            last_used_at_unix: None,
        },
    )
    .await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "provider": "github",
                "installation_id": installation_id,
                "stored": stored,
            }))
            .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else {
        println!("Connected GitHub App installation {installation_id}.");
        println!("Stored: {}", stored.join(", "));
    }

    Ok(())
}

async fn run_oauth_connect(mut request: OAuthConnectRequest) -> Result<(), String> {
    let discovery = if request.authorization_endpoint.is_none() || request.token_endpoint.is_none()
    {
        Some(discover_oauth_server(&request.resource).await?)
    } else {
        None
    };
    if let Some(discovery) = discovery.as_ref() {
        ensure_pkce_support(&discovery.metadata)?;
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

    let code = wait_for_oauth_code(listener, &redirect_uri, &state)?;
    let token = exchange_authorization_code(
        &token_endpoint,
        AuthorizationCodeExchange {
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            token_auth_method: &token_auth_method,
            redirect_uri: &redirect_uri,
            resource: &request.resource,
            scopes: request.scopes.as_deref(),
            code: &code,
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

async fn resolve_oauth_client(
    request: &OAuthConnectRequest,
    discovery: Option<&OAuthDiscoveryResult>,
    registration_endpoint: Option<&str>,
) -> Result<(String, Option<String>, String), String> {
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

async fn run_connect_list(json_output: bool) -> Result<(), String> {
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

async fn run_connect_revoke(provider_name: &str, json_output: bool) -> Result<(), String> {
    let provider = connect_secret_provider()?;
    for id in connector_secret_ids(provider_name) {
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

async fn run_connect_refresh(provider_name: &str, json_output: bool) -> Result<(), String> {
    let mut stored = load_connector_token(provider_name).await?;
    let refresh_token = stored.refresh_token.clone().ok_or_else(|| {
        format!("stored connector token for {provider_name} does not include a refresh token")
    })?;
    let refreshed = request_token(
        &reqwest::Client::new(),
        &stored.token_endpoint,
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

fn derive_linear_resource_types(
    triggers: &[package::ResolvedTriggerConfig],
) -> Result<Vec<String>, String> {
    let mut resource_types = BTreeSet::new();
    for trigger in triggers {
        for event in &trigger.match_.events {
            let resource = linear_resource_type_for_event(event).ok_or_else(|| {
                format!(
                    "trigger '{}' uses unsupported Linear event '{}'",
                    trigger.id, event
                )
            })?;
            resource_types.insert(resource.to_string());
        }
    }
    if resource_types.is_empty() {
        return Err(
            "no Linear resource types could be derived from trigger match.events".to_string(),
        );
    }
    Ok(resource_types.into_iter().collect())
}

fn linear_resource_type_for_event(event: &str) -> Option<&'static str> {
    let prefix = event
        .split('.')
        .next()
        .unwrap_or(event)
        .trim()
        .to_ascii_lowercase();
    match prefix.as_str() {
        "issue" => Some("Issue"),
        "comment" | "issue_comment" | "issuecomment" => Some("Comment"),
        "issue_label" | "issuelabel" => Some("IssueLabel"),
        "project" => Some("Project"),
        "cycle" => Some("Cycle"),
        "customer" => Some("Customer"),
        "customer_request" | "customerrequest" => Some("CustomerRequest"),
        _ => None,
    }
}

async fn resolve_linear_auth(
    args: &ConnectLinearArgs,
    manifest_dir: &Path,
) -> Result<String, String> {
    if let Some(token) = args.access_token.as_ref() {
        return Ok(format!("Bearer {token}"));
    }
    if let Some(api_key) = args.api_key.as_ref() {
        return Ok(api_key.clone());
    }

    let secret_id = args
        .access_token_secret
        .as_deref()
        .or(args.api_key_secret.as_deref())
        .ok_or_else(|| {
            "provide --access-token, --access-token-secret, --api-key, or --api-key-secret"
                .to_string()
        })
        .and_then(|raw| parse_secret_id(raw).ok_or_else(|| format!("invalid secret id `{raw}`")))?;
    let provider = harn_vm::secrets::configured_default_chain(secret_namespace_for(manifest_dir))
        .map_err(|error| format!("failed to configure secret providers: {error}"))?;
    let secret = provider
        .get(&secret_id)
        .await
        .map_err(|error| format!("failed to load secret `{secret_id}`: {error}"))?;
    let value = secret.with_exposed(|bytes| String::from_utf8_lossy(bytes).to_string());
    if args.access_token_secret.is_some() {
        Ok(format!("Bearer {value}"))
    } else {
        Ok(value)
    }
}

fn resolve_manifest_path(explicit: Option<&str>) -> Result<(PathBuf, PathBuf), String> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!("manifest not found: {}", path.display()));
        }
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("manifest has no parent directory: {}", path.display()))?;
        return Ok((path, dir));
    }

    find_nearest_manifest(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .ok_or_else(|| {
            "could not find a harn.toml in the current directory or its parents".to_string()
        })
}

fn find_nearest_manifest(start: &Path) -> Option<(PathBuf, PathBuf)> {
    const MAX_PARENT_DIRS: usize = 16;
    let mut cursor = Some(start.to_path_buf());
    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let base = if dir.is_dir() {
            dir
        } else {
            dir.parent()?.to_path_buf()
        };
        let candidate = base.join(MANIFEST);
        if candidate.is_file() {
            return Some((candidate, base));
        }
        if base.join(".git").exists() {
            break;
        }
        cursor = base.parent().map(Path::to_path_buf);
    }
    None
}

fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
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

fn secret_namespace_for(manifest_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = manifest_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}

fn format_linear_graphql_error(status: u16, payload: &JsonValue) -> String {
    let messages = payload
        .get("errors")
        .and_then(JsonValue::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("message").and_then(JsonValue::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    if messages.is_empty() {
        format!("Linear GraphQL request failed with status {status}")
    } else {
        format!("Linear GraphQL request failed with status {status}: {messages}")
    }
}

fn github_install_url(args: &ConnectGithubArgs, state: &str) -> Result<Url, String> {
    let raw = if let Some(url) = args.install_url.as_ref() {
        url.clone()
    } else {
        let slug = args
            .app_slug
            .as_ref()
            .ok_or_else(|| "provide --app-slug, --install-url, or --installation-id".to_string())?;
        format!("https://github.com/apps/{slug}/installations/new")
    };
    let mut url =
        Url::parse(&raw).map_err(|error| format!("invalid GitHub install URL: {error}"))?;
    url.query_pairs_mut().append_pair("state", state);
    Ok(url)
}

fn bind_loopback_listener(redirect_uri: &str) -> Result<(TcpListener, String), String> {
    let mut parsed =
        Url::parse(redirect_uri).map_err(|error| format!("Invalid redirect URI: {error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    if host != "127.0.0.1" && host != "localhost" {
        return Err("Redirect URI must bind to 127.0.0.1 or localhost".to_string());
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    let listener = TcpListener::bind((host, port))
        .map_err(|error| format!("Failed to bind redirect URI {redirect_uri}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Failed to configure redirect listener: {error}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|error| format!("Failed to inspect redirect listener: {error}"))?
        .port();
    parsed
        .set_port(Some(actual_port))
        .map_err(|_| "failed to render redirect listener port".to_string())?;
    Ok((listener, parsed.to_string()))
}

fn wait_for_oauth_code(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<String, String> {
    let query = wait_for_callback_query(listener, redirect_uri, Some(expected_state))?;
    query
        .into_iter()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value)
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())
}

fn wait_for_github_installation(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: Option<&str>,
) -> Result<String, String> {
    let query = wait_for_callback_query(listener, redirect_uri, expected_state)?;
    query
        .into_iter()
        .find(|(key, _)| key == "installation_id")
        .map(|(_, value)| value)
        .ok_or_else(|| "GitHub callback did not include installation_id".to_string())
}

fn wait_for_callback_query(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let parsed_redirect =
        Url::parse(redirect_uri).map_err(|error| format!("Invalid redirect URI: {error}"))?;
    let expected_path = parsed_redirect.path().to_string();
    let expected_origin = loopback_origin(&parsed_redirect)?;
    let deadline = Instant::now() + OAUTH_CALLBACK_TIMEOUT;

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0u8; 8192];
                let bytes_read = stream
                    .read(&mut buffer)
                    .map_err(|error| format!("Failed to read OAuth callback: {error}"))?;
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                let response;
                let result = parse_callback_request(
                    &request,
                    &expected_path,
                    expected_state,
                    &expected_origin,
                );
                match result {
                    Ok(query) => {
                        response = html_response(
                            200,
                            "Authorization complete. You can close this window.",
                        );
                        let _ = stream.write_all(response.as_bytes());
                        return Ok(query);
                    }
                    Err(error) => {
                        response = html_response(400, &error);
                        let _ = stream.write_all(response.as_bytes());
                        return Err(error);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("OAuth callback timed out after 5 minutes".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("Failed to accept OAuth callback: {error}")),
        }
    }
}

fn parse_callback_request(
    request: &str,
    expected_path: &str,
    expected_state: Option<&str>,
    expected_origin: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut lines = request.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "OAuth callback request was empty".to_string())?;
    let path_and_query = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "OAuth callback request line was invalid".to_string())?;
    let origin = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("origin")
            .then(|| value.trim().to_string())
    });
    if let Some(origin) = origin {
        if origin != expected_origin && origin != "null" {
            return Err("OAuth callback Origin header did not match the redirect URI".to_string());
        }
    }

    let callback_url = Url::parse(&format!("{expected_origin}{path_and_query}"))
        .map_err(|error| format!("OAuth callback URL was invalid: {error}"))?;
    if callback_url.path() != expected_path {
        return Err("Invalid callback path".to_string());
    }

    let query = callback_url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if let Some(expected_state) = expected_state {
        let actual_state = query
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.as_str());
        if actual_state != Some(expected_state) {
            return Err("State mismatch".to_string());
        }
    }
    if let Some((_, error)) = query.iter().find(|(key, _)| key == "error") {
        return Err(format!("Authorization failed: {error}"));
    }
    Ok(query)
}

fn loopback_origin(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    Ok(format!("{}://{}:{}", url.scheme(), host, port))
}

async fn discover_oauth_server(resource: &str) -> Result<OAuthDiscoveryResult, String> {
    let resource_url =
        Url::parse(resource).map_err(|error| format!("Invalid resource URL: {error}"))?;
    let resource_metadata =
        fetch_first_json::<OAuthProtectedResource>(&protected_resource_candidates(&resource_url))
            .await?
            .ok_or_else(|| "OAuth protected resource metadata not found".to_string())?;
    let auth_server_url = resource_metadata
        .authorization_servers
        .first()
        .cloned()
        .ok_or_else(|| {
            "OAuth protected resource metadata did not advertise an authorization server"
                .to_string()
        })?;
    let auth_server = Url::parse(&auth_server_url).map_err(|error| {
        format!("Invalid authorization server URL '{auth_server_url}': {error}")
    })?;
    let metadata =
        fetch_first_json::<OAuthServerMetadata>(&authorization_server_candidates(&auth_server))
            .await?
            .ok_or_else(|| "Authorization server metadata not found".to_string())?;
    Ok(OAuthDiscoveryResult { metadata })
}

fn protected_resource_candidates(resource_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let path = resource_url
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/');
    if !path.is_empty() {
        let mut url = resource_url.clone();
        url.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
        url.set_query(None);
        url.set_fragment(None);
        urls.push(url);
    }
    let mut root = resource_url.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    root.set_fragment(None);
    urls.push(root);
    urls
}

fn authorization_server_candidates(auth_server_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let path = auth_server_url.path().trim_end_matches('/');
    if !path.is_empty() && path != "/" {
        let trimmed = path.trim_start_matches('/');
        let mut oauth = auth_server_url.clone();
        oauth.set_path(&format!(
            "/.well-known/oauth-authorization-server/{trimmed}"
        ));
        oauth.set_query(None);
        oauth.set_fragment(None);
        urls.push(oauth);

        let mut oidc = auth_server_url.clone();
        oidc.set_path(&format!("/.well-known/openid-configuration/{trimmed}"));
        oidc.set_query(None);
        oidc.set_fragment(None);
        urls.push(oidc);
    }

    let mut oauth = auth_server_url.clone();
    oauth.set_path("/.well-known/oauth-authorization-server");
    oauth.set_query(None);
    oauth.set_fragment(None);
    urls.push(oauth);

    let mut oidc = auth_server_url.clone();
    oidc.set_path("/.well-known/openid-configuration");
    oidc.set_query(None);
    oidc.set_fragment(None);
    urls.push(oidc);
    urls
}

async fn fetch_first_json<T: for<'de> Deserialize<'de>>(
    candidates: &[Url],
) -> Result<Option<T>, String> {
    let client = reqwest::Client::new();
    for candidate in candidates {
        let response = match client.get(candidate.clone()).send().await {
            Ok(response) => response,
            Err(_) => continue,
        };
        if !response.status().is_success() {
            continue;
        }
        let parsed = response
            .json::<T>()
            .await
            .map_err(|error| format!("Failed to parse {}: {error}", candidate))?;
        return Ok(Some(parsed));
    }
    Ok(None)
}

fn ensure_pkce_support(metadata: &OAuthServerMetadata) -> Result<(), String> {
    let methods = &metadata.code_challenge_methods_supported;
    if methods.is_empty() || methods.iter().any(|method| method == "S256") {
        return Ok(());
    }
    Err("Authorization server does not advertise PKCE S256 support".to_string())
}

async fn dynamic_client_registration(
    registration_endpoint: &str,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> Result<DynamicClientRegistrationResponse, String> {
    let client = reqwest::Client::new();
    let mut body = serde_json::json!({
        "client_name": "Harn CLI",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(scopes) = scopes {
        body["scope"] = serde_json::json!(scopes);
    }
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

fn determine_token_auth_method(
    metadata: &OAuthServerMetadata,
    client_secret: Option<&String>,
) -> Result<String, String> {
    let methods = &metadata.token_endpoint_auth_methods_supported;
    if client_secret.is_some() {
        if methods.is_empty() || methods.iter().any(|method| method == "client_secret_post") {
            return Ok("client_secret_post".to_string());
        }
        if methods.iter().any(|method| method == "client_secret_basic") {
            return Ok("client_secret_basic".to_string());
        }
        return Err(
            "Authorization server does not support client_secret_post or client_secret_basic"
                .to_string(),
        );
    }

    if methods.is_empty() || methods.iter().any(|method| method == "none") {
        return Ok("none".to_string());
    }
    Err("Authorization server requires client authentication. Supply --client-secret or configure a registered client.".to_string())
}

fn validate_token_auth_method(method: &str) -> Result<(), String> {
    match method {
        "none" | "client_secret_post" | "client_secret_basic" => Ok(()),
        other => Err(format!(
            "unsupported token auth method '{other}'; expected none, client_secret_post, or client_secret_basic"
        )),
    }
}

fn build_authorization_url(
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

async fn exchange_authorization_code(
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

struct AuthorizationCodeExchange<'a> {
    client_id: &'a str,
    client_secret: Option<&'a str>,
    token_auth_method: &'a str,
    redirect_uri: &'a str,
    resource: &'a str,
    scopes: Option<&'a str>,
    code: &'a str,
    code_verifier: &'a str,
}

async fn request_token(
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

fn generate_pkce_pair() -> (String, String) {
    let verifier = random_hex(32);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn random_hex(bytes: usize) -> String {
    let raw: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    raw.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn connect_secret_provider() -> Result<KeyringSecretProvider, String> {
    let manifest_dir = resolve_manifest_path(None)
        .map(|(_, dir)| dir)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Ok(KeyringSecretProvider::new(secret_namespace_for(
        &manifest_dir,
    )))
}

async fn save_connector_token(token: &StoredConnectorToken) -> Result<(), String> {
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

async fn load_connector_token(provider_name: &str) -> Result<StoredConnectorToken, String> {
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

fn connector_oauth_token_id(provider: &str) -> SecretId {
    SecretId::new(provider.to_string(), "oauth-token")
}

fn connector_secret_ids(provider: &str) -> Vec<SecretId> {
    vec![
        SecretId::new(provider.to_string(), "oauth-token"),
        SecretId::new(provider.to_string(), "access-token"),
        SecretId::new(provider.to_string(), "refresh-token"),
    ]
}

async fn load_connect_index(provider: &KeyringSecretProvider) -> Result<ConnectIndex, String> {
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

async fn save_connect_index(
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

async fn upsert_index_entry(
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

async fn remove_index_entry(
    provider: &KeyringSecretProvider,
    provider_name: &str,
) -> Result<(), String> {
    let mut index = load_connect_index(provider).await?;
    index
        .providers
        .retain(|item| item.provider != provider_name);
    save_connect_index(provider, &index).await
}

fn connect_index_id() -> SecretId {
    SecretId::new(CONNECT_INDEX_NAMESPACE, CONNECT_INDEX_NAME)
}

fn connector_token_summary(token: &StoredConnectorToken) -> JsonValue {
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

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn format_expiry(unix: i64) -> String {
    unix.to_string()
}

fn html_response(status: u16, message: &str) -> String {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        _ => "HTTP/1.1 404 Not Found",
    };
    let title = if status == 200 {
        "Authorization Complete"
    } else {
        "Authorization Failed"
    };
    format!(
        "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body><h1>{title}</h1><p>{message}</p></body></html>"
    )
}

struct OAuthDiscoveryResult {
    metadata: OAuthServerMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_linear_resource_types_from_trigger_events() {
        let manifest = package::ResolvedTriggerConfig {
            id: "linear-issues".to_string(),
            kind: package::TriggerKind::Webhook,
            provider: harn_vm::ProviderId::from("linear"),
            autonomy_tier: harn_vm::AutonomyTier::Shadow,
            match_: package::TriggerMatchExpr {
                events: vec!["issue.update".to_string(), "comment.create".to_string()],
                extra: Default::default(),
            },
            when: None,
            when_budget: None,
            handler: "handlers::on_linear".to_string(),
            dedupe_key: None,
            retry: package::TriggerRetrySpec::default(),
            dispatch_priority: package::TriggerDispatchPriority::Normal,
            budget: package::TriggerBudgetSpec::default(),
            concurrency: None,
            throttle: None,
            rate_limit: None,
            debounce: None,
            singleton: None,
            batch: None,
            window: None,
            priority_flow: None,
            secrets: Default::default(),
            filter: None,
            kind_specific: Default::default(),
            manifest_dir: PathBuf::from("/tmp"),
            manifest_path: PathBuf::from("/tmp/harn.toml"),
            package_name: None,
            exports: Default::default(),
            table_index: 0,
            shape_error: None,
        };
        let resource_types = derive_linear_resource_types(&[manifest]).expect("resource types");
        assert_eq!(
            resource_types,
            vec!["Comment".to_string(), "Issue".to_string()]
        );
    }

    #[test]
    fn linear_resource_type_mapping_covers_customer_request() {
        assert_eq!(
            linear_resource_type_for_event("customer_request.update"),
            Some("CustomerRequest")
        );
    }

    #[test]
    fn authorization_url_includes_pkce_and_resource_indicator() {
        let url = build_authorization_url(
            "https://auth.example.com/oauth/authorize",
            "client",
            "http://127.0.0.1:49152/oauth/callback",
            "state",
            "challenge",
            "https://api.example.com/resource",
            Some("read write"),
        )
        .expect("authorization URL");
        let pairs = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(pairs.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(pairs.get("code_challenge").unwrap(), "challenge");
        assert_eq!(
            pairs.get("resource").unwrap(),
            "https://api.example.com/resource"
        );
        assert_eq!(pairs.get("scope").unwrap(), "read write");
    }

    #[test]
    fn loopback_listener_rewrites_zero_port() {
        let (_listener, redirect_uri) =
            bind_loopback_listener("http://127.0.0.1:0/oauth/callback").expect("loopback listener");
        let parsed = Url::parse(&redirect_uri).unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_ne!(parsed.port(), Some(0));
        assert_eq!(parsed.path(), "/oauth/callback");
    }

    #[test]
    fn callback_request_rejects_wrong_origin() {
        let request = "GET /oauth/callback?code=abc&state=xyz HTTP/1.1\r\nOrigin: http://evil.example\r\n\r\n";
        let error = parse_callback_request(
            request,
            "/oauth/callback",
            Some("xyz"),
            "http://127.0.0.1:49152",
        )
        .unwrap_err();
        assert!(error.contains("Origin"));
    }

    #[test]
    fn github_install_url_adds_state() {
        let args = ConnectGithubArgs {
            app_slug: Some("harn-test".to_string()),
            app_id: None,
            installation_id: None,
            install_url: None,
            redirect_uri: "http://127.0.0.1:0/gh-install-callback".to_string(),
            private_key_file: None,
            webhook_secret: None,
            webhook_secret_file: None,
            no_open: true,
            json: false,
        };
        let url = github_install_url(&args, "state123").expect("install URL");
        assert_eq!(
            url.as_str(),
            "https://github.com/apps/harn-test/installations/new?state=state123"
        );
    }
}
