//! A2A outbound push delivery and authentication helpers.
use super::*;

pub(super) async fn deliver_push_configs(
    configs: Vec<JsonValue>,
    task: JsonValue,
) -> Vec<Result<(), PushDeliveryError>> {
    let mut results = Vec::with_capacity(configs.len());
    for config in configs {
        results.push(deliver_push_config(&config, &task).await);
    }
    results
}

pub(super) async fn deliver_push_config(
    config: &JsonValue,
    task: &JsonValue,
) -> Result<(), PushDeliveryError> {
    let url = config
        .get("url")
        .and_then(JsonValue::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| PushDeliveryError::Config("push config missing url".to_string()))?;
    if let Some(error) = harn_vm::egress::connector_error_for_url("a2a_push_delivery", url) {
        return Err(PushDeliveryError::Egress(error.to_string()));
    }

    let auth = config.get("authentication");
    let client = push_http_client(auth).await?;
    let payload = json!({ "statusUpdate": task });
    let task_id = task
        .get("id")
        .or_else(|| task.get("taskId"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let mut request = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/a2a+json")
        .json(&payload);
    request = apply_push_auth(request, &client, config, auth, task_id, url).await?;
    request
        .send()
        .await
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "send push notification: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "push notification rejected: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?;
    Ok(())
}

pub(super) async fn push_http_client(
    auth: Option<&JsonValue>,
) -> Result<reqwest::Client, PushDeliveryError> {
    let mut builder = reqwest::Client::builder()
        .user_agent("harn-a2a-push")
        .timeout(StdDuration::from_secs(20))
        .redirect(harn_vm::egress::redirect_policy(
            "a2a_push_delivery_redirect",
            5,
        ));

    if let Some(auth) = auth {
        if let Some(ca_path) = string_field(auth, &["ca_cert", "caCert", "ca_bundle", "caBundle"]) {
            let pem = tokio::fs::read(ca_path).await.map_err(|error| {
                PushDeliveryError::Config(format!("read mTLS CA bundle {ca_path}: {error}"))
            })?;
            match reqwest::Certificate::from_pem_bundle(&pem) {
                Ok(certs) => {
                    for cert in certs {
                        builder = builder.add_root_certificate(cert);
                    }
                }
                Err(_) => {
                    let cert = reqwest::Certificate::from_pem(&pem).map_err(|error| {
                        PushDeliveryError::Config(format!(
                            "parse mTLS CA bundle {ca_path}: {error}"
                        ))
                    })?;
                    builder = builder.add_root_certificate(cert);
                }
            }
        }
        if authentication_schemes(auth).iter().any(|scheme| {
            matches!(
                scheme.as_str(),
                "mtls" | "mutualtls" | "mutual-tls" | "mutual_tls"
            )
        }) {
            builder = builder.identity(push_client_identity(auth).await?);
        }
    }

    builder
        .build()
        .map_err(|error| PushDeliveryError::Config(format!("build push HTTP client: {error}")))
}

pub(super) async fn apply_push_auth(
    mut request: reqwest::RequestBuilder,
    client: &reqwest::Client,
    config: &JsonValue,
    auth: Option<&JsonValue>,
    task_id: &str,
    webhook_url: &str,
) -> Result<reqwest::RequestBuilder, PushDeliveryError> {
    let Some(auth) = auth else {
        if let Some(token) = config.get("token").and_then(JsonValue::as_str) {
            return Ok(request.bearer_auth(token));
        }
        return Ok(request);
    };

    for scheme in authentication_schemes(auth) {
        match scheme.as_str() {
            "bearer" | "bearertoken" | "bearer-token" => {
                let token = string_field(auth, &["credentials", "token", "access_token"])
                    .or_else(|| config.get("token").and_then(JsonValue::as_str))
                    .ok_or_else(|| {
                        PushDeliveryError::Config(
                            "Bearer push authentication requires credentials".to_string(),
                        )
                    })?;
                return Ok(request.bearer_auth(token));
            }
            "basic" => {
                if let Some(credentials) = string_field(auth, &["credentials"]) {
                    return Ok(request.header(
                        reqwest::header::AUTHORIZATION,
                        format!("Basic {credentials}"),
                    ));
                }
                let username = required_string(auth, &["username", "client_id", "clientId"])?;
                let password =
                    required_string(auth, &["password", "client_secret", "clientSecret"])?;
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                return Ok(
                    request.header(reqwest::header::AUTHORIZATION, format!("Basic {encoded}"))
                );
            }
            "apikey" | "api-key" | "api_key" => {
                let header = string_field(auth, &["header", "name"]).unwrap_or("X-API-Key");
                let value = required_string(auth, &["credentials", "value", "token"])?;
                return Ok(request.header(header, value));
            }
            "oauth2" | "oauth" => {
                let token = fetch_oauth2_access_token(client, auth).await?;
                return Ok(request.bearer_auth(token));
            }
            "openidconnect" | "openid-connect" | "oidc" => {
                let token = fetch_oidc_id_token(client, auth, webhook_url).await?;
                return Ok(request.bearer_auth(token));
            }
            "mtls" | "mutualtls" | "mutual-tls" | "mutual_tls" => {}
            other => {
                let credentials = string_field(auth, &["credentials"]).ok_or_else(|| {
                    PushDeliveryError::Config(format!(
                        "push authentication scheme `{other}` requires credentials"
                    ))
                })?;
                request = request.header(
                    reqwest::header::AUTHORIZATION,
                    format!("{other} {credentials}"),
                );
                return Ok(request);
            }
        }
    }

    if let Some(token) = config.get("token").and_then(JsonValue::as_str) {
        return Ok(request.header("X-A2A-Token", token));
    }
    if !task_id.is_empty() {
        return Ok(request.header("X-A2A-Task-ID", task_id));
    }
    Ok(request)
}

pub(super) async fn fetch_oauth2_access_token(
    client: &reqwest::Client,
    auth: &JsonValue,
) -> Result<String, PushDeliveryError> {
    let token_url = required_string(
        auth,
        &["token_url", "tokenUrl", "token_endpoint", "tokenEndpoint"],
    )?;
    if let Some(error) = harn_vm::egress::connector_error_for_url("a2a_push_oauth_token", token_url)
    {
        return Err(PushDeliveryError::Egress(error.to_string()));
    }
    let client_id = required_string(auth, &["client_id", "clientId"])?;
    let client_secret = string_field(auth, &["client_secret", "clientSecret"]);
    let mut form = vec![("grant_type".to_string(), "client_credentials".to_string())];
    if let Some(scope) = scope_string(auth) {
        form.push(("scope".to_string(), scope));
    }
    if let Some(audience) = string_field(auth, &["audience", "aud"]) {
        form.push(("audience".to_string(), audience.to_string()));
    }
    if client_secret.is_none() {
        form.push(("client_id".to_string(), client_id.to_string()));
    }
    let mut request = client.post(token_url).form(&form);
    if let Some(secret) = client_secret {
        request = request.basic_auth(client_id, Some(secret));
    }
    let response = request
        .send()
        .await
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "fetch OAuth2 token: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "OAuth2 token endpoint rejected request: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .json::<JsonValue>()
        .await
        .map_err(|error| {
            PushDeliveryError::Http(format!("decode OAuth2 token response: {error}"))
        })?;
    let token_type = response
        .get("token_type")
        .and_then(JsonValue::as_str)
        .unwrap_or("Bearer");
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(PushDeliveryError::Config(format!(
            "unsupported OAuth2 token_type `{token_type}`"
        )));
    }
    response
        .get("access_token")
        .and_then(JsonValue::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            PushDeliveryError::Http("OAuth2 token response missing access_token".to_string())
        })
}

