//! Durable Harn session-store primitive.
//!
//! One source of truth for persisted agent/session events (`Message`,
//! `ToolCall`, `ToolResult`, `Plan`, `Compaction`, `SystemReminder`,
//! `Hypothesis`, `Receipt`, `Reminder`, `PermissionDecision`, plus
//! arbitrary `Custom` shapes), snapshots, replay, fork/truncate, signed
//! receipts (Ed25519 over canonical JSON), and retention. Server and host
//! adapters can layer transport, auth, and product policy on top without
//! reimplementing transcript storage semantics.
//!
//! ## Layout
//!
//! - [`event`] - event taxonomy and canonical JSON encoder
//! - [`signing`] - Ed25519 chain hashes and receipt signatures
//! - [`store`] - public `SessionStore` trait and shared types
//! - [`memory`] - in-memory backend for tests and headless dev
//! - [`sqlite`] - persistent SQLite backend for local/self-hosted use
//! - [`retention`] - declarative per-tenant retention policy

pub mod event;
pub mod memory;
pub(crate) mod memory_helpers;
pub mod retention;
pub mod signing;
pub mod sqlite;
pub mod store;

pub use event::{
    canonical_event_bytes, canonical_json_bytes, AppendEvent, EventId, EventSignature,
    SessionEventKind, StoredEvent,
};
pub use memory::MemorySessionStore;
pub use retention::{ArchiveSink, RetentionPolicy, SharedArchiveSink, Tombstone};
pub use signing::{
    chain_root_fold, chain_root_hash, chain_root_init, compute_record_hash, re_anchor_events,
    verify_event, verify_event_chain, verify_receipt_root, SessionSigner, VerifyError,
    ALGORITHM as SIGNATURE_ALGORITHM,
};
pub use sqlite::SqliteSessionStore;
pub use store::{
    CreateSession, EventPage, ForkResult, ListFilter, ReadRange, SessionId, SessionMeta,
    SessionStatus, SessionStore, SharedSessionStore, Snapshot, SnapshotId, StoreError, StoreHooks,
    StoreResult, SweepReport, TruncateResult, VerifyFailure, VerifyReport, MAX_READ_BATCH,
};
