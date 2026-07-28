use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronEventPayload {
    pub cron_id: Option<String>,
    pub schedule: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub tick_at: OffsetDateTime,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenericWebhookPayload {
    pub source: Option<String>,
    pub content_type: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aPushPayload {
    pub task_id: Option<String>,
    pub task_state: Option<String>,
    pub artifact: Option<JsonValue>,
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_chain: Option<JsonValue>,
    pub raw: JsonValue,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEventPayload {
    pub event: String,
    pub source: Option<String>,
    pub stream: Option<String>,
    pub partition: Option<String>,
    pub offset: Option<String>,
    pub key: Option<String>,
    pub timestamp: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub raw: JsonValue,
}

/// Payload emitted by `emit_channel(...)` to channel-source triggers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEventPayload {
    pub id: String,
    pub name: String,
    pub name_resolved: String,
    pub scope: String,
    pub scope_id: String,
    pub payload: JsonValue,
    pub emitted_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
}

/// Package-owned payload emitted by a Harn connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionProviderPayload {
    pub provider: String,
    pub schema_name: String,
    pub raw: JsonValue,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderPayload {
    Extension(ExtensionProviderPayload),
    Known(KnownProviderPayload),
}

impl ProviderPayload {
    pub fn provider(&self) -> &str {
        match self {
            Self::Known(known) => known.provider(),
            Self::Extension(payload) => payload.provider.as_str(),
        }
    }
}

// Keep the public payload hierarchy PartialEq-only; deriving Eq here would
// implicitly widen TriggerEvent's public trait contract.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum KnownProviderPayload {
    #[serde(rename = "cron")]
    Cron(CronEventPayload),
    #[serde(rename = "webhook")]
    Webhook(GenericWebhookPayload),
    #[serde(rename = "a2a-push")]
    A2aPush(A2aPushPayload),
    #[serde(rename = "kafka")]
    Kafka(StreamEventPayload),
    #[serde(rename = "nats")]
    Nats(StreamEventPayload),
    #[serde(rename = "pulsar")]
    Pulsar(StreamEventPayload),
    #[serde(rename = "postgres-cdc")]
    PostgresCdc(StreamEventPayload),
    #[serde(rename = "email")]
    Email(StreamEventPayload),
    #[serde(rename = "websocket")]
    Websocket(StreamEventPayload),
    #[serde(rename = "channel")]
    Channel(ChannelEventPayload),
}

impl KnownProviderPayload {
    pub fn provider(&self) -> &str {
        match self {
            Self::Cron(_) => "cron",
            Self::Webhook(_) => "webhook",
            Self::A2aPush(_) => "a2a-push",
            Self::Kafka(_) => "kafka",
            Self::Nats(_) => "nats",
            Self::Pulsar(_) => "pulsar",
            Self::PostgresCdc(_) => "postgres-cdc",
            Self::Email(_) => "email",
            Self::Websocket(_) => "websocket",
            Self::Channel(_) => "channel",
        }
    }
}
