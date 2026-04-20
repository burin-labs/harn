use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::Duration;

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::secrets::{SecretId, SecretVersion};
use crate::triggers::{
    redact_headers, HeaderRedactionPolicy, ProviderId, ProviderPayload, SignatureStatus, TraceId,
    TriggerEvent, TriggerEventId,
};

pub mod variants;

pub use variants::WebhookSignatureVariant;

#[cfg(test)]
mod tests;

pub const WEBHOOK_PROVIDER_ID: &str = "webhook";
const DEFAULT_EVENT_KIND: &str = "webhook";

#[derive(Clone, Debug)]
pub(crate) struct WebhookProviderProfile {
    provider_id: ProviderId,
    payload_schema_name: String,
    default_signature_variant: WebhookSignatureVariant,
}

impl WebhookProviderProfile {
    pub(crate) fn webhook() -> Self {
        Self::new(
            ProviderId::from(WEBHOOK_PROVIDER_ID),
            "GenericWebhookPayload",
            WebhookSignatureVariant::Standard,
        )
    }

    pub(crate) fn new(
        provider_id: ProviderId,
        payload_schema_name: impl Into<String>,
        default_signature_variant: WebhookSignatureVariant,
    ) -> Self {
        Self {
            provider_id,
            payload_schema_name: payload_schema_name.into(),
            default_signature_variant,
        }
    }
}

pub struct GenericWebhookConnector {
    profile: WebhookProviderProfile,
    kinds: Vec<TriggerKind>,
    client: Arc<GenericWebhookClient>,
    state: RwLock<ConnectorState>,
}

#[derive(Default)]
struct ConnectorState {
    ctx: Option<ConnectorCtx>,
    bindings: HashMap<String, ActivatedWebhookBinding>,
}

#[derive(Clone, Debug)]
struct ActivatedWebhookBinding {
    #[allow(dead_code)]
    binding_id: String,
    path: Option<String>,
    signing_secret: SecretId,
    signature_variant: WebhookSignatureVariant,
    timestamp_tolerance: Option<Duration>,
    source: Option<String>,
}

#[derive(Default)]
struct GenericWebhookClient;

#[async_trait]
impl ConnectorClient for GenericWebhookClient {
    async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
        Err(ClientError::MethodNotFound(format!(
            "generic webhook connector has no outbound method `{method}`"
        )))
    }
}

impl GenericWebhookConnector {
    pub fn new() -> Self {
        Self::with_profile(WebhookProviderProfile::webhook())
    }

    pub(crate) fn with_profile(profile: WebhookProviderProfile) -> Self {
        Self {
            profile,
            kinds: vec![TriggerKind::from("webhook")],
            client: Arc::new(GenericWebhookClient),
            state: RwLock::new(ConnectorState::default()),
        }
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedWebhookBinding, ConnectorError> {
        let state = self.state.read().expect("webhook connector state poisoned");
        let binding = if let Some(binding_id) =
            raw.metadata.get("binding_id").and_then(JsonValue::as_str)
        {
            state.bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "generic webhook connector has no active binding `{binding_id}`"
                ))
            })?
        } else if state.bindings.len() == 1 {
            state
                .bindings
                .values()
                .next()
                .cloned()
                .expect("checked single binding")
        } else {
            return Err(ConnectorError::Unsupported(
                "generic webhook connector requires raw.metadata.binding_id when multiple bindings are active".to_string(),
            ));
        };
        Ok(binding)
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .read()
            .expect("webhook connector state poisoned")
            .ctx
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "generic webhook connector must be initialized before use".to_string(),
                )
            })
    }
}

impl Default for GenericWebhookConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connector for GenericWebhookConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.profile.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        self.state
            .write()
            .expect("webhook connector state poisoned")
            .ctx = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let mut configured = HashMap::new();
        let mut paths = BTreeSet::new();
        for binding in bindings {
            let activated = ActivatedWebhookBinding::from_binding(
                binding,
                self.profile.default_signature_variant,
            )?;
            if let Some(path) = &activated.path {
                if !paths.insert(path.clone()) {
                    return Err(ConnectorError::Activation(format!(
                        "generic webhook connector path `{path}` is configured by multiple bindings"
                    )));
                }
            }
            configured.insert(binding.binding_id.clone(), activated);
        }

        self.state
            .write()
            .expect("webhook connector state poisoned")
            .bindings = configured;
        Ok(ActivationHandle::new(
            self.provider_id().clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let provider = self.profile.provider_id.clone();
        let received_at = raw.received_at;
        let effective_headers = effective_headers(&raw.headers, binding.source.as_deref());
        let secret = load_secret(&ctx, &binding.signing_secret)?;
        binding.signature_variant.verify(
            ctx.event_log.as_ref(),
            &provider,
            &raw.body,
            &effective_headers,
            secret.as_str(),
            binding.timestamp_tolerance,
            received_at,
        )?;

        let normalized_body = normalize_body(&raw.body, &effective_headers);
        let kind = derive_kind(&raw, &effective_headers, &normalized_body);
        let dedupe_key = derive_dedupe_key(
            binding.signature_variant,
            &effective_headers,
            &normalized_body,
            &raw.body,
        );

        let provider_payload = ProviderPayload::normalize(
            &provider,
            kind.as_str(),
            &effective_headers,
            normalized_body,
        )
        .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;

        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider,
            kind,
            received_at,
            occurred_at: raw
                .occurred_at
                .or_else(|| infer_occurred_at(&provider_payload)),
            dedupe_key,
            trace_id: TraceId::new(),
            tenant_id: raw.tenant_id.clone(),
            headers: redact_headers(&effective_headers, &HeaderRedactionPolicy::default()),
            batch: None,
            provider_payload,
            signature_status: SignatureStatus::Verified,
            dedupe_claimed: false,
        })
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named(self.profile.payload_schema_name.clone())
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