pub(super) async fn fetch_oidc_id_token(
    client: &reqwest::Client,
    auth: &JsonValue,
    webhook_url: &str,
) -> Result<String, PushDeliveryError> {
    let metadata = oidc_metadata(client, auth).await?;
    let token = fetch_oauth2_token_response(client, auth, &metadata.token_endpoint).await?;
    let id_token = token
        .get("id_token")
        .and_then(JsonValue::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            PushDeliveryError::Http("OIDC token response missing id_token".to_string())
        })?;
    let issuer = string_field(auth, &["issuer", "iss"]).unwrap_or(&metadata.issuer);
    let audience = string_field(auth, &["audience", "aud"])
        .or_else(|| string_field(auth, &["client_id", "clientId"]))
        .unwrap_or(webhook_url);
    // Default to RS256 — the canonical OIDC ID-token signing algorithm
    // — but let push configurations opt into HMAC variants for tests
    // and providers that don't ship asymmetric keys. The verifier
    // refuses to accept a token whose `header.alg` does not match what
    // the caller asked for, which closes the JWT alg-confusion attack
    // surface even though jsonwebtoken 10.x cross-checks key type.
    let mut verify_options = JwtVerificationOptions::default()
        .with_issuer(issuer)
        .with_audience(audience)
        .require_spec_claims(["exp", "iss", "aud"])
        .with_egress_label("a2a_push_oidc_jwks");
    if let Some(raw_algorithm) = string_field(auth, &["algorithm", "alg"]) {
        verify_options = verify_options.with_algorithm(parse_oidc_jwt_algorithm(raw_algorithm)?);
    }
    harn_vm::connectors::verify_jwt_json(
        client,
        id_token,
        JwtKeySource::Url(&metadata.jwks_uri),
        &verify_options,
    )
    .await
    .map_err(|error| PushDeliveryError::Auth(format!("validate OIDC ID token: {error}")))?;
    Ok(id_token.to_string())
}

