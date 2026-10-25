//! Acknowledged event prefixes in the canonical session-store namespace.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{StoreError, StoreResult, StoredEvent};

pub const CANONICAL_SESSION_BOUNDARY_SCHEMA: &str = "harn.canonical_session_boundary.v1";
pub const CANONICAL_HISTORY_BOUNDARIES_SCHEMA: &str = "harn.canonical_history_boundaries.v1";

/// Correlate a producer's opaque identity with its acknowledged stored prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryPosition {
    pub source_event_id: String,
    pub origin_session_id: String,
    pub before_boundary: CanonicalSessionBoundary,
    pub boundary: CanonicalSessionBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryBoundaries {
    pub tip: CanonicalSessionBoundary,
    pub positions: Vec<CanonicalHistoryPosition>,
}

impl CanonicalHistoryBoundaries {
    pub(crate) fn from_events(session_id: &str, events: &[StoredEvent]) -> Self {
        Self {
            tip: events.last().map_or_else(
                || CanonicalSessionBoundary::empty(session_id),
                CanonicalSessionBoundary::acknowledged,
            ),
            positions: events
                .iter()
                .enumerate()
                .filter_map(|(index, event)| {
                    event
                        .headers
                        .get("source_event_id")
                        .map(|identity| CanonicalHistoryPosition {
                            source_event_id: identity.clone(),
                            origin_session_id: event.canonical_origin_session_id().into(),
                            // Source-linked positions omit metadata rows. The
                            // store owns the actual preceding durable prefix.
                            before_boundary: if index == 0 {
                                CanonicalSessionBoundary::empty(session_id)
                            } else {
                                CanonicalSessionBoundary::acknowledged(&events[index - 1])
                            },
                            boundary: CanonicalSessionBoundary::acknowledged(event),
                        })
                })
                .collect(),
        }
    }
}

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
