//! The canonical session store handle: a SQLite store plus its claim on being
//! watched.
//!
//! Its own module because the two things it holds have different owners.
//! `session_store` maps Harn values onto the store contract; the watch claim
//! belongs to `session_wal_watch`. Binding them in one value is what makes
//! "a store nobody has open is not watched" a property of the type rather than
//! a rule somebody has to remember at every call site (harn#7960).

use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, EventId, ListFilter, ReadRange, SearchQuery, SessionStore,
    SqliteSessionStore, StoreHooks, StoredEvent, VerifyReport,
};

#[cfg(test)]
use crate::value::VmError;

/// An open canonical store, together with its claim on being watched.
///
/// The watch registration is a field rather than something the opener leaves
/// behind in a process-global list, because "may this file be watched" is a
/// question about whether anyone still has it open. Keeping the two in one
/// value means a caller cannot hold the store without the claim, or drop the
/// store and leave a reader and a thread attached to the file (harn#7960).
///
/// It carries the full [`SessionStore`] contract by delegation, and derefs to
/// the concrete store for the inherent accessors, so callers use it exactly as
/// they used the store before.
///
/// Cloneable for the same reason [`SqliteSessionStore`] is: a caller that
/// clones the handle is still holding the store open, so the claim is shared
/// rather than copied and the path stops being watched only when the last
/// clone goes away.
#[derive(Clone)]
pub struct CanonicalStore {
    store: SqliteSessionStore,
    _watch: Arc<super::session_wal_watch::StoreWatchRegistration>,
}

impl CanonicalStore {
    /// Take a claim on watching this store's file and hold it with the handle.
    pub(crate) fn new(store: SqliteSessionStore) -> Self {
        let watch = super::session_change::watch_store(store.path());
        Self {
            store,
            _watch: Arc::new(watch),
        }
    }

    /// An in-memory canonical store. Nothing on disk, so nothing to watch.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self, VmError> {
        let store = SqliteSessionStore::open_in_memory()
            .map_err(|error| VmError::Runtime(format!("session_store: {error}")))?;
        Ok(Self::new(store))
    }
}

impl std::ops::Deref for CanonicalStore {
    type Target = SqliteSessionStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

#[async_trait::async_trait]
impl SessionStore for CanonicalStore {
    fn hooks(&self) -> &StoreHooks {
        self.store.hooks()
    }

    async fn create(
        &self,
        request: CreateSession,
    ) -> harn_session_store::StoreResult<harn_session_store::SessionMeta> {
        self.store.create(request).await
    }

    async fn update(
        &self,
        session_id: &str,
        request: harn_session_store::UpdateSession,
    ) -> harn_session_store::StoreResult<harn_session_store::SessionMeta> {
        self.store.update(session_id, request).await
    }

    async fn describe(
        &self,
        session_id: &str,
    ) -> harn_session_store::StoreResult<harn_session_store::SessionMeta> {
        self.store.describe(session_id).await
    }

    async fn list(
        &self,
        filter: ListFilter,
    ) -> harn_session_store::StoreResult<Vec<harn_session_store::SessionMeta>> {
        self.store.list(filter).await
    }

    async fn append(
        &self,
        session_id: &str,
        event: AppendEvent,
    ) -> harn_session_store::StoreResult<StoredEvent> {
        self.store.append(session_id, event).await
    }

    async fn read(
        &self,
        session_id: &str,
        range: ReadRange,
    ) -> harn_session_store::StoreResult<harn_session_store::EventPage> {
        self.store.read(session_id, range).await
    }

    async fn fork(
        &self,
        session_id: &str,
        at_event_id: EventId,
        child_id: Option<harn_session_store::SessionId>,
    ) -> harn_session_store::StoreResult<harn_session_store::ForkResult> {
        self.store.fork(session_id, at_event_id, child_id).await
    }

    async fn truncate(
        &self,
        session_id: &str,
        at_event_id: EventId,
    ) -> harn_session_store::StoreResult<harn_session_store::TruncateResult> {
        self.store.truncate(session_id, at_event_id).await
    }

    async fn snapshot(
        &self,
        session_id: &str,
    ) -> harn_session_store::StoreResult<harn_session_store::Snapshot> {
        self.store.snapshot(session_id).await
    }

    async fn replay(
        &self,
        snapshot_id: &harn_session_store::SnapshotId,
    ) -> harn_session_store::StoreResult<harn_session_store::Snapshot> {
        self.store.replay(snapshot_id).await
    }

    async fn close(&self, session_id: &str) -> harn_session_store::StoreResult<StoredEvent> {
        self.store.close(session_id).await
    }

    async fn soft_delete(
        &self,
        session_id: &str,
    ) -> harn_session_store::StoreResult<harn_session_store::SessionMeta> {
        self.store.soft_delete(session_id).await
    }

    async fn hard_delete(&self, session_id: &str) -> harn_session_store::StoreResult<()> {
        self.store.hard_delete(session_id).await
    }

    async fn verify(&self, session_id: &str) -> harn_session_store::StoreResult<VerifyReport> {
        self.store.verify(session_id).await
    }

    async fn search(
        &self,
        query: SearchQuery,
    ) -> harn_session_store::StoreResult<harn_session_store::SearchResponse> {
        self.store.search(query).await
    }

    async fn sweep_retention(
        &self,
        policy: &harn_session_store::RetentionPolicy,
        now_ms: i64,
    ) -> harn_session_store::StoreResult<harn_session_store::SweepReport> {
        self.store.sweep_retention(policy, now_ms).await
    }
}
