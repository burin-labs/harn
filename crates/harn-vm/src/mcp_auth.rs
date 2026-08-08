//! MCP OAuth/OIDC authorization helpers.
//!
//! The MCP authorization profile is an HTTP transport profile. This module
//! keeps discovery, challenge parsing, issuer binding, and registration-mode
//! decisions in one place so Harn clients and servers do not each carry partial
//! copies of the OAuth/OIDC rules.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use reqwest::header::{ACCEPT, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use url::Url;

pub const OAUTH_PROTECTED_RESOURCE_WELL_KNOWN_PATH: &str = "/.well-known/oauth-protected-resource";
pub const OAUTH_AUTHORIZATION_SERVER_WELL_KNOWN_PATH: &str =
    "/.well-known/oauth-authorization-server";
pub const OIDC_CONFIGURATION_WELL_KNOWN_PATH: &str = "/.well-known/openid-configuration";
pub const DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL: &str =
    "https://harnlang.com/.well-known/oauth-client.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WwwAuthenticateChallenge {
    pub scheme: String,
    pub params: BTreeMap<String, String>,
}

impl WwwAuthenticateChallenge {
    pub fn bearer_resource_metadata(&self) -> Option<&str> {
        self.scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| self.params.get("resource_metadata").map(String::as_str))
            .flatten()
    }

    pub fn bearer_scope(&self) -> Option<&str> {
        self.scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| self.params.get("scope").map(String::as_str))
            .flatten()
    }

    /// The RFC 6750 §3 `error` code carried by a Bearer challenge, e.g.
    /// `invalid_token` or `insufficient_scope`. A resource server returns a
    /// `403` with `error="insufficient_scope"` when the presented token is
    /// valid but lacks a required scope — the cue to run a step-up
    /// authorization requesting the additional scope from [`bearer_scope`].
    pub fn bearer_error(&self) -> Option<&str> {
        self.scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| self.params.get("error").map(String::as_str))
            .flatten()
    }

    /// True when this Bearer challenge signals `insufficient_scope` — a valid
    /// token missing a required scope, resolvable by re-authorizing with the
    /// elevated scope.
    pub fn is_insufficient_scope(&self) -> bool {
        self.bearer_error()
            .is_some_and(|error| error.eq_ignore_ascii_case("insufficient_scope"))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OAuthProtectedResourceMetadata {
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    #[serde(default)]
    pub bearer_methods_supported: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OAuthAuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    #[serde(default)]
    pub client_id_metadata_document_supported: bool,
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OAuthDynamicClientRegistrationResponse {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthAuthorizationServerMetadataKind {
    OAuthAuthorizationServer,
    OpenIdConfiguration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthAuthorizationServerMetadataCandidate {
    pub url: Url,
    pub kind: OAuthAuthorizationServerMetadataKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthClientRegistrationMode {
    PreRegistered,
    ClientIdMetadataDocument,
    DynamicClientRegistration,
    Manual,
}

impl OAuthClientRegistrationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreRegistered => "pre_registered",
            Self::ClientIdMetadataDocument => "client_id_metadata_document",
            Self::DynamicClientRegistration => "dynamic_client_registration",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthClientAuthMode {
    Cimd,
    Dcr,
    Static,
    Byo,
}

impl OAuthClientAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cimd => "cimd",
            Self::Dcr => "dcr",
            Self::Static => "static",
            Self::Byo => "byo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthApplicationType {
    Native,
    Web,
}

impl OAuthApplicationType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Web => "web",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OAuthClientRegistrationOptions<'a> {
    pub client_id: Option<&'a str>,
    pub client_secret: Option<&'a str>,
    pub client_id_metadata_document_url: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct OAuthClientAuthOptions<'a> {
    pub mode: Option<OAuthClientAuthMode>,
    pub client_id: Option<&'a str>,
    pub client_secret: Option<&'a str>,
    pub client_id_metadata_document_url: Option<&'a str>,
    pub static_secret_id: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClientAuthSelection<'a> {
    pub mode: OAuthClientAuthMode,
    pub client_id: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct McpOAuthDiscovery {
    pub protected_resource_metadata_url: Url,
    pub protected_resource_metadata: OAuthProtectedResourceMetadata,
    pub authorization_server_issuer: String,
    pub authorization_server_metadata_url: Url,
    pub authorization_server_metadata_kind: OAuthAuthorizationServerMetadataKind,
    pub authorization_server_metadata: OAuthAuthorizationServerMetadata,
    pub challenge: Option<WwwAuthenticateChallenge>,
    pub scopes: Vec<String>,
}

#[derive(Debug)]
pub enum McpOAuthDiscoveryError {
    InvalidResourceUrl(String),
    InvalidResourceMetadataUrl(String),
    InvalidAuthorizationServerUrl { issuer: String, error: String },
    ProtectedResourceMetadataNotFound,
    MissingAuthorizationServer,
    AuthorizationServerMetadataNotFound { issuer: String },
    AuthorizationServerIssuerMismatch { expected: String, actual: String },
    AuthorizationServerIssuerMissing { expected: String },
    Json { url: String, error: String },
}

impl fmt::Display for McpOAuthDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResourceUrl(error) => write!(f, "invalid MCP resource URL: {error}"),
            Self::InvalidResourceMetadataUrl(error) => {
                write!(f, "invalid resource_metadata URL in WWW-Authenticate: {error}")
            }
            Self::InvalidAuthorizationServerUrl { issuer, error } => {
                write!(f, "invalid authorization server URL '{issuer}': {error}")
            }
            Self::ProtectedResourceMetadataNotFound => {
                write!(f, "OAuth protected resource metadata not found")
            }
            Self::MissingAuthorizationServer => write!(
                f,
                "OAuth protected resource metadata did not advertise an authorization server"
            ),
            Self::AuthorizationServerMetadataNotFound { issuer } => {
                write!(f, "authorization server metadata not found for issuer '{issuer}'")
            }
            Self::AuthorizationServerIssuerMismatch { expected, actual } => write!(
                f,
                "authorization server metadata issuer mismatch: expected '{expected}', got '{actual}'"
            ),
            Self::AuthorizationServerIssuerMissing { expected } => write!(
                f,
                "authorization server metadata for '{expected}' did not include an issuer"
            ),
            Self::Json { url, error } => write!(f, "failed to parse {url}: {error}"),
        }
    }
}

impl std::error::Error for McpOAuthDiscoveryError {}

pub fn parse_www_authenticate(header: &str) -> Vec<WwwAuthenticateChallenge> {
    let mut challenges = Vec::<WwwAuthenticateChallenge>::new();
    let mut current: Option<WwwAuthenticateChallenge> = None;

    for segment in split_challenge_segments(header) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let (first, rest) = split_first_token(segment);
        let starts_challenge = !first.contains('=');
        if starts_challenge {
            if let Some(challenge) = current.take() {
                challenges.push(challenge);
            }
            let mut challenge = WwwAuthenticateChallenge {
                scheme: first.to_string(),
                params: BTreeMap::new(),
            };
            if !rest.trim().is_empty() {
                parse_auth_param(rest.trim(), &mut challenge.params);
            }
            current = Some(challenge);
        } else if let Some(challenge) = current.as_mut() {
            parse_auth_param(segment, &mut challenge.params);
        }
    }

    if let Some(challenge) = current {
        challenges.push(challenge);
    }
    challenges
}

