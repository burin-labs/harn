//! Interactive MCP OAuth flow engine — the harn-owned core that every
//! surface (the `harn mcp login` CLI, and the ACP `mcp/authorize` /
//! `mcp/oauth_callback` requests) drives. It builds the authorization URL,
//! exchanges the authorization code, refreshes tokens (single-flight, with a
//! cross-process advisory lock), and stores them in the OS keyring. No client
//! ever speaks OAuth directly: token exchange and storage stay in harn.
//!
//! The pure protocol rules (discovery, PKCE policy, issuer binding,
//! registration-mode selection, RFC 8707 resource indicators) live in
//! [`crate::mcp_auth`]; this module is the network/IO/state orchestration on
//! top of them.
//!
//! Flow:
//! 1. [`begin_authorization`] does discovery + client resolution, mints a PKCE
//!    pair + `state`, registers a [`PendingAuthorization`] keyed by `state`,
//!    and returns the browser authorization URL.
//! 2. The caller opens the URL. The redirect (loopback for TUI/headless, a
//!    client URL scheme for the GUI) carries `code` + `state` back.
//! 3. [`complete_authorization`] pops the pending flow by `state`, validates
//!    the issuer, exchanges the code, and persists the token.
//!
//! Tokens are keyed by `(resource, issuer, client_id)`, with a per-`(resource,
//! issuer)` index recording the active client so callers that don't know the
//! client id (status, logout, a 401-triggered refresh) resolve the right one.
//! Refreshes are single-flight: an in-process async mutex plus a file lock
//! serialize concurrent refreshers (clients + daemon) so they don't stampede
//! the token endpoint or revoke each other's rotated refresh tokens.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use base64::Engine;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::mcp_auth::{
    authorization_code_token_form, build_oauth_authorization_url, canonical_resource_indicator,
    determine_token_endpoint_auth_method, discover_mcp_oauth, dynamic_client_registration_body,
    ensure_pkce_s256_supported, refresh_token_form, select_oauth_client_auth,
    validate_authorization_response_issuer_value, validate_issuer_binding,
    validate_token_endpoint_auth_method, McpOAuthDiscovery, OAuthAuthorizationCodeTokenForm,
    OAuthAuthorizationServerMetadata, OAuthAuthorizationUrlOptions, OAuthClientAuthMode,
    OAuthClientAuthOptions, OAuthDynamicClientRegistrationResponse, OAuthRefreshTokenForm,
    DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL,
};
use crate::secrets::{KeyringSecretProvider, SecretBytes, SecretError, SecretId, SecretProvider};

/// Keyring service namespace for stored MCP OAuth tokens. Shared by every
/// surface so a token minted by `harn mcp login` is the same one the ACP
/// `mcp/oauth_callback` path reads and refreshes.
const KEYRING_SERVICE: &str = "dev.harn.mcp";

/// Override directory for the cross-process refresh lock files (tests).
const OAUTH_LOCK_DIR_ENV: &str = "HARN_MCP_OAUTH_LOCK_DIR";
const HARN_HOME_ENV: &str = "HARN_HOME";

/// Refresh a token this many seconds before its advertised expiry so a call
/// never races the clock against the authorization server.
const TOKEN_REFRESH_SKEW_SECS: i64 = 60;

/// Upper bound on concurrently pending (begun-but-not-completed) authorizations.
/// Caps memory from abandoned flows in a long-lived server.
const MAX_PENDING_FLOWS: usize = 32;

/// A persisted MCP OAuth token plus everything needed to refresh it without
/// re-running discovery. Keyed in the keyring by `(resource, issuer, client_id)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredMcpToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at_unix: Option<i64>,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub token_endpoint_auth_method: String,
    pub issuer: String,
    pub resource: String,
    #[serde(default)]
    pub scopes: Option<String>,
}

/// The raw token-endpoint response shape (a subset; unknown fields ignored).
#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Inputs to [`begin_authorization`]. The server is identified by its URL; the
/// client may be pre-registered (`client_id`/`client_secret`) or left to
/// dynamic registration. `redirect_uri` is the loopback or client-scheme URL
/// the authorization server will redirect to. `mode`/`static_secret_id` carry
/// an explicit `[mcp.auth]` selection so the engine resolves the same client
/// auth the runtime does (CIMD by default when unset).
#[derive(Clone, Debug, Default)]
pub struct BeginAuthorization {
    pub server_url: String,
    pub redirect_uri: String,
    pub mode: Option<OAuthClientAuthMode>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub static_secret_id: Option<String>,
    pub scopes: Option<String>,
}

