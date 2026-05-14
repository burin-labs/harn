use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

const AUTHORIZATION_SERVERS_ENV: &str = "HARN_MCP_OAUTH_AUTHORIZATION_SERVERS";
const RESOURCE_ENV: &str = "HARN_MCP_OAUTH_RESOURCE";
const SCOPES_ENV: &str = "HARN_MCP_OAUTH_SCOPES";
const INTROSPECTION_URL_ENV: &str = "HARN_MCP_OAUTH_INTROSPECTION_URL";
const INTROSPECTION_CLIENT_ID_ENV: &str = "HARN_MCP_OAUTH_INTROSPECTION_CLIENT_ID";
const INTROSPECTION_CLIENT_SECRET_ENV: &str = "HARN_MCP_OAUTH_INTROSPECTION_CLIENT_SECRET";
const INTROSPECTION_TOKEN_ENV: &str = "HARN_MCP_OAUTH_INTROSPECTION_TOKEN";
const JWKS_URL_ENV: &str = "HARN_MCP_OAUTH_JWKS_URL";
const ISSUER_ENV: &str = "HARN_MCP_OAUTH_ISSUER";
const AUDIENCE_ENV: &str = "HARN_MCP_OAUTH_AUDIENCE";
const JWKS_REFRESH: Duration = Duration::from_secs(5 * 60);
const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct OAuthResourceServer {
    config: OAuthResourceConfig,
    client: reqwest::Client,
    jwks_cache: Arc<Mutex<Option<CachedJwks>>>,
}

#[derive(Clone)]
struct OAuthResourceConfig {
    authorization_servers: Vec<String>,
    resource: Option<String>,
    scopes: Vec<String>,
    introspection: Option<IntrospectionConfig>,
    jwt: Option<JwtConfig>,
    issuer: Option<String>,
    audiences: Vec<String>,
}

#[derive(Clone)]
struct IntrospectionConfig {
    url: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    bearer_token: Option<String>,
}

#[derive(Clone)]
struct JwtConfig {
    jwks_url: String,
}

#[derive(Clone)]
struct CachedJwks {
    fetched_at: Instant,
    set: JwkSet,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OAuthTokenError {
    InvalidToken(String),
    InsufficientScope,
}

#[derive(Default)]
struct TokenClaims {
    issuer: Option<String>,
    audiences: Vec<String>,
    scopes: BTreeSet<String>,
    expires_at_unix: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<StringOrStrings>,
    #[serde(default)]
    resource: Option<StringOrStrings>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scp: Option<StringOrStrings>,
    #[serde(default)]
    exp: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrStrings {
    One(String),
    Many(Vec<String>),
}

impl StringOrStrings {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

impl OAuthResourceServer {
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let authorization_servers = split_list_env(AUTHORIZATION_SERVERS_ENV);
        if authorization_servers.is_empty() {
            return Ok(None);
        }

        let introspection = env_nonempty(INTROSPECTION_URL_ENV).map(|url| IntrospectionConfig {
            url,
            client_id: env_nonempty(INTROSPECTION_CLIENT_ID_ENV),
            client_secret: env_nonempty(INTROSPECTION_CLIENT_SECRET_ENV),
            bearer_token: env_nonempty(INTROSPECTION_TOKEN_ENV),
        });
        let jwt = env_nonempty(JWKS_URL_ENV).map(|jwks_url| JwtConfig { jwks_url });
        if introspection.is_none() && jwt.is_none() {
            return Err(format!(
                "{AUTHORIZATION_SERVERS_ENV} requires {INTROSPECTION_URL_ENV} or {JWKS_URL_ENV}"
            ));
        }

        let resource = env_nonempty(RESOURCE_ENV);
        let audiences = split_list_env(AUDIENCE_ENV);
        if resource.is_none() && audiences.is_empty() {
            return Err(format!(
                "{AUTHORIZATION_SERVERS_ENV} requires {RESOURCE_ENV} or {AUDIENCE_ENV}; refusing to derive OAuth audiences from request headers"
            ));
        }

        Ok(Some(Self {
            config: OAuthResourceConfig {
                authorization_servers,
                resource,
                scopes: split_scope_env(SCOPES_ENV),
                introspection,
                jwt,
                issuer: env_nonempty(ISSUER_ENV),
                audiences,
            },
            client: reqwest::Client::builder()
                .timeout(OAUTH_HTTP_TIMEOUT)
                .build()
                .map_err(|error| format!("failed to build OAuth HTTP client: {error}"))?,
            jwks_cache: Arc::new(Mutex::new(None)),
        }))
    }

    pub(crate) fn metadata(&self, headers: &HeaderMap, mcp_path: &str) -> JsonValue {
        let mut metadata = json!({
            "resource": self.resource_uri(headers, mcp_path),
            "authorization_servers": self.config.authorization_servers,
            "bearer_methods_supported": ["header"],
        });
        if !self.config.scopes.is_empty() {
            metadata["scopes_supported"] = json!(self.config.scopes);
        }
        metadata
    }