pub fn parse_www_authenticate_headers<'a>(
    headers: impl IntoIterator<Item = &'a str>,
) -> Vec<WwwAuthenticateChallenge> {
    headers
        .into_iter()
        .flat_map(parse_www_authenticate)
        .collect()
}

pub fn bearer_challenge_from_headers<'a>(
    headers: impl IntoIterator<Item = &'a str>,
) -> Option<WwwAuthenticateChallenge> {
    let mut first_bearer = None;
    for challenge in parse_www_authenticate_headers(headers) {
        if !challenge.scheme.eq_ignore_ascii_case("bearer") {
            continue;
        }
        if challenge.bearer_resource_metadata().is_some() {
            return Some(challenge);
        }
        first_bearer.get_or_insert(challenge);
    }
    first_bearer
}

/// Canonicalize an MCP server URL into the RFC 8707 resource indicator that
/// MUST be sent as `resource` in both the authorization request and the token
/// request.
///
/// The MCP authorization profile reuses RFC 8707 §2 resource-indicator
/// canonicalization: the scheme and host are lowercased, a default port for the
/// scheme is dropped, and the fragment, query, and trailing slash are removed.
pub fn canonical_resource_indicator(server_url: &str) -> Result<String, McpOAuthDiscoveryError> {
    let mut url = Url::parse(server_url)
        .map_err(|error| McpOAuthDiscoveryError::InvalidResourceUrl(error.to_string()))?;
    // The `url` crate already lowercases the scheme and host on parse, but be
    // explicit so the canonical form is stable regardless of input casing.
    url.set_fragment(None);
    url.set_query(None);
    let _ = url.set_scheme(&url.scheme().to_ascii_lowercase());
    if let Some(host) = url.host_str() {
        let lowered = host.to_ascii_lowercase();
        if lowered != host {
            let _ = url.set_host(Some(&lowered));
        }
    }
    // Drop a default port so `https://host:443` and `https://host` canonicalize
    // identically, while leaving non-default ports intact.
    if let Some(port) = url.port() {
        if Some(port) == default_port_for_scheme(url.scheme()) {
            let _ = url.set_port(None);
        }
    }
    let mut canonical = url.to_string();
    canonical = canonical.trim_end_matches('/').to_string();
    Ok(canonical)
}

#[derive(Clone, Copy, Debug)]
pub struct OAuthAuthorizationUrlOptions<'a> {
    pub authorization_endpoint: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub resource: &'a str,
    pub scopes: Option<&'a str>,
}

pub fn build_oauth_authorization_url(
    options: OAuthAuthorizationUrlOptions<'_>,
) -> Result<Url, String> {
    let mut url = Url::parse(options.authorization_endpoint)
        .map_err(|error| format!("Invalid authorization endpoint: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", options.client_id);
        query.append_pair("redirect_uri", options.redirect_uri);
        query.append_pair("state", options.state);
        query.append_pair("code_challenge", options.code_challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("resource", options.resource);
        if let Some(scopes) = options.scopes {
            query.append_pair("scope", scopes);
        }
    }
    Ok(url)
}

#[derive(Clone, Copy, Debug)]
pub struct OAuthAuthorizationCodeTokenForm<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub resource: &'a str,
    pub scopes: Option<&'a str>,
}

pub fn authorization_code_token_form(
    request: OAuthAuthorizationCodeTokenForm<'_>,
) -> Vec<(&'static str, String)> {
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
    form
}

