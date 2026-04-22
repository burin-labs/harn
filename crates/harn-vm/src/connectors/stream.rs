use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::triggers::{
    redact_headers, HeaderRedactionPolicy, ProviderId, ProviderPayload, SignatureStatus, TraceId,
    TriggerEvent, TriggerEventId,
};

pub struct StreamConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    schema_name: String,
    client: Arc<StreamClient>,
    state: RwLock<ConnectorState>,
}

#[derive(Default)]
struct ConnectorState {
    ctx: Option<ConnectorCtx>,
    bindings: HashMap<String, ActivatedStreamBinding>,
}

#[derive(Clone, Debug)]
struct ActivatedStreamBinding {
    match_events: Vec<String>,
    stream: JsonValue,
}

#[derive(Default)]
struct StreamClient;

#[async_trait]
impl ConnectorClient for StreamClient {
    async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
        Err(ClientError::MethodNotFound(format!(
            "stream connector has no outbound method `{method}`"
        )))
    }
}

impl StreamConnector {
    pub fn new(provider_id: ProviderId, schema_name: impl Into<String>) -> Self {
        Self {
            provider_id,
            kinds: vec![TriggerKind::from("stream")],
            schema_name: schema_name.into(),
            client: Arc::new(StreamClient),
            state: RwLock::new(ConnectorState::default()),
        }
    }

    fn binding_for_raw(&self, raw: &RawInbound) -> Result<ActivatedStreamBinding, ConnectorError> {
        let state = self.state.read().expect("stream connector state poisoned");
        let binding = if let Some(binding_id) =
            raw.metadata.get("binding_id").and_then(JsonValue::as_str)
        {
            state.bindings.get(binding_id).cloned().ok_or_else(|| {
                ConnectorError::Unsupported(format!(
                    "stream connector has no active binding `{binding_id}`"
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
                "stream connector requires raw.metadata.binding_id when multiple bindings are active"
                    .to_string(),
            ));
        };
        Ok(binding)
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.state
            .read()
            .expect("stream connector state poisoned")
            .ctx
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "stream connector must be initialized before use".to_string(),
                )
            })
    }
}

#[async_trait]
impl Connector for StreamConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        self.state
            .write()
            .expect("stream connector state poisoned")
            .ctx = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let mut configured = HashMap::new();
        for binding in bindings {
            let activated = ActivatedStreamBinding::from_binding(binding)?;
            configured.insert(binding.binding_id.clone(), activated);
        }

        self.state
            .write()
            .expect("stream connector state poisoned")
            .bindings = configured;
        Ok(ActivationHandle::new(
            self.provider_id().clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(&self, raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        let _ctx = self.ctx()?;
        let binding = self.binding_for_raw(&raw)?;
        let body = normalized_body(&raw)?;
        let kind = stream_event_kind(&binding, &body);
        let dedupe_key = stream_dedupe_key(&binding, &raw, &body);
        let provider_payload =
            ProviderPayload::normalize(&self.provider_id, &kind, &raw.headers, body)
                .map_err(|error| ConnectorError::Unsupported(error.to_string()))?;
        let occurred_at = raw
            .occurred_at
            .or_else(|| infer_occurred_at(&provider_payload));

        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider: self.provider_id.clone(),
            kind,
            received_at: raw.received_at,
            occurred_at,
            dedupe_key,
            trace_id: TraceId::new(),
            tenant_id: raw.tenant_id,
            headers: redact_headers(&raw.headers, &HeaderRedactionPolicy::default()),
            batch: None,
            raw_body: Some(raw.body),
            provider_payload,
            signature_status: SignatureStatus::Unsigned,
            dedupe_claimed: false,
        })
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named(self.schema_name.clone())
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

impl ActivatedStreamBinding {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config = binding.config.as_object().ok_or_else(|| {
            ConnectorError::Activation(format!(
                "stream binding '{}' config must be an object",
                binding.binding_id
            ))
        })?;
        let match_events = config
            .get("match")
            .and_then(|value| value.get("events"))
            .and_then(JsonValue::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let stream = config.get("stream").cloned().unwrap_or(JsonValue::Null);

        Ok(Self {
            match_events,
            stream,
        })
    }
}

fn normalized_body(raw: &RawInbound) -> Result<JsonValue, ConnectorError> {
    let content_type = header_value(&raw.headers, "content-type").unwrap_or_default();
    if content_type.contains("json") {
        return raw.json_body();
    }
    if let Ok(value) = serde_json::from_slice(&raw.body) {
        return Ok(value);
    }
    use base64::Engine;
    Ok(json!({
        "raw_base64": base64::engine::general_purpose::STANDARD.encode(&raw.body),
        "raw_utf8": std::str::from_utf8(&raw.body).ok(),
    }))
}

fn stream_event_kind(binding: &ActivatedStreamBinding, body: &JsonValue) -> String {
    body.get("kind")
        .and_then(JsonValue::as_str)
        .or_else(|| body.get("event").and_then(JsonValue::as_str))
        .or_else(|| body.get("type").and_then(JsonValue::as_str))
        .map(ToString::to_string)
        .or_else(|| binding.match_events.first().cloned())
        .unwrap_or_else(|| "stream.message".to_string())
}

fn stream_dedupe_key(
    binding: &ActivatedStreamBinding,
    raw: &RawInbound,
    body: &JsonValue,
) -> String {
    header_value(&raw.headers, "x-harn-stream-id")
        .map(ToString::to_string)
        .or_else(|| stringish(body, &["dedupe_key", "event_id", "id", "key", "message_id"]))
        .or_else(|| {
            let stream_name = stringish(body, &["stream", "topic", "subject", "channel", "slot"])
                .or_else(|| {
                    stringish(
                        &binding.stream,
                        &["stream", "topic", "subject", "channel", "slot"],
                    )
                });
            let offset = stringish(body, &["offset", "sequence", "lsn"]);
            match (stream_name, offset) {
                (Some(stream), Some(offset)) => Some(format!("{stream}:{offset}")),
                _ => None,
            }
        })
        .unwrap_or_else(|| fallback_body_digest(&raw.body))
}

fn infer_occurred_at(payload: &ProviderPayload) -> Option<OffsetDateTime> {
    let ProviderPayload::Known(known) = payload else {
        return None;
    };
    let payload = match known {
        crate::triggers::event::KnownProviderPayload::Kafka(payload)
        | crate::triggers::event::KnownProviderPayload::Nats(payload)
        | crate::triggers::event::KnownProviderPayload::Pulsar(payload)
        | crate::triggers::event::KnownProviderPayload::PostgresCdc(payload)
        | crate::triggers::event::KnownProviderPayload::Email(payload)
        | crate::triggers::event::KnownProviderPayload::Websocket(payload) => payload,
        _ => return None,
    };
    payload.timestamp.as_deref().and_then(|timestamp| {
        OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339).ok()
    })
}