    pub(crate) fn resource_metadata_url(&self, headers: &HeaderMap, mcp_path: &str) -> String {
        format!(
            "{}{}",
            request_origin(headers),
            protected_resource_metadata_path(mcp_path)
        )
    }

    pub(crate) fn challenge_header(
        &self,
        headers: &HeaderMap,
        mcp_path: &str,
        error: Option<OAuthChallengeError>,
    ) -> HeaderValue {
        let mut parts = vec![format!(
            "resource_metadata=\"{}\"",
            quote_value(&self.resource_metadata_url(headers, mcp_path))
        )];
        if !self.config.scopes.is_empty() {
            parts.push(format!(
                "scope=\"{}\"",
                quote_value(&self.config.scopes.join(" "))
            ));
        }
        if let Some(error) = error {
            let (code, description) = match error {
                OAuthChallengeError::InvalidToken(description) => ("invalid_token", description),
                OAuthChallengeError::InsufficientScope => (
                    "insufficient_scope",
                    "Token does not include the required MCP scope".to_string(),
                ),
            };
            parts.insert(0, format!("error=\"{}\"", quote_value(code)));
            parts.push(format!(
                "error_description=\"{}\"",
                quote_value(&description)
            ));
        }
        HeaderValue::from_str(&format!("Bearer {}", parts.join(", ")))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer"))
    }

    pub(crate) async fn validate_bearer(
        &self,
        token: &str,
        headers: &HeaderMap,
        mcp_path: &str,
    ) -> Result<(), OAuthTokenError> {
        let claims = if let Some(introspection) = &self.config.introspection {
            self.introspect_token(introspection, token).await?
        } else if self.config.jwt.is_some() {
            self.validate_jwt(token).await?
        } else {
            return Err(OAuthTokenError::InvalidToken(
                "OAuth token validation is not configured".to_string(),
            ));
        };
        self.validate_claims(claims, headers, mcp_path)
    }

    fn resource_uri(&self, headers: &HeaderMap, mcp_path: &str) -> String {
        self.config
            .resource
            .clone()
            .unwrap_or_else(|| format!("{}{}", request_origin(headers), normalize_path(mcp_path)))
    }

    async fn introspect_token(
        &self,
        config: &IntrospectionConfig,
        token: &str,
    ) -> Result<TokenClaims, OAuthTokenError> {
        let mut form = vec![
            ("token", token.to_string()),
            ("token_type_hint", "access_token".to_string()),
        ];
        if config.client_secret.is_none() {
            if let Some(client_id) = &config.client_id {
                form.push(("client_id", client_id.clone()));
            }
        }
        let mut request = self.client.post(&config.url).form(&form);
        if let Some(bearer_token) = &config.bearer_token {
            request = request.bearer_auth(bearer_token);
        } else if let Some(client_id) = &config.client_id {
            if let Some(client_secret) = &config.client_secret {
                request = request.basic_auth(client_id, Some(client_secret));
            }
        }
        let response = request.send().await.map_err(|error| {
            OAuthTokenError::InvalidToken(format!("introspection request failed: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(OAuthTokenError::InvalidToken(format!(
                "introspection endpoint returned {}",
                response.status()
            )));
        }
        let response: IntrospectionResponse = response.json().await.map_err(|error| {
            OAuthTokenError::InvalidToken(format!("invalid introspection response: {error}"))
        })?;
        if !response.active {
            return Err(OAuthTokenError::InvalidToken(
                "access token is inactive".to_string(),
            ));
        }

        let mut audiences = Vec::new();
        if let Some(aud) = response.aud {
            audiences.extend(aud.into_vec());
        }
        if let Some(resource) = response.resource {
            audiences.extend(resource.into_vec());
        }
        let mut scopes = parse_scope_string(response.scope.as_deref());
        if let Some(scp) = response.scp {
            scopes.extend(scp.into_vec());
        }
        Ok(TokenClaims {
            issuer: response.iss,
            audiences,
            scopes: scopes.into_iter().collect(),
            expires_at_unix: response.exp,
        })
    }

    async fn validate_jwt(&self, token: &str) -> Result<TokenClaims, OAuthTokenError> {
        let header = decode_header(token).map_err(|error| {
            OAuthTokenError::InvalidToken(format!("invalid JWT header: {error}"))
        })?;
        let jwks = self.jwks().await?;
        let jwk = header
            .kid
            .as_deref()
            .and_then(|kid| {
                jwks.keys
                    .iter()
                    .find(|jwk| jwk.common.key_id.as_deref() == Some(kid))
            })
            .or_else(|| (jwks.keys.len() == 1).then(|| &jwks.keys[0]))
            .ok_or_else(|| {
                OAuthTokenError::InvalidToken("no matching JWT signing key".to_string())
            })?;
        let key = DecodingKey::from_jwk(jwk).map_err(|error| {
            OAuthTokenError::InvalidToken(format!("invalid JWK signing key: {error}"))
        })?;
        let mut validation = Validation::new(header.alg);
        validation.validate_aud = false;
        if let Some(issuer) = &self.config.issuer {
            validation.set_issuer(&[issuer]);
        }
        let data = decode::<JsonValue>(token, &key, &validation).map_err(|error| {
            OAuthTokenError::InvalidToken(format!("JWT validation failed: {error}"))
        })?;
        Ok(token_claims_from_json(data.claims))
    }

