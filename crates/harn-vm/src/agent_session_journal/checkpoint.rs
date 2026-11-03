use harn_session_store::{SessionMeta, SessionStore, SessionWriteLease, StoreError};

use super::CanonicalStoreDir;
use crate::value::VmError;

/// A canonical checkpoint inspected while excluding competing writers.
pub(crate) struct CanonicalSessionCheckpoint {
    pub state_dir: CanonicalStoreDir,
    pub meta: SessionMeta,
    _lease: SessionWriteLease,
}

/// Select and inspect a closed stream under the journal's writer discipline.
/// Reject an unrelated store address before opening or creating any database.
pub(crate) async fn inspect_closed_session_checkpoint(
    session_id: &str,
    captured_dir: Option<CanonicalStoreDir>,
    accepts_scope: impl FnOnce(&CanonicalStoreDir) -> bool,
) -> Result<Option<CanonicalSessionCheckpoint>, VmError> {
    let state_dir = match captured_dir {
        Some(dir) => dir,
        None => crate::stdlib::session_store::canonical_store_state_dir(None)?,
    };
    if !accepts_scope(&state_dir) {
        return Ok(None);
    }
    let lease =
        SessionWriteLease::try_acquire(state_dir.as_path(), session_id).map_err(|error| {
            crate::stdlib::session_store::session_lease_error("session continuation", error)
        })?;
    let store = crate::stdlib::session_store::open_store(&state_dir)?;
    let meta = match store.describe(session_id).await {
        Ok(meta) => meta,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(VmError::Runtime(format!("session continuation: {error}"))),
    };
    Ok(Some(CanonicalSessionCheckpoint {
        state_dir,
        meta,
        _lease: lease,
    }))
}