#[derive(Clone, Copy, Debug)]
pub struct OAuthRefreshTokenForm<'a> {
    pub client_id: &'a str,
    pub refresh_token: &'a str,
    pub resource: &'a str,
}

pub fn refresh_token_form(request: OAuthRefreshTokenForm<'_>) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", request.refresh_token.to_string()),
        ("client_id", request.client_id.to_string()),
        ("resource", request.resource.to_string()),
    ]
}

pub fn protected_resource_metadata_candidates(resource_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let path = resource_url
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/');
    if !path.is_empty() {
        let mut url = resource_url.clone();
        url.set_path(&format!(
            "{OAUTH_PROTECTED_RESOURCE_WELL_KNOWN_PATH}/{path}"
        ));
        url.set_query(None);
        url.set_fragment(None);
        urls.push(url);
    }
    let mut root = resource_url.clone();
    root.set_path(OAUTH_PROTECTED_RESOURCE_WELL_KNOWN_PATH);
    root.set_query(None);
    root.set_fragment(None);
    urls.push(root);
    urls
}

pub fn protected_resource_metadata_path(mcp_path: &str) -> String {
    let mcp_path = normalize_path(mcp_path);
    if mcp_path == "/" {
        OAUTH_PROTECTED_RESOURCE_WELL_KNOWN_PATH.to_string()
    } else {
        format!("{OAUTH_PROTECTED_RESOURCE_WELL_KNOWN_PATH}{mcp_path}")
    }
}

pub fn authorization_server_metadata_candidates(
    auth_server_url: &Url,
) -> Vec<OAuthAuthorizationServerMetadataCandidate> {
    let mut urls = Vec::new();
    let path = auth_server_url.path();
    let has_path = !path.is_empty() && path != "/";
    if has_path {
        let trimmed = path.trim_start_matches('/');

        let mut oauth = auth_server_url.clone();
        oauth.set_path(&format!(
            "{OAUTH_AUTHORIZATION_SERVER_WELL_KNOWN_PATH}/{trimmed}"
        ));
        oauth.set_query(None);
        oauth.set_fragment(None);
        urls.push(OAuthAuthorizationServerMetadataCandidate {
            url: oauth,
            kind: OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
        });

        let mut oidc_inserted = auth_server_url.clone();
        oidc_inserted.set_path(&format!("{OIDC_CONFIGURATION_WELL_KNOWN_PATH}/{trimmed}"));
        oidc_inserted.set_query(None);
        oidc_inserted.set_fragment(None);
        urls.push(OAuthAuthorizationServerMetadataCandidate {
            url: oidc_inserted,
            kind: OAuthAuthorizationServerMetadataKind::OpenIdConfiguration,
        });

        let mut oidc_appended = auth_server_url.clone();
        let base = path.trim_end_matches('/');
        oidc_appended.set_path(&format!("{base}{OIDC_CONFIGURATION_WELL_KNOWN_PATH}"));
        oidc_appended.set_query(None);
        oidc_appended.set_fragment(None);
        urls.push(OAuthAuthorizationServerMetadataCandidate {
            url: oidc_appended,
            kind: OAuthAuthorizationServerMetadataKind::OpenIdConfiguration,
        });
        return urls;
    }

    let mut oauth = auth_server_url.clone();
    oauth.set_path(OAUTH_AUTHORIZATION_SERVER_WELL_KNOWN_PATH);
    oauth.set_query(None);
    oauth.set_fragment(None);
    urls.push(OAuthAuthorizationServerMetadataCandidate {
        url: oauth,
        kind: OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
    });

    let mut oidc = auth_server_url.clone();
    oidc.set_path(OIDC_CONFIGURATION_WELL_KNOWN_PATH);
    oidc.set_query(None);
    oidc.set_fragment(None);
    urls.push(OAuthAuthorizationServerMetadataCandidate {
        url: oidc,
        kind: OAuthAuthorizationServerMetadataKind::OpenIdConfiguration,
    });
    urls
}

pub async fn discover_mcp_oauth(
    client: &reqwest::Client,
    resource: &str,
) -> Result<McpOAuthDiscovery, McpOAuthDiscoveryError> {
    let resource_url = Url::parse(resource)
        .map_err(|error| McpOAuthDiscoveryError::InvalidResourceUrl(error.to_string()))?;
    discover_mcp_oauth_from_url(client, &resource_url).await
}

