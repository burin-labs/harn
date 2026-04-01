use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::{env, fs, process};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::package::{self, McpServerConfig};

const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:9783/oauth/callback";
const KEYRING_SERVICE: &str = "dev.burin.harn.mcp";
const TOKEN_REFRESH_SKEW_SECS: i64 = 60;
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

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
    pub resource: String,
    pub scopes: Option<String>,
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
}

pub(crate) enum AuthResolution {
    None,
    Bearer(String),
}

struct LoginOptions {
    target: Option<String>,
    explicit_url: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scopes: Option<String>,
    redirect_uri: String,
}

pub(crate) async fn handle_mcp_command(args: &[String]) {
    match args.first().map(|value| value.as_str()) {
        Some("login") => {
            let options = parse_login_options(&args[1..]);
            if let Err(error) = login(options).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Some("logout") => {
            let target = args.get(1).cloned();
            let explicit_url = flag_value(&args[2..], "--url");
            let server = resolve_server_reference(target.as_ref(), explicit_url.as_ref())
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            delete_stored_token(&server.url).unwrap_or_else(|error| {
                eprintln!("error: {error}");
                process::exit(1);
            });
            println!(
                "Removed stored OAuth token for {} ({})",
                server.name, server.url
            );
        }
        Some("status") => {
            let target = args.get(1).cloned();
            let explicit_url = flag_value(&args[2..], "--url");
            let server = resolve_server_reference(target.as_ref(), explicit_url.as_ref())
                .unwrap_or_else(|error| {
                    eprintln!("error: {error}");
                    process::exit(1);
                });
            match load_stored_token(&server.url) {
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
        Some("redirect-uri") => {
            println!("{DEFAULT_REDIRECT_URI}");
        }
        _ => {
            eprintln!("Usage:");
            eprintln!("  harn mcp login <name|url> [--url <url>] [--client-id <id>] [--client-secret <secret>] [--scope <scopes>] [--redirect-uri <uri>]");
            eprintln!("  harn mcp logout <name|url> [--url <url>]");
            eprintln!("  harn mcp status <name|url> [--url <url>]");
            eprintln!("  harn mcp redirect-uri");
            process::exit(1);
        }
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

    let Some(mut stored) = load_stored_token(&server.url)? else {
        return Ok(AuthResolution::None);
    };

    if token_needs_refresh(&stored) {
        stored = refresh_token_if_needed(&stored).await?;
        save_stored_token(&stored)?;
    }

    Ok(AuthResolution::Bearer(stored.access_token))
}

fn parse_login_options(args: &[String]) -> LoginOptions {
    let target = args
        .first()
        .filter(|value| !value.starts_with("--"))
        .cloned();
    LoginOptions {
        target,
        explicit_url: flag_value(args, "--url"),
        client_id: flag_value(args, "--client-id"),
        client_secret: flag_value(args, "--client-secret"),
        scopes: flag_value(args, "--scope"),
        redirect_uri: flag_value(args, "--redirect-uri")
            .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string()),
    }
}

async fn login(options: LoginOptions) -> Result<(), String> {
    let server = resolve_server_reference(options.target.as_ref(), options.explicit_url.as_ref())?;
    let discovery = discover_oauth_server(&server.url).await?;
    ensure_pkce_support(&discovery.metadata)?;

    let (client_id, client_secret, token_auth_method) = if let Some(client_id) =
        options.client_id.clone().or(server.client_id.clone())
    {
        let token_auth_method = determine_token_auth_method(
            &discovery.metadata,
            options
                .client_secret
                .clone()
                .or(server.client_secret.clone())
                .as_ref(),
        )?;
        (
            client_id,
            options
                .client_secret
                .clone()
                .or(server.client_secret.clone()),
            token_auth_method,
        )
    } else if let Some(registration_endpoint) = &discovery.metadata.registration_endpoint {
        let registration = dynamic_client_registration(
            registration_endpoint,
            &options.redirect_uri,
            options.scopes.as_deref().or(server.scopes.as_deref()),
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
            "No client_id available. Supply --client-id (optionally --client-secret) or use a server that supports dynamic client registration.".to_string()
        );
    };

    let (code_verifier, code_challenge) = generate_pkce_pair();
    let state = random_hex(16);
    let callback_listener = bind_callback_listener(&options.redirect_uri)?;

    let auth_url = build_authorization_url(
        &discovery.metadata.authorization_endpoint,
        &client_id,
        &options.redirect_uri,
        &state,
        &code_challenge,
        &server.url,
        options.scopes.as_deref().or(server.scopes.as_deref()),
    )?;

    println!("Server: {} ({})", server.name, server.url);
    println!("Redirect URI: {}", options.redirect_uri);
    println!("Protocol Version: {MCP_PROTOCOL_VERSION}");
    println!("Opening browser for OAuth authorization...");

    if webbrowser::open(auth_url.as_str()).is_err() {
        println!("Open this URL manually:\n{}", auth_url);
    }

    let code = wait_for_oauth_code(callback_listener, &options.redirect_uri, &state)?;
    let token = exchange_authorization_code(
        &discovery.metadata,
        &client_id,
        client_secret.clone(),
        &token_auth_method,
        &options.redirect_uri,
        &server.url,
        options.scopes.as_deref().or(server.scopes.as_deref()),
        &code,
        &code_verifier,
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
        resource: server.url.clone(),
        scopes: options.scopes.or(server.scopes),
    };
    save_stored_token(&stored)?;
    println!("OAuth token stored for {}.", server.name);
    Ok(())
}

fn resolve_server_reference(
    target: Option<&String>,
    explicit_url: Option<&String>,
) -> Result<ResolvedMcpServer, String> {
    if let Some(url) = explicit_url {
        return Ok(ResolvedMcpServer {
            name: target.cloned().unwrap_or_else(|| infer_name_from_url(url)),
            url: url.clone(),
            client_id: None,
            client_secret: None,
            scopes: None,
        });
    }

    let target = target.ok_or_else(|| "Missing MCP server name or URL".to_string())?;
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
    let resource_url =
        Url::parse(server_url).map_err(|error| format!("Invalid server URL: {error}"))?;
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

fn wait_for_oauth_code(
    listener: TcpListener,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<String, String> {
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

    callback_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())
}

async fn exchange_authorization_code(
    metadata: &OAuthServerMetadata,
    client_id: &str,
    client_secret: Option<String>,
    token_auth_method: &str,
    redirect_uri: &str,
    resource: &str,
    scopes: Option<&str>,
    code: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", client_id.to_string()),
        ("code_verifier", code_verifier.to_string()),
        ("resource", resource.to_string()),
    ];
    if let Some(scopes) = scopes {
        form.push(("scope", scopes.to_string()));
    }
    request_token(
        &client,
        &metadata.token_endpoint,
        token_auth_method,
        client_id,
        client_secret.as_deref(),
        &form,
    )
    .await
}

async fn refresh_token_if_needed(token: &StoredOAuthToken) -> Result<StoredOAuthToken, String> {
    if !token_needs_refresh(token) {
        return Ok(token.clone());
    }

    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        "Stored OAuth token has expired and does not include a refresh token".to_string()
    })?;
    let client = reqwest::Client::new();
    let form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token),
        ("client_id", token.client_id.clone()),
        ("resource", token.resource.clone()),
    ];
    let refreshed = request_token(
        &client,
        &token.token_endpoint,
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
        token_endpoint: token.token_endpoint.clone(),
        client_id: token.client_id.clone(),
        client_secret: token.client_secret.clone(),
        token_endpoint_auth_method: token.token_endpoint_auth_method.clone(),
        resource: token.resource.clone(),
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

fn save_stored_token(token: &StoredOAuthToken) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, &token_store_account(&token.resource))
        .map_err(|error| format!("Failed to open keyring entry: {error}"))?;
    let payload = serde_json::to_string(token)
        .map_err(|error| format!("Failed to serialize OAuth token: {error}"))?;
    entry
        .set_password(&payload)
        .map_err(|error| format!("Failed to store OAuth token in keyring: {error}"))
}