    async fn jwks(&self) -> Result<JwkSet, OAuthTokenError> {
        if let Some(cached) = self
            .jwks_cache
            .lock()
            .expect("JWKS cache poisoned")
            .as_ref()
            .filter(|cached| cached.fetched_at.elapsed() < JWKS_REFRESH)
            .cloned()
        {
            return Ok(cached.set);
        }
        let Some(jwt) = &self.config.jwt else {
            return Err(OAuthTokenError::InvalidToken(
                "JWT validation is not configured".to_string(),
            ));
        };
        let set = self
            .client
            .get(&jwt.jwks_url)
            .send()
            .await
            .map_err(|error| OAuthTokenError::InvalidToken(format!("JWKS fetch failed: {error}")))?
            .error_for_status()
            .map_err(|error| OAuthTokenError::InvalidToken(format!("JWKS fetch failed: {error}")))?
            .json::<JwkSet>()
            .await
            .map_err(|error| OAuthTokenError::InvalidToken(format!("invalid JWKS: {error}")))?;
        *self.jwks_cache.lock().expect("JWKS cache poisoned") = Some(CachedJwks {
            fetched_at: Instant::now(),
            set: set.clone(),
        });
        Ok(set)
    }

    fn validate_claims(
        &self,
        claims: TokenClaims,
        headers: &HeaderMap,
        mcp_path: &str,
    ) -> Result<(), OAuthTokenError> {
        if let Some(exp) = claims.expires_at_unix {
            if exp <= time::OffsetDateTime::now_utc().unix_timestamp() {
                return Err(OAuthTokenError::InvalidToken(
                    "access token is expired".to_string(),
                ));
            }
        }
        if let Some(expected_issuer) = &self.config.issuer {
            if claims.issuer.as_deref() != Some(expected_issuer.as_str()) {
                return Err(OAuthTokenError::InvalidToken(
                    "access token issuer does not match".to_string(),
                ));
            }
        }

        let mut expected_audiences = self.config.audiences.clone();
        if expected_audiences.is_empty() {
            expected_audiences.push(self.resource_uri(headers, mcp_path));
        }
        if !audience_matches(&claims.audiences, &expected_audiences) {
            return Err(OAuthTokenError::InvalidToken(
                "access token audience does not match this MCP resource".to_string(),
            ));
        }
        if !self
            .config
            .scopes
            .iter()
            .all(|scope| claims.scopes.contains(scope))
        {
            return Err(OAuthTokenError::InsufficientScope);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum OAuthChallengeError {
    InvalidToken(String),
    InsufficientScope,
}

pub(crate) fn protected_resource_metadata_path(mcp_path: &str) -> String {
    let mcp_path = normalize_path(mcp_path);
    if mcp_path == "/" {
        "/.well-known/oauth-protected-resource".to_string()
    } else {
        format!("/.well-known/oauth-protected-resource{mcp_path}")
    }
}

fn token_claims_from_json(value: JsonValue) -> TokenClaims {
    let issuer = value
        .get("iss")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let mut audiences = json_strings(value.get("aud"));
    audiences.extend(json_strings(value.get("resource")));
    let mut scopes = parse_scope_string(value.get("scope").and_then(JsonValue::as_str));
    scopes.extend(json_strings(value.get("scp")));
    TokenClaims {
        issuer,
        audiences,
        scopes: scopes.into_iter().collect(),
        expires_at_unix: value.get("exp").and_then(JsonValue::as_i64),
    }
}

fn json_strings(value: Option<&JsonValue>) -> Vec<String> {
    match value {
        Some(JsonValue::String(value)) => vec![value.clone()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .filter_map(JsonValue::as_str)
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn request_origin(headers: &HeaderMap) -> String {
    let scheme = header_str(headers, "x-forwarded-proto")
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| value.eq_ignore_ascii_case("https") || value.eq_ignore_ascii_case("http"))
        .unwrap_or("http")
        .to_ascii_lowercase();
    let host = header_str(headers, "x-forwarded-host")
        .and_then(|value| value.split(',').next())
        .or_else(|| header_str(headers, "host"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("127.0.0.1")
        .to_ascii_lowercase();
    format!("{scheme}://{host}")
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
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

fn audience_matches(actual: &[String], expected: &[String]) -> bool {
    actual.iter().any(|actual| {
        expected
            .iter()
            .any(|expected| actual.eq_ignore_ascii_case(expected))
    })
}

fn split_list_env(name: &str) -> Vec<String> {
    env_nonempty(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn split_scope_env(name: &str) -> Vec<String> {
    env_nonempty(name)
        .map(|value| {
            value
                .split(|character: char| character == ',' || character.is_whitespace())
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_scope_string(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split_whitespace()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn quote_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