fn stringish(raw: &JsonValue, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        let value = raw.get(*field)?;
        value
            .as_str()
            .map(ToString::to_string)
            .or_else(|| value.as_i64().map(|number| number.to_string()))
            .or_else(|| value.as_u64().map(|number| number.to_string()))
    })
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256:{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{RateLimiterFactory, TriggerBinding};
    use crate::event_log::{install_memory_for_current_thread, reset_active_event_log};
    use crate::secrets::{
        RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
    };
    use crate::triggers::InboxIndex;

    struct EmptySecretProvider;

    #[async_trait::async_trait]
    impl SecretProvider for EmptySecretProvider {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Ok(())
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            Ok(RotationHandle {
                provider: self.namespace().to_string(),
                id: id.clone(),
                from_version: None,
                to_version: None,
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            "empty"
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn stream_connector_normalizes_json_inbound() {
        install_memory_for_current_thread(128);
        let event_log = crate::event_log::active_event_log().expect("event log");
        let inbox = Arc::new(
            InboxIndex::new(
                event_log.clone(),
                Arc::new(crate::connectors::MetricsRegistry::default()),
            )
            .await
            .expect("inbox"),
        );
        let mut connector = StreamConnector::new(ProviderId::from("kafka"), "StreamEventPayload");
        connector
            .init(ConnectorCtx {
                event_log,
                secrets: Arc::new(EmptySecretProvider),
                inbox,
                metrics: Arc::new(crate::connectors::MetricsRegistry::default()),
                rate_limiter: Arc::new(RateLimiterFactory::default()),
            })
            .await
            .expect("init");
        connector
            .activate(&[TriggerBinding {
                provider: ProviderId::from("kafka"),
                kind: TriggerKind::from("stream"),
                binding_id: "quotes".to_string(),
                dedupe_key: None,
                dedupe_retention_days: 7,
                config: json!({
                    "match": {"events": ["quote.tick"]},
                    "stream": {"topic": "quotes"}
                }),
            }])
            .await
            .expect("activate");

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let mut raw = RawInbound::new(
            "",
            headers,
            serde_json::to_vec(&json!({
                "key": "acct-1",
                "offset": 42,
                "value": {"amount": 10}
            }))
            .unwrap(),
        );
        raw.metadata = json!({"binding_id": "quotes"});

        let event = connector.normalize_inbound(raw).await.expect("event");
        assert_eq!(event.provider.as_str(), "kafka");
        assert_eq!(event.kind, "quote.tick");
        assert_eq!(event.dedupe_key, "acct-1");
        let ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Kafka(payload)) =
            event.provider_payload
        else {
            panic!("expected kafka stream payload");
        };
        assert_eq!(payload.stream.as_deref(), None);
        assert_eq!(payload.key.as_deref(), Some("acct-1"));
        reset_active_event_log();
    }
}
