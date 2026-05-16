use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

use super::payloads::ProviderPayload;
use crate::triggers::test_util::clock;

fn serialize_optional_bytes_b64<S>(
    value: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use base64::Engine;

    match value {
        Some(bytes) => {
            serializer.serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_bytes_b64<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    use base64::Engine;

    let encoded = Option::<String>::deserialize(deserializer)?;
    encoded
        .map(|text| {
            base64::engine::general_purpose::STANDARD
                .decode(text.as_bytes())
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TriggerEventId(pub String);

impl TriggerEventId {
    pub fn new() -> Self {
        Self(format!("trigger_evt_{}", Uuid::now_v7()))
    }
}

impl Default for TriggerEventId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ProviderId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(pub String);

impl TraceId {
    pub fn new() -> Self {
        Self(format!("trace_{}", Uuid::now_v7()))
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignatureStatus {
    Verified,
    Unsigned,
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerEvent {
    pub id: TriggerEventId,
    pub provider: ProviderId,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub occurred_at: Option<OffsetDateTime>,
    pub dedupe_key: String,
    pub trace_id: TraceId,
    pub tenant_id: Option<TenantId>,
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Vec<JsonValue>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_bytes_b64",
        deserialize_with = "deserialize_optional_bytes_b64"
    )]
    pub raw_body: Option<Vec<u8>>,
    pub provider_payload: ProviderPayload,
    pub signature_status: SignatureStatus,
    #[serde(skip)]
    pub dedupe_claimed: bool,
}

impl TriggerEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: ProviderId,
        kind: impl Into<String>,
        occurred_at: Option<OffsetDateTime>,
        dedupe_key: impl Into<String>,
        tenant_id: Option<TenantId>,
        headers: BTreeMap<String, String>,
        provider_payload: ProviderPayload,
        signature_status: SignatureStatus,
    ) -> Self {
        Self {
            id: TriggerEventId::new(),
            provider,
            kind: kind.into(),
            received_at: clock::now_utc(),
            occurred_at,
            dedupe_key: dedupe_key.into(),
            trace_id: TraceId::new(),
            tenant_id,
            headers,
            batch: None,
            raw_body: None,
            provider_payload,
            signature_status,
            dedupe_claimed: false,
        }
    }

    pub fn dedupe_claimed(&self) -> bool {
        self.dedupe_claimed
    }

    pub fn mark_dedupe_claimed(&mut self) {
        self.dedupe_claimed = true;
    }
}

/// Header-focused name for the unified [`crate::redact::RedactionPolicy`].
///
/// Trigger ingest paths use this alias at call sites that only redact
/// HTTP headers, while the underlying policy also covers URLs, JSON
/// fields, and free-form strings.
pub type HeaderRedactionPolicy = crate::redact::RedactionPolicy;

pub fn redact_headers(
    headers: &BTreeMap<String, String>,
    policy: &HeaderRedactionPolicy,
) -> BTreeMap<String, String> {
    policy.redact_headers(headers)
}
