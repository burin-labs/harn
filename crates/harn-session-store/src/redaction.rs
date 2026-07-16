//! Dependency-inverted redaction hooks for canonical session events.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::event::{AppendEvent, StoredEvent};
use crate::identity::normalize_identity_headers;
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
    let Some(policy) = hooks.redaction.as_ref() else {
        return Ok(());
    };
    policy.redact_json_in_place(&mut event.payload);
    event.headers = policy.redact_headers(&event.headers);
    identity
        .apply_to_headers(&mut event.headers)
        .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
    Ok(())
}

pub(crate) fn redact_stored_events(
    hooks: &StoreHooks,
    events: &mut [StoredEvent],
) -> StoreResult<()> {
    let Some(policy) = hooks.redaction.as_ref() else {
        return Ok(());
    };
    for event in events {
        let identity = normalize_identity_headers(&mut event.headers)
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        policy.redact_json_in_place(&mut event.payload);
        event.headers = policy.redact_headers(&event.headers);
        identity
            .apply_to_headers(&mut event.headers)
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
    }
    Ok(())
}
