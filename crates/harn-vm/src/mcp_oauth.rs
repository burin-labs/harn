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

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::mcp_auth::{
    authorization_code_token_form, build_oauth_authorization_url, canonical_resource_indicator,
    determine_token_endpoint_auth_method, discover_mcp_oauth, dynamic_client_registration_body,
    ensure_pkce_s256_supported, refresh_token_form, select_oauth_client_auth,
    validate_authorization_response_issuer_value, validate_issuer_binding,
    validate_token_endpoint_auth_method, McpOAuthDiscovery, OAuthAuthorizationCodeTokenForm,
    OAuthAuthorizationServerMetadata, OAuthAuthorizationUrlOptions, OAuthClientAuthMode,
    OAuthClientAuthOptions, OAuthClientAuthSelection, OAuthDynamicClientRegistrationResponse,
    OAuthRefreshTokenForm, DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL,
};
mod redirect;
mod storage;
mod token_import;

use token_import::stored_token_for_import;

#[cfg(test)]
use storage::token_store_account;
use storage::{
    delete_stored_token_and_active_index, load_stored_token_from_store, KeyringOAuthTokenStorage,
    OAuthTokenStorage, OAuthTokenStoreKey,
};

use redirect::{
    generate_pkce_pair, random_hex, redirect_compatibility_error,
    redirect_compatibility_error_for_mode, select_redirect_policy, CallbackCapabilities,
};
pub use redirect::{
    AuthorizationCallback, BeginAuthorization, ImportStoredToken, LoopbackCallback,
    PendingAuthorization,
};

/// Override directory for the cross-process refresh lock files (tests).
const OAUTH_LOCK_DIR_ENV: &str = "HARN_MCP_OAUTH_LOCK_DIR";
const HARN_HOME_ENV: &str = "HARN_HOME";

/// Refresh a token this many seconds before its advertised expiry so a call
/// never races the clock against the authorization server. A five-minute skew
/// also leaves enough room for clients, daemons, and apps to converge on a
/// rotated refresh token before the old access token expires.
const TOKEN_REFRESH_SKEW_SECS: i64 = 5 * 60;

/// Upper bound on concurrently pending (begun-but-not-completed) authorizations.
/// Caps memory from abandoned flows in a long-lived server.
const MAX_PENDING_FLOWS: usize = 32;

const AUTH_COMPLETION_CHANNEL_CAPACITY: usize = 64;

/// Overall timeout for OAuth control-plane HTTP requests (code exchange,
/// refresh, discovery, dynamic registration, token exchange). reqwest's
/// default is *no* timeout, so a token endpoint that accepts TCP but never
/// responds would otherwise wedge the request — and, on the refresh path, the
/// single-flight refresh lock — forever. The connect timeout matches the llm
/// transport clients; 30s overall is generous for short control-plane calls.
const OAUTH_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const OAUTH_HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Upper bound on waiting for the single-flight refresh locks (the in-process
/// mutex and the cross-process file lock). A healthy holder finishes within
/// one bounded token request ([`OAUTH_HTTP_TIMEOUT`]) plus storage IO, so
/// waiting longer means the holder is wedged; failing fast with a clear error
/// beats silently blocking every later 401 recovery behind it.
const OAUTH_REFRESH_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

const TOKEN_EXCHANGE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_PREFIX: &str = "urn:ietf:params:oauth:token-type:";
const TOKEN_TYPE_ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";

/// Test hook: shrinks [`OAUTH_HTTP_TIMEOUT`] (milliseconds) so stalled-endpoint
/// tests stay fast. `0` means "use the production timeout".
#[cfg(test)]
static OAUTH_HTTP_TIMEOUT_OVERRIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn oauth_http_timeout() -> std::time::Duration {
    #[cfg(test)]
    {
        let ms = OAUTH_HTTP_TIMEOUT_OVERRIDE_MS.load(std::sync::atomic::Ordering::Relaxed);
        if ms > 0 {
            return std::time::Duration::from_millis(ms);
        }
    }
    OAUTH_HTTP_TIMEOUT
}

/// HTTP client for OAuth endpoints. Never use a bare `reqwest::Client::new()`
/// in this module: it has no request timeout, and every OAuth call site here
/// either runs under (or feeds) the single-flight refresh lock or blocks an
/// interactive authorization, so an unbounded request wedges MCP auth
/// process-wide.
fn oauth_http_client() -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .connect_timeout(OAUTH_HTTP_CONNECT_TIMEOUT)
        .timeout(oauth_http_timeout())
        .redirect(crate::egress::redirect_policy("mcp_oauth_redirect", 10));
    crate::egress::install_ssrf_guard(builder)
        .build()
        .expect("MCP OAuth HTTP client configuration should be valid")
}

