use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::{env, fs, process};

use async_trait::async_trait;
use base64::Engine;
use fs2::FileExt;
use harn_vm::mcp_auth::{
    authorization_code_token_form, build_oauth_authorization_url, canonical_resource_indicator,
    determine_token_endpoint_auth_method, discover_mcp_oauth, dynamic_client_registration_body,
    ensure_pkce_s256_supported, refresh_token_form, select_client_registration_mode,
    validate_authorization_response_issuer, validate_issuer_binding,
    validate_token_endpoint_auth_method, OAuthAuthorizationCodeTokenForm,
    OAuthAuthorizationUrlOptions, OAuthClientRegistrationMode, OAuthClientRegistrationOptions,
    OAuthRefreshTokenForm,
};
use harn_vm::secrets::{KeyringSecretProvider, SecretBytes, SecretId, SecretProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::cli::{McpCommand, McpLoginArgs, McpServerRefArgs};
use crate::package::{self, McpServerConfig};

mod oauth_resource;
pub(crate) mod presets;
pub(crate) mod serve;

const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:9783/oauth/callback";
const KEYRING_SERVICE: &str = "dev.harn.mcp";
const OAUTH_LOCK_DIR_ENV: &str = "HARN_MCP_OAUTH_LOCK_DIR";
const HARN_HOME_ENV: &str = "HARN_HOME";
const TOKEN_REFRESH_SKEW_SECS: i64 = 60;
use harn_vm::mcp_protocol::PROTOCOL_VERSION as MCP_PROTOCOL_VERSION;

#[derive(Clone)]
pub(crate) struct ResolvedMcpServer {
    pub name: String,
    pub url: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredOAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: Option<i64>,
    pub token_endpoint: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint_auth_method: String,
    pub issuer: String,
    pub resource: String,
    pub scopes: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OAuthTokenStoreKey {
    resource: String,
    issuer: String,
    client_id: String,
}

impl OAuthTokenStoreKey {
    fn new(resource: &str, issuer: &str, client_id: &str) -> Self {
        Self {
            resource: resource.to_string(),
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
        }
    }

    fn from_token(token: &StoredOAuthToken) -> Self {
        Self::new(&token.resource, &token.issuer, &token.client_id)
    }

    fn account(&self) -> String {
        token_store_account(&self.resource, &self.issuer, &self.client_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOAuthClientIndex {
    client_id: String,
}

#[async_trait]
trait OAuthTokenStorage: Send + Sync {
    async fn load_token(
        &self,
        key: &OAuthTokenStoreKey,
    ) -> Result<Option<StoredOAuthToken>, String>;
    async fn save_token(&self, token: &StoredOAuthToken) -> Result<(), String>;
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

struct KeyringOAuthTokenStorage {
    provider: KeyringSecretProvider,
}

impl Default for KeyringOAuthTokenStorage {
    fn default() -> Self {
        Self {
            provider: oauth_token_provider(),
        }
    }
}

type OAuthServerMetadata = harn_vm::mcp_auth::OAuthAuthorizationServerMetadata;
type DynamicClientRegistrationResponse = harn_vm::mcp_auth::OAuthDynamicClientRegistrationResponse;

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

pub(crate) enum AuthResolution {
    None,
    Bearer(String),
}

pub(crate) async fn handle_mcp_command(command: &McpCommand) {
    match command {
        McpCommand::Serve(args) => {
            if let Err(error) = serve::run(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        McpCommand::Login(options) => {
            if let Err(error) = login(options).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        McpCommand::Logout(server_ref) => {
            let server = resolve_server_reference(server_ref).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            let discovery = discover_oauth_server(&server.url)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            let resource = canonical_server_resource(&server.url).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            delete_stored_token(&resource, &discovery.issuer, server.client_id.as_deref())
                .await
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            println!(
                "Removed stored OAuth token for {} ({})",
                server.name, server.url
            );
        }
        McpCommand::Status(server_ref) => {
            // With no target/url, report every configured MCP server.
            // With a target, keep the focused per-server OAuth detail.
            if server_ref.target.is_none() && server_ref.url.is_none() {
                if let Err(error) = run_mcp_status_report(server_ref.json).await {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
                return;
            }
            let server = resolve_server_reference(server_ref).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            let discovery = discover_oauth_server(&server.url)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            let resource = canonical_server_resource(&server.url).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            match load_stored_token(&resource, &discovery.issuer, server.client_id.as_deref()).await
            {
                Ok(Some(token)) => {
                    println!("Server: {}", server.name);
                    println!("URL: {}", server.url);
                    println!("Connected: yes");
                    println!("Protocol: {MCP_PROTOCOL_VERSION}");
                    println!(
                        "Expires: {}",
                        token
                            .expires_at_unix
                            .map(format_expiry)
                            .unwrap_or_else(|| "unknown".to_string())
                    );
                    println!("Client ID: {}", token.client_id);
                    println!("Token auth method: {}", token.token_endpoint_auth_method);
                    println!("Issuer: {}", token.issuer);
                }
                Ok(None) => {
                    println!("Server: {}", server.name);
                    println!("URL: {}", server.url);
                    println!("Connected: no");
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
        }
        McpCommand::RedirectUri => {
            println!("{DEFAULT_REDIRECT_URI}");
        }
        McpCommand::Presets(args) => {
            presets::run(args);
        }
    }
}

/// Schema version for `harn mcp status --json`. Bump when the
/// `McpStatusReport` shape changes in a way agents must detect.
pub(crate) const MCP_STATUS_SCHEMA_VERSION: u32 = 1;

/// Aggregate readiness report for every MCP server declared in the
/// nearest `harn.toml`. Mirrors the `connect status` report shape: a
/// versioned envelope with the resolving manifest path plus one entry
/// per server, sorted by name for stable diffs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct McpStatusReport {
    pub schema_version: u32,
    /// Absolute path of the `harn.toml` that declared these servers,
    /// or `None` when no manifest was found in the cwd ancestry.
    pub manifest: Option<String>,
    pub servers: Vec<McpServerStatus>,
}

/// One MCP server's configured shape and derived connection state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct McpServerStatus {
    pub name: String,
    /// `"stdio"` or `"http"` — the configured transport.
    pub transport: String,
    /// Derived connection state: `connected`, `disconnected`,
    /// `auth_required`, or `error`.
    pub state: String,
    /// Remote URL for HTTP transports; empty for stdio.
    pub url: String,
    /// `true` when the server boots lazily (on first use) rather than
    /// eagerly at session start.
    pub lazy: bool,
    /// Count of tools advertised by the server, when a live handle is
    /// available in the process registry; `None` otherwise (the
    /// standalone CLI does not boot servers to probe them).
    pub tools: Option<usize>,
    /// Resource count when known; see `tools`.
    pub resources: Option<usize>,
    /// Prompt count when known; see `tools`.
    pub prompts: Option<usize>,
    /// Human-readable diagnostic when `state` is `error`, else `None`.
    pub last_error: Option<String>,
}

/// Build and print the all-servers MCP status report.
async fn run_mcp_status_report(json: bool) -> Result<(), String> {
    let report = mcp_status_report().await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to encode JSON output: {error}"))?
        );
    } else if report.servers.is_empty() {
        println!("No [[mcp]] servers declared in the nearest harn.toml.");
    } else {
        for server in &report.servers {
            let counts = match (server.tools, server.resources, server.prompts) {
                (Some(t), Some(r), Some(p)) => format!("tools={t} resources={r} prompts={p}"),
                _ => "tools=? resources=? prompts=?".to_string(),
            };
            let mut line = format!(
                "{}\t{}\t{}\t{}",
                server.name, server.transport, server.state, counts
            );
            if let Some(error) = &server.last_error {
                line.push_str(&format!("\terror={error}"));
            }
            println!("{line}");
        }
    }
    Ok(())
}

/// Assemble the report by reading the nearest manifest's `[[mcp]]`
/// entries and deriving each server's state. HTTP servers that rely on
/// OAuth probe local stored-token state (no network beyond discovery,
/// which the OAuth helpers already perform); stdio servers report as
/// `connected` since they have no auth gate. Live tool/resource/prompt
/// counts are filled from the in-process registry when a handle exists
/// (e.g. when invoked inside a running session); otherwise `None`.
pub(crate) async fn mcp_status_report() -> McpStatusReport {
    let (manifest_path, servers) = match find_manifest() {
        Ok((path, manifest)) => (Some(path.display().to_string()), manifest.mcp),
        Err(_) => (None, Vec::new()),
    };

    let mut entries = Vec::with_capacity(servers.len());
    for server in servers {
        entries.push(derive_server_status(&server).await);
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    McpStatusReport {
        schema_version: MCP_STATUS_SCHEMA_VERSION,
        manifest: manifest_path,
        servers: entries,
    }
}

async fn derive_server_status(server: &McpServerConfig) -> McpServerStatus {
    let transport = server
        .transport
        .clone()
        .unwrap_or_else(|| "stdio".to_string());
    let registry = harn_vm::mcp_snapshot_status()
        .into_iter()
        .find(|entry| entry.name == server.name);
    let active = registry.as_ref().map(|entry| entry.active).unwrap_or(false);

    let mut last_error = None;
    let state = if transport == "http" && !server.url.is_empty() {
        // A configured static bearer token is treated as connected; an
        // OAuth server is `auth_required` until a token is stored.
        if server.auth_token.as_deref().is_some_and(|t| !t.is_empty()) {
            "connected".to_string()
        } else {
            match resolve_auth_for_server(server).await {
                Ok(AuthResolution::Bearer(_)) => "connected".to_string(),
                Ok(AuthResolution::None) => "auth_required".to_string(),
                Err(error) => {
                    last_error = Some(error);
                    "error".to_string()
                }
            }
        }
    } else if active {
        // Stdio (or already-booted) server with a live handle.
        "connected".to_string()
    } else {
        // Declared stdio server with no live handle in this process.
        "disconnected".to_string()
    };

    McpServerStatus {
        name: server.name.clone(),
        transport,
        state,
        url: server.url.clone(),
        lazy: server.lazy,
        // The standalone CLI does not boot servers, so counts are only
        // known when a live handle is already registered in-process.
        tools: None,
        resources: None,
        prompts: None,
        last_error,
    }
}

pub(crate) async fn resolve_auth_for_server(
    server: &McpServerConfig,
) -> Result<AuthResolution, String> {
    if let Some(token) = &server.auth_token {
        if !token.is_empty() {
            return Ok(AuthResolution::Bearer(token.clone()));
        }
    }

    let transport = server.transport.as_deref().unwrap_or("stdio");
    if transport != "http" || server.url.is_empty() {
        return Ok(AuthResolution::None);
    }

    let discovery = discover_oauth_server(&server.url).await?;
    let resource = canonical_server_resource(&server.url)?;
    let store = KeyringOAuthTokenStorage::default();
    let Some(mut stored) = load_stored_token_from_store(
        &store,
        &resource,
        &discovery.issuer,
        server.client_id.as_deref(),
    )
    .await?
    else {
        return Ok(AuthResolution::None);
    };
    validate_issuer_binding(&stored.issuer, &discovery.issuer)?;

    if token_needs_refresh(&stored) {
        stored = refresh_stored_token_with_store(&store, &stored, &discovery, None).await?;
    }

    Ok(AuthResolution::Bearer(stored.access_token))
}

async fn login(options: &McpLoginArgs) -> Result<(), String> {
    let server = resolve_server_reference(&McpServerRefArgs {
        target: options.target.clone(),
        url: options.url.clone(),
        json: false,
    })?;
    // RFC 8707 resource indicator: the MCP server's canonical URI, sent in both
    // the authorization and token requests regardless of AS advertisement.
    let resource = canonical_server_resource(&server.url)?;
    let discovery = discover_oauth_server(&server.url).await?;
    ensure_pkce_support(&discovery.metadata)?;
    let requested_scopes = options
        .scope
        .clone()
        .or(server.scopes.clone())
        .or_else(|| (!discovery.scopes.is_empty()).then(|| discovery.scopes.join(" ")));

    let configured_client_id = options.client_id.clone().or(server.client_id.clone());
    let configured_client_secret = options
        .client_secret
        .clone()
        .or(server.client_secret.clone());
    let registration_mode = select_client_registration_mode(
        &discovery.metadata,
        OAuthClientRegistrationOptions {
            client_id: configured_client_id.as_deref(),
            client_secret: configured_client_secret.as_deref(),
            client_id_metadata_document_url: configured_client_id.as_deref(),
        },
    );
    let (client_id, client_secret, token_auth_method) = if let Some(client_id) =
        configured_client_id.clone()
    {
        let token_auth_method =
            determine_token_auth_method(&discovery.metadata, configured_client_secret.as_ref())?;
        (client_id, configured_client_secret, token_auth_method)
    } else if registration_mode == OAuthClientRegistrationMode::DynamicClientRegistration {
        let registration_endpoint = discovery
            .metadata
            .registration_endpoint
            .as_deref()
            .ok_or_else(|| "dynamic client registration endpoint missing".to_string())?;
        let registration = dynamic_client_registration(
            registration_endpoint,
            &options.redirect_uri,
            requested_scopes.as_deref(),
        )
        .await?;
        let auth_method = registration
            .token_endpoint_auth_method
            .clone()
            .unwrap_or_else(|| "none".to_string());
        (
            registration.client_id,
            registration.client_secret,
            auth_method,
        )
    } else {
        return Err(
            "No client_id available. Supply --client-id, use a Client ID Metadata Document URL as --client-id when supported, or use a server that supports dynamic client registration.".to_string()
        );
    };

    let (code_verifier, code_challenge) = generate_pkce_pair();
    let state = random_hex(16);
    let callback_listener = bind_callback_listener(&options.redirect_uri)?;

    let auth_url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
        authorization_endpoint: &discovery.metadata.authorization_endpoint,
        client_id: &client_id,
        redirect_uri: &options.redirect_uri,
        state: &state,
        code_challenge: &code_challenge,
        resource: &resource,
        scopes: requested_scopes.as_deref(),
    })?;

    println!("Server: {} ({})", server.name, server.url);
    println!("Redirect URI: {}", options.redirect_uri);
    println!("Protocol Version: {MCP_PROTOCOL_VERSION}");
    println!("Opening browser for OAuth authorization...");

    if webbrowser::open(auth_url.as_str()).is_err() {
        println!("Open this URL manually:\n{auth_url}");
    }

    let callback = wait_for_oauth_response(callback_listener, &options.redirect_uri, &state)?;
    validate_authorization_response_issuer(&discovery.metadata, callback.issuer.as_deref())?;
    let token = exchange_authorization_code(
        &discovery.metadata,
        AuthorizationCodeExchange {
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            token_auth_method: &token_auth_method,
            redirect_uri: &options.redirect_uri,
            resource: &resource,
            scopes: requested_scopes.as_deref(),
            code: &callback.code,
            code_verifier: &code_verifier,
        },
    )
    .await?;

    let stored = StoredOAuthToken {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_unix: token
            .expires_in
            .map(|seconds| current_unix_timestamp().saturating_add(seconds)),
        token_endpoint: discovery.metadata.token_endpoint.clone(),
        client_id,
        client_secret,
        token_endpoint_auth_method: token_auth_method,
        issuer: discovery.issuer,
        resource,
        scopes: requested_scopes,
    };
    save_stored_token(&stored).await?;
    println!("OAuth token stored for {}.", server.name);
    Ok(())
}

fn resolve_server_reference(server_ref: &McpServerRefArgs) -> Result<ResolvedMcpServer, String> {
    if let Some(url) = &server_ref.url {
        return Ok(ResolvedMcpServer {
            name: server_ref
                .target
                .clone()
                .unwrap_or_else(|| infer_name_from_url(url)),
            url: url.clone(),
            client_id: None,
            client_secret: None,
            scopes: None,
        });
    }

    let target = server_ref
        .target
        .as_ref()
        .ok_or_else(|| "Missing MCP server name or URL".to_string())?;
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(ResolvedMcpServer {
            name: infer_name_from_url(target),
            url: target.clone(),
            client_id: None,
            client_secret: None,
            scopes: None,
        });
    }

    let (_, manifest) = find_manifest()?;
    let server = manifest
        .mcp
        .into_iter()
        .find(|entry| entry.name == *target)
        .ok_or_else(|| format!("No [[mcp]] entry named '{target}' in the nearest harn.toml"))?;
    if server.url.is_empty() {
        return Err(format!(
            "MCP server '{target}' does not define a remote URL. Use --url for ad hoc login or add url = ... to harn.toml."
        ));
    }

    Ok(ResolvedMcpServer {
        name: server.name,
        url: server.url,
        client_id: server.client_id,
        client_secret: server.client_secret,
        scopes: server.scopes,
    })
}

fn find_manifest() -> Result<(PathBuf, package::Manifest), String> {
    let mut dir =
        env::current_dir().map_err(|error| format!("Failed to read current directory: {error}"))?;
    loop {
        let manifest_path = dir.join("harn.toml");
        if manifest_path.is_file() {
            let content = fs::read_to_string(&manifest_path)
                .map_err(|error| format!("Failed to read {}: {error}", manifest_path.display()))?;
            let manifest = toml::from_str::<package::Manifest>(&content)
                .map_err(|error| format!("Failed to parse {}: {error}", manifest_path.display()))?;
            return Ok((manifest_path, manifest));
        }
        if !dir.pop() {
            break;
        }
    }
    Err("No harn.toml found in the current directory or its parents".to_string())
}

async fn discover_oauth_server(server_url: &str) -> Result<OAuthDiscoveryResult, String> {
    let client = reqwest::Client::new();
    let discovery = discover_mcp_oauth(&client, server_url)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OAuthDiscoveryResult {
        metadata: discovery.authorization_server_metadata,
        issuer: discovery.authorization_server_issuer,
        scopes: discovery.scopes,
    })
}

fn canonical_server_resource(server_url: &str) -> Result<String, String> {
    canonical_resource_indicator(server_url).map_err(|error| error.to_string())
}

fn ensure_pkce_support(metadata: &OAuthServerMetadata) -> Result<(), String> {
    ensure_pkce_s256_supported(metadata)
}

async fn dynamic_client_registration(
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

fn determine_token_auth_method(
    metadata: &OAuthServerMetadata,
    client_secret: Option<&String>,
) -> Result<String, String> {
    determine_token_endpoint_auth_method(metadata, client_secret.map(String::as_str))
}

fn bind_callback_listener(redirect_uri: &str) -> Result<TcpListener, String> {
    let parsed =
        Url::parse(redirect_uri).map_err(|error| format!("Invalid redirect URI: {error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "Redirect URI must include a host".to_string())?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "Redirect URI must include a port".to_string())?;
    let listener = TcpListener::bind((host, port))
        .map_err(|error| format!("Failed to bind redirect URI {redirect_uri}: {error}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|error| format!("Failed to configure redirect listener: {error}"))?;
    Ok(listener)
}

struct OAuthCallbackResponse {
    code: String,
    issuer: Option<String>,
}

fn wait_for_oauth_response(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<OAuthCallbackResponse, String> {
    let expected_path = Url::parse(redirect_uri)
        .map_err(|error| format!("Invalid redirect URI: {error}"))?
        .path()
        .to_string();

    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("Failed to accept OAuth callback: {error}"))?;
    let mut buffer = [0u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .map_err(|error| format!("Failed to read OAuth callback: {error}"))?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| "OAuth callback request was empty".to_string())?;
    let path_and_query = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "OAuth callback request line was invalid".to_string())?;
    let callback_url = Url::parse(&format!("http://127.0.0.1{path_and_query}"))
        .map_err(|error| format!("OAuth callback URL was invalid: {error}"))?;

    let response = if callback_url.path() != expected_path {
        html_response(404, "Invalid callback path")
    } else if callback_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .as_deref()
        != Some(expected_state)
    {
        html_response(400, "State mismatch")
    } else if let Some(error) = callback_url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .map(|(_, value)| value.into_owned())
    {
        let _ = stream
            .write_all(html_response(400, &format!("Authorization failed: {error}")).as_bytes());
        return Err(format!("Authorization failed: {error}"));
    } else {
        html_response(200, "Authorization complete. You can close this window.")
    };
    let _ = stream.write_all(response.as_bytes());

    let code = callback_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())?;
    let issuer = callback_url
        .query_pairs()
        .find(|(key, _)| key == "iss")
        .map(|(_, value)| value.into_owned());
    Ok(OAuthCallbackResponse { code, issuer })
}

async fn exchange_authorization_code(
    metadata: &OAuthServerMetadata,
    request: AuthorizationCodeExchange<'_>,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let form = authorization_code_token_form(OAuthAuthorizationCodeTokenForm {
        client_id: request.client_id,
        redirect_uri: request.redirect_uri,
        code: request.code,
        code_verifier: request.code_verifier,
        resource: request.resource,
        scopes: request.scopes,
    });
    request_token(
        &client,
        &metadata.token_endpoint,
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

async fn refresh_token_if_needed(
    token: &StoredOAuthToken,
    discovery: &OAuthDiscoveryResult,
) -> Result<StoredOAuthToken, String> {
    if !token_needs_refresh(token) {
        return Ok(token.clone());
    }
    validate_issuer_binding(&token.issuer, &discovery.issuer)?;

    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        "Stored OAuth token has expired and does not include a refresh token".to_string()
    })?;
    // Re-send the RFC 8707 resource indicator on refresh and keep the stored
    // key aligned with the canonical resource.
    let resource =
        canonical_resource_indicator(&token.resource).unwrap_or_else(|_| token.resource.clone());
    let client = reqwest::Client::new();
    let form = refresh_token_form(OAuthRefreshTokenForm {
        client_id: &token.client_id,
        refresh_token: &refresh_token,
        resource: &resource,
    });
    let refreshed = request_token(
        &client,
        &discovery.metadata.token_endpoint,
        &token.token_endpoint_auth_method,
        &token.client_id,
        token.client_secret.as_deref(),
        &form,
    )
    .await?;
    Ok(StoredOAuthToken {
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .or_else(|| token.refresh_token.clone()),
        expires_at_unix: refreshed
            .expires_in
            .map(|seconds| current_unix_timestamp().saturating_add(seconds)),
        token_endpoint: discovery.metadata.token_endpoint.clone(),
        client_id: token.client_id.clone(),
        client_secret: token.client_secret.clone(),
        token_endpoint_auth_method: token.token_endpoint_auth_method.clone(),
        issuer: token.issuer.clone(),
        resource,
        scopes: token.scopes.clone(),
    })
}

async fn request_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    token_auth_method: &str,
    client_id: &str,
    client_secret: Option<&str>,
    form: &[(&str, String)],
) -> Result<TokenResponse, String> {
    validate_token_endpoint_auth_method(token_auth_method)?;
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
    response
        .json::<TokenResponse>()
        .await
        .map_err(|error| format!("Invalid token response: {error}"))
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

fn token_needs_refresh(token: &StoredOAuthToken) -> bool {
    match token.expires_at_unix {
        Some(expires_at) => {
            expires_at <= current_unix_timestamp().saturating_add(TOKEN_REFRESH_SKEW_SECS)
        }
        None => false,
    }
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

async fn refresh_stored_token_with_store<S: OAuthTokenStorage + ?Sized>(
    store: &S,
    token: &StoredOAuthToken,
    discovery: &OAuthDiscoveryResult,
    lock_dir_override: Option<PathBuf>,
) -> Result<StoredOAuthToken, String> {
    let key = OAuthTokenStoreKey::from_token(token);
    let _guard = acquire_oauth_refresh_lock(&key, lock_dir_override.as_deref()).await?;
    let Some(current) = store.load_token(&key).await? else {
        return Err("Stored OAuth token disappeared before it could be refreshed".to_string());
    };
    validate_issuer_binding(&current.issuer, &discovery.issuer)?;
    if !token_needs_refresh(&current) {
        return Ok(current);
    }

    let refreshed = refresh_token_if_needed(&current, discovery).await?;
    store.save_token(&refreshed).await?;
    Ok(refreshed)
}

struct OAuthRefreshLockGuard {
    _async_guard: tokio::sync::OwnedMutexGuard<()>,
    file: fs::File,
}

impl Drop for OAuthRefreshLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

async fn acquire_oauth_refresh_lock(
    key: &OAuthTokenStoreKey,
    lock_dir_override: Option<&Path>,
) -> Result<OAuthRefreshLockGuard, String> {
    let mutex = oauth_refresh_mutex(key);
    let async_guard = mutex.lock_owned().await;
    let lock_path = oauth_refresh_lock_path(key, lock_dir_override);
    let file = tokio::task::spawn_blocking(move || {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create OAuth token lock directory `{}`: {error}",
                    parent.display()
                )
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "Failed to open OAuth token lock `{}`: {error}",
                    lock_path.display()
                )
            })?;
        file.lock_exclusive().map_err(|error| {
            format!(
                "Failed to acquire OAuth token lock `{}`: {error}",
                lock_path.display()
            )
        })?;
        Ok::<fs::File, String>(file)
    })
    .await
    .map_err(|error| format!("OAuth token lock task failed: {error}"))??;
    Ok(OAuthRefreshLockGuard {
        _async_guard: async_guard,
        file,
    })
}

fn oauth_refresh_mutex(key: &OAuthTokenStoreKey) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
        OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("OAuth refresh lock registry poisoned");
    locks
        .entry(key.account())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

fn oauth_refresh_lock_path(key: &OAuthTokenStoreKey, lock_dir_override: Option<&Path>) -> PathBuf {
    let dir = lock_dir_override
        .map(Path::to_path_buf)
        .unwrap_or_else(default_oauth_lock_dir);
    dir.join(format!("{}.lock", key.account()))
}

fn default_oauth_lock_dir() -> PathBuf {
    if let Some(path) = env::var_os(OAUTH_LOCK_DIR_ENV) {
        return PathBuf::from(path);
    }
    harn_home_dir().join("mcp-oauth-locks")
}

fn harn_home_dir() -> PathBuf {
    if let Some(path) = env::var_os(HARN_HOME_ENV) {
        return PathBuf::from(path);
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".harn");
    }
    if let Some(home) = env::var_os("USERPROFILE") {
        return PathBuf::from(home).join(".harn");
    }
    env::temp_dir().join("harn")
}

#[async_trait]
impl OAuthTokenStorage for KeyringOAuthTokenStorage {
    async fn load_token(
        &self,
        key: &OAuthTokenStoreKey,
    ) -> Result<Option<StoredOAuthToken>, String> {
        let payload = match self.provider.get(&token_secret_id(key)).await {
            Ok(secret) => secret,
            Err(harn_vm::secrets::SecretError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(format!("Failed to read OAuth token from keyring: {error}")),
        };
        let token = payload
            .with_exposed(|bytes| serde_json::from_slice::<StoredOAuthToken>(bytes))
            .map_err(|error| format!("Stored OAuth token was invalid JSON: {error}"))?;
        validate_token_store_binding(&token, key)?;
        Ok(Some(token))
    }

    async fn save_token(&self, token: &StoredOAuthToken) -> Result<(), String> {
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
            Err(harn_vm::secrets::SecretError::NotFound { .. }) => return Ok(None),
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

async fn save_stored_token(token: &StoredOAuthToken) -> Result<(), String> {
    let store = KeyringOAuthTokenStorage::default();
    let key = OAuthTokenStoreKey::from_token(token);
    let _guard = acquire_oauth_refresh_lock(&key, None).await?;
    store.save_token(token).await
}

async fn load_stored_token(
    resource: &str,
    issuer: &str,
    client_id_hint: Option<&str>,
) -> Result<Option<StoredOAuthToken>, String> {
    let store = KeyringOAuthTokenStorage::default();
    load_stored_token_from_store(&store, resource, issuer, client_id_hint).await
}

async fn load_stored_token_from_store<S: OAuthTokenStorage + ?Sized>(
    store: &S,
    resource: &str,
    issuer: &str,
    client_id_hint: Option<&str>,
) -> Result<Option<StoredOAuthToken>, String> {
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

async fn delete_stored_token(
    resource: &str,
    issuer: &str,
    client_id_hint: Option<&str>,
) -> Result<(), String> {
    let store = KeyringOAuthTokenStorage::default();
    let client_id = match client_id_hint {
        Some(client_id) => client_id.to_string(),
        None => match store.load_active_client_id(resource, issuer).await? {
            Some(client_id) => client_id,
            None => {
                store.delete_active_client_id(resource, issuer).await?;
                return Ok(());
            }
        },
    };
    let key = OAuthTokenStoreKey::new(resource, issuer, &client_id);
    let _guard = acquire_oauth_refresh_lock(&key, None).await?;
    store.delete_token(&key).await?;
    if store
        .load_active_client_id(resource, issuer)
        .await?
        .as_deref()
        == Some(client_id.as_str())
    {
        store.delete_active_client_id(resource, issuer).await?;
    }
    Ok(())
}

fn token_store_account(resource: &str, issuer: &str, client_id: &str) -> String {
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

fn oauth_token_provider() -> KeyringSecretProvider {
    KeyringSecretProvider::new(KEYRING_SERVICE)
}

fn token_secret_id(key: &OAuthTokenStoreKey) -> SecretId {
    SecretId::new("", key.account())
}

fn token_client_index_secret_id(resource: &str, issuer: &str) -> SecretId {
    SecretId::new("", token_client_index_account(resource, issuer))
}

fn validate_token_store_binding(
    token: &StoredOAuthToken,
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

fn format_expiry(unix: i64) -> String {
    unix.to_string()
}

fn infer_name_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_string()))
        .unwrap_or_else(|| "remote".to_string())
}

fn html_response(status: u16, message: &str) -> String {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        _ => "HTTP/1.1 404 Not Found",
    };
    let (title, accent, badge) = match status {
        200 => ("Authorization Complete", "#159f6b", "Connected"),
        400 => ("Authorization Failed", "#c76b19", "Retry Needed"),
        _ => ("Callback Error", "#b42318", "Invalid Request"),
    };
    let message = html_escape(message);
    format!(
        r#"{status_line}
Content-Type: text/html; charset=utf-8
Connection: close

<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ margin: 0; font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: radial-gradient(circle at top, rgba(21,159,107,.12), transparent 35%), #0f1115; color: #f5f7fa; min-height: 100vh; display: grid; place-items: center; }}
.card {{ width: min(560px, calc(100vw - 32px)); background: rgba(17, 24, 39, 0.88); border: 1px solid rgba(255,255,255,0.08); border-radius: 20px; padding: 28px; box-shadow: 0 24px 80px rgba(0,0,0,0.35); }}
.badge {{ display: inline-block; padding: 6px 10px; border-radius: 999px; background: {accent}; color: white; font-size: 12px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }}
h1 {{ margin: 16px 0 10px; font-size: 28px; line-height: 1.1; }}
p {{ margin: 0; color: #c6cfdb; font-size: 15px; line-height: 1.55; }}
.hint {{ margin-top: 18px; color: #98a4b3; font-size: 13px; }}
.dot {{ width: 14px; height: 14px; border-radius: 999px; background: {accent}; box-shadow: 0 0 0 8px rgba(255,255,255,0.06); }}
.row {{ display: flex; align-items: center; gap: 12px; margin-bottom: 10px; }}
</style>
</head>
<body>
<main class="card">
<div class="row"><div class="dot"></div><span class="badge">{badge}</span></div>
<h1>{title}</h1>
<p>{message}</p>
<p class="hint">You can close this tab and return to Harn.</p>
</main>
</body>
</html>"#
    )
}

fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[derive(Clone)]
struct OAuthDiscoveryResult {
    metadata: OAuthServerMetadata,
    issuer: String,
    scopes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Form, Json, Router};
    use harn_vm::mcp_auth::{
        authorization_server_metadata_candidates, protected_resource_metadata_candidates,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    #[test]
    fn protected_resource_candidate_prefers_path_specific_url() {
        let url = Url::parse("https://example.com/mcp/notion").unwrap();
        let candidates = protected_resource_metadata_candidates(&url);
        assert_eq!(
            candidates[0].as_str(),
            "https://example.com/.well-known/oauth-protected-resource/mcp/notion"
        );
        assert_eq!(
            candidates[1].as_str(),
            "https://example.com/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn authorization_server_candidate_prefers_path_specific_metadata() {
        let url = Url::parse("https://auth.example.com/oauth").unwrap();
        let candidates = authorization_server_metadata_candidates(&url);
        assert_eq!(
            candidates[0].url.as_str(),
            "https://auth.example.com/.well-known/oauth-authorization-server/oauth"
        );
        assert_eq!(
            candidates[1].url.as_str(),
            "https://auth.example.com/.well-known/openid-configuration/oauth"
        );
        assert_eq!(
            candidates[2].url.as_str(),
            "https://auth.example.com/oauth/.well-known/openid-configuration"
        );
    }

    #[test]
    fn token_store_account_is_stable() {
        let first =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-a");
        let second =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-a");
        let other_issuer = token_store_account(
            "https://mcp.notion.com",
            "https://other.example",
            "client-a",
        );
        let other_client =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-b");
        assert_eq!(first, second);
        assert_ne!(first, other_issuer);
        assert_ne!(first, other_client);
    }

    #[tokio::test]
    async fn expired_token_refresh_is_singleflight() {
        #[derive(Clone, Default)]
        struct MemoryOAuthTokenStorage {
            tokens: Arc<Mutex<StdHashMap<OAuthTokenStoreKey, StoredOAuthToken>>>,
            index: Arc<Mutex<StdHashMap<(String, String), String>>>,
        }

        #[async_trait]
        impl OAuthTokenStorage for MemoryOAuthTokenStorage {
            async fn load_token(
                &self,
                key: &OAuthTokenStoreKey,
            ) -> Result<Option<StoredOAuthToken>, String> {
                Ok(self.tokens.lock().await.get(key).cloned())
            }

            async fn save_token(&self, token: &StoredOAuthToken) -> Result<(), String> {
                self.tokens
                    .lock()
                    .await
                    .insert(OAuthTokenStoreKey::from_token(token), token.clone());
                self.save_active_client_id(&token.resource, &token.issuer, &token.client_id)
                    .await
            }

            async fn delete_token(&self, key: &OAuthTokenStoreKey) -> Result<(), String> {
                self.tokens.lock().await.remove(key);
                Ok(())
            }

            async fn load_active_client_id(
                &self,
                resource: &str,
                issuer: &str,
            ) -> Result<Option<String>, String> {
                Ok(self
                    .index
                    .lock()
                    .await
                    .get(&(resource.to_string(), issuer.to_string()))
                    .cloned())
            }

            async fn save_active_client_id(
                &self,
                resource: &str,
                issuer: &str,
                client_id: &str,
            ) -> Result<(), String> {
                self.index.lock().await.insert(
                    (resource.to_string(), issuer.to_string()),
                    client_id.to_string(),
                );
                Ok(())
            }

            async fn delete_active_client_id(
                &self,
                resource: &str,
                issuer: &str,
            ) -> Result<(), String> {
                self.index
                    .lock()
                    .await
                    .remove(&(resource.to_string(), issuer.to_string()));
                Ok(())
            }
        }

        #[derive(Clone)]
        struct TokenEndpointState {
            calls: Arc<AtomicUsize>,
        }

        async fn token_endpoint(
            State(state): State<TokenEndpointState>,
            Form(form): Form<StdHashMap<String, String>>,
        ) -> Json<serde_json::Value> {
            assert_eq!(
                form.get("grant_type").map(String::as_str),
                Some("refresh_token")
            );
            assert_eq!(
                form.get("refresh_token").map(String::as_str),
                Some("refresh-old")
            );
            state.calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "access_token": "access-new",
                "refresh_token": "refresh-new",
                "expires_in": 3600
            }))
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_endpoint_url = format!("http://{}/token", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/token", post(token_endpoint))
            .with_state(TokenEndpointState {
                calls: calls.clone(),
            });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let store = Arc::new(MemoryOAuthTokenStorage::default());
        let stale = StoredOAuthToken {
            access_token: "access-old".to_string(),
            refresh_token: Some("refresh-old".to_string()),
            expires_at_unix: Some(current_unix_timestamp().saturating_sub(1)),
            token_endpoint: token_endpoint_url.clone(),
            client_id: "client-a".to_string(),
            client_secret: None,
            token_endpoint_auth_method: "none".to_string(),
            issuer: "https://auth.example".to_string(),
            resource: "https://mcp.example/mcp".to_string(),
            scopes: None,
        };
        store.save_token(&stale).await.unwrap();

        let discovery = OAuthDiscoveryResult {
            metadata: OAuthServerMetadata {
                issuer: stale.issuer.clone(),
                authorization_endpoint: "https://auth.example/authorize".to_string(),
                token_endpoint: token_endpoint_url,
                registration_endpoint: None,
                token_endpoint_auth_methods_supported: vec!["none".to_string()],
                code_challenge_methods_supported: vec!["S256".to_string()],
                scopes_supported: Vec::new(),
                client_id_metadata_document_supported: false,
                authorization_response_iss_parameter_supported: false,
                extra: Default::default(),
            },
            issuer: stale.issuer.clone(),
            scopes: Vec::new(),
        };
        let lock_dir = tempfile::tempdir().unwrap();

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let token = stale.clone();
            let discovery = discovery.clone();
            let lock_dir = lock_dir.path().to_path_buf();
            tasks.push(tokio::spawn(async move {
                refresh_stored_token_with_store(store.as_ref(), &token, &discovery, Some(lock_dir))
                    .await
                    .unwrap()
            }));
        }

        for task in tasks {
            let refreshed = task.await.unwrap();
            assert_eq!(refreshed.access_token, "access-new");
            assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-new"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let stored = store
            .load_token(&OAuthTokenStoreKey::from_token(&stale))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, "access-new");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-new"));
        server.abort();
    }

    #[test]
    fn callback_html_response_escapes_message() {
        let response = html_response(400, "<script>alert('x')</script>&");
        assert!(response.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;"));
        assert!(!response.contains("<script>"));
    }

    #[test]
    fn authorization_url_includes_canonical_resource_indicator() {
        let resource = canonical_resource_indicator("https://MCP.Example.com:443/mcp/").unwrap();
        let url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
            authorization_endpoint: "https://auth.example.com/authorize",
            client_id: "client-123",
            redirect_uri: "http://127.0.0.1:9783/oauth/callback",
            state: "state-abc",
            code_challenge: "challenge-xyz",
            resource: &resource,
            scopes: Some("mcp.read"),
        })
        .unwrap();
        let resource_param = url
            .query_pairs()
            .find(|(key, _)| key == "resource")
            .map(|(_, value)| value.into_owned());
        assert_eq!(
            resource_param.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
    }

    fn parse_server(toml_table: &str) -> McpServerConfig {
        toml::from_str::<McpServerConfig>(toml_table).expect("mcp server config")
    }

    #[tokio::test]
    async fn stdio_server_without_live_handle_is_disconnected() {
        let server =
            parse_server("name = \"fs\"\ntransport = \"stdio\"\ncommand = \"/bin/true\"\n");
        let status = derive_server_status(&server).await;
        assert_eq!(status.name, "fs");
        assert_eq!(status.transport, "stdio");
        assert_eq!(status.state, "disconnected");
        assert_eq!(status.url, "");
        assert!(status.tools.is_none());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn http_server_with_static_token_is_connected() {
        let server = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example.com/mcp\"\nauth_token = \"static-bearer\"\n",
        );
        let status = derive_server_status(&server).await;
        assert_eq!(status.transport, "http");
        assert_eq!(status.state, "connected");
        assert_eq!(status.url, "https://mcp.example.com/mcp");
    }

    #[test]
    fn status_report_serializes_with_stable_keys() {
        let report = McpStatusReport {
            schema_version: MCP_STATUS_SCHEMA_VERSION,
            manifest: Some("/repo/harn.toml".to_string()),
            servers: vec![McpServerStatus {
                name: "fs".to_string(),
                transport: "stdio".to_string(),
                state: "disconnected".to_string(),
                url: String::new(),
                lazy: true,
                tools: None,
                resources: None,
                prompts: None,
                last_error: None,
            }],
        };
        let value: serde_json::Value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["manifest"], "/repo/harn.toml");
        assert_eq!(value["servers"][0]["name"], "fs");
        assert_eq!(value["servers"][0]["transport"], "stdio");
        assert_eq!(value["servers"][0]["state"], "disconnected");
        assert_eq!(value["servers"][0]["lazy"], true);
        assert!(value["servers"][0]["tools"].is_null());
        assert!(value["servers"][0]["last_error"].is_null());
    }
}
