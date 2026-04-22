use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::triggers::event::KnownProviderPayload;
use crate::triggers::{
    redact_headers, A2aPushPayload, HeaderRedactionPolicy, ProviderId, ProviderPayload,
    SignatureStatus, TraceId, TriggerEvent, TriggerEventId,
};

const PROVIDER_ID: &str = "a2a-push";
const JWKS_REFRESH: StdDuration = StdDuration::from_secs(24 * 60 * 60);

pub struct A2aPushConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    client: Arc<A2aPushClient>,
    state: RwLock<A2aPushState>,
    http: reqwest::Client,
}

#[derive(Default)]
struct A2aPushState {
    ctx: Option<ConnectorCtx>,
    bindings: HashMap<String, ActivatedA2aPushBinding>,
}

#[derive(Clone, Debug)]
struct ActivatedA2aPushBinding {
    binding_id: String,
    expected_iss: Option<String>,
    expected_aud: Option<String>,
    expected_token: Option<String>,
    jwks_url: Option<String>,
    inline_jwks: Option<JwkSet>,
    auth_scheme: A2aPushAuthScheme,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum A2aPushAuthScheme {
    #[default]
    Jwt,
    BearerToken,
    Unsigned,
}

#[derive(Default)]
struct A2aPushClient;

#[derive(Clone, Debug)]
struct CachedJwks {
    fetched_at: Instant,
    jwks: JwkSet,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct A2aPushJwtClaims {
    iss: String,
    aud: AudienceClaim,
    iat: i64,
    exp: i64,
    jti: String,
    #[serde(default, rename = "taskId")]
    task_id_camel: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum AudienceClaim {
    One(String),
    Many(Vec<String>),
}

#[async_trait]
impl ConnectorClient for A2aPushClient {
    async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
        Err(ClientError::MethodNotFound(format!(
            "a2a-push connector has no outbound method `{method}`"
        )))
    }
}

impl A2aPushConnector {
    pub fn new() -> Self {
        Self {
            provider_id: ProviderId::from(PROVIDER_ID),
            kinds: vec![TriggerKind::from(PROVIDER_ID)],
            client: Arc::new(A2aPushClient),
            state: RwLock::new(A2aPushState::default()),
            http: reqwest::Client::new(),
        }
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .read()
            .expect("a2a-push connector state poisoned")
            .ctx
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "a2a-push connector must be initialized before use".to_string(),
                )
            })
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedA2aPushBinding, ConnectorError> {
        let state = self
            .state
            .read()
            .expect("a2a-push connector state poisoned");
        if let Some(binding_id) = raw.metadata.get("binding_id").and_then(JsonValue::as_str) {
            return state.bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "a2a-push connector has no active binding `{binding_id}`"
                ))
            });
        }
        if state.bindings.len() == 1 {
            return Ok(state
                .bindings
                .values()
                .next()
                .cloned()
                .expect("checked single binding"));
        }
        Err(ConnectorError::Unsupported(
            "a2a-push connector requires raw.metadata.binding_id when multiple bindings are active"
                .to_string(),
        ))
    }

    async fn verify(
        &self,
        ctx: &ConnectorCtx,
        binding: &ActivatedA2aPushBinding,
        raw: &RawInbound,
        body: &JsonValue,
    ) -> Result<(SignatureStatus, Option<A2aPushJwtClaims>, String), ConnectorError> {
        match binding.auth_scheme {
            A2aPushAuthScheme::Unsigned => Ok((
                SignatureStatus::Unsigned,
                None,
                fallback_dedupe_key(&raw.body),
            )),
            A2aPushAuthScheme::BearerToken => {
                let token = bearer_token(&raw.headers)?;
                let expected = binding.expected_token.as_deref().ok_or_else(|| {
                    ConnectorError::Activation(format!(
                        "a2a-push binding `{}` requires token for bearer auth",
                        binding.binding_id
                    ))
                })?;
                if token != expected {
                    return Err(ConnectorError::invalid_signature(
                        "a2a-push bearer token did not match expected token",
                    ));
                }
                Ok((
                    SignatureStatus::Verified,
                    None,
                    fallback_dedupe_key(&raw.body),
                ))
            }
            A2aPushAuthScheme::Jwt => {
                let jwt = bearer_token(&raw.headers)?;
                let claims = self.verify_jwt(binding, jwt).await?;
                if let Some(expected_token) = binding.expected_token.as_deref() {
                    let observed = claims
                        .token
                        .as_deref()
                        .or_else(|| header_value(&raw.headers, "x-a2a-token"))
                        .or_else(|| body.get("token").and_then(JsonValue::as_str));
                    if observed != Some(expected_token) {
                        return Err(ConnectorError::invalid_signature(
                            "a2a-push JWT token claim/header did not match expected token",
                        ));
                    }
                }
                let now = OffsetDateTime::now_utc().unix_timestamp();
                if claims.exp <= now {
                    return Err(ConnectorError::invalid_signature(
                        "a2a-push JWT exp is not in the future",
                    ));
                }
                if claims.iat > now + 60 {
                    return Err(ConnectorError::invalid_signature(
                        "a2a-push JWT iat is in the future",
                    ));
                }
                let ttl = StdDuration::from_secs((claims.exp - now).max(1) as u64);
                if !ctx
                    .inbox
                    .insert_if_new(&binding.binding_id, &claims.jti, ttl)
                    .await?
                {
                    return Err(ConnectorError::DuplicateDelivery(format!(
                        "a2a-push JWT jti `{}` has already been accepted",
                        claims.jti
                    )));
                }
                Ok((SignatureStatus::Verified, Some(claims.clone()), claims.jti))
            }
        }
    }

    async fn verify_jwt(
        &self,
        binding: &ActivatedA2aPushBinding,
        token: &str,
    ) -> Result<A2aPushJwtClaims, ConnectorError> {
        let header = decode_header(token)
            .map_err(|error| ConnectorError::invalid_signature(error.to_string()))?;
        let jwks = self.jwks(binding).await?;
        let jwk = match header.kid.as_deref() {
            Some(kid) => jwks.find(kid).ok_or_else(|| {
                ConnectorError::invalid_signature(format!(
                    "a2a-push JWT kid `{kid}` was not found in JWKS"
                ))
            })?,
            None if jwks.keys.len() == 1 => &jwks.keys[0],
            None => {
                return Err(ConnectorError::invalid_signature(
                    "a2a-push JWT missing kid and JWKS contains multiple keys",
                ))
            }
        };
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|error| ConnectorError::invalid_signature(error.to_string()))?;
        let mut validation = Validation::new(header.alg);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        validation.set_issuer(&[required(binding.expected_iss.as_deref(), "expected_iss")?]);
        validation.set_audience(&[required(binding.expected_aud.as_deref(), "expected_aud")?]);
        let token = decode::<A2aPushJwtClaims>(token, &key, &validation)
            .map_err(|error| ConnectorError::invalid_signature(error.to_string()))?;
        if token.claims.jti.trim().is_empty() {
            return Err(ConnectorError::invalid_signature(
                "a2a-push JWT missing jti",
            ));
        }
        Ok(token.claims)
    }

    async fn jwks(&self, binding: &ActivatedA2aPushBinding) -> Result<JwkSet, ConnectorError> {
        if let Some(jwks) = &binding.inline_jwks {
            return Ok(jwks.clone());
        }
        let Some(jwks_url) = binding.jwks_url.as_deref() else {
            return Err(ConnectorError::Activation(format!(
                "a2a-push binding `{}` requires jwks_url for JWT auth",
                binding.binding_id
            )));
        };
        if let Some(cached) = cached_jwks(jwks_url) {
            return Ok(cached);
        }
        let jwks = self
            .http
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
}