/// A legacy bearer/refresh token that a thin client found in an older
/// surface-specific store. Discovery and canonical key selection still happen
/// here so the imported token lands in the same Harn-owned store as a fresh
/// [`begin_authorization`] / [`complete_authorization`] flow.
#[derive(Clone, Debug)]
pub struct ImportStoredToken {
    pub server_url: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: Option<i64>,
    pub token_endpoint: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub scopes: Option<String>,
}

/// The browser-facing result of [`begin_authorization`]: the URL to open and
/// the `state` that the matching [`complete_authorization`] call must echo.
#[derive(Clone, Debug, Serialize)]
pub struct PendingAuthorization {
    pub authorize_url: String,
    pub state: String,
    pub redirect_uri: String,
    /// Canonical RFC 8707 resource indicator for the server.
    pub resource: String,
    /// Authorization server issuer resolved during discovery.
    pub issuer: String,
}

/// Server-side flow state held between [`begin_authorization`] and
/// [`complete_authorization`], keyed by `state`. Holds the PKCE verifier and
/// resolved client credentials — never leaves the harn process.
#[derive(Clone, Debug)]
struct PendingFlow {
    code_verifier: String,
    redirect_uri: String,
    client_id: String,
    client_secret: Option<String>,
    token_auth_method: String,
    token_endpoint: String,
    issuer: String,
    resource: String,
    scopes: Option<String>,
    /// Whether the authorization server advertised RFC 9207 `iss` support, so
    /// [`complete_authorization`] can enforce the response binding.
    iss_response_supported: bool,
}

fn pending_flows() -> &'static Mutex<HashMap<String, PendingFlow>> {
    static FLOWS: OnceLock<Mutex<HashMap<String, PendingFlow>>> = OnceLock::new();
    FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Begin an interactive authorization for an MCP server: discover the
/// authorization server, resolve (or dynamically register) the client, mint a
/// PKCE pair + `state`, and register a pending flow. Returns the URL to open.
pub async fn begin_authorization(
    request: BeginAuthorization,
) -> Result<PendingAuthorization, String> {
    let resource = canonical_resource_indicator(&request.server_url).map_err(|e| e.to_string())?;
    let discovery = discover(&request.server_url).await?;
    ensure_pkce_s256_supported(&discovery.authorization_server_metadata)?;

    let scopes = request
        .scopes
        .clone()
        .or_else(|| (!discovery.scopes.is_empty()).then(|| discovery.scopes.join(" ")));

    let (client_id, client_secret, token_auth_method) = resolve_client(
        &discovery.authorization_server_metadata,
        request.mode,
        request.client_id.clone(),
        request.client_secret.clone(),
        request.static_secret_id.as_deref(),
        &request.redirect_uri,
        scopes.as_deref(),
    )
    .await?;

    let (code_verifier, code_challenge) = generate_pkce_pair();
    let state = random_hex(16);
    let authorize_url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
        authorization_endpoint: &discovery
            .authorization_server_metadata
            .authorization_endpoint,
        client_id: &client_id,
        redirect_uri: &request.redirect_uri,
        state: &state,
        code_challenge: &code_challenge,
        resource: &resource,
        scopes: scopes.as_deref(),
    })?;

    let mut flows = pending_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Bound the registry so abandoned flows (begun but never completed) cannot
    // grow without limit in a long-lived server. Authorization is user-driven
    // and rare, so a modest cap is ample; evict an arbitrary stale entry when
    // full rather than tracking timestamps.
    if flows.len() >= MAX_PENDING_FLOWS {
        if let Some(stale) = flows.keys().next().cloned() {
            flows.remove(&stale);
        }
    }
    flows.insert(
        state.clone(),
        PendingFlow {
            code_verifier,
            redirect_uri: request.redirect_uri.clone(),
            client_id,
            client_secret,
            token_auth_method,
            token_endpoint: discovery
                .authorization_server_metadata
                .token_endpoint
                .clone(),
            issuer: discovery.authorization_server_issuer.clone(),
            resource: resource.clone(),
            scopes,
            iss_response_supported: discovery
                .authorization_server_metadata
                .authorization_response_iss_parameter_supported,
        },
    );
    drop(flows);

    Ok(PendingAuthorization {
        authorize_url: authorize_url.to_string(),
        state,
        redirect_uri: request.redirect_uri,
        resource,
        issuer: discovery.authorization_server_issuer,
    })
}

