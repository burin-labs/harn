//! Dependency-inverted redaction hooks for canonical session events.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::event::{AppendEvent, StoredEvent};
use crate::identity::{normalize_identity_headers, EventIdentity, EventIdentityError};
use crate::store::{StoreError, StoreHooks, StoreResult};

/// Minimal object-safe contract implemented by a host's redaction policy.
///
/// Agent policy stays outside this storage crate. The store only guarantees
/// that the supplied transformation runs before hashing/signing and again as
/// defense in depth when events are read.
pub trait EventRedactor: Send + Sync {
    fn redact_json_in_place(&self, value: &mut Value);
    fn redact_headers(&self, headers: &BTreeMap<String, String>) -> BTreeMap<String, String>;
}

pub type SharedEventRedactor = Arc<dyn EventRedactor>;

pub(crate) fn prepare_append_event(hooks: &StoreHooks, event: &mut AppendEvent) -> StoreResult<()> {
    let identity = normalize_identity_headers(&mut event.headers)
        .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
    apply_event_redaction(hooks, &mut event.payload, &mut event.headers, &identity)
        .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
    Ok(())
}

pub(crate) fn prepare_stored_events_for_persistence(
    hooks: &StoreHooks,
    events: &mut [StoredEvent],
) -> StoreResult<()> {
    for event in events {
        let identity = normalize_stored_identity(event)?;
        apply_event_redaction(hooks, &mut event.payload, &mut event.headers, &identity).map_err(
            |error| {
                StoreError::Backend(format!(
                    "redaction policy changed producer identity for stored event {}: {error}",
                    event.event_id
                ))
            },
        )?;
    }
    Ok(())
}

pub(crate) fn redact_stored_events(
    hooks: &StoreHooks,
    events: &mut [StoredEvent],
) -> StoreResult<()> {
    for event in events {
        let original_payload = event.payload.clone();
        let original_headers = event.headers.clone();
        let identity = normalize_stored_identity(event)?;
        apply_event_redaction(hooks, &mut event.payload, &mut event.headers, &identity).map_err(
            |error| {
                StoreError::Backend(format!(
                    "redaction policy changed producer identity for stored event {}: {error}",
                    event.event_id
                ))
            },
        )?;
        if event.payload != original_payload || event.headers != original_headers {
            event.mark_redacted_projection();
        }
    }
    Ok(())
}

fn normalize_stored_identity(event: &mut StoredEvent) -> StoreResult<EventIdentity> {
    loop {
        match normalize_identity_headers(&mut event.headers) {
            Ok(identity) => return Ok(identity),
            Err(EventIdentityError::Invalid { field, .. }) => {
                relocate_historical_identity_header(&mut event.headers, field);
            }
            Err(error) => {
                return Err(StoreError::Backend(format!(
                    "stored event {} has invalid producer identity: {error}",
                    event.event_id
                )))
            }
        }
    }
}

fn relocate_historical_identity_header(
    headers: &mut BTreeMap<String, String>,
    field: crate::identity::EventIdentityField,
) {
    let Some(value) = headers.remove(field.header_name()) else {
        return;
    };
    let base = format!("harn.legacy.header.{}", field.header_name());
    let mut key = base.clone();
    let mut suffix = 2;
    while headers.contains_key(&key) {
        key = format!("{base}.{suffix}");
        suffix += 1;
    }
    headers.insert(key, value);
}

fn apply_event_redaction(
    hooks: &StoreHooks,
    payload: &mut Value,
    headers: &mut BTreeMap<String, String>,
    identity: &EventIdentity,
) -> Result<(), EventIdentityError> {
    let Some(policy) = hooks.redaction.as_ref() else {
        return Ok(());
    };
    policy.redact_json_in_place(payload);
    *headers = policy.redact_headers(headers);
    identity.apply_to_headers(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SessionEventKind;

    fn stored_event(headers: BTreeMap<String, String>) -> StoredEvent {
        StoredEvent {
            event_id: 1,
            session_id: "session".to_string(),
            tenant_id: None,
            parent_event_id: None,
            actor: None,
            kind: SessionEventKind::Message,
            payload: Value::Null,
            tags: Vec::new(),
            headers,
            ts_ms: 0,
            ts: "1970-01-01T00:00:00Z".to_string(),
            record_hash: "sha256:source".to_string(),
            prev_hash: None,
            signed_by: None,
        }
    }

    #[test]
    fn historical_invalid_identity_headers_become_explicit_projections() {
        let mut events = vec![stored_event(BTreeMap::from([
            ("run_id".to_string(), " ".to_string()),
            ("harn.legacy.header.run_id".to_string(), "older".to_string()),
            ("turn_id".to_string(), " turn-1 ".to_string()),
        ]))];

        redact_stored_events(&StoreHooks::default(), &mut events).unwrap();

        let event = &events[0];
        assert_eq!(event.headers["harn.legacy.header.run_id"], "older");
        assert_eq!(event.headers["harn.legacy.header.run_id.2"], " ");
        assert_eq!(event.headers["turn_id"], "turn-1");
        assert!(!event.headers.contains_key("run_id"));
        assert!(event.is_redacted_projection());
    }
}