impl Default for A2aPushConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connector for A2aPushConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        self.state
            .write()
            .expect("a2a-push connector state poisoned")
            .ctx = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let mut configured = HashMap::new();
        for binding in bindings {
            configured.insert(
                binding.binding_id.clone(),
                ActivatedA2aPushBinding::from_binding(binding)?,
            );
        }
        self.state
            .write()
            .expect("a2a-push connector state poisoned")
            .bindings = configured;
        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let body = raw.json_body()?;
        let (signature_status, claims, dedupe_key) =
            self.verify(&ctx, &binding, &raw, &body).await?;
        let normalized = normalize_a2a_push(&body, claims.as_ref());
        let kind = normalized.kind.clone();
        let mut event = TriggerEvent {
            id: TriggerEventId::new(),
            provider: self.provider_id.clone(),
            kind,
            received_at: raw.received_at,
            occurred_at: raw.occurred_at.or_else(|| infer_occurred_at(&body)),
            dedupe_key,
            trace_id: TraceId::new(),
            tenant_id: raw.tenant_id.clone(),
            headers: redact_headers(&raw.headers, &HeaderRedactionPolicy::default()),
            batch: None,
            raw_body: Some(raw.body.clone()),
            provider_payload: ProviderPayload::Known(KnownProviderPayload::A2aPush(normalized)),
            signature_status,
            dedupe_claimed: false,
        };
        if claims.is_some() {
            event.mark_dedupe_claimed();
        }
        Ok(event)
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named("A2aPushPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

impl ActivatedA2aPushBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: A2aPushBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "a2a-push binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let push = config.a2a_push;
        let auth_scheme = if push.auth_scheme.is_none() && push.is_empty() {
            A2aPushAuthScheme::Unsigned
        } else {
            A2aPushAuthScheme::parse(push.auth_scheme.as_deref())?
        };
        let expected_token = push.expected_token.or(push.token);
        if auth_scheme == A2aPushAuthScheme::Jwt {
            if push.expected_iss.as_deref().is_none_or(str::is_empty) {
                return Err(ConnectorError::Activation(format!(
                    "a2a-push binding `{}` requires a2a_push.expected_iss for JWT auth",
                    binding.binding_id
                )));
            }
            if push.expected_aud.as_deref().is_none_or(str::is_empty) {
                return Err(ConnectorError::Activation(format!(
                    "a2a-push binding `{}` requires a2a_push.expected_aud for JWT auth",
                    binding.binding_id
                )));
            }
            if push.jwks_url.as_deref().is_none_or(str::is_empty) && push.inline_jwks.is_none() {
                return Err(ConnectorError::Activation(format!(
                    "a2a-push binding `{}` requires a2a_push.jwks_url for JWT auth",
                    binding.binding_id
                )));
            }
        }
        if auth_scheme == A2aPushAuthScheme::BearerToken && expected_token.is_none() {
            return Err(ConnectorError::Activation(format!(
                "a2a-push binding `{}` requires a2a_push.expected_token for bearer-token auth",
                binding.binding_id
            )));
        }
        Ok(Self {
            binding_id: binding.binding_id.clone(),
            expected_iss: push.expected_iss,
            expected_aud: push.expected_aud.or(push.audience),
            expected_token,
            jwks_url: push.jwks_url,
            inline_jwks: push.inline_jwks,
            auth_scheme,
        })
    }
}