/// Per-MCP-server opt-in for exchanging the stored subject bearer for a
/// delegated request bearer before outbound HTTP MCP calls.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpTokenExchangeConfig {
    /// `None` means enabled when the table is present. The enclosing
    /// `Option<McpTokenExchangeConfig>` keeps the default server behavior off.
    pub enabled: Option<bool>,
    /// Override token endpoint. When absent, MCP OAuth discovery supplies the
    /// authorization server token endpoint.
    pub token_url: Option<String>,
    pub actor_token: Option<String>,
    pub actor_token_type: Option<String>,
    pub subject_token_type: Option<String>,
    pub requested_token_type: Option<String>,
    /// Optional client authentication for the token-exchange request when the
    /// subject bearer came from static config. Stored OAuth bearers use their
    /// persisted client credentials by default.
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub token_endpoint_auth_method: Option<String>,
    /// Optional RFC 8693 `resource` parameter. Defaults to the MCP resource
    /// indicator; accepts a string or list of strings.
    pub resource: Option<serde_json::Value>,
    /// Optional RFC 8693 `audience` parameter; accepts a string or list of
    /// strings.
    pub audience: Option<serde_json::Value>,
    /// Optional requested scope string. Defaults to the current actor's scoped
    /// hop when the session actor chain carries scopes.
    pub scope: Option<String>,
    /// Deployment-specific token-exchange form fields.
    pub extra_params: BTreeMap<String, serde_json::Value>,
}

impl McpTokenExchangeConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegatedMcpBearer {
    pub bearer: String,
    pub base_bearer: String,
}

#[derive(Clone, Copy, Debug)]
struct TokenExchangeClientAuth<'a> {
    client_id: &'a str,
    client_secret: Option<&'a str>,
    token_endpoint_auth_method: &'a str,
}

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
    /// Non-credential extras the token endpoint returned alongside the tokens
    /// (everything except `access_token`/`refresh_token`/`expires_in`), such as a
    /// workspace name or owner record. Captured so an identity probe (harn#3349)
    /// can render a "logged in as …" string without a follow-up network call.
    /// `None` for tokens stored before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_response_extra: Option<serde_json::Value>,
}

/// The raw token-endpoint response shape. The named fields are the credential
/// material; `extra` flat-captures every other field (workspace/identity hints
/// some providers inline) without it touching the stored credentials.
#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OAuthEndpointError {
    context: &'static str,
    status: reqwest::StatusCode,
    oauth_error: Option<String>,
    body_len: usize,
}

impl OAuthEndpointError {
    fn is_invalid_grant(&self) -> bool {
        self.oauth_error.as_deref() == Some("invalid_grant")
    }
}

impl std::fmt::Display for OAuthEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.body_len == 0 {
            return write!(f, "{}: {}", self.context, self.status);
        }
        if let Some(error) = &self.oauth_error {
            return write!(
                f,
                "{}: {} (oauth error `{}`, {} byte response body omitted)",
                self.context, self.status, error, self.body_len
            );
        }
        write!(
            f,
            "{}: {} ({} byte response body omitted)",
            self.context, self.status, self.body_len
        )
    }
}

#[derive(Debug)]
enum TokenRequestError {
    Endpoint(OAuthEndpointError),
    Other(String),
}

impl TokenRequestError {
    fn is_invalid_grant(&self) -> bool {
        matches!(self, Self::Endpoint(error) if error.is_invalid_grant())
    }
}

impl std::fmt::Display for TokenRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Endpoint(error) => write!(f, "{error}"),
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Debug)]
enum TokenRefreshError {
    InvalidGrant,
    Other(String),
}