pub async fn discover_mcp_oauth_from_url(
    client: &reqwest::Client,
    resource_url: &Url,
) -> Result<McpOAuthDiscovery, McpOAuthDiscoveryError> {
    let challenge = fetch_resource_challenge(client, resource_url).await;
    let challenged_metadata_url = challenge
        .as_ref()
        .and_then(WwwAuthenticateChallenge::bearer_resource_metadata)
        .map(|url| {
            Url::parse(url).map_err(|error| {
                McpOAuthDiscoveryError::InvalidResourceMetadataUrl(error.to_string())
            })
        })
        .transpose()?;

    let metadata_candidates = challenged_metadata_url
        .into_iter()
        .chain(protected_resource_metadata_candidates(resource_url))
        .collect::<Vec<_>>();
    let (protected_resource_metadata_url, protected_resource_metadata) =
        fetch_first_json::<OAuthProtectedResourceMetadata>(client, &metadata_candidates)
            .await?
            .ok_or(McpOAuthDiscoveryError::ProtectedResourceMetadataNotFound)?;
    let authorization_server_issuer = protected_resource_metadata
        .authorization_servers
        .first()
        .cloned()
        .ok_or(McpOAuthDiscoveryError::MissingAuthorizationServer)?;
    let auth_server_url = Url::parse(&authorization_server_issuer).map_err(|error| {
        McpOAuthDiscoveryError::InvalidAuthorizationServerUrl {
            issuer: authorization_server_issuer.clone(),
            error: error.to_string(),
        }
    })?;
    let (authorization_server_metadata_url, authorization_server_metadata_kind, metadata) =
        fetch_authorization_server_metadata(client, &authorization_server_issuer, &auth_server_url)
            .await?;
    let scopes = select_oauth_scopes(
        challenge
            .as_ref()
            .and_then(WwwAuthenticateChallenge::bearer_scope),
        &protected_resource_metadata.scopes_supported,
    );
    Ok(McpOAuthDiscovery {
        protected_resource_metadata_url,
        protected_resource_metadata,
        authorization_server_issuer,
        authorization_server_metadata_url,
        authorization_server_metadata_kind,
        authorization_server_metadata: metadata,
        challenge,
        scopes,
    })
}

pub async fn fetch_authorization_server_metadata(
    client: &reqwest::Client,
    expected_issuer: &str,
    auth_server_url: &Url,
) -> Result<
    (
        Url,
        OAuthAuthorizationServerMetadataKind,
        OAuthAuthorizationServerMetadata,
    ),
    McpOAuthDiscoveryError,
> {
    let candidates = authorization_server_metadata_candidates(auth_server_url);
    for candidate in candidates {
        let Some(metadata) =
            fetch_json::<OAuthAuthorizationServerMetadata>(client, &candidate.url).await?
        else {
            continue;
        };
        validate_authorization_server_issuer(expected_issuer, &metadata)?;
        return Ok((candidate.url, candidate.kind, metadata));
    }
    Err(
        McpOAuthDiscoveryError::AuthorizationServerMetadataNotFound {
            issuer: expected_issuer.to_string(),
        },
    )
}

pub fn validate_authorization_server_issuer(
    expected_issuer: &str,
    metadata: &OAuthAuthorizationServerMetadata,
) -> Result<(), McpOAuthDiscoveryError> {
    if metadata.issuer.is_empty() {
        return Err(McpOAuthDiscoveryError::AuthorizationServerIssuerMissing {
            expected: expected_issuer.to_string(),
        });
    }
    if metadata.issuer != expected_issuer {
        return Err(McpOAuthDiscoveryError::AuthorizationServerIssuerMismatch {
            expected: expected_issuer.to_string(),
            actual: metadata.issuer.clone(),
        });
    }
    Ok(())
}

pub fn validate_authorization_response_issuer(
    metadata: &OAuthAuthorizationServerMetadata,
    response_issuer: Option<&str>,
) -> Result<(), String> {
    validate_authorization_response_issuer_value(
        &metadata.issuer,
        metadata.authorization_response_iss_parameter_supported,
        response_issuer,
    )
}

/// Validate the RFC 9207 `iss` authorization-response parameter against the
/// expected issuer. A redirect's `iss` (when present) must match; when the
/// authorization server advertises `iss` support it MUST also be present.
/// Callers that hold the parsed metadata should prefer
/// [`validate_authorization_response_issuer`]; this value form is for paths
/// that only retain the issuer + support flag (e.g. a pending OAuth flow).
pub fn validate_authorization_response_issuer_value(
    expected_issuer: &str,
    iss_supported: bool,
    response_issuer: Option<&str>,
) -> Result<(), String> {
    match (iss_supported, response_issuer) {
        (_, Some(actual)) if actual == expected_issuer => Ok(()),
        (_, Some(actual)) => Err(format!(
            "authorization response issuer mismatch: expected '{expected_issuer}', got '{actual}'"
        )),
        (true, None) => Err(
            "authorization response did not include required RFC 9207 iss parameter".to_string(),
        ),
        (false, None) => Ok(()),
    }
}

pub fn validate_issuer_binding(stored_issuer: &str, current_issuer: &str) -> Result<(), String> {
    if stored_issuer == current_issuer {
        Ok(())
    } else {
        Err(format!(
            "stored OAuth credentials are bound to issuer '{stored_issuer}', but the MCP resource now advertises '{current_issuer}'"
        ))
    }
}

pub fn select_oauth_scopes(
    challenge_scope: Option<&str>,
    scopes_supported: &[String],
) -> Vec<String> {
    let challenged = split_scope_value(challenge_scope);
    if challenged.is_empty() {
        dedupe_scopes(scopes_supported.iter().map(String::as_str))
    } else {
        challenged
    }
}

pub fn accumulate_oauth_scopes<'a>(
    existing: impl IntoIterator<Item = &'a str>,
    challenged: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    dedupe_scopes(existing.into_iter().chain(challenged))
}

pub fn split_scope_value(value: Option<&str>) -> Vec<String> {
    dedupe_scopes(
        value
            .unwrap_or_default()
            .split_whitespace()
            .map(str::trim)
            .filter(|scope| !scope.is_empty()),
    )
}