/// Complete an authorization started by [`begin_authorization`]: look up the
/// pending flow by `state`, validate the issuer binding, exchange the code,
/// and persist the token (recording it as the active client for the resource).
pub async fn complete_authorization(
    state: &str,
    code: &str,
    issuer: Option<&str>,
) -> Result<StoredMcpToken, String> {
    let flow = pending_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(state)
        .ok_or_else(|| "no pending MCP authorization matches this state".to_string())?;

    // RFC 9207: a returned `iss` must match the bound issuer, and when the AS
    // advertises `iss` support it MUST be present.
    validate_authorization_response_issuer_value(
        &flow.issuer,
        flow.iss_response_supported,
        issuer,
    )?;

    let client = reqwest::Client::new();
    let form = authorization_code_token_form(OAuthAuthorizationCodeTokenForm {
        client_id: &flow.client_id,
        redirect_uri: &flow.redirect_uri,
        code,
        code_verifier: &flow.code_verifier,
        resource: &flow.resource,
        scopes: flow.scopes.as_deref(),
    });
    let token = request_token(
        &client,
        &flow.token_endpoint,
        &flow.token_auth_method,
        &flow.client_id,
        flow.client_secret.as_deref(),
        &form,
    )
    .await?;

    let stored = StoredMcpToken {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_unix: expires_at_from_expires_in(token.expires_in)?,
        token_endpoint: flow.token_endpoint,
        client_id: flow.client_id,
        client_secret: flow.client_secret,
        token_endpoint_auth_method: flow.token_auth_method,
        issuer: flow.issuer,
        resource: flow.resource,
        scopes: flow.scopes,
    };
    save_stored_token(&stored).await?;
    Ok(stored)
}

/// Resolve a bearer token for an already-authorized MCP server, refreshing it
/// (single-flight) if it is within the expiry skew. Returns `None` when no
/// token is stored (the caller should treat this as "auth required").
pub async fn resolve_bearer(server_url: &str) -> Result<Option<String>, String> {
    let discovery = discover(server_url).await?;
    let resource = canonical_resource_indicator(server_url).map_err(|e| e.to_string())?;
    let store = KeyringOAuthTokenStorage::default();
    let Some(mut stored) = load_stored_token_from_store(
        &store,
        &resource,
        &discovery.authorization_server_issuer,
        None,
    )
    .await?
    else {
        return Ok(None);
    };
    validate_issuer_binding(&stored.issuer, &discovery.authorization_server_issuer)?;
    if token_needs_refresh(&stored) {
        stored = refresh_stored_token_with_store(&store, &stored, &discovery, None).await?;
    }
    Ok(Some(stored.access_token))
}

/// Import an existing token into the canonical Harn MCP OAuth store.
pub async fn import_stored_token(request: ImportStoredToken) -> Result<StoredMcpToken, String> {
    let discovery = discover(&request.server_url).await?;
    let stored = stored_token_for_import(&request, &discovery)?;
    save_stored_token(&stored).await?;
    Ok(stored)
}

/// Discover the OAuth authorization server protecting an MCP server URL.
pub async fn discover(server_url: &str) -> Result<McpOAuthDiscovery, String> {
    let client = reqwest::Client::new();
    discover_mcp_oauth(&client, server_url)
        .await
        .map_err(|error| error.to_string())
}

/// Load a stored token by `(resource, issuer)`, using `client_id_hint` when
/// known and otherwise the recorded active client. `None` when none is stored.
pub async fn load_token(
    resource: &str,
    issuer: &str,
    client_id_hint: Option<&str>,
) -> Result<Option<StoredMcpToken>, String> {
    let store = KeyringOAuthTokenStorage::default();
    load_stored_token_from_store(&store, resource, issuer, client_id_hint).await
}

