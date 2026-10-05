//! Acknowledged canonical history positions, never observability event IDs.

use harn_session_store::{SessionStore, StoredEvent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::value::VmError;

pub use harn_session_store::{CanonicalSessionBoundary, CANONICAL_SESSION_BOUNDARY_SCHEMA};
pub const CANONICAL_HISTORY_BOUNDARIES_METHOD: &str = "harn.session_history.boundaries";

pub fn canonical_history_boundaries_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CanonicalHistoryBoundaries))
        .expect("canonical boundary schema serializes")
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
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<CanonicalHistoryBoundaries, VmError> {
    crate::agent_session_journal::flush(session_id).await?;
    store
        .describe(session_id)
        .await
        .map_err(|error| VmError::Runtime(format!("canonical history boundary: {error}")))?;
    let events = store
        .read_all(session_id)
        .await
        .map_err(|error| VmError::Runtime(format!("canonical history boundary: {error}")))?;
    Ok(CanonicalHistoryBoundaries::from_events(session_id, &events))
}
