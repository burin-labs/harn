//! Turning a Harn dict into a [`TriggerEvent`].
//!
//! `trigger_fire` lets a script synthesize an event by hand, so this module
//! fills in every field the wire format requires but an author would not want
//! to write out — id, timestamps, dedupe key, trace id, signature status — and
//! synthesizes a provider payload matching the named provider's schema when the
//! caller did not supply one.

use uuid::Uuid;

use crate::triggers::test_util::clock;
use crate::triggers::{
    ProviderCatalogError, ProviderId, ProviderPayload, TriggerEvent, TriggerEventId,
};
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
            default_provider_payload(provider.as_str(), kind.as_str(), raw_event)?,
        );
    }

    let mut event: TriggerEvent = serde_json::from_value(json).map_err(|error| {
        VmError::Runtime(format!("trigger_fire: trigger event parse error: {error}"))
    })?;
    event.kind = event.provider_payload.event_kind(&event.kind);
    Ok(event)
}

fn default_provider_payload(
    provider: &str,
    kind: &str,
    raw_event: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let headers = serde_json::from_value(raw_event["headers"].clone()).unwrap_or_default();
    let payload = match ProviderPayload::normalize(
        &ProviderId::from(provider),
        kind,
        &headers,
        raw_event.clone(),
    ) {
        Ok(payload) => payload,
        Err(ProviderCatalogError::UnknownProvider(_)) => {
            ProviderPayload::extension(provider, "TriggerEvent", raw_event)
        }
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "trigger_fire: provider payload normalization failed: {error}"
            )))
        }
    };
    serde_json::to_value(payload).map_err(|error| {
        VmError::Runtime(format!(
            "trigger_fire: provider payload serialization failed: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_events_use_current_provider_payload_contracts() {
        let pinned = time::OffsetDateTime::UNIX_EPOCH;
        let _clock = clock::install_override(clock::MockClock::new(pinned));
        for provider in ["github", "slack", "linear", "synthetic-custom"] {
            let raw = serde_json::json!({"provider": provider, "kind": "opened", "number": 42});
            let event = parse_trigger_event(&crate::stdlib::json_to_vm_value(&raw)).unwrap();
            let ProviderPayload::Extension(payload) = event.provider_payload else {
                panic!("package and custom providers must use extension payloads");
            };
            assert_eq!(payload.provider, provider);
            assert_eq!(payload.raw, raw);
        }
        let cron = parse_trigger_event(&crate::stdlib::json_to_vm_value(&serde_json::json!({
            "provider": "cron", "kind": "cron.tick"
        })))
        .unwrap();
        let ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Cron(payload)) =
            cron.provider_payload
        else {
            panic!("built-in cron must retain its typed payload");
        };
        assert_eq!(payload.tick_at, pinned);
    }
}