/// Delete a stored token by `(resource, issuer)`, resolving `client_id_hint` or
/// the active client, and clearing the active-client index when it pointed here.
pub async fn delete_token(
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

/// Resolve the `(client_id, client_secret, token_endpoint_auth_method)` for a
/// flow via the unified auth selection (CIMD by default when the server
/// advertises it; otherwise BYO client id, dynamic registration, or an error).
async fn resolve_client(
    metadata: &OAuthAuthorizationServerMetadata,
    mode: Option<OAuthClientAuthMode>,
    client_id: Option<String>,
    client_secret: Option<String>,
    static_secret_id: Option<&str>,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> Result<(String, Option<String>, String), String> {
    let selection = select_oauth_client_auth(
        metadata,
        OAuthClientAuthOptions {
            mode,
            client_id: client_id.as_deref(),
            client_secret: client_secret.as_deref(),
            client_id_metadata_document_url: client_id.as_deref(),
            static_secret_id,
        },
    )?;
    match selection.mode {
        OAuthClientAuthMode::Cimd => {
            // CIMD presents an HTTPS client-metadata-document URL as the
            // `client_id`; the client is public, so token auth is `none`.
            let resolved_client_id = selection
                .client_id
                .unwrap_or(DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL)
                .to_string();
            Ok((resolved_client_id, None, "none".to_string()))
        }
        OAuthClientAuthMode::Byo => {
            let resolved_client_id = selection
                .client_id
                .ok_or_else(|| "BYO OAuth auth requires client_id".to_string())?
                .to_string();
            let token_auth_method =
                determine_token_endpoint_auth_method(metadata, client_secret.as_deref())?;
            Ok((resolved_client_id, client_secret, token_auth_method))
        }
        OAuthClientAuthMode::Dcr => {
            let registration_endpoint = metadata
                .registration_endpoint
                .as_deref()
                .ok_or_else(|| "dynamic client registration endpoint missing".to_string())?;
            let registration =
                dynamic_client_registration(registration_endpoint, redirect_uri, scopes).await?;
            let auth_method = registration
                .token_endpoint_auth_method
                .clone()
                .unwrap_or_else(|| "none".to_string());
            validate_token_endpoint_auth_method(&auth_method)?;
            Ok((
                registration.client_id,
                registration.client_secret,
                auth_method,
            ))
        }
        OAuthClientAuthMode::Static => Err(
            "static MCP auth uses a stored bearer token and does not run interactive OAuth"
                .to_string(),
        ),
    }
}

async fn dynamic_client_registration(
    registration_endpoint: &str,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> Result<OAuthDynamicClientRegistrationResponse, String> {
    let client = reqwest::Client::new();
    let body = dynamic_client_registration_body("Harn", [redirect_uri], scopes);
    let response = client
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("Dynamic client registration failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(oauth_http_error(
            "Dynamic client registration failed",
            status,
            &body,
        ));
    }
    response
        .json::<OAuthDynamicClientRegistrationResponse>()
        .await
        .map_err(|error| format!("Invalid dynamic client registration response: {error}"))
}

/// Build the refreshed token from a successful refresh response, re-sending the
/// RFC 8707 resource indicator and keeping the stored key canonicalized.
async fn refresh_token(
    token: &StoredMcpToken,
    discovery: &McpOAuthDiscovery,
) -> Result<StoredMcpToken, String> {
    validate_issuer_binding(&token.issuer, &discovery.authorization_server_issuer)?;
    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        "Stored OAuth token has expired and does not include a refresh token".to_string()
    })?;
    let resource =
        canonical_resource_indicator(&token.resource).unwrap_or_else(|_| token.resource.clone());
    let client = reqwest::Client::new();
    let form = refresh_token_form(OAuthRefreshTokenForm {
        client_id: &token.client_id,
        refresh_token: &refresh_token,
        resource: &resource,
    });
    let token_endpoint = discovery
        .authorization_server_metadata
        .token_endpoint
        .clone();
    let refreshed = request_token(
        &client,
        &token_endpoint,
        &token.token_endpoint_auth_method,
        &token.client_id,
        token.client_secret.as_deref(),
        &form,
    )
    .await?;
    Ok(StoredMcpToken {
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .or_else(|| token.refresh_token.clone()),
        expires_at_unix: expires_at_from_expires_in(refreshed.expires_in)?,
        token_endpoint,
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
        return Err(oauth_http_error("Token request failed", status, &body));
    }
    response
        .json::<TokenResponse>()
        .await
        .map_err(|error| format!("Invalid token response: {error}"))
}

fn token_needs_refresh(token: &StoredMcpToken) -> bool {
    match token.expires_at_unix {
        Some(expires_at) => {
            expires_at <= current_unix_timestamp().saturating_add(TOKEN_REFRESH_SKEW_SECS)
        }
        None => false,
    }
}

