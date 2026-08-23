//! Observation hook for committed session-metadata changes.

use std::sync::Arc;

use super::store::SessionMeta;

/// Minimal object-safe contract for watching session metadata change.
///
/// A session's title, model, or usage can move from any caller holding a
/// store handle — an HTTP `PATCH`, a titling pass, a usage checkpoint. A
/// surface that already projected that metadata to a person has no way to
/// learn it moved, so it keeps rendering a stale name until something else
/// makes it re-read. The two backends also cannot each grow their own
/// notification, or the "when does a change count" rule drifts the way
/// `resolve_title_update` exists to stop the title rule drifting.
///
/// Storage does not know what a surface wants to do about a change, so the
/// store publishes the committed [`SessionMeta`] and stays out of transport.
/// Implementations must not block and must not re-enter the store: the store
/// calls this only after its own lock or transaction is released, but a slow
/// observer still stalls the caller that made the change.
pub trait SessionChangeObserver: Send + Sync {
    /// Called once per committed metadata update, with the row as persisted.
    fn session_updated(&self, meta: &SessionMeta);
}

pub type SharedSessionChangeObserver = Arc<dyn SessionChangeObserver>;