fn load_stored_token(resource: &str) -> Result<Option<StoredOAuthToken>, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, &token_store_account(resource))
        .map_err(|error| format!("Failed to open keyring entry: {error}"))?;
    let payload = match entry.get_password() {
        Ok(value) => value,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(format!("Failed to read OAuth token from keyring: {error}")),
    };
    let token = serde_json::from_str::<StoredOAuthToken>(&payload)
        .map_err(|error| format!("Stored OAuth token was invalid JSON: {error}"))?;
    Ok(Some(token))
}

fn delete_stored_token(resource: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, &token_store_account(resource))
        .map_err(|error| format!("Failed to open keyring entry: {error}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!(
            "Failed to delete OAuth token from keyring: {error}"
        )),
    }
}

fn token_store_account(resource: &str) -> String {
    let digest = Sha256::digest(resource.as_bytes());
    format!(
        "mcp-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
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
</html>"#,
        status_line = status_line,
        title = title,
        accent = accent,
        badge = badge,
        message = message
    )
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

struct OAuthDiscoveryResult {
    metadata: OAuthServerMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_resource_candidate_prefers_path_specific_url() {
        let url = Url::parse("https://example.com/mcp/notion").unwrap();
        let candidates = protected_resource_candidates(&url);
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
        let candidates = authorization_server_candidates(&url);
        assert_eq!(
            candidates[0].as_str(),
            "https://auth.example.com/.well-known/oauth-authorization-server/oauth"
        );
        assert_eq!(
            candidates[1].as_str(),
            "https://auth.example.com/.well-known/openid-configuration/oauth"
        );
    }

    #[test]
    fn token_store_account_is_stable() {
        let first = token_store_account("https://mcp.notion.com");
        let second = token_store_account("https://mcp.notion.com");
        assert_eq!(first, second);
    }
}