pub fn select_client_registration_mode(
    metadata: &OAuthAuthorizationServerMetadata,
    options: OAuthClientRegistrationOptions<'_>,
) -> OAuthClientRegistrationMode {
    match select_oauth_client_auth(
        metadata,
        OAuthClientAuthOptions {
            client_id: options.client_id,
            client_secret: options.client_secret,
            client_id_metadata_document_url: options.client_id_metadata_document_url,
            ..OAuthClientAuthOptions::default()
        },
    ) {
        Ok(selection) => match selection.mode {
            OAuthClientAuthMode::Cimd => OAuthClientRegistrationMode::ClientIdMetadataDocument,
            OAuthClientAuthMode::Dcr => OAuthClientRegistrationMode::DynamicClientRegistration,
            OAuthClientAuthMode::Byo => OAuthClientRegistrationMode::PreRegistered,
            OAuthClientAuthMode::Static => OAuthClientRegistrationMode::Manual,
        },
        Err(_) => OAuthClientRegistrationMode::Manual,
    }
}

pub fn select_oauth_client_auth<'a>(
    metadata: &OAuthAuthorizationServerMetadata,
    options: OAuthClientAuthOptions<'a>,
) -> Result<OAuthClientAuthSelection<'a>, String> {
    if let Some(mode) = options.mode {
        return match mode {
            OAuthClientAuthMode::Static => {
                if options.static_secret_id.is_none() {
                    return Err("static MCP auth requires a secret_id".to_string());
                }
                Ok(OAuthClientAuthSelection {
                    mode,
                    client_id: None,
                })
            }
            OAuthClientAuthMode::Byo => {
                let client_id = options
                    .client_id
                    .ok_or_else(|| "BYO OAuth auth requires client_id".to_string())?;
                Ok(OAuthClientAuthSelection {
                    mode,
                    client_id: Some(client_id),
                })
            }
            OAuthClientAuthMode::Cimd => {
                if !metadata.client_id_metadata_document_supported {
                    return Err(
                        "authorization server does not advertise Client ID Metadata Document support"
                            .to_string(),
                    );
                }
                let client_id = cimd_client_id(options);
                if !is_client_id_metadata_document_url(client_id) {
                    return Err(
                        "CIMD OAuth auth requires an HTTPS client metadata document URL"
                            .to_string(),
                    );
                }
                Ok(OAuthClientAuthSelection {
                    mode,
                    client_id: Some(client_id),
                })
            }
            OAuthClientAuthMode::Dcr => {
                if metadata.registration_endpoint.is_none() {
                    return Err(
                        "authorization server does not advertise dynamic client registration"
                            .to_string(),
                    );
                }
                Ok(OAuthClientAuthSelection {
                    mode,
                    client_id: None,
                })
            }
        };
    }

    if options.static_secret_id.is_some() {
        return Ok(OAuthClientAuthSelection {
            mode: OAuthClientAuthMode::Static,
            client_id: None,
        });
    }

    if let Some(client_id) = options.client_id {
        if options.client_secret.is_none()
            && is_client_id_metadata_document_url(client_id)
            && metadata.client_id_metadata_document_supported
        {
            return Ok(OAuthClientAuthSelection {
                mode: OAuthClientAuthMode::Cimd,
                client_id: Some(client_id),
            });
        }
        return Ok(OAuthClientAuthSelection {
            mode: OAuthClientAuthMode::Byo,
            client_id: Some(client_id),
        });
    }

    if metadata.client_id_metadata_document_supported {
        return Ok(OAuthClientAuthSelection {
            mode: OAuthClientAuthMode::Cimd,
            client_id: Some(cimd_client_id(options)),
        });
    }
    if metadata.registration_endpoint.is_some() {
        return Ok(OAuthClientAuthSelection {
            mode: OAuthClientAuthMode::Dcr,
            client_id: None,
        });
    }
    Err("No OAuth client authentication mode is available. Configure auth.mode = \"byo\" with a client_id, auth.mode = \"static\" with a secret_id, or use an authorization server that supports CIMD or dynamic client registration.".to_string())
}

fn cimd_client_id<'a>(options: OAuthClientAuthOptions<'a>) -> &'a str {
    options
        .client_id_metadata_document_url
        .or(options.client_id)
        .unwrap_or(DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL)
}

pub fn is_client_id_metadata_document_url(client_id: &str) -> bool {
    Url::parse(client_id)
        .ok()
        .filter(|url| url.scheme() == "https")
        .and_then(|url| {
            let path = url.path().trim_matches('/');
            (!path.is_empty()).then_some(())
        })
        .is_some()
}

pub fn ensure_pkce_s256_supported(
    metadata: &OAuthAuthorizationServerMetadata,
) -> Result<(), String> {
    let methods = &metadata.code_challenge_methods_supported;
    if methods.is_empty() || methods.iter().any(|method| method == "S256") {
        return Ok(());
    }
    Err("Authorization server does not advertise PKCE S256 support".to_string())
}

pub fn determine_token_endpoint_auth_method(
    metadata: &OAuthAuthorizationServerMetadata,
    client_secret: Option<&str>,
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

pub fn validate_token_endpoint_auth_method(method: &str) -> Result<(), String> {
    match method {
        "none" | "client_secret_post" | "client_secret_basic" => Ok(()),
        other => Err(format!(
            "unsupported token auth method '{other}'; expected none, client_secret_post, or client_secret_basic"
        )),
    }
}

pub fn application_type_for_redirect_uris<'a>(
    redirect_uris: impl IntoIterator<Item = &'a str>,
) -> OAuthApplicationType {
    if redirect_uris.into_iter().all(redirect_uri_is_native) {
        OAuthApplicationType::Native
    } else {
        OAuthApplicationType::Web
    }
}