impl std::fmt::Display for TokenRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGrant => {
                write!(
                    f,
                    "Stored OAuth refresh token was rejected with invalid_grant; re-authorization required"
                )
            }
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}

/// Wrap non-empty token-response extras as a JSON object for persistence.
fn token_response_extra(
    extra: serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    (!extra.is_empty()).then_some(serde_json::Value::Object(extra))
}

/// Server-side flow state held between [`begin_authorization`] and
/// [`complete_authorization`], keyed by `state`. Holds the PKCE verifier and
/// resolved client credentials — never leaves the harn process.
#[derive(Clone, Debug)]
struct PendingFlow {
    code_verifier: String,
    redirect_uri: String,
    client_mode: OAuthClientAuthMode,
    allow_ephemeral_port: bool,
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
    begin_authorization_with_callback(request, &AuthorizationCallback::Exact).await
}

/// Begin authorization through a concrete callback adapter. This is the seam
/// shared by the CLI loopback listener and protocol-host redirect capture.
pub async fn begin_authorization_with_callback(
    request: BeginAuthorization,
    callback: &AuthorizationCallback,
) -> Result<PendingAuthorization, String> {
    let resource = canonical_resource_indicator(&request.server_url).map_err(|e| e.to_string())?;
    let discovery = discover(&request.server_url).await?;
    ensure_pkce_s256_supported(&discovery.authorization_server_metadata)?;

    let scopes = request
        .scopes
        .clone()
        .or_else(|| (!discovery.scopes.is_empty()).then(|| discovery.scopes.join(" ")));

    let selection = select_oauth_client_auth(
        &discovery.authorization_server_metadata,
        OAuthClientAuthOptions {
            mode: request.mode,
            client_id: request.client_id.as_deref(),
            client_secret: request.client_secret.as_deref(),
            client_id_metadata_document_url: request.client_id.as_deref(),
            static_secret_id: request.static_secret_id.as_deref(),
        },
    )
    .map_err(|error| {
        redirect_compatibility_error_for_mode(
            &request.redirect_uri,
            request.mode.map_or("auto", OAuthClientAuthMode::as_str),
            &error,
            false,
        )
    })?;
    let client_mode = selection.mode;
    let capabilities = match callback {
        AuthorizationCallback::Exact => CallbackCapabilities::Exact,
        AuthorizationCallback::Loopback(_) => CallbackCapabilities::LoopbackWithEphemeralPort,
    };
    let redirect_policy = select_redirect_policy(
        &discovery.authorization_server_metadata,
        client_mode,
        &request.redirect_uri,
        capabilities,
    )?;
    let redirect_uri = match callback {
        AuthorizationCallback::Exact => redirect_policy.requested_redirect_uri.clone(),
        AuthorizationCallback::Loopback(callback) => callback.acquire(&redirect_policy)?,
    };

    let (client_id, client_secret, token_auth_method) = resolve_selected_client(
        &discovery.authorization_server_metadata,
        selection,
        request.client_secret.clone(),
        &redirect_uri,
        scopes.as_deref(),
    )
    .await
    .map_err(|error| {
        redirect_compatibility_error(
            &redirect_uri,
            client_mode,
            &format!("client setup failed: {error}"),
            redirect_policy.allow_ephemeral_port,
        )
    })?;

    let (code_verifier, code_challenge) = generate_pkce_pair();
    let state = random_hex(16);
    let authorize_url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
        authorization_endpoint: &discovery
            .authorization_server_metadata
            .authorization_endpoint,
        client_id: &client_id,
        redirect_uri: &redirect_uri,
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
            redirect_uri: redirect_uri.clone(),
            client_mode,
            allow_ephemeral_port: redirect_policy.allow_ephemeral_port,
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
        redirect_uri,
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

    let client = oauth_http_client();
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
    .await
    .map_err(|error| {
        redirect_compatibility_error(
            &flow.redirect_uri,
            flow.client_mode,
            &format!("authorization-code exchange failed: {error}"),
            flow.allow_ephemeral_port,
        )
    })?;

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
        token_response_extra: token_response_extra(token.extra),
    };
    save_stored_token(&stored).await?;
    notify_authorization_completed(&stored);
    Ok(stored)
}

