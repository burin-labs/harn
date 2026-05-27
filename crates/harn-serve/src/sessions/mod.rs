//! Session-store primitive for `harn-serve` (issue #2502).
//!
//! The agent-session/transcript primitive owned by `harn-serve`. One
//! source of truth for events (`Message`, `ToolCall`, `ToolResult`,
//! `Plan`, `Compaction`, `SystemReminder`, `Hypothesis`, `Receipt`,
//! `Reminder`, `PermissionDecision`, plus arbitrary `Custom` shapes),
//! snapshots, replay, fork/truncate, signed receipts (Ed25519 over
//! canonical JSON), retention, and HTTP exposure. Subsumes the parallel
//! session DBs in `tui/util/conversation-sessions.ts`,
//! `BurinCore/AgentContext/*.swift`, and the `harn-cloud-tapes` /
//! `harn-cloud-receipts` crates (see issue #2496 epic A.14).
//!
//! ## Layout
//!
//! - [`event`] — event taxonomy + canonical JSON encoder
//! - [`signing`] — Ed25519 chain hashes + receipt signatures
//! - [`store`] — public `SessionStore` trait + shared types
//! - [`memory`] — in-memory backend (tests + headless dev)
//! - [`sqlite`] — persistent SQLite backend (local self-host, TUI)
//! - [`retention`] — declarative per-tenant retention policy
//! - [`api`] — axum router exposing `/v1/sessions/*`
//!
//! The Postgres backend is intentionally absent until A.3 (#2500) lands
//! the `harn-hostlib::postgres` bindings — the trait surface is wide
//! enough to drop a `PgSessionStore` in without changing callers.

pub mod api;
pub mod event;
pub mod memory;
pub(crate) mod memory_helpers;
pub mod retention;
pub mod signing;
pub mod sqlite;
pub mod store;

#[cfg(test)]
mod tests;

pub use api::sessions_router;
pub use event::{
    canonical_event_bytes, canonical_json_bytes, AppendEvent, EventId, EventSignature,
    SessionEventKind, StoredEvent,
};
pub use memory::MemorySessionStore;
pub use retention::{ArchiveSink, RetentionPolicy, SharedArchiveSink};
pub use signing::{
    chain_root_hash, compute_record_hash, verify_event, verify_receipt_root, SessionSigner,
    VerifyError, ALGORITHM as SIGNATURE_ALGORITHM,
};
pub use sqlite::SqliteSessionStore;
pub use store::{
    CreateSession, EventPage, ForkResult, ListFilter, ReadRange, SessionId, SessionMeta,
    SessionStatus, SessionStore, SharedSessionStore, Snapshot, SnapshotId, StoreError, StoreHooks,
    StoreResult, SweepReport, TruncateResult, VerifyFailure, VerifyReport, MAX_READ_BATCH,
};
