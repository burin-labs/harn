//! Acknowledged canonical history positions, never observability event IDs.

use harn_session_store::SessionStore;

use crate::value::VmError;

pub use harn_session_store::{
    CanonicalHistoryBoundaries, CanonicalHistoryPosition, CanonicalSessionBoundary,
    CANONICAL_HISTORY_BOUNDARIES_SCHEMA, CANONICAL_SESSION_BOUNDARY_SCHEMA,
};
pub const CANONICAL_HISTORY_BOUNDARIES_METHOD: &str = "harn.session_history.boundaries";

pub fn canonical_history_boundaries_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(CanonicalHistoryBoundaries))
        .expect("canonical boundary schema serializes");
    schema["$id"] = serde_json::json!(CANONICAL_HISTORY_BOUNDARIES_SCHEMA);
    schema
}

/// Flush the owning journal before acknowledging any canonical history row.
pub async fn canonical_history_boundaries(
    store: &dyn SessionStore,
    root: &std::path::Path,
    session_id: &str,
) -> Result<CanonicalHistoryBoundaries, VmError> {
    super::durable_fork::ensure_canonical_parent(store, root, session_id).await?;
    store
        .history_boundaries(session_id)
        .await
        .map_err(|error| VmError::Runtime(format!("canonical history boundary: {error}")))
}