pub fn dynamic_client_registration_body<'a>(
    client_name: &str,
    redirect_uris: impl IntoIterator<Item = &'a str>,
    scopes: Option<&str>,
) -> JsonValue {
    let redirect_uris = redirect_uris
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let application_type =
        application_type_for_redirect_uris(redirect_uris.iter().map(String::as_str));
    let mut body = json!({
        "client_name": client_name,
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": application_type.as_str(),
    });
    if let Some(scopes) = scopes.filter(|scopes| !scopes.trim().is_empty()) {
        body["scope"] = json!(scopes);
    }
    body
}

pub fn bearer_challenge_value(
    resource_metadata_url: &str,
    scopes: &[String],
    error: Option<BearerChallengeError<'_>>,
) -> String {
    let mut parts = vec![format!(
        "resource_metadata=\"{}\"",
        quote_auth_value(resource_metadata_url)
    )];
    if !scopes.is_empty() {
        parts.push(format!("scope=\"{}\"", quote_auth_value(&scopes.join(" "))));
    }
    if let Some(error) = error {
        parts.insert(0, format!("error=\"{}\"", quote_auth_value(error.code)));
        if let Some(description) = error.description {
            parts.push(format!(
                "error_description=\"{}\"",
                quote_auth_value(description)
            ));
        }
    }
    format!("Bearer {}", parts.join(", "))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BearerChallengeError<'a> {
    pub code: &'a str,
    pub description: Option<&'a str>,
}

fn split_challenge_segments(header: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_quote = false;
    let mut escaped = false;
    for (index, character) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if in_quote => escaped = true,
            '"' => in_quote = !in_quote,
            ',' if !in_quote => {
                segments.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&header[start..]);
    segments
}

fn split_first_token(segment: &str) -> (&str, &str) {
    let trimmed = segment.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(index) => (&trimmed[..index], &trimmed[index..]),
        None => (trimmed, ""),
    }
}

fn parse_auth_param(segment: &str, params: &mut BTreeMap<String, String>) {
    let Some((key, raw_value)) = segment.split_once('=') else {
        return;
    };
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty() {
        return;
    }
    params.insert(key, parse_auth_value(raw_value.trim()));
}

fn parse_auth_value(raw_value: &str) -> String {
    let Some(stripped) = raw_value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return raw_value.trim().to_string();
    };
    let mut value = String::new();
    let mut chars = stripped.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(escaped) = chars.next() {
                value.push(escaped);
            }
        } else {
            value.push(character);
        }
    }
    value
}

async fn fetch_resource_challenge(
    client: &reqwest::Client,
    resource_url: &Url,
) -> Option<WwwAuthenticateChallenge> {
    let response = client
        .get(resource_url.clone())
        .header(ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    let header_values = response
        .headers()
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>();
    bearer_challenge_from_headers(header_values)
}

async fn fetch_first_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    candidates: &[Url],
) -> Result<Option<(Url, T)>, McpOAuthDiscoveryError> {
    for candidate in candidates {
        if let Some(parsed) = fetch_json::<T>(client, candidate).await? {
            return Ok(Some((candidate.clone(), parsed)));
        }
    }
    Ok(None)
}

async fn fetch_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &Url,
) -> Result<Option<T>, McpOAuthDiscoveryError> {
    let response = match client.get(url.clone()).send().await {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if !response.status().is_success() {
        return Ok(None);
    }
    response
        .json::<T>()
        .await
        .map(Some)
        .map_err(|error| McpOAuthDiscoveryError::Json {
            url: url.to_string(),
            error: error.to_string(),
        })
}

fn dedupe_scopes<'a>(scopes: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for scope in scopes {
        let scope = scope.trim();
        if !scope.is_empty() && seen.insert(scope.to_string()) {
            ordered.push(scope.to_string());
        }
    }
    ordered
}