/// Resolve a bearer token for an already-authorized MCP server, refreshing it
/// (single-flight) if it is within the expiry skew. Returns `None` when no
/// token is stored (the caller should treat this as "auth required").
pub async fn resolve_bearer(server_url: &str) -> Result<Option<String>, String> {
    Ok(resolve_stored_token_with_discovery(server_url)
        .await?
        .map(|(token, _)| token.access_token))
}

/// Resolve a bearer for an MCP request by refreshing the stored subject token
/// under the existing single-flight lock, then exchanging it for a transient
/// delegated token when the server has opted in.
pub async fn resolve_delegated_bearer_from_store(
    server_url: &str,
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
) -> Result<Option<DelegatedMcpBearer>, String> {
    let Some((stored, discovery)) = resolve_stored_token_with_discovery(server_url).await? else {
        return Ok(None);
    };
    validate_issuer_binding(&stored.issuer, &discovery.authorization_server_issuer)?;
    let exchanged = exchange_bearer_for_actor_chain(
        server_url,
        &stored.access_token,
        &discovery,
        config,
        actor_chain,
        TokenExchangeClientAuth {
            client_id: &stored.client_id,
            client_secret: stored.client_secret.as_deref(),
            token_endpoint_auth_method: &stored.token_endpoint_auth_method,
        },
    )
    .await?;
    let bearer = exchanged.unwrap_or_else(|| stored.access_token.clone());
    Ok(Some(DelegatedMcpBearer {
        bearer,
        base_bearer: stored.access_token,
    }))
}

/// Exchange a caller-supplied subject bearer. Used for static MCP bearer
/// configs, which have no Harn-owned refresh token to update.
pub async fn exchange_configured_bearer_for_actor_chain(
    server_url: &str,
    subject_bearer: &str,
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
) -> Result<Option<String>, String> {
    let discovery = if config
        .token_url
        .as_deref()
        .is_some_and(|token_url| !token_url.trim().is_empty())
    {
        None
    } else {
        Some(discover(server_url).await?)
    };
    match discovery.as_ref() {
        Some(discovery) => {
            exchange_bearer_for_actor_chain(
                server_url,
                subject_bearer,
                discovery,
                config,
                actor_chain,
                config_token_exchange_client_auth(config),
            )
            .await
        }
        None => {
            exchange_bearer_for_actor_chain_with_endpoint(
                server_url,
                subject_bearer,
                config
                    .token_url
                    .as_deref()
                    .expect("checked token_url presence above"),
                config,
                actor_chain,
                config_token_exchange_client_auth(config),
            )
            .await
        }
    }
}

fn config_token_exchange_client_auth(
    config: &McpTokenExchangeConfig,
) -> TokenExchangeClientAuth<'_> {
    TokenExchangeClientAuth {
        client_id: config.client_id.as_deref().unwrap_or(""),
        client_secret: config.client_secret.as_deref(),
        token_endpoint_auth_method: config
            .token_endpoint_auth_method
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("none"),
    }
}

async fn resolve_stored_token_with_discovery(
    server_url: &str,
) -> Result<Option<(StoredMcpToken, McpOAuthDiscovery)>, String> {
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
    Ok(Some((stored, discovery)))
}

/// Import an existing token into the canonical Harn MCP OAuth store.
pub async fn import_stored_token(request: ImportStoredToken) -> Result<StoredMcpToken, String> {
    let discovery = discover(&request.server_url).await?;
    let stored = stored_token_for_import(&request, &discovery)?;
    save_stored_token(&stored).await?;
    notify_authorization_completed(&stored);
    Ok(stored)
}

fn auth_completion_sender() -> &'static broadcast::Sender<StoredMcpToken> {
    static SENDER: OnceLock<broadcast::Sender<StoredMcpToken>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, _) = broadcast::channel(AUTH_COMPLETION_CHANNEL_CAPACITY);
        sender
    })
}

pub(crate) fn subscribe_authorization_completions() -> broadcast::Receiver<StoredMcpToken> {
    auth_completion_sender().subscribe()
}

pub(crate) fn notify_authorization_completed(token: &StoredMcpToken) {
    let _ = auth_completion_sender().send(token.clone());
}

