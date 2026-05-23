//! Shared connector building blocks that are useful to both Rust shims and
//! Harn-authored connector packages.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration as StdDuration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use super::{hmac, Connector, ConnectorError};

const DEFAULT_JWKS_CACHE_TTL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

/// Base connector contract name for shared runtime code.
///
/// This stays as a blanket extension over `Connector` so there is one
/// object-safe implementation contract for registry and adapter code.
pub trait ConnectorBase: Connector {}

impl<T: Connector + ?Sized> ConnectorBase for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HmacSignatureAlgorithm {
    Sha1,
    Sha256,
}

impl HmacSignatureAlgorithm {
    pub fn parse(raw: &str) -> Result<Self, ConnectorError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sha1" | "hmac-sha1" => Ok(Self::Sha1),
            "sha256" | "hmac-sha256" | "" => Ok(Self::Sha256),
            other => Err(ConnectorError::Unsupported(format!(
                "unsupported HMAC signature algorithm `{other}`"
            ))),
        }
    }
}

/// Verify a raw HMAC signature value using constant-time comparison.
///
/// `signature` may be a bare hex digest or a provider-style `sha256=<hex>` /
/// `sha1=<hex>` value. Provider-specific timestamp and canonical-message
/// checks belong in `hmac::verify_hmac_signed`.
pub fn verify_hmac_signature(
    body: &[u8],
    signature: &str,
    secret: &str,
    algorithm: HmacSignatureAlgorithm,
) -> Result<bool, ConnectorError> {
    let signature = signature.trim();
    let signature = signature
        .strip_prefix("sha256=")
        .or_else(|| signature.strip_prefix("sha1="))
        .unwrap_or(signature);
    let provided = hex::decode(signature).map_err(|error| ConnectorError::InvalidHeader {
        name: "signature".to_string(),
        detail: error.to_string(),
    })?;
    let expected = match algorithm {
        HmacSignatureAlgorithm::Sha1 => hmac::hmac_sha1(secret.as_bytes(), body),
        HmacSignatureAlgorithm::Sha256 => hmac::hmac_sha256(secret.as_bytes(), body),
    };
    Ok(hmac::secure_eq(&expected, &provided))
}