fn redirect_uri_is_native(redirect_uri: &str) -> bool {
    let Ok(url) = Url::parse(redirect_uri) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return true;
    }
    matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
    )
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn quote_auth_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(
        issuer: &str,
        registration_endpoint: Option<&str>,
    ) -> OAuthAuthorizationServerMetadata {
        OAuthAuthorizationServerMetadata {
            issuer: issuer.to_string(),
            authorization_endpoint: format!("{issuer}/authorize"),
            token_endpoint: format!("{issuer}/token"),
            registration_endpoint: registration_endpoint.map(ToString::to_string),
            token_endpoint_auth_methods_supported: vec!["none".to_string()],
            code_challenge_methods_supported: vec!["S256".to_string()],
            scopes_supported: Vec::new(),
            client_id_metadata_document_supported: false,
            authorization_response_iss_parameter_supported: false,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn canonical_resource_indicator_strips_trailing_slash() {
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com/").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com/mcp/").unwrap(),
            "https://mcp.example.com/mcp"
        );
    }

    #[test]
    fn bearer_error_and_insufficient_scope_detection() {
        let challenges =
            parse_www_authenticate(r#"Bearer error="insufficient_scope", scope="repo admin""#);
        let challenge = challenges.first().expect("one Bearer challenge");
        assert_eq!(challenge.bearer_error(), Some("insufficient_scope"));
        assert_eq!(challenge.bearer_scope(), Some("repo admin"));
        assert!(challenge.is_insufficient_scope());

        // A plain 401 Bearer challenge with no error param is not a scope gap.
        let plain = parse_www_authenticate(r#"Bearer scope="repo""#);
        let plain = plain.first().expect("one Bearer challenge");
        assert_eq!(plain.bearer_error(), None);
        assert!(!plain.is_insufficient_scope());

        // A non-Bearer scheme never reports a Bearer error.
        let basic = parse_www_authenticate(r#"Basic realm="x", error="insufficient_scope""#);
        let basic = basic.first().expect("one challenge");
        assert_eq!(basic.bearer_error(), None);
        assert!(!basic.is_insufficient_scope());
    }

    #[test]
    fn canonical_resource_indicator_strips_fragment_and_query() {
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com/mcp?token=secret#section")
                .unwrap(),
            "https://mcp.example.com/mcp"
        );
    }

    #[test]
    fn canonical_resource_indicator_lowercases_scheme_and_host() {
        assert_eq!(
            canonical_resource_indicator("HTTPS://MCP.Example.COM/Path").unwrap(),
            "https://mcp.example.com/Path"
        );
    }

    #[test]
    fn canonical_resource_indicator_drops_default_ports() {
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com:443/").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource_indicator("http://mcp.example.com:80").unwrap(),
            "http://mcp.example.com"
        );
        assert_eq!(
            canonical_resource_indicator("https://mcp.example.com:8443/mcp").unwrap(),
            "https://mcp.example.com:8443/mcp"
        );
    }

    #[test]
    fn canonical_resource_indicator_preserves_non_empty_path_segments() {
        assert_eq!(
            canonical_resource_indicator("https://example.com/mcp/notion/").unwrap(),
            "https://example.com/mcp/notion"
        );
    }

    #[test]
    fn oauth_authorization_url_includes_resource_indicator() {
        let url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
            authorization_endpoint: "https://auth.example.com/authorize",
            client_id: "client-123",
            redirect_uri: "http://127.0.0.1:9783/oauth/callback",
            state: "state-abc",
            code_challenge: "challenge-xyz",
            resource: "https://mcp.example.com/mcp",
            scopes: Some("mcp.read"),
        })
        .unwrap();
        let params = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            params.get("resource").map(|value| value.as_ref()),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            params.get("scope").map(|value| value.as_ref()),
            Some("mcp.read")
        );
    }

    #[test]
    fn token_forms_include_resource_indicator() {
        let code_form = authorization_code_token_form(OAuthAuthorizationCodeTokenForm {
            client_id: "client-123",
            redirect_uri: "http://127.0.0.1:9783/oauth/callback",
            code: "code-abc",
            code_verifier: "verifier-xyz",
            resource: "https://mcp.example.com/mcp",
            scopes: Some("mcp.read"),
        });
        assert!(code_form.contains(&("resource", "https://mcp.example.com/mcp".to_string())));
        assert!(code_form.contains(&("scope", "mcp.read".to_string())));

        let refresh_form = refresh_token_form(OAuthRefreshTokenForm {
            client_id: "client-123",
            refresh_token: "refresh-abc",
            resource: "https://mcp.example.com/mcp",
        });
        assert!(refresh_form.contains(&("resource", "https://mcp.example.com/mcp".to_string())));
    }

    #[test]
    fn canonical_resource_indicator_rejects_invalid_url() {
        assert!(canonical_resource_indicator("not a url").is_err());
    }

    #[test]
    fn parses_bearer_challenge_resource_metadata_and_scope() {
        let challenges = parse_www_authenticate(
            r#"Bearer realm="mcp", resource_metadata="https://mcp.example/.well-known/oauth-protected-resource", scope="files:read files:write""#,
        );
        assert_eq!(challenges.len(), 1);
        let challenge = &challenges[0];
        assert_eq!(
            challenge.bearer_resource_metadata(),
            Some("https://mcp.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            split_scope_value(challenge.bearer_scope()),
            vec!["files:read", "files:write"]
        );
    }

    #[test]
    fn parses_multiple_www_authenticate_challenges() {
        let challenge = bearer_challenge_from_headers([
            r#"Basic realm="old""#,
            r#"Bearer error="insufficient_scope", scope="admin", resource_metadata="https://mcp.example/meta""#,
        ])
        .expect("bearer challenge");
        assert_eq!(
            challenge.params.get("error").map(String::as_str),
            Some("insufficient_scope")
        );
        assert_eq!(
            challenge.bearer_resource_metadata(),
            Some("https://mcp.example/meta")
        );
    }

    #[test]
    fn bearer_challenge_selection_prefers_resource_metadata() {
        let challenge = bearer_challenge_from_headers([
            r#"Bearer realm="old", Bearer resource_metadata="https://mcp.example/meta""#,
        ])
        .expect("bearer challenge");
        assert_eq!(
            challenge.bearer_resource_metadata(),
            Some("https://mcp.example/meta")
        );
    }

    #[test]
    fn authorization_server_candidates_include_oidc_path_appending() {
        let issuer = Url::parse("https://auth.example.com/tenant1").unwrap();
        let candidates = authorization_server_metadata_candidates(&issuer);
        let urls = candidates
            .iter()
            .map(|candidate| candidate.url.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn validates_authorization_server_issuer_without_normalization() {
        let mut metadata = metadata("https://auth.example.com", None);
        validate_authorization_server_issuer("https://auth.example.com", &metadata).unwrap();
        metadata.issuer = "https://auth.example.com/".to_string();
        let err = validate_authorization_server_issuer("https://auth.example.com", &metadata)
            .expect_err("issuer mismatch");
        assert!(err.to_string().contains("issuer mismatch"));
    }

    #[test]
    fn authorization_response_issuer_validation_follows_rfc9207_advertisement() {
        let mut metadata = metadata("https://auth.example.com", None);
        assert!(validate_authorization_response_issuer(&metadata, None).is_ok());
        assert!(
            validate_authorization_response_issuer(&metadata, Some("https://other.example"))
                .is_err()
        );
        metadata.authorization_response_iss_parameter_supported = true;
        assert!(validate_authorization_response_issuer(&metadata, None).is_err());
        assert!(validate_authorization_response_issuer(
            &metadata,
            Some("https://auth.example.com")
        )
        .is_ok());
    }

    #[test]
    fn scope_selection_prefers_challenge_scope_then_metadata_scope() {
        assert_eq!(
            select_oauth_scopes(Some("files:read files:write files:read"), &[]),
            vec!["files:read", "files:write"]
        );
        assert_eq!(
            select_oauth_scopes(None, &["basic".to_string(), "profile".to_string()]),
            vec!["basic", "profile"]
        );
    }

    #[test]
    fn client_registration_mode_selection_is_explicit() {
        let mut meta = metadata(
            "https://auth.example.com",
            Some("https://auth.example.com/reg"),
        );
        assert_eq!(
            select_client_registration_mode(&meta, OAuthClientRegistrationOptions::default()),
            OAuthClientRegistrationMode::DynamicClientRegistration
        );
        assert_eq!(
            select_client_registration_mode(
                &meta,
                OAuthClientRegistrationOptions {
                    client_id: Some("static-client"),
                    ..OAuthClientRegistrationOptions::default()
                },
            ),
            OAuthClientRegistrationMode::PreRegistered
        );
        meta.client_id_metadata_document_supported = true;
        assert_eq!(
            select_client_registration_mode(&meta, OAuthClientRegistrationOptions::default()),
            OAuthClientRegistrationMode::ClientIdMetadataDocument
        );
        assert_eq!(
            select_client_registration_mode(
                &meta,
                OAuthClientRegistrationOptions {
                    client_id: Some("https://client.example/oauth/client.json"),
                    ..OAuthClientRegistrationOptions::default()
                },
            ),
            OAuthClientRegistrationMode::ClientIdMetadataDocument
        );
    }

    #[test]
    fn oauth_client_auth_prefers_cimd_before_dcr() {
        let mut meta = metadata(
            "https://auth.example.com",
            Some("https://auth.example.com/reg"),
        );
        meta.client_id_metadata_document_supported = true;
        let selection = select_oauth_client_auth(&meta, OAuthClientAuthOptions::default()).unwrap();
        assert_eq!(selection.mode, OAuthClientAuthMode::Cimd);
        assert_eq!(
            selection.client_id,
            Some(DEFAULT_MCP_OAUTH_CLIENT_ID_METADATA_DOCUMENT_URL)
        );
    }

    #[test]
    fn oauth_client_auth_falls_back_to_dcr_without_cimd() {
        let meta = metadata(
            "https://auth.example.com",
            Some("https://auth.example.com/reg"),
        );
        let selection = select_oauth_client_auth(&meta, OAuthClientAuthOptions::default()).unwrap();
        assert_eq!(selection.mode, OAuthClientAuthMode::Dcr);
        assert_eq!(selection.client_id, None);
    }

    #[test]
    fn oauth_client_auth_accepts_byo_secret_references_as_byo() {
        let meta = metadata("https://auth.example.com", None);
        let selection = select_oauth_client_auth(
            &meta,
            OAuthClientAuthOptions {
                mode: Some(OAuthClientAuthMode::Byo),
                client_id: Some("registered-client"),
                client_secret: Some("secret-from-store"),
                ..OAuthClientAuthOptions::default()
            },
        )
        .unwrap();
        assert_eq!(selection.mode, OAuthClientAuthMode::Byo);
        assert_eq!(selection.client_id, Some("registered-client"));
    }

    #[test]
    fn explicit_cimd_auth_requires_metadata_document_url() {
        let mut meta = metadata("https://auth.example.com", None);
        meta.client_id_metadata_document_supported = true;
        let error = select_oauth_client_auth(
            &meta,
            OAuthClientAuthOptions {
                mode: Some(OAuthClientAuthMode::Cimd),
                client_id: Some("registered-client"),
                ..OAuthClientAuthOptions::default()
            },
        )
        .expect_err("invalid CIMD client id");
        assert!(error.contains("metadata document URL"));
    }

    #[test]
    fn dynamic_registration_body_marks_loopback_clients_native() {
        let body = dynamic_client_registration_body(
            "Harn CLI",
            ["http://127.0.0.1:49152/oauth/callback"],
            Some("mcp.read"),
        );
        assert_eq!(body["application_type"], "native");
        assert_eq!(body["token_endpoint_auth_method"], "none");
        assert_eq!(body["grant_types"][1], "refresh_token");
        assert_eq!(body["scope"], "mcp.read");
    }

    #[test]
    fn token_refresh_binding_rejects_cross_issuer_reuse() {
        assert!(
            validate_issuer_binding("https://issuer-a.example", "https://issuer-a.example").is_ok()
        );
        assert!(
            validate_issuer_binding("https://issuer-a.example", "https://issuer-b.example")
                .is_err()
        );
    }
}