pub(crate) async fn wait_for_authorization_completion(
    resource: &str,
    timeout: std::time::Duration,
    mut receiver: broadcast::Receiver<StoredMcpToken>,
) -> Result<StoredMcpToken, String> {
    tokio::time::timeout(timeout, async {
        loop {
            match receiver.recv().await {
                Ok(token) if token.resource == resource => return Ok(token),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("MCP authorization completion channel closed".to_string());
                }
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for MCP authorization for {resource}"))?
}

/// Discover the OAuth authorization server protecting an MCP server URL.
pub async fn discover(server_url: &str) -> Result<McpOAuthDiscovery, String> {
    let client = oauth_http_client();
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
    delete_stored_token_and_active_index(&store, &key).await
}

/// Resolve the `(client_id, client_secret, token_endpoint_auth_method)` for a
/// flow via the unified auth selection (CIMD by default when the server
/// advertises it; otherwise BYO client id, dynamic registration, or an error).
async fn resolve_selected_client(
    metadata: &OAuthAuthorizationServerMetadata,
    selection: OAuthClientAuthSelection<'_>,
    client_secret: Option<String>,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> Result<(String, Option<String>, String), String> {
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
            // DCR registrations are deliberately per-authorization. If a
            // loopback bind falls back to a different port, this registers
            // that effective URI instead of reusing immutable client metadata
            // from an earlier authorization.
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
    let client = oauth_http_client();
    let body = dynamic_client_registration_body("Harn", [redirect_uri], scopes);
    let response = client
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            format!(
                "Dynamic client registration failed: {}",
                crate::egress::redact_reqwest_error(&error)
            )
        })?;
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
) -> Result<StoredMcpToken, TokenRefreshError> {
    validate_issuer_binding(&token.issuer, &discovery.authorization_server_issuer)
        .map_err(TokenRefreshError::Other)?;
    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        TokenRefreshError::Other(
            "Stored OAuth token has expired and does not include a refresh token".to_string(),
        )
    })?;
    let resource =
        canonical_resource_indicator(&token.resource).unwrap_or_else(|_| token.resource.clone());
    let client = oauth_http_client();
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
    .await
    .map_err(|error| {
        if error.is_invalid_grant() {
            TokenRefreshError::InvalidGrant
        } else {
            TokenRefreshError::Other(error.to_string())
        }
    })?;
    Ok(StoredMcpToken {
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .or_else(|| token.refresh_token.clone()),
        expires_at_unix: expires_at_from_expires_in(refreshed.expires_in)
            .map_err(TokenRefreshError::Other)?,
        token_endpoint,
        client_id: token.client_id.clone(),
        client_secret: token.client_secret.clone(),
        token_endpoint_auth_method: token.token_endpoint_auth_method.clone(),
        issuer: token.issuer.clone(),
        resource,
        scopes: token.scopes.clone(),
        // Prefer fresh extras if the refresh carried any; otherwise keep the
        // identity hints captured at first authorization.
        token_response_extra: token_response_extra(refreshed.extra)
            .or_else(|| token.token_response_extra.clone()),
    })
}

async fn request_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    token_auth_method: &str,
    client_id: &str,
    client_secret: Option<&str>,
    form: &[(&str, String)],
) -> Result<TokenResponse, TokenRequestError> {
    validate_token_endpoint_auth_method(token_auth_method).map_err(TokenRequestError::Other)?;
    let mut request = client.post(token_endpoint).form(form);
    match token_auth_method {
        "client_secret_basic" => {
            let client_secret = client_secret.ok_or_else(|| {
                TokenRequestError::Other(
                    "Missing client secret for client_secret_basic".to_string(),
                )
            })?;
            request = request.basic_auth(client_id, Some(client_secret));
        }
        "client_secret_post" => {
            let client_secret = client_secret.ok_or_else(|| {
                TokenRequestError::Other("Missing client secret for client_secret_post".to_string())
            })?;
            let mut extended = form.to_vec();
            extended.push(("client_secret", client_secret.to_string()));
            request = client.post(token_endpoint).form(&extended);
        }
        _ => {}
    }
    let response = request.send().await.map_err(|error| {
        TokenRequestError::Other(format!(
            "Token request failed: {}",
            crate::egress::redact_reqwest_error(&error)
        ))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(TokenRequestError::Endpoint(oauth_endpoint_error(
            "Token request failed",
            status,
            &body,
        )));
    }
    response
        .json::<TokenResponse>()
        .await
        .map_err(|error| TokenRequestError::Other(format!("Invalid token response: {error}")))
}

async fn exchange_bearer_for_actor_chain(
    server_url: &str,
    subject_bearer: &str,
    discovery: &McpOAuthDiscovery,
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
    client_auth: TokenExchangeClientAuth<'_>,
) -> Result<Option<String>, String> {
    let token_endpoint = config
        .token_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&discovery.authorization_server_metadata.token_endpoint);
    exchange_bearer_for_actor_chain_with_endpoint(
        server_url,
        subject_bearer,
        token_endpoint,
        config,
        actor_chain,
        client_auth,
    )
    .await
}

