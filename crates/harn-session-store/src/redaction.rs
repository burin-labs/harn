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
    if hooks.redaction.is_none() {
        return Ok(());
    }
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
    normalize_identity_headers(&mut event.headers).map_err(|error| {
        StoreError::Backend(format!(
            "stored event {} has invalid producer identity: {error}",
            event.event_id
        ))
    })
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