#[derive(Default, Deserialize)]
struct A2aPushBindingConfig {
    #[serde(default)]
    a2a_push: A2aPushConfig,
}

#[derive(Default, Deserialize)]
struct A2aPushConfig {
    expected_iss: Option<String>,
    expected_aud: Option<String>,
    audience: Option<String>,
    jwks_url: Option<String>,
    inline_jwks: Option<JwkSet>,
    auth_scheme: Option<String>,
    expected_token: Option<String>,
    token: Option<String>,
}

impl A2aPushConfig {
    fn is_empty(&self) -> bool {
        self.expected_iss.is_none()
            && self.expected_aud.is_none()
            && self.audience.is_none()
            && self.jwks_url.is_none()
            && self.inline_jwks.is_none()
            && self.expected_token.is_none()
            && self.token.is_none()
    }
}

impl A2aPushAuthScheme {
    fn parse(raw: Option<&str>) -> Result<Self, ConnectorError> {
        match raw.unwrap_or("jwt").trim().to_ascii_lowercase().as_str() {
            "jwt" | "bearer-jwt" => Ok(Self::Jwt),
            "bearer" | "bearer-token" | "api-key" => Ok(Self::BearerToken),
            "none" | "unsigned" => Ok(Self::Unsigned),
            other => Err(ConnectorError::Activation(format!(
                "unsupported a2a-push auth_scheme `{other}`"
            ))),
        }
    }
}