impl ActivatedWebhookBinding {
    fn from_binding(
        binding: &TriggerBinding,
        default_signature_variant: WebhookSignatureVariant,
    ) -> Result<Self, ConnectorError> {
        let config: WebhookBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(|error| {
                ConnectorError::Activation(format!(
                    "generic webhook binding `{}` has invalid config: {error}",
                    binding.binding_id
                ))
            })?;
        let signing_secret =
            parse_secret_id(config.secrets.signing_secret.as_deref()).ok_or_else(|| {
                ConnectorError::Activation(format!(
                    "generic webhook binding `{}` requires secrets.signing_secret",
                    binding.binding_id
                ))
            })?;
        let signature_variant = match config.webhook.signature_scheme.as_deref() {
            Some(raw) => WebhookSignatureVariant::parse(Some(raw))?,
            None => default_signature_variant,
        };
        let timestamp_tolerance = match config.webhook.timestamp_tolerance_secs {
            Some(seconds) if seconds < 0 => {
                return Err(ConnectorError::Activation(format!(
                    "generic webhook binding `{}` has a negative timestamp_tolerance_secs",
                    binding.binding_id
                )))
            }
            Some(seconds) => Some(Duration::seconds(seconds)),
            None => signature_variant.default_timestamp_window(),
        };

        Ok(Self {
            binding_id: binding.binding_id.clone(),
            path: config.match_config.path,
            signing_secret,
            signature_variant,
            timestamp_tolerance,
            source: config.webhook.source,
        })
    }
}

#[derive(Default, Deserialize)]
struct WebhookBindingConfig {
    #[serde(default, rename = "match")]
    match_config: WebhookMatchConfig,
    #[serde(default)]
    secrets: WebhookSecretsConfig,
    #[serde(default)]
    webhook: WebhookConnectorConfig,
}

#[derive(Default, Deserialize)]
struct WebhookMatchConfig {
    path: Option<String>,
}

#[derive(Default, Deserialize)]
struct WebhookSecretsConfig {
    signing_secret: Option<String>,
}

#[derive(Default, Deserialize)]
struct WebhookConnectorConfig {
    signature_scheme: Option<String>,
    timestamp_tolerance_secs: Option<i64>,
    source: Option<String>,
}

fn effective_headers(
    headers: &BTreeMap<String, String>,
    source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut effective = headers.clone();
    if let Some(content_type) = header_value(headers, "content-type") {
        effective
            .entry("Content-Type".to_string())
            .or_insert_with(|| content_type.to_string());
    }
    if let Some(source) = source.or_else(|| header_value(headers, "x-webhook-source")) {
        effective
            .entry("X-Webhook-Source".to_string())
            .or_insert_with(|| source.to_string());
    }
    if let Some(event) = header_value(headers, "x-github-event") {
        effective
            .entry("X-GitHub-Event".to_string())
            .or_insert_with(|| event.to_string());
    }
    if let Some(delivery) = header_value(headers, "x-github-delivery") {
        effective
            .entry("X-GitHub-Delivery".to_string())
            .or_insert_with(|| delivery.to_string());
    }
    effective
}

fn load_secret(ctx: &ConnectorCtx, secret_id: &SecretId) -> Result<String, ConnectorError> {
    let secret = futures::executor::block_on(ctx.secrets.get(secret_id))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                ConnectorError::Secret(format!(
                    "generic webhook signing secret `{secret_id}` is not valid UTF-8: {error}"
                ))
            })
    })
}

fn normalize_body(body: &[u8], headers: &BTreeMap<String, String>) -> JsonValue {
    let content_type = header_value(headers, "content-type").unwrap_or_default();
    if content_type.contains("json") {
        if let Ok(value) = serde_json::from_slice(body) {
            return value;
        }
    }
    serde_json::from_slice(body).unwrap_or_else(|_| {
        json!({
            "raw_base64": BASE64_STANDARD.encode(body),
            "raw_utf8": std::str::from_utf8(body).ok(),
        })
    })
}

fn derive_kind(raw: &RawInbound, headers: &BTreeMap<String, String>, body: &JsonValue) -> String {
    if !raw.kind.trim().is_empty() {
        return raw.kind.clone();
    }
    header_value(headers, "x-github-event")
        .map(ToString::to_string)
        .or_else(|| {
            body.get("type")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            body.get("event")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| DEFAULT_EVENT_KIND.to_string())
}

fn derive_dedupe_key(
    variant: WebhookSignatureVariant,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
    raw_body: &[u8],
) -> String {
    match variant {
        WebhookSignatureVariant::Standard => header_value(headers, "webhook-id")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        WebhookSignatureVariant::Stripe => body
            .get("id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        WebhookSignatureVariant::GitHub => header_value(headers, "x-github-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        WebhookSignatureVariant::Slack => body
            .get("event_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
    }
}

fn infer_occurred_at(provider_payload: &ProviderPayload) -> Option<time::OffsetDateTime> {
    let ProviderPayload::Known(payload) = provider_payload else {
        return None;
    };
    let raw = match payload {
        crate::triggers::event::KnownProviderPayload::Webhook(payload) => &payload.raw,
        _ => return None,
    };
    raw.get("timestamp")
        .and_then(JsonValue::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn parse_secret_id(raw: Option<&str>) -> Option<SecretId> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, SecretVersion::Exact(version))
        }
        None => (trimmed, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(SecretId::new(namespace, name).with_version(version))
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("sha256:{}", hex::encode(digest))
}