fn expires_at_from_expires_in(expires_in: Option<i64>) -> Result<Option<i64>, String> {
    let Some(seconds) = expires_in else {
        return Ok(None);
    };
    if seconds < 0 {
        return Err("Token response `expires_in` must be non-negative".to_string());
    }
    Ok(Some(current_unix_timestamp().saturating_add(seconds)))
}

fn oauth_http_error(context: &str, status: reqwest::StatusCode, body: &str) -> String {
    if body.trim().is_empty() {
        format!("{context}: {status}")
    } else {
        format!(
            "{context}: {status} ({} byte response body omitted)",
            body.len()
        )
    }
}

// --- single-flight refresh + cross-process lock ------------------------------

/// Refresh a stored token under both an in-process async mutex and a
/// cross-process file lock, re-loading and re-checking inside the lock so
/// concurrent refreshers (clients + daemon) collapse to a single token request
/// and never revoke each other's rotated refresh token.
async fn refresh_stored_token_with_store<S: OAuthTokenStorage + ?Sized>(
    store: &S,
    token: &StoredMcpToken,
    discovery: &McpOAuthDiscovery,
    lock_dir_override: Option<PathBuf>,
) -> Result<StoredMcpToken, String> {
    let key = OAuthTokenStoreKey::from_token(token);
    let _guard = acquire_oauth_refresh_lock(&key, lock_dir_override.as_deref()).await?;
    let Some(current) = store.load_token(&key).await? else {
        return Err("Stored OAuth token disappeared before it could be refreshed".to_string());
    };
    validate_issuer_binding(&current.issuer, &discovery.authorization_server_issuer)?;
    if !token_needs_refresh(&current) {
        return Ok(current);
    }
    let refreshed = refresh_token(&current, discovery).await?;
    store.save_token(&refreshed).await?;
    Ok(refreshed)
}

struct OAuthRefreshLockGuard {
    _async_guard: tokio::sync::OwnedMutexGuard<()>,
    file: File,
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
        Ok::<File, String>(file)
    })
    .await
    .map_err(|error| format!("OAuth token lock task failed: {error}"))??;
    Ok(OAuthRefreshLockGuard {
        _async_guard: async_guard,
        file,
    })
}

fn oauth_refresh_mutex(key: &OAuthTokenStoreKey) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(|poison| poison.into_inner());
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
    if let Some(path) = std::env::var_os(OAUTH_LOCK_DIR_ENV) {
        return PathBuf::from(path);
    }
    harn_home_dir().join("mcp-oauth-locks")
}

fn harn_home_dir() -> PathBuf {
    if let Some(path) = std::env::var_os(HARN_HOME_ENV) {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".harn");
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(home).join(".harn");
    }
    std::env::temp_dir().join("harn")
}

// --- keyring storage ---------------------------------------------------------

/// Keyring key for one token: `(resource, issuer, client_id)`.
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

    fn from_token(token: &StoredMcpToken) -> Self {
        Self::new(&token.resource, &token.issuer, &token.client_id)
    }

    fn account(&self) -> String {
        token_store_account(&self.resource, &self.issuer, &self.client_id)
    }
}

/// The active client id for a `(resource, issuer)` pair, so callers that don't
/// supply a client id resolve the most recently authorized one.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOAuthClientIndex {
    client_id: String,
}