fn normalize_a2a_push(body: &JsonValue, claims: Option<&A2aPushJwtClaims>) -> A2aPushPayload {
    let status_update = body.get("statusUpdate");
    let artifact_update = body.get("artifactUpdate");
    let task = body.get("task");
    let message = body.get("message");
    let task_id = status_update
        .and_then(|value| value.get("taskId"))
        .or_else(|| artifact_update.and_then(|value| value.get("taskId")))
        .or_else(|| task.and_then(|value| value.get("id")))
        .or_else(|| body.get("taskId"))
        .or_else(|| body.get("task_id"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .or_else(|| claims.and_then(|claims| claims.task_id()));
    let task_state = status_update
        .and_then(|value| value.pointer("/status/state"))
        .or_else(|| task.and_then(|value| value.pointer("/status/state")))
        .or_else(|| body.pointer("/status/state"))
        .and_then(JsonValue::as_str)
        .map(normalize_task_state);
    let artifact = artifact_update
        .and_then(|value| value.get("artifact"))
        .or_else(|| task.and_then(|value| value.get("artifacts")))
        .cloned();
    let sender = body
        .get("sender")
        .or_else(|| body.get("fromAgentUrl"))
        .or_else(|| body.get("from_agent_url"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .or_else(|| claims.map(|claims| claims.iss.clone()));
    let kind = if let Some(state) = task_state.as_deref() {
        format!("a2a.task.{state}")
    } else if artifact_update.is_some() {
        "a2a.task.artifact".to_string()
    } else if message.is_some() {
        "a2a.task.message".to_string()
    } else {
        "a2a.task.update".to_string()
    };

    A2aPushPayload {
        task_id,
        task_state,
        artifact,
        sender,
        raw: body.clone(),
        kind,
    }
}

impl A2aPushJwtClaims {
    fn task_id(&self) -> Option<String> {
        self.task_id.clone().or_else(|| self.task_id_camel.clone())
    }
}

fn normalize_task_state(state: &str) -> String {
    match state {
        "cancelled" => "canceled".to_string(),
        other => other.to_string(),
    }
}

fn required<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, ConnectorError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ConnectorError::Activation(format!("a2a-push JWT auth requires {name}")))
}

fn bearer_token(headers: &BTreeMap<String, String>) -> Result<&str, ConnectorError> {
    let authorization = header_value(headers, "authorization")
        .ok_or_else(|| ConnectorError::MissingHeader("authorization".to_string()))?;
    let Some((scheme, value)) = authorization.split_once(' ') else {
        return Err(ConnectorError::InvalidHeader {
            name: "authorization".to_string(),
            detail: "expected `<scheme> <credentials>`".to_string(),
        });
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(ConnectorError::InvalidHeader {
            name: "authorization".to_string(),
            detail: "expected Bearer scheme".to_string(),
        });
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(ConnectorError::InvalidHeader {
            name: "authorization".to_string(),
            detail: "bearer token is empty".to_string(),
        });
    }
    Ok(value)
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn infer_occurred_at(body: &JsonValue) -> Option<OffsetDateTime> {
    body.get("timestamp")
        .or_else(|| body.pointer("/statusUpdate/metadata/timestamp"))
        .or_else(|| body.pointer("/artifactUpdate/metadata/timestamp"))
        .and_then(JsonValue::as_str)
        .and_then(|value| {
            OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
}

fn fallback_dedupe_key(raw_body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw_body);
    format!("sha256:{}", hex::encode(digest))
}

static JWKS_CACHE: std::sync::OnceLock<RwLock<HashMap<String, CachedJwks>>> =
    std::sync::OnceLock::new();

fn cached_jwks(url: &str) -> Option<JwkSet> {
    let cache = JWKS_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let cache = cache.read().expect("a2a-push JWKS cache poisoned");
    let cached = cache.get(url)?;
    (cached.fetched_at.elapsed() < JWKS_REFRESH).then(|| cached.jwks.clone())
}

fn store_cached_jwks(url: &str, jwks: JwkSet) {
    let cache = JWKS_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    cache.write().expect("a2a-push JWKS cache poisoned").insert(
        url.to_string(),
        CachedJwks {
            fetched_at: Instant::now(),
            jwks,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{Connector, ConnectorCtx};
    use crate::event_log::{AnyEventLog, MemoryEventLog};
    use crate::secrets::{
        RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
    };
    use crate::triggers::InboxIndex;
    use crate::{MetricsRegistry, RateLimiterFactory};
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde_json::json;
    use std::sync::Arc;

    struct EmptySecrets;

    #[async_trait]
    impl SecretProvider for EmptySecrets {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn put(&self, id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            "test"
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    async fn connector_with_binding(
        binding: TriggerBinding,
    ) -> (A2aPushConnector, Arc<InboxIndex>) {
        let event_log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)));
        let metrics = Arc::new(MetricsRegistry::default());
        let inbox = Arc::new(
            InboxIndex::new(event_log.clone(), metrics.clone())
                .await
                .unwrap(),
        );
        let ctx = ConnectorCtx {
            event_log,
            secrets: Arc::new(EmptySecrets),
            inbox: inbox.clone(),
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::default()),
        };
        let mut connector = A2aPushConnector::new();
        connector.init(ctx).await.unwrap();
        connector.activate(&[binding]).await.unwrap();
        (connector, inbox)
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

    fn jwt(jti: &str, token: &str) -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".to_string());
        encode(
            &header,
            &A2aPushJwtClaims {
                iss: "reviewer.prod".to_string(),
                aud: AudienceClaim::One("https://orchestrator.test/a2a/review".to_string()),
                iat: OffsetDateTime::now_utc().unix_timestamp(),
                exp: OffsetDateTime::now_utc().unix_timestamp() + 300,
                jti: jti.to_string(),
                task_id_camel: Some("task-123".to_string()),
                task_id: None,
                token: Some(token.to_string()),
            },
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap()
    }

    fn jwt_binding() -> TriggerBinding {
        TriggerBinding {
            provider: ProviderId::from(PROVIDER_ID),
            kind: TriggerKind::from(PROVIDER_ID),
            binding_id: "reviewer-task-update".to_string(),
            dedupe_key: None,
            dedupe_retention_days: 1,
            config: json!({
                "a2a_push": {
                    "expected_iss": "reviewer.prod",
                    "expected_aud": "https://orchestrator.test/a2a/review",
                    "expected_token": "opaque-token",
                    "inline_jwks": hs_jwks(),
                }
            }),
        }
    }

    #[tokio::test]
    async fn normalizes_completed_status_update() {
        let (connector, _inbox) = connector_with_binding(jwt_binding()).await;
        let body = serde_json::to_vec(&json!({
            "statusUpdate": {
                "taskId": "task-123",
                "contextId": "ctx-1",
                "status": {"state": "completed"}
            }
        }))
        .unwrap();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            format!("Bearer {}", jwt("jti-1", "opaque-token")),
        );
        let mut raw = RawInbound::new("", headers, body);
        raw.metadata = json!({"binding_id": "reviewer-task-update"});

        let event = connector.normalize_inbound(raw).await.unwrap();
        assert_eq!(event.kind, "a2a.task.completed");
        assert_eq!(event.dedupe_key, "jti-1");
        assert!(event.dedupe_claimed());
        let ProviderPayload::Known(KnownProviderPayload::A2aPush(payload)) = event.provider_payload
        else {
            panic!("expected a2a payload");
        };
        assert_eq!(payload.task_id.as_deref(), Some("task-123"));
        assert_eq!(payload.task_state.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn rejects_replayed_jti() {
        let (connector, _inbox) = connector_with_binding(jwt_binding()).await;
        let body = serde_json::to_vec(&json!({
            "statusUpdate": {
                "taskId": "task-123",
                "contextId": "ctx-1",
                "status": {"state": "completed"}
            }
        }))
        .unwrap();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            format!("Bearer {}", jwt("jti-replay", "opaque-token")),
        );
        let mut first = RawInbound::new("", headers.clone(), body.clone());
        first.metadata = json!({"binding_id": "reviewer-task-update"});
        connector.normalize_inbound(first).await.unwrap();
        let mut second = RawInbound::new("", headers, body);
        second.metadata = json!({"binding_id": "reviewer-task-update"});

        let error = connector.normalize_inbound(second).await.unwrap_err();
        assert!(matches!(error, ConnectorError::DuplicateDelivery(_)));
    }
}