async fn exchange_bearer_for_actor_chain_with_endpoint(
    server_url: &str,
    subject_bearer: &str,
    token_endpoint: &str,
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
    client_auth: TokenExchangeClientAuth<'_>,
) -> Result<Option<String>, String> {
    let Some(form) = token_exchange_form(server_url, subject_bearer, config, actor_chain)? else {
        return Ok(None);
    };
    validate_token_endpoint_auth_method(client_auth.token_endpoint_auth_method)?;
    let client = oauth_http_client();
    let response = send_token_exchange_request(&client, token_endpoint, &form, client_auth).await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if token_exchange_unsupported(status, &body) {
            return Ok(None);
        }
        return Err(oauth_http_error("Token exchange failed", status, &body));
    }
    let token = response
        .json::<TokenResponse>()
        .await
        .map_err(|error| format!("Invalid token exchange response: {error}"))?;
    Ok(Some(token.access_token))
}

async fn send_token_exchange_request(
    client: &reqwest::Client,
    token_endpoint: &str,
    form: &[(String, String)],
    client_auth: TokenExchangeClientAuth<'_>,
) -> Result<reqwest::Response, String> {
    let mut request = client.post(token_endpoint).form(form);
    match client_auth.token_endpoint_auth_method {
        "client_secret_basic" => {
            let client_secret = client_auth
                .client_secret
                .ok_or_else(|| "Missing client secret for client_secret_basic".to_string())?;
            if client_auth.client_id.trim().is_empty() {
                return Err("Missing client_id for client_secret_basic".to_string());
            }
            request = request.basic_auth(client_auth.client_id, Some(client_secret));
        }
        "client_secret_post" => {
            let client_secret = client_auth
                .client_secret
                .ok_or_else(|| "Missing client secret for client_secret_post".to_string())?;
            if client_auth.client_id.trim().is_empty() {
                return Err("Missing client_id for client_secret_post".to_string());
            }
            let mut extended = form.to_vec();
            extended.push(("client_id".to_string(), client_auth.client_id.to_string()));
            extended.push(("client_secret".to_string(), client_secret.to_string()));
            request = client.post(token_endpoint).form(&extended);
        }
        "none" => {}
        other => {
            return Err(format!(
                "unsupported token auth method '{other}'; expected none, client_secret_post, or client_secret_basic"
            ))
        }
    }
    request.send().await.map_err(|error| {
        format!(
            "Token exchange request failed: {}",
            crate::egress::redact_reqwest_error(&error)
        )
    })
}

