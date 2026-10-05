//! Acknowledged canonical history positions, never observability event IDs.

use std::path::Path;

use harn_session_store::{SessionStore, StoredEvent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::value::VmError;

pub const CANONICAL_SESSION_BOUNDARY_SCHEMA: &str = "harn.canonical_session_boundary.v1";
pub const CANONICAL_HISTORY_BOUNDARIES_METHOD: &str = "harn.session_history.boundaries";

pub fn canonical_history_boundaries_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CanonicalHistoryBoundaries))
        .expect("canonical boundary schema serializes")
}

/// A persisted prefix of one canonical session. The hash prevents an event
/// number from another event domain or a rewritten row from being accepted.
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

    fn acknowledged(event: &StoredEvent) -> Self {
        Self {
            schema: CANONICAL_SESSION_BOUNDARY_SCHEMA.into(),
            session_id: event.session_id.clone(),
            event_id: Some(event.event_id),
            record_hash: Some(event.source_record_hash().into()),
        }
    }

    pub(super) fn validate(&self, session_id: &str, events: &[StoredEvent]) -> Result<(), VmError> {
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
            Err(VmError::Runtime(
                "canonical_boundary.invalid: boundary is stale, foreign, or not acknowledged by this canonical session".into(),
            ))
        }
    }
}

/// An event identity and its acknowledged canonical position. Consumers bind
/// their displayed entries to these identities rather than counting rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryPosition {
    pub source_event_id: String,
    pub boundary: CanonicalSessionBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalHistoryBoundaries {
    pub tip: CanonicalSessionBoundary,
    pub positions: Vec<CanonicalHistoryPosition>,
}

impl CanonicalHistoryBoundaries {
    pub(super) fn from_events(session_id: &str, events: &[StoredEvent]) -> Self {
        Self {
            tip: events.last().map_or_else(
                || CanonicalSessionBoundary::empty(session_id),
                CanonicalSessionBoundary::acknowledged,
            ),
            positions: events
                .iter()
                .filter_map(|event| {
                    event
                        .headers
                        .get("source_event_id")
                        .map(|identity| CanonicalHistoryPosition {
                            source_event_id: identity.clone(),
                            boundary: CanonicalSessionBoundary::acknowledged(event),
                        })
                })
                .collect(),
        }
    }
}

/// Flush the owning journal before acknowledging any canonical history row.
pub async fn canonical_history_boundaries(
    root: &Path,
    session_id: &str,
) -> Result<CanonicalHistoryBoundaries, VmError> {
    crate::agent_session_journal::flush(session_id).await?;
    let store = crate::stdlib::session_store::open_canonical_store(root)?;
    store
        .describe(session_id)
        .await
        .map_err(|error| VmError::Runtime(format!("canonical history boundary: {error}")))?;
    let events = crate::stdlib::session_store::read_all_events(&store, session_id).await?;
    Ok(CanonicalHistoryBoundaries::from_events(session_id, &events))
}