/// Storage abstraction over the keyring so the single-flight refresh path can
/// be exercised against an in-memory double in tests.
#[async_trait]
trait OAuthTokenStorage: Send + Sync {
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

struct KeyringOAuthTokenStorage {
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

/// Persist a token (under the refresh lock) and record it as the active client.
async fn save_stored_token(token: &StoredMcpToken) -> Result<(), String> {
    let store = KeyringOAuthTokenStorage::default();
    let key = OAuthTokenStoreKey::from_token(token);
    let _guard = acquire_oauth_refresh_lock(&key, None).await?;
    store.save_token(token).await
}

async fn load_stored_token_from_store<S: OAuthTokenStorage + ?Sized>(
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

fn stored_token_for_import(
    request: &ImportStoredToken,
    discovery: &McpOAuthDiscovery,
) -> Result<StoredMcpToken, String> {
    let server_url = request.server_url.trim();
    if server_url.is_empty() {
        return Err("MCP token import requires server_url".to_string());
    }
    let access_token = request.access_token.trim();
    if access_token.is_empty() {
        return Err("MCP token import requires access_token".to_string());
    }
    let client_id = request.client_id.trim();
    if client_id.is_empty() {
        return Err("MCP token import requires client_id".to_string());
    }

    let resource = canonical_resource_indicator(server_url).map_err(|error| error.to_string())?;
    let client_secret = request
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(str::to_string);
    let token_endpoint_auth_method = request
        .token_endpoint_auth_method
        .as_deref()
        .map(str::trim)
        .filter(|method| !method.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if client_secret.is_some() {
                "client_secret_post".to_string()
            } else {
                "none".to_string()
            }
        });
    validate_token_endpoint_auth_method(&token_endpoint_auth_method)?;

    if request
        .expires_at_unix
        .is_some_and(|expires_at| expires_at < 0)
    {
        return Err("MCP token import expires_at must be non-negative".to_string());
    }

    Ok(StoredMcpToken {
        access_token: access_token.to_string(),
        refresh_token: request
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        expires_at_unix: request.expires_at_unix,
        token_endpoint: request
            .token_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                discovery
                    .authorization_server_metadata
                    .token_endpoint
                    .clone()
            }),
        client_id: client_id.to_string(),
        client_secret,
        token_endpoint_auth_method,
        issuer: discovery.authorization_server_issuer.clone(),
        resource,
        scopes: request
            .scopes
            .as_deref()
            .map(str::trim)
            .filter(|scopes| !scopes.is_empty())
            .map(str::to_string),
    })
}

// --- crypto/util -------------------------------------------------------------

