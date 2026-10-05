//! Acknowledged event prefixes in the canonical session-store namespace.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{StoreError, StoreResult, StoredEvent};

pub const CANONICAL_SESSION_BOUNDARY_SCHEMA: &str = "harn.canonical_session_boundary.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSessionBoundary {
    pub schema: String,
    pub session_id: String,
    pub event_id: Option<u64>,
    pub record_hash: Option<String>,
}

impl CanonicalSessionBoundary {
    pub fn empty(session_id: &str) -> Self {
        Self {
            schema: CANONICAL_SESSION_BOUNDARY_SCHEMA.into(),
            session_id: session_id.into(),
            event_id: None,
            record_hash: None,
        }
    }

    pub fn acknowledged(event: &StoredEvent) -> Self {
        Self {
            schema: CANONICAL_SESSION_BOUNDARY_SCHEMA.into(),
            session_id: event.session_id.clone(),
            event_id: Some(event.event_id),
            record_hash: Some(event.source_record_hash().into()),
        }
    }

    /// Called inside the backend's existing fork transaction or store lock.
    pub fn validate(&self, session_id: &str, events: &[StoredEvent]) -> StoreResult<()> {
        let valid = self.schema == CANONICAL_SESSION_BOUNDARY_SCHEMA
            && self.session_id == session_id
            && match (self.event_id, self.record_hash.as_deref()) {
                (None, None) => true,
                (Some(id), Some(hash)) => events.iter().any(|event| {
                    event.event_id == id
                        && event.session_id == session_id
                        && event.source_record_hash() == hash
                }),
                _ => false,
            };
        if valid {
            Ok(())
        } else {
            Err(StoreError::Conflict(
                "canonical_boundary.invalid: boundary is stale, foreign, or not acknowledged by this canonical session".into(),
            ))
        }
    }
}
