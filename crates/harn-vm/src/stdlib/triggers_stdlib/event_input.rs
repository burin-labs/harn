//! Turning a Harn dict into a [`TriggerEvent`].
//!
//! `trigger_fire` lets a script synthesize an event by hand, so this module
//! fills in every field the wire format requires but an author would not want
//! to write out — id, timestamps, dedupe key, trace id, signature status — and
//! synthesizes a provider payload matching the named provider\'s schema when the
//! caller did not supply one.

use uuid::Uuid;

use crate::triggers::test_util::clock;
use crate::triggers::{TriggerEvent, TriggerEventId};
use crate::value::{VmError, VmValue};

pub(super) fn parse_trigger_event(value: &VmValue) -> Result<TriggerEvent, VmError> {
    let mut json = crate::llm::vm_value_to_json(value);
    let raw_event = json.clone();
    let Some(object) = json.as_object_mut() else {
        return Err(VmError::Runtime(
            "trigger_fire: trigger event must be a dict-like value".to_string(),
        ));
    };

    let provider = object
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            VmError::Runtime(
                "trigger_fire: trigger event is missing string field `provider`".to_string(),
            )
        })?;
    let kind = object
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            VmError::Runtime(
                "trigger_fire: trigger event is missing string field `kind`".to_string(),
            )
        })?;

    object
        .entry("id")
        .or_insert_with(|| serde_json::json!(TriggerEventId::new().0));
    object.entry("received_at").or_insert_with(|| {
        serde_json::json!(clock::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default())
    });
    object
        .entry("occurred_at")
        .or_insert(serde_json::Value::Null);
    object.entry("dedupe_key").or_insert_with(|| {
        serde_json::json!(format!("synthetic:{provider}:{kind}:{}", Uuid::now_v7()))
    });
    object
        .entry("trace_id")
        .or_insert_with(|| serde_json::json!(crate::TraceId::new().0));
    object.entry("tenant_id").or_insert(serde_json::Value::Null);
    object
        .entry("headers")
        .or_insert_with(|| serde_json::json!({}));
    object.entry("signature_status").or_insert_with(|| {
        serde_json::json!({
            "state": "unsigned",
        })
    });

    if !object.contains_key("provider_payload") {
        object.insert(
            "provider_payload".to_string(),
            default_provider_payload(provider.as_str(), kind.as_str(), raw_event),
        );
    }

    serde_json::from_value(json).map_err(|error| {
        VmError::Runtime(format!("trigger_fire: trigger event parse error: {error}"))
    })
}

fn default_provider_payload(
    provider: &str,
    kind: &str,
    raw_event: serde_json::Value,
) -> serde_json::Value {
    match provider {
        "github" => serde_json::json!({
            "provider": "github",
            "event": kind,
            "action": serde_json::Value::Null,
            "delivery_id": serde_json::Value::Null,
            "installation_id": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "slack" => serde_json::json!({
            "provider": "slack",
            "event": kind,
            "event_id": serde_json::Value::Null,
            "api_app_id": serde_json::Value::Null,
            "team_id": serde_json::Value::Null,
            "channel_id": serde_json::Value::Null,
            "user_id": serde_json::Value::Null,
            "event_ts": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "linear" => serde_json::json!({
            "provider": "linear",
            "event": kind.split('.').next().unwrap_or(kind),
            "action": kind.split('.').nth(1).unwrap_or("update"),
            "delivery_id": serde_json::Value::Null,
            "organization_id": serde_json::Value::Null,
            "webhook_id": serde_json::Value::Null,
            "url": serde_json::Value::Null,
            "created_at": serde_json::Value::Null,
            "actor": serde_json::Value::Null,
            "webhook_timestamp": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "notion" => serde_json::json!({
            "provider": "notion",
            "event": kind,
            "workspace_id": serde_json::Value::Null,
            "request_id": serde_json::Value::Null,
            "subscription_id": serde_json::Value::Null,
            "integration_id": serde_json::Value::Null,
            "attempt_number": serde_json::Value::Null,
            "entity_id": serde_json::Value::Null,
            "entity_type": serde_json::Value::Null,
            "api_version": serde_json::Value::Null,
            "verification_token": serde_json::Value::Null,
            "polled": serde_json::Value::Null,
            "raw": raw_event,
        }),
        "cron" => serde_json::json!({
            "provider": "cron",
            "cron_id": serde_json::Value::Null,
            "schedule": serde_json::Value::Null,
            "tick_at": clock::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            "raw": raw_event,
        }),
        "webhook" => serde_json::json!({
            "provider": "webhook",
            "source": "trigger_fire",
            "content_type": "application/json",
            "raw": raw_event,
        }),
        "a2a-push" => serde_json::json!({
            "provider": "a2a-push",
            "task_id": serde_json::Value::Null,
            "task_state": serde_json::Value::Null,
            "artifact": serde_json::Value::Null,
            "sender": serde_json::Value::Null,
            "raw": raw_event,
            "kind": "a2a.task.update",
        }),
        _ => serde_json::json!({
            "provider": provider,
            "schema_name": "TriggerEvent",
            "raw": raw_event,
        }),
    }
}