fn token_exchange_form(
    server_url: &str,
    subject_bearer: &str,
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
) -> Result<Option<Vec<(String, String)>>, String> {
    if !config.is_enabled() || !actor_chain.is_delegated() {
        return Ok(None);
    }
    let actor_token = match config
        .actor_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(actor_token) => actor_token,
        None => {
            return Err(
                "MCP token exchange actor_token is required for delegated actor-chain requests"
                    .to_string(),
            )
        }
    };
    let subject_token_type = normalize_token_type(
        config.subject_token_type.as_deref(),
        TOKEN_TYPE_ACCESS_TOKEN,
        "subject_token_type",
    )?;
    let actor_token_type = normalize_token_type(
        config.actor_token_type.as_deref(),
        TOKEN_TYPE_JWT,
        "actor_token_type",
    )?;
    let requested_token_type = config
        .requested_token_type
        .as_deref()
        .map(|value| {
            normalize_token_type(Some(value), TOKEN_TYPE_ACCESS_TOKEN, "requested_token_type")
        })
        .transpose()?;
    let mut form = vec![
        (
            "grant_type".to_string(),
            TOKEN_EXCHANGE_GRANT_TYPE.to_string(),
        ),
        ("subject_token".to_string(), subject_bearer.to_string()),
        ("subject_token_type".to_string(), subject_token_type),
        ("actor_token".to_string(), actor_token.to_string()),
        ("actor_token_type".to_string(), actor_token_type),
    ];
    if let Some(requested_token_type) = requested_token_type {
        form.push(("requested_token_type".to_string(), requested_token_type));
    }

    match config.resource.as_ref() {
        Some(value) => append_form_values(&mut form, "resource", value)?,
        None => {
            let resource =
                canonical_resource_indicator(server_url).map_err(|error| error.to_string())?;
            form.push(("resource".to_string(), resource));
        }
    }
    if let Some(value) = config.audience.as_ref() {
        append_form_values(&mut form, "audience", value)?;
    }
    if let Some(scope) = requested_scope(config, actor_chain) {
        form.push(("scope".to_string(), scope));
    }
    for (key, value) in &config.extra_params {
        append_form_values(&mut form, key, value)?;
    }
    Ok(Some(form))
}

fn requested_scope(
    config: &McpTokenExchangeConfig,
    actor_chain: &crate::actor_chain::ActorChain,
) -> Option<String> {
    config
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let scopes = actor_chain.current_entry().scopes().collect::<Vec<_>>();
            (!scopes.is_empty()).then(|| scopes.join(" "))
        })
}

fn normalize_token_type(
    value: Option<&str>,
    default_value: &str,
    field: &str,
) -> Result<String, String> {
    let raw = value.unwrap_or(default_value).trim();
    if raw.is_empty() {
        return Err(format!("MCP token exchange {field} must not be empty"));
    }
    if raw.starts_with("urn:") {
        return Ok(raw.to_string());
    }
    let normalized = match raw.to_ascii_lowercase().as_str() {
        "access" | "access_token" => "access_token",
        "refresh_token" => "refresh_token",
        "id_token" => "id_token",
        "jwt" => "jwt",
        "saml1" => "saml1",
        "saml2" => "saml2",
        _ => {
            return Err(format!(
                "MCP token exchange {field} uses unsupported token type `{raw}`"
            ))
        }
    };
    Ok(format!("{TOKEN_TYPE_PREFIX}{normalized}"))
}

fn append_form_values(
    form: &mut Vec<(String, String)>,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    for item in form_value_strings(key, value)? {
        form.push((key.to_string(), item));
    }
    Ok(())
}

fn form_value_strings(key: &str, value: &serde_json::Value) -> Result<Vec<String>, String> {
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::String(value) => Ok(vec![value.clone()]),
        serde_json::Value::Bool(value) => Ok(vec![value.to_string()]),
        serde_json::Value::Number(value) => Ok(vec![value.to_string()]),
        serde_json::Value::Array(values) => {
            let mut out = Vec::new();
            for value in values {
                match value {
                    serde_json::Value::String(value) => out.push(value.clone()),
                    serde_json::Value::Bool(value) => out.push(value.to_string()),
                    serde_json::Value::Number(value) => out.push(value.to_string()),
                    serde_json::Value::Null => {}
                    _ => return Err(format!("MCP token exchange `{key}` values must be scalars")),
                }
            }
            Ok(out)
        }
        _ => Err(format!(
            "MCP token exchange `{key}` must be a scalar or list"
        )),
    }
}

