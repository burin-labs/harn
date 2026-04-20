use std::collections::{BTreeMap, BTreeSet};

use harn_vm::connectors::ConnectorError;
use harn_vm::event_log::MemoryEventLog;
use harn_vm::ProviderId;
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub subject: String,
    pub scheme: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClaims {
    pub subject: String,
    pub issuer: String,
    pub audience: Option<String>,
    pub scopes: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
    pub headers: BTreeMap<String, String>,
    pub validated_oauth: Option<OAuthClaims>,
}

impl AuthRequest {
    pub fn bearer_token(&self) -> Option<&str> {
        self.headers
            .get("authorization")
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim())
            .filter(|value| !value.is_empty())
    }

    pub fn api_key(&self) -> Option<&str> {
        self.headers
            .get("x-api-key")
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| self.bearer_token())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyAuthConfig {
    pub keys: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacAuthConfig {
    pub shared_secret: String,
    pub provider: String,
    pub timestamp_window: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuth21AuthConfig {
    pub issuer: String,
    pub audience: Option<String>,
    pub required_scopes: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethodConfig {
    ApiKey(ApiKeyAuthConfig),
    Hmac(HmacAuthConfig),
    OAuth21(OAuth21AuthConfig),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthPolicy {
    pub methods: Vec<AuthMethodConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Authorized(AuthenticatedPrincipal),
    Rejected(String),
}

impl AuthPolicy {
    pub fn allow_all() -> Self {
        Self::default()
    }

    pub async fn authorize(&self, request: &AuthRequest) -> AuthorizationDecision {
        if self.methods.is_empty() {
            return AuthorizationDecision::Authorized(AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
            });
        }

        let mut failures = Vec::new();
        for method in &self.methods {
            match authorize_method(method, request).await {
                Ok(principal) => return AuthorizationDecision::Authorized(principal),
                Err(message) => failures.push(message),
            }
        }

        AuthorizationDecision::Rejected(failures.join("; "))
    }
}

async fn authorize_method(
    method: &AuthMethodConfig,
    request: &AuthRequest,
) -> Result<AuthenticatedPrincipal, String> {
    match method {
        AuthMethodConfig::ApiKey(config) => {
            let Some(api_key) = request.api_key() else {
                return Err("missing API key".to_string());
            };
            if config.keys.contains(api_key) {
                Ok(AuthenticatedPrincipal {
                    subject: "api-key".to_string(),
                    scheme: "api_key".to_string(),
                })
            } else {
                Err("invalid API key".to_string())
            }
        }
        AuthMethodConfig::Hmac(config) => {
            let log = MemoryEventLog::new(8);
            harn_vm::connectors::hmac::verify_hmac_authorization(
                &log,
                &ProviderId::new(config.provider.clone()),
                &request.method,
                &request.path,
                &request.body,
                &request.headers,
                &config.shared_secret,
                config.timestamp_window,
                OffsetDateTime::now_utc(),
            )
            .await
            .map_err(connector_error_message)?;
            Ok(AuthenticatedPrincipal {
                subject: "hmac".to_string(),
                scheme: "hmac".to_string(),
            })
        }
        AuthMethodConfig::OAuth21(config) => {
            let Some(claims) = &request.validated_oauth else {
                return Err("oauth token was not validated by the transport".to_string());
            };
            if claims.issuer != config.issuer {
                return Err(format!(
                    "oauth issuer mismatch: expected '{}', got '{}'",
                    config.issuer, claims.issuer
                ));
            }
            if config
                .audience
                .as_ref()
                .zip(claims.audience.as_ref())
                .is_some_and(|(expected, actual)| expected != actual)
            {
                return Err("oauth audience mismatch".to_string());
            }
            if !config.required_scopes.is_subset(&claims.scopes) {
                return Err("oauth scope requirement not satisfied".to_string());
            }
            Ok(AuthenticatedPrincipal {
                subject: claims.subject.clone(),
                scheme: "oauth21".to_string(),
            })
        }
    }
}

fn connector_error_message(error: ConnectorError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    #[tokio::test]
    async fn api_key_policy_accepts_matching_bearer_token() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: BTreeSet::from(["secret".to_string()]),
            })],
        };
        let request = AuthRequest {
            headers: BTreeMap::from([("authorization".to_string(), "Bearer secret".to_string())]),
            ..AuthRequest::default()
        };
        let decision = policy.authorize(&request).await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[tokio::test]
    async fn hmac_policy_accepts_valid_canonical_request_signature() {
        let timestamp = OffsetDateTime::now_utc().unix_timestamp().to_string();
        let body = br#"{"ok":true}"#;
        let hash = Sha256::digest(body);
        let body_hash = hex::encode(hash);
        let signed = format!("POST\n/mcp\n{timestamp}\n{body_hash}");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret").expect("mac key");
        mac.update(signed.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let authorization = format!("HMAC-SHA256 timestamp={timestamp},signature={signature}");

        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::Hmac(HmacAuthConfig {
                shared_secret: "shared-secret".to_string(),
                provider: "harn-serve".to_string(),
                timestamp_window: Duration::seconds(60),
            })],
        };
        let request = AuthRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            body: body.to_vec(),
            headers: BTreeMap::from([("authorization".to_string(), authorization)]),
            validated_oauth: None,
        };

        let decision = policy.authorize(&request).await;
        assert!(matches!(decision, AuthorizationDecision::Authorized(_)));
    }

    #[tokio::test]
    async fn oauth_policy_requires_transport_validated_claims() {
        let policy = AuthPolicy {
            methods: vec![AuthMethodConfig::OAuth21(OAuth21AuthConfig {
                issuer: "https://issuer.example".to_string(),
                audience: Some("harn-serve".to_string()),
                required_scopes: BTreeSet::from(["invoke".to_string()]),
            })],
        };
        let request = AuthRequest {
            validated_oauth: Some(OAuthClaims {
                subject: "alice".to_string(),
                issuer: "https://issuer.example".to_string(),
                audience: Some("harn-serve".to_string()),
                scopes: BTreeSet::from(["invoke".to_string(), "read".to_string()]),
            }),
            ..AuthRequest::default()
        };

        let decision = policy.authorize(&request).await;
        assert_eq!(
            decision,
            AuthorizationDecision::Authorized(AuthenticatedPrincipal {
                subject: "alice".to_string(),
                scheme: "oauth21".to_string(),
            })
        );
    }
}