fn generate_pkce_pair() -> (String, String) {
    let verifier = random_hex(32);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn token_store_account_is_stable_and_dimension_sensitive() {
        let first =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-a");
        let second =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-a");
        let other_issuer = token_store_account(
            "https://mcp.notion.com",
            "https://other.example",
            "client-a",
        );
        let other_resource =
            token_store_account("https://mcp.linear.app", "https://auth.example", "client-a");
        let other_client =
            token_store_account("https://mcp.notion.com", "https://auth.example", "client-b");
        assert_eq!(first, second);
        assert_ne!(first, other_issuer);
        assert_ne!(first, other_resource);
        assert_ne!(first, other_client);
        assert!(first.starts_with("mcp-token-"));
    }

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let (verifier, challenge) = generate_pkce_pair();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        assert_eq!(verifier.len(), 64);
    }

    #[test]
    fn token_needs_refresh_respects_skew() {
        let mut token = StoredMcpToken {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at_unix: None,
            token_endpoint: "https://auth/token".into(),
            client_id: "c".into(),
            client_secret: None,
            token_endpoint_auth_method: "none".into(),
            issuer: "https://auth".into(),
            resource: "https://mcp".into(),
            scopes: None,
        };
        assert!(!token_needs_refresh(&token));
        token.expires_at_unix = Some(current_unix_timestamp() + 3600);
        assert!(!token_needs_refresh(&token));
        token.expires_at_unix = Some(current_unix_timestamp() + TOKEN_REFRESH_SKEW_SECS - 1);
        assert!(token_needs_refresh(&token));
    }

    #[test]
    fn token_response_rejects_negative_expiry() {
        let error = expires_at_from_expires_in(Some(-1)).unwrap_err();
        assert!(error.contains("expires_in"), "{error}");
    }

    #[test]
    fn oauth_http_error_omits_response_body() {
        let error = oauth_http_error(
            "Token request failed",
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"access_token":"secret","error_description":"bad"}"#,
        );
        assert!(error.contains("400 Bad Request"), "{error}");
        assert!(error.contains("response body omitted"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        assert!(!error.contains("bad"), "{error}");
    }

    fn test_discovery() -> McpOAuthDiscovery {
        McpOAuthDiscovery {
            protected_resource_metadata_url: url::Url::parse(
                "https://mcp.example/.well-known/oauth-protected-resource",
            )
            .unwrap(),
            protected_resource_metadata: Default::default(),
            authorization_server_issuer: "https://auth.example".to_string(),
            authorization_server_metadata_url: url::Url::parse(
                "https://auth.example/.well-known/oauth-authorization-server",
            )
            .unwrap(),
            authorization_server_metadata_kind:
                crate::mcp_auth::OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
            authorization_server_metadata: OAuthAuthorizationServerMetadata {
                issuer: "https://auth.example".to_string(),
                authorization_endpoint: "https://auth.example/authorize".to_string(),
                token_endpoint: "https://auth.example/token".to_string(),
                registration_endpoint: None,
                token_endpoint_auth_methods_supported: vec!["none".to_string()],
                code_challenge_methods_supported: vec!["S256".to_string()],
                scopes_supported: Vec::new(),
                client_id_metadata_document_supported: false,
                authorization_response_iss_parameter_supported: false,
                extra: Default::default(),
            },
            challenge: None,
            scopes: Vec::new(),
        }
    }

    #[test]
    fn token_import_uses_discovery_binding_and_legacy_auth_defaults() {
        let discovery = test_discovery();
        let token = stored_token_for_import(
            &ImportStoredToken {
                server_url: "https://mcp.example/mcp".to_string(),
                access_token: "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at_unix: Some(123),
                token_endpoint: Some("  ".to_string()),
                client_id: "client".to_string(),
                client_secret: Some(" secret ".to_string()),
                token_endpoint_auth_method: Some(" client_secret_post ".to_string()),
                scopes: Some(" read write ".to_string()),
            },
            &discovery,
        )
        .unwrap();

        assert_eq!(token.issuer, "https://auth.example");
        assert_eq!(token.resource, "https://mcp.example/mcp");
        assert_eq!(token.token_endpoint, "https://auth.example/token");
        assert_eq!(token.client_id, "client");
        assert_eq!(token.client_secret.as_deref(), Some("secret"));
        assert_eq!(token.token_endpoint_auth_method, "client_secret_post");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(token.expires_at_unix, Some(123));
        assert_eq!(token.scopes.as_deref(), Some("read write"));
    }

    #[test]
    fn token_import_rejects_negative_expiry() {
        let discovery = test_discovery();
        let error = stored_token_for_import(
            &ImportStoredToken {
                server_url: "https://mcp.example/mcp".to_string(),
                access_token: "access".to_string(),
                refresh_token: None,
                expires_at_unix: Some(-1),
                token_endpoint: None,
                client_id: "client".to_string(),
                client_secret: None,
                token_endpoint_auth_method: None,
                scopes: None,
            },
            &discovery,
        )
        .unwrap_err();
        assert!(error.contains("expires_at"), "{error}");
    }

    #[tokio::test]
    async fn complete_authorization_rejects_unknown_state() {
        let error = complete_authorization("no-such-state", "code", None)
            .await
            .unwrap_err();
        assert!(error.contains("no pending MCP authorization"), "{error}");
    }

    /// Concurrent refreshers must collapse to a single token-endpoint call and
    /// converge on the rotated token.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn expired_token_refresh_is_singleflight() {
        #[derive(Clone, Default)]
        struct MemoryStore {
            tokens: Arc<AsyncMutex<StdHashMap<OAuthTokenStoreKey, StoredMcpToken>>>,
            index: Arc<AsyncMutex<StdHashMap<(String, String), String>>>,
        }

        #[async_trait]
        impl OAuthTokenStorage for MemoryStore {
            async fn load_token(
                &self,
                key: &OAuthTokenStoreKey,
            ) -> Result<Option<StoredMcpToken>, String> {
                Ok(self.tokens.lock().await.get(key).cloned())
            }
            async fn save_token(&self, token: &StoredMcpToken) -> Result<(), String> {
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

        // A minimal token endpoint over a raw TCP listener that counts hits.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_endpoint_url = format!("http://{}/token", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                server_calls.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                let body = r#"{"access_token":"access-new","refresh_token":"refresh-new","expires_in":3600}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            }
        });

        let store = Arc::new(MemoryStore::default());
        let stale = StoredMcpToken {
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

        let discovery_meta = OAuthAuthorizationServerMetadata {
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
        };
        let discovery = McpOAuthDiscovery {
            protected_resource_metadata_url: url::Url::parse(
                "https://mcp.example/.well-known/oauth-protected-resource",
            )
            .unwrap(),
            protected_resource_metadata: Default::default(),
            authorization_server_issuer: stale.issuer.clone(),
            authorization_server_metadata_url: url::Url::parse(
                "https://auth.example/.well-known/oauth-authorization-server",
            )
            .unwrap(),
            authorization_server_metadata_kind:
                crate::mcp_auth::OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
            authorization_server_metadata: discovery_meta,
            challenge: None,
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
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "refresh must be single-flight"
        );
        let stored = store
            .load_token(&OAuthTokenStoreKey::from_token(&stale))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, "access-new");
        server.abort();
    }
}
