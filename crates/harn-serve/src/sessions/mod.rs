//! HTTP adapter for the reusable Harn session-store primitive.
//!
//! `harn-session-store` owns the durable event/store/signing/retention
//! semantics. `harn-serve` adds the Axum router and reexports the store
//! surface so existing server consumers do not need to know whether they
//! are linked through the transport crate or the storage crate directly.

pub mod api;

#[cfg(test)]
mod tests;

pub use api::sessions_router;
pub use harn_session_store::{
    canonical_event_bytes, canonical_json_bytes, chain_root_fold, chain_root_hash, chain_root_init,
    compute_record_hash, re_anchor_events, verify_event, verify_receipt_root, AppendEvent,
    ArchiveSink, CreateSession, EventId, EventPage, EventSignature, ForkResult, ListFilter,
    MemorySessionStore, ReadRange, RetentionPolicy, SessionEventKind, SessionId, SessionMeta,
    SessionSigner, SessionStatus, SessionStore, SharedArchiveSink, SharedSessionStore, Snapshot,
    SnapshotId, SqliteSessionStore, StoreError, StoreHooks, StoreResult, StoredEvent, SweepReport,
    Tombstone, TruncateResult, VerifyError, VerifyFailure, VerifyReport, MAX_READ_BATCH,
    SIGNATURE_ALGORITHM,
};