fn parse_oidc_jwt_algorithm(value: &str) -> Result<jsonwebtoken::Algorithm, PushDeliveryError> {
    use jsonwebtoken::Algorithm;
    match value.trim() {
        "HS256" => Ok(Algorithm::HS256),
        "HS384" => Ok(Algorithm::HS384),
        "HS512" => Ok(Algorithm::HS512),
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        "ES256" => Ok(Algorithm::ES256),
        "ES384" => Ok(Algorithm::ES384),
        "PS256" => Ok(Algorithm::PS256),
        "PS384" => Ok(Algorithm::PS384),
        "PS512" => Ok(Algorithm::PS512),
        "EdDSA" => Ok(Algorithm::EdDSA),
        other => Err(PushDeliveryError::Config(format!(
            "unsupported OIDC JWT algorithm `{other}`"
        ))),
    }
}

pub(super) async fn fetch_oauth2_token_response(
    client: &reqwest::Client,
    auth: &JsonValue,
    token_url: &str,
) -> Result<JsonValue, PushDeliveryError> {
    if let Some(error) = harn_vm::egress::connector_error_for_url("a2a_push_oidc_token", token_url)
    {
        return Err(PushDeliveryError::Egress(error.to_string()));
    }
    let client_id = required_string(auth, &["client_id", "clientId"])?;
    let client_secret = string_field(auth, &["client_secret", "clientSecret"]);
    let mut form = vec![("grant_type".to_string(), "client_credentials".to_string())];
    if let Some(scope) = scope_string(auth) {
        form.push(("scope".to_string(), scope));
    }
    if client_secret.is_none() {
        form.push(("client_id".to_string(), client_id.to_string()));
    }
    let mut request = client.post(token_url).form(&form);
    if let Some(secret) = client_secret {
        request = request.basic_auth(client_id, Some(secret));
    }
    request
        .send()
        .await
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "fetch OIDC token: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "OIDC token endpoint rejected request: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .json::<JsonValue>()
        .await
        .map_err(|error| PushDeliveryError::Http(format!("decode OIDC token response: {error}")))
}

#[derive(Clone, Debug)]
pub(super) struct OidcMetadata {
    issuer: String,
    token_endpoint: String,
    jwks_uri: String,
}