fn token_exchange_unsupported(status: reqwest::StatusCode, body: &str) -> bool {
    if status != reqwest::StatusCode::BAD_REQUEST {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    matches!(
        value.get("error").and_then(serde_json::Value::as_str),
        Some("unsupported_grant_type" | "unsupported_token_type")
    )
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

fn oauth_http_error(context: &'static str, status: reqwest::StatusCode, body: &str) -> String {
    oauth_endpoint_error(context, status, body).to_string()
}

fn oauth_endpoint_error(
    context: &'static str,
    status: reqwest::StatusCode,
    body: &str,
) -> OAuthEndpointError {
    OAuthEndpointError {
        context,
        status,
        oauth_error: oauth_error_code(body),
        body_len: body.len(),
    }
}

fn oauth_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = value.get("error")?.as_str()?.trim();
    (!error.is_empty()).then(|| error.to_string())
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
    match refresh_token(&current, discovery).await {
        Ok(refreshed) => {
            store.save_token(&refreshed).await?;
            Ok(refreshed)
        }
        Err(TokenRefreshError::InvalidGrant) => {
            delete_stored_token_and_active_index(store, &key).await?;
            Err(TokenRefreshError::InvalidGrant.to_string())
        }
        Err(TokenRefreshError::Other(error)) => Err(error),
    }
}

struct OAuthRefreshLockGuard {
    _async_guard: tokio::sync::OwnedMutexGuard<()>,
    file: File,
}

impl std::fmt::Debug for OAuthRefreshLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthRefreshLockGuard")
            .finish_non_exhaustive()
    }
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
    acquire_oauth_refresh_lock_with_timeout(key, lock_dir_override, OAUTH_REFRESH_LOCK_TIMEOUT)
        .await
}

/// Acquire the single-flight refresh locks with a bounded wait. Both phases
/// are time-limited so one wedged refresher (in this process or another) can
/// only ever delay — never permanently block — later 401 recovery:
///
/// 1. The in-process async mutex is awaited under `tokio::time::timeout`.
/// 2. The cross-process file lock uses a non-blocking `try_lock` with
///    retry/backoff instead of a blocking `lock`, which would otherwise pin a
///    `spawn_blocking` thread indefinitely while another *process* holds the
///    lock.
async fn acquire_oauth_refresh_lock_with_timeout(
    key: &OAuthTokenStoreKey,
    lock_dir_override: Option<&Path>,
    lock_timeout: std::time::Duration,
) -> Result<OAuthRefreshLockGuard, String> {
    let mutex = oauth_refresh_mutex(key);
    let async_guard = tokio::time::timeout(lock_timeout, mutex.lock_owned())
        .await
        .map_err(|_| {
            format!(
                "Timed out after {}s waiting for the in-process OAuth refresh lock for `{}`; \
                 a concurrent refresh of this token appears to be stuck (most likely a \
                 token-endpoint request that never completed)",
                lock_timeout.as_secs(),
                key.account()
            )
        })?;
    let lock_path = oauth_refresh_lock_path(key, lock_dir_override);
    let open_path = lock_path.clone();
    let file = crate::task_spawn::spawn_blocking(move || {
        if let Some(parent) = open_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create OAuth token lock directory `{}`: {error}",
                    parent.display()
                )
            })?;
        }
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&open_path)
            .map_err(|error| {
                format!(
                    "Failed to open OAuth token lock `{}`: {error}",
                    open_path.display()
                )
            })
    })
    .await
    .map_err(|error| format!("OAuth token lock task failed: {error}"))??;

    let deadline = tokio::time::Instant::now() + lock_timeout;
    let mut backoff = std::time::Duration::from_millis(25);
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "Timed out after {}s waiting for the cross-process OAuth refresh lock \
                         `{}`; another harn process refreshing this token appears to be stuck",
                        lock_timeout.as_secs(),
                        lock_path.display()
                    ));
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_millis(500));
            }
            Err(error) => {
                return Err(format!(
                    "Failed to acquire OAuth token lock `{}`: {error}",
                    lock_path.display()
                ));
            }
        }
    }
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
    crate::user_dirs::home_dir()
        .map(|home| home.join(".harn"))
        .unwrap_or_else(|| std::env::temp_dir().join("harn"))
}

/// Persist a token (under the refresh lock) and record it as the active client.
async fn save_stored_token(token: &StoredMcpToken) -> Result<(), String> {
    let store = KeyringOAuthTokenStorage::default();
    let key = OAuthTokenStoreKey::from_token(token);
    let _guard = acquire_oauth_refresh_lock(&key, None).await?;
    store.save_token(token).await
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
