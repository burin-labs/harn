use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use time::OffsetDateTime;

use super::payloads::*;
use super::util::{json_stringish, parse_rfc3339};

pub(super) fn cron_payload(
    _kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let cron_id = raw
        .get("cron_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let schedule = raw
        .get("schedule")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let tick_at = raw
        .get("tick_at")
        .and_then(JsonValue::as_str)
        .and_then(parse_rfc3339)
        .unwrap_or_else(OffsetDateTime::now_utc);
    ProviderPayload::Known(KnownProviderPayload::Cron(CronEventPayload {
        cron_id,
        schedule,
        tick_at,
        raw,
    }))
}

pub(super) fn webhook_payload(
    _kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
        source: headers.get("X-Webhook-Source").cloned(),
        content_type: headers.get("Content-Type").cloned(),
        raw,
    }))
}

pub(super) fn a2a_push_payload(
    _kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let actor_chain =
        crate::a2a::actor_chain_from_metadata(&raw).map(|chain| chain.to_json_value());
    let task_id = raw
        .get("task_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let sender = raw
        .get("sender")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let task_state = raw
        .pointer("/status/state")
        .or_else(|| raw.pointer("/statusUpdate/status/state"))
        .and_then(JsonValue::as_str)
        .map(|state| match state {
            "cancelled" => "canceled".to_string(),
            other => other.to_string(),
        });
    let artifact = raw
        .pointer("/artifactUpdate/artifact")
        .or_else(|| raw.get("artifact"))
        .cloned();
    let kind = task_state
        .as_deref()
        .map(|state| format!("a2a.task.{state}"))
        .unwrap_or_else(|| "a2a.task.update".to_string());
    ProviderPayload::Known(KnownProviderPayload::A2aPush(A2aPushPayload {
        task_id,
        task_state,
        artifact,
        sender,
        actor_chain,
        raw,
        kind,
    }))
}

pub(super) fn kafka_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Kafka(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn nats_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Nats(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn pulsar_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Pulsar(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn postgres_cdc_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::PostgresCdc(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn email_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Email(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn websocket_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Websocket(stream_payload(
        kind, headers, raw,
    )))
}

fn stream_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> StreamEventPayload {
    StreamEventPayload {
        event: kind.to_string(),
        source: json_stringish(&raw, &["source", "connector", "origin"]),
        stream: json_stringish(
            &raw,
            &["stream", "topic", "subject", "channel", "mailbox", "slot"],
        ),
        partition: json_stringish(&raw, &["partition", "shard", "consumer"]),
        offset: json_stringish(&raw, &["offset", "sequence", "lsn", "message_id"]),
        key: json_stringish(&raw, &["key", "message_key", "id", "event_id"]),
        timestamp: json_stringish(&raw, &["timestamp", "occurred_at", "received_at", "ts"]),
        headers: headers.clone(),
        raw,
    }
}