pub(super) async fn oidc_metadata(
    client: &reqwest::Client,
    auth: &JsonValue,
) -> Result<OidcMetadata, PushDeliveryError> {
    if let (Some(token_endpoint), Some(jwks_uri)) = (
        string_field(
            auth,
            &["token_url", "tokenUrl", "token_endpoint", "tokenEndpoint"],
        ),
        string_field(auth, &["jwks_url", "jwksUrl", "jwks_uri", "jwksUri"]),
    ) {
        let issuer = required_string(auth, &["issuer", "iss"])?;
        return Ok(OidcMetadata {
            issuer: issuer.to_string(),
            token_endpoint: token_endpoint.to_string(),
            jwks_uri: jwks_uri.to_string(),
        });
    }
    let discovery_url = string_field(
        auth,
        &[
            "discovery_url",
            "discoveryUrl",
            "openid_configuration_url",
            "openidConfigurationUrl",
        ],
    )
    .map(str::to_string)
    .or_else(|| {
        string_field(auth, &["issuer", "iss"]).map(|issuer| {
            format!(
                "{}/.well-known/openid-configuration",
                issuer.trim_end_matches('/')
            )
        })
    })
    .ok_or_else(|| {
        PushDeliveryError::Config(
            "OIDC push authentication requires discovery_url or token/jwks URLs".to_string(),
        )
    })?;
    if let Some(error) =
        harn_vm::egress::connector_error_for_url("a2a_push_oidc_discovery", &discovery_url)
    {
        return Err(PushDeliveryError::Egress(error.to_string()));
    }
    let metadata = client
        .get(&discovery_url)
        .send()
        .await
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "fetch OIDC metadata: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            PushDeliveryError::Http(format!(
                "OIDC metadata endpoint rejected request: {}",
                harn_vm::egress::redact_reqwest_error(&error)
            ))
        })?
        .json::<JsonValue>()
        .await
        .map_err(|error| PushDeliveryError::Http(format!("decode OIDC metadata: {error}")))?;
    Ok(OidcMetadata {
        issuer: metadata
            .get("issuer")
            .and_then(JsonValue::as_str)
            .filter(|issuer| !issuer.trim().is_empty())
            .ok_or_else(|| PushDeliveryError::Http("OIDC metadata missing issuer".to_string()))?
            .to_string(),
        token_endpoint: metadata
            .get("token_endpoint")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                PushDeliveryError::Http("OIDC metadata missing token_endpoint".to_string())
            })?
            .to_string(),
        jwks_uri: metadata
            .get("jwks_uri")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| PushDeliveryError::Http("OIDC metadata missing jwks_uri".to_string()))?
            .to_string(),
    })
}

pub(super) async fn push_client_identity(
    auth: &JsonValue,
) -> Result<reqwest::Identity, PushDeliveryError> {
    if let Some(identity_path) = string_field(auth, &["client_identity", "clientIdentity"]) {
        let bytes = tokio::fs::read(identity_path).await.map_err(|error| {
            PushDeliveryError::Config(format!("read mTLS identity {identity_path}: {error}"))
        })?;
        return reqwest::Identity::from_pem(&bytes).map_err(|error| {
            PushDeliveryError::Config(format!("parse mTLS identity {identity_path}: {error}"))
        });
    }
    let cert_path = required_string(auth, &["client_cert", "clientCert", "cert"])?;
    let key_path = required_string(auth, &["client_key", "clientKey", "key"])?;
    let mut identity_pem = tokio::fs::read(cert_path).await.map_err(|error| {
        PushDeliveryError::Config(format!("read mTLS client cert {cert_path}: {error}"))
    })?;
    let key = tokio::fs::read(key_path).await.map_err(|error| {
        PushDeliveryError::Config(format!("read mTLS client key {key_path}: {error}"))
    })?;
    identity_pem.extend_from_slice(b"\n");
    identity_pem.extend_from_slice(&key);
    reqwest::Identity::from_pem(&identity_pem)
        .map_err(|error| PushDeliveryError::Config(format!("parse mTLS identity: {error}")))
}

pub(super) fn authentication_schemes(auth: &JsonValue) -> Vec<String> {
    let mut schemes = Vec::new();
    if let Some(values) = auth.get("schemes").and_then(JsonValue::as_array) {
        schemes.extend(values.iter().filter_map(JsonValue::as_str));
    }
    if let Some(scheme) = auth.get("scheme").and_then(JsonValue::as_str) {
        schemes.push(scheme);
    }
    if let Some(kind) = auth.get("type").and_then(JsonValue::as_str) {
        schemes.push(kind);
    }
    schemes
        .into_iter()
        .map(|scheme| scheme.replace([' ', '_'], "").to_ascii_lowercase())
        .collect()
}

pub(super) fn string_field<'a>(value: &'a JsonValue, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(JsonValue::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn required_string<'a>(
    value: &'a JsonValue,
    names: &[&str],
) -> Result<&'a str, PushDeliveryError> {
    string_field(value, names).ok_or_else(|| {
        PushDeliveryError::Config(format!(
            "missing required push authentication field `{}`",
            names[0]
        ))
    })
}

pub(super) fn scope_string(auth: &JsonValue) -> Option<String> {
    if let Some(scope) = string_field(auth, &["scope"]) {
        return Some(scope.to_string());
    }
    auth.get("scopes")
        .and_then(JsonValue::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|scope| !scope.is_empty())
}

#[derive(Debug)]
pub(super) enum PushDeliveryError {
    Auth(String),
    Config(String),
    Egress(String),
    Http(String),
}

impl std::fmt::Display for PushDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(message)
            | Self::Config(message)
            | Self::Egress(message)
            | Self::Http(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PushDeliveryError {}