#[derive(Clone, Debug)]
pub enum JwtKeySource<'a> {
    Inline(&'a JwkSet),
    Url(&'a str),
}

#[derive(Clone, Debug)]
pub struct JwtVerificationOptions {
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub required_spec_claims: Vec<String>,
    pub jwks_cache_ttl: StdDuration,
    pub egress_label: &'static str,
    /// Signing algorithm the caller expects this token to be signed
    /// with. The verifier asserts `header.alg == expected_algorithm`
    /// BEFORE constructing the `Validation` so an attacker-controlled
    /// `alg` header cannot down/up-grade the verification algorithm
    /// (the canonical "alg confusion" footgun). Defaults to RS256
    /// (`Algorithm::RS256`) — change it explicitly via
    /// [`JwtVerificationOptions::with_algorithm`] when a connector
    /// uses HS256 / ES256 / etc.
    pub expected_algorithm: Algorithm,
}

impl Default for JwtVerificationOptions {
    fn default() -> Self {
        Self {
            issuer: None,
            audience: None,
            required_spec_claims: Vec::new(),
            jwks_cache_ttl: DEFAULT_JWKS_CACHE_TTL,
            egress_label: "connector:jwks",
            expected_algorithm: Algorithm::RS256,
        }
    }
}

impl JwtVerificationOptions {
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    pub fn require_spec_claims(
        mut self,
        claims: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.required_spec_claims = claims.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_egress_label(mut self, egress_label: &'static str) -> Self {
        self.egress_label = egress_label;
        self
    }

    pub fn with_jwks_cache_ttl(mut self, ttl: StdDuration) -> Self {
        self.jwks_cache_ttl = ttl;
        self
    }

    /// Override the expected signing algorithm. Use this when the
    /// connector you're verifying is known to sign with something
    /// other than RS256 (e.g. HS256 for Slack-style shared secrets,
    /// ES256 for some Apple flows).
    pub fn with_algorithm(mut self, algorithm: Algorithm) -> Self {
        self.expected_algorithm = algorithm;
        self
    }
}

#[derive(Clone, Debug)]
struct CachedJwks {
    fetched_at: Instant,
    jwks: JwkSet,
}

static JWKS_CACHE: OnceLock<RwLock<HashMap<String, CachedJwks>>> = OnceLock::new();

pub async fn resolve_jwks(
    http: &reqwest::Client,
    source: JwtKeySource<'_>,
    options: &JwtVerificationOptions,
) -> Result<JwkSet, ConnectorError> {
    match source {
        JwtKeySource::Inline(jwks) => Ok(jwks.clone()),
        JwtKeySource::Url(jwks_url) => fetch_cached_jwks(http, jwks_url, options).await,
    }
}

pub async fn verify_jwt_claims<T>(
    http: &reqwest::Client,
    token: &str,
    source: JwtKeySource<'_>,
    options: &JwtVerificationOptions,
) -> Result<T, ConnectorError>
where
    T: DeserializeOwned,
{
    let header = decode_header(token)
        .map_err(|error| ConnectorError::invalid_signature(error.to_string()))?;
    // Defense-in-depth against JWT "alg confusion": refuse to accept a
    // token whose self-declared algorithm does not match what the
    // caller said to expect. jsonwebtoken 10.x cross-checks JWK key
    // type against `header.alg`, which closes the immediate bypass,
    // but the canonical defense is to never trust `header.alg` for
    // algorithm selection in the first place.
    if header.alg != options.expected_algorithm {
        return Err(ConnectorError::invalid_signature(format!(
            "JWT header alg {:?} does not match expected {:?}",
            header.alg, options.expected_algorithm
        )));
    }
    let jwks = resolve_jwks(http, source.clone(), options).await?;
    let jwks = match (source, header.kid.as_deref()) {
        (JwtKeySource::Url(jwks_url), Some(kid)) if jwks.find(kid).is_none() => {
            fetch_uncached_jwks(http, jwks_url, options).await?
        }
        _ => jwks,
    };
    let jwk = jwk_for_header(&jwks, header.kid.as_deref())?;
    let key = DecodingKey::from_jwk(jwk)
        .map_err(|error| ConnectorError::invalid_signature(error.to_string()))?;
    let mut validation = Validation::new(options.expected_algorithm);
    if !options.required_spec_claims.is_empty() {
        let claims = options
            .required_spec_claims
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        validation.set_required_spec_claims(&claims);
    }
    if let Some(issuer) = options.issuer.as_deref() {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = options.audience.as_deref() {
        validation.set_audience(&[audience]);
    }
    decode::<T>(token, &key, &validation)
        .map(|token| token.claims)
        .map_err(|error| ConnectorError::invalid_signature(error.to_string()))
}

fn jwk_for_header<'a>(
    jwks: &'a JwkSet,
    kid: Option<&str>,
) -> Result<&'a jsonwebtoken::jwk::Jwk, ConnectorError> {
    match kid {
        Some(kid) => jwks.find(kid).ok_or_else(|| {
            ConnectorError::invalid_signature(format!("JWT kid `{kid}` was not found in JWKS"))
        }),
        None if jwks.keys.len() == 1 => Ok(&jwks.keys[0]),
        None => Err(ConnectorError::invalid_signature(
            "JWT missing kid and JWKS contains multiple keys",
        )),
    }
}

pub async fn verify_jwt_json(
    http: &reqwest::Client,
    token: &str,
    source: JwtKeySource<'_>,
    options: &JwtVerificationOptions,
) -> Result<JsonValue, ConnectorError> {
    verify_jwt_claims(http, token, source, options).await
}

async fn fetch_cached_jwks(
    http: &reqwest::Client,
    jwks_url: &str,
    options: &JwtVerificationOptions,
) -> Result<JwkSet, ConnectorError> {
    if let Some(cached) = cached_jwks(jwks_url, options.jwks_cache_ttl) {
        return Ok(cached);
    }
    fetch_uncached_jwks(http, jwks_url, options).await
}

async fn fetch_uncached_jwks(
    http: &reqwest::Client,
    jwks_url: &str,
    options: &JwtVerificationOptions,
) -> Result<JwkSet, ConnectorError> {
    if let Some(error) = crate::egress::connector_error_for_url(options.egress_label, jwks_url) {
        return Err(error);
    }
    let jwks = http
        .get(jwks_url)
        .send()
        .await
        .map_err(|error| ConnectorError::Activation(format!("fetch JWKS: {error}")))?
        .error_for_status()
        .map_err(|error| ConnectorError::Activation(format!("fetch JWKS: {error}")))?
        .json::<JwkSet>()
        .await
        .map_err(|error| ConnectorError::Activation(format!("decode JWKS: {error}")))?;
    store_cached_jwks(jwks_url, jwks.clone());
    Ok(jwks)
}

fn cached_jwks(url: &str, ttl: StdDuration) -> Option<JwkSet> {
    let cache = JWKS_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let cache = cache.read().expect("connector JWKS cache poisoned");
    let cached = cache.get(url)?;
    (cached.fetched_at.elapsed() < ttl).then(|| cached.jwks.clone())
}

fn store_cached_jwks(url: &str, jwks: JwkSet) {
    let cache = JWKS_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    cache
        .write()
        .expect("connector JWKS cache poisoned")
        .insert(
            url.to_string(),
            CachedJwks {
                fetched_at: Instant::now(),
                jwks,
            },
        );
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorPage {
    pub items: Vec<JsonValue>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Collect cursor-paginated results without baking in a provider response
/// schema. The caller owns page construction; this helper owns loop safety.
pub async fn paginate_cursor<F, Fut>(
    initial_cursor: Option<String>,
    max_pages: Option<usize>,
    mut fetch: F,
) -> Result<Vec<JsonValue>, ConnectorError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<CursorPage, ConnectorError>>,
{
    let mut cursor = initial_cursor;
    let mut pages = 0usize;
    let mut results = Vec::new();
    loop {
        if max_pages.is_some_and(|limit| pages >= limit) {
            break;
        }
        let page = fetch(cursor.clone()).await?;
        results.extend(page.items);
        pages += 1;
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
        if cursor.as_deref().is_none_or(str::is_empty) {
            return Err(ConnectorError::Json(
                "cursor-paginated connector response set has_more without next_cursor".to_string(),
            ));
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, Deserialize, Serialize)]
    struct Claims {
        iss: String,
        aud: String,
        exp: i64,
        jti: String,
    }

    fn hs_jwks() -> JwkSet {
        serde_json::from_value(json!({
            "keys": [{
                "kty": "oct",
                "kid": "test-key",
                "alg": "HS256",
                "k": "c2VjcmV0"
            }]
        }))
        .unwrap()
    }

    fn hs_token() -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".to_string());
        encode(
            &header,
            &Claims {
                iss: "issuer".to_string(),
                aud: "audience".to_string(),
                exp: 4_102_444_800,
                jti: "jwt-1".to_string(),
            },
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap()
    }

    #[test]
    fn hmac_signature_accepts_provider_prefixed_hex() {
        let body = b"Hello, World!";
        let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify_hmac_signature(
            body,
            signature,
            "It's a Secret to Everybody",
            HmacSignatureAlgorithm::Sha256,
        )
        .unwrap());
    }

    #[tokio::test]
    async fn jwt_claims_verify_against_inline_jwks() {
        let http = reqwest::Client::new();
        let claims: Claims = verify_jwt_claims(
            &http,
            &hs_token(),
            JwtKeySource::Inline(&hs_jwks()),
            &JwtVerificationOptions::default()
                .with_algorithm(Algorithm::HS256)
                .with_issuer("issuer")
                .with_audience("audience")
                .require_spec_claims(["exp", "iss", "aud"]),
        )
        .await
        .unwrap();
        assert_eq!(claims.jti, "jwt-1");
    }

    #[tokio::test]
    async fn jwt_claims_reject_alg_confusion() {
        // Token is signed with HS256; verifier is told to expect
        // RS256. Even though jsonwebtoken would catch this downstream
        // because the JWK is symmetric, our up-front guard refuses
        // before constructing `Validation`, which is the canonical
        // defense against alg-confusion exploits.
        let http = reqwest::Client::new();
        let result = verify_jwt_claims::<Claims>(
            &http,
            &hs_token(),
            JwtKeySource::Inline(&hs_jwks()),
            &JwtVerificationOptions::default()
                .with_algorithm(Algorithm::RS256)
                .with_issuer("issuer")
                .with_audience("audience"),
        )
        .await;
        let error = result.expect_err("HS256 token should not verify under RS256");
        let message = error.to_string();
        assert!(
            message.contains("alg") && message.contains("expected"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn paginate_cursor_collects_until_has_more_is_false() {
        let pages = [
            CursorPage {
                items: vec![json!({"id": 1})],
                next_cursor: Some("b".to_string()),
                has_more: true,
            },
            CursorPage {
                items: vec![json!({"id": 2})],
                next_cursor: None,
                has_more: false,
            },
        ];
        let mut index = 0usize;
        let results = paginate_cursor(None, None, |_cursor| {
            let page = pages[index].clone();
            index += 1;
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(results, vec![json!({"id": 1}), json!({"id": 2})]);
    }
}
