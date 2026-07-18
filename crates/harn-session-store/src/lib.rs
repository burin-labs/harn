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
//! - [`identity`] - typed producer identity over canonical signed headers
//! - [`redaction`] - dependency-inverted event redaction contract
//! - [`signing`] - Ed25519 chain hashes and receipt signatures
//! - [`store`] - public `SessionStore` trait and shared types
//! - [`memory`] - in-memory backend for tests and headless dev
//! - [`sqlite`] - persistent SQLite backend for local/self-hosted use
//! - [`retention`] - declarative per-tenant retention policy

pub mod event;
pub mod identity;
pub mod memory;
pub(crate) mod memory_helpers;
pub mod redaction;
pub mod retention;
pub mod signing;
pub mod sqlite;
pub mod store;

pub use event::{
    canonical_event_bytes, canonical_json_bytes, AppendEvent, EventId, EventSignature,
    SessionEventKind, StoredEvent,
};
pub use identity::{EventIdentity, EventIdentityError, EventIdentityField};
pub use memory::MemorySessionStore;
pub use redaction::{EventRedactor, SharedEventRedactor};
pub use retention::{ArchiveSink, RetentionPolicy, SharedArchiveSink, Tombstone};
pub use signing::{
    chain_root_fold, chain_root_hash, chain_root_init, compute_record_hash, re_anchor_events,
    verify_event, verify_event_chain, verify_receipt_root, verify_session_chain, SessionSigner,
    VerifyError, ALGORITHM as SIGNATURE_ALGORITHM,
};
pub use sqlite::SqliteSessionStore;
pub use store::{
    CreateSession, EventPage, ForkResult, ImportResult, ImportSession, ListFilter, ReadRange,
    SessionId, SessionImporter, SessionMeta, SessionStatus, SessionStore, SharedSessionStore,
    Snapshot, SnapshotId, StoreContention, StoreError, StoreHooks, StoreResult, SweepReport,
    TruncateResult, VerifyFailure, VerifyReport, MAX_READ_BATCH,
};
