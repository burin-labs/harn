//! `SessionStore` trait + the shared types every backend speaks.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::event::{AppendEvent, EventId, StoredEvent};
use super::redaction::SharedEventRedactor;
use super::retention::{RetentionPolicy, SharedArchiveSink, Tombstone};
use super::signing::SessionSigner;

pub type SessionId = String;

/// Result of a fork operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkResult {
    pub child_session_id: SessionId,
    pub forked_from_event_id: EventId,
    pub copied_event_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncateResult {
    pub kept_event_count: usize,
    pub removed_event_count: usize,
    pub new_tip_event_id: Option<EventId>,
}

/// Per-session retention/lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Open,
    Closed,
    /// Soft-deleted; will become `HardDeleted` once the grace window
    /// elapses (enforced by retention sweeps; see [`crate::retention`]).
    SoftDeleted,
    HardDeleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    pub tenant_id: Option<String>,
    pub persona: Option<String>,
    pub parent_session_id: Option<SessionId>,
    pub created_at_ms: i64,
    pub created_at: String,
    /// Last `append`/`fork` timestamp; refreshed on every mutation.
    pub updated_at_ms: i64,
    pub updated_at: String,
    pub status: SessionStatus,
    pub event_count: usize,
    pub last_event_id: Option<EventId>,
    pub chain_root_hash: Option<String>,
    pub closed_at_ms: Option<i64>,
    pub closed_at: Option<String>,
    pub soft_deleted_at_ms: Option<i64>,
    pub ttl_seconds: Option<u64>,
    pub tags: Vec<String>,
    pub attributes: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    #[serde(default)]
    pub id: Option<SessionId>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

/// One atomic, idempotent import into a new canonical session.
///
/// `source_id` names the external source independently of the target session;
/// its receipt survives session deletion so retired sources cannot resurrect
/// data. Reusing a source id with a different digest is a conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSession {
    pub source_id: String,
    pub source_digest: String,
    pub session: CreateSession,
    #[serde(default)]
    pub events: Vec<AppendEvent>,
}

impl ImportSession {
    /// Validate the backend-independent import contract.
    pub fn validate(&self) -> StoreResult<()> {
        if self.source_id.trim().is_empty() || self.source_digest.trim().is_empty() {
            return Err(StoreError::InvalidInput(
                "import source_id and source_digest must be non-empty".to_string(),
            ));
        }
        if self
            .session
            .id
            .as_deref()
            .is_none_or(|session_id| session_id.trim().is_empty())
        {
            return Err(StoreError::InvalidInput(
                "import session id must be explicit and non-empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportResult {
    pub source_id: String,
    pub source_digest: String,
    pub session_id: SessionId,
    pub event_count: usize,
    /// True only for the call that committed the import.
    pub imported: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListFilter {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub status: Option<SessionStatus>,
    #[serde(default)]
    pub tag: Option<String>,
    /// Inclusive lower bound on `created_at_ms`.
    #[serde(default)]
    pub created_after_ms: Option<i64>,
    /// Inclusive upper bound on `created_at_ms`.
    #[serde(default)]
    pub created_before_ms: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRange {
    /// Inclusive lower bound; `None` means start from the genesis event.
    #[serde(default)]
    pub from_event_id: Option<EventId>,
    /// Inclusive upper bound; `None` means up to the latest event.
    #[serde(default)]
    pub to_event_id: Option<EventId>,
    /// Maximum number of events to return. Capped at
    /// [`MAX_READ_BATCH`] by the store; callers iterate by advancing
    /// `from_event_id` on the returned cursor.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Page of events plus the cursor needed to continue reading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<StoredEvent>,
    /// Inclusive `from_event_id` to pass into the next read to resume.
    /// `None` when the requested range was fully drained.
    pub next_cursor: Option<EventId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub session: SessionMeta,
    pub events: Vec<StoredEvent>,
    pub captured_at_ms: i64,
    pub captured_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub session_id: SessionId,
    pub chain_root_hash: String,
    pub event_count: usize,
    pub signed_event_count: usize,
    pub failures: Vec<VerifyFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyFailure {
    pub event_id: EventId,
    pub reason: String,
}

/// Errors returned by every backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    NotFound(String),
    AlreadyExists(String),
    Conflict(String),
    InvalidInput(String),
    Tenant(String),
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) => write!(f, "not found: {message}"),
            Self::AlreadyExists(message) => write!(f, "already exists: {message}"),
            Self::Conflict(message) => write!(f, "conflict: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Tenant(message) => write!(f, "tenant: {message}"),
            Self::Backend(message) => write!(f, "backend error: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type StoreResult<T> = Result<T, StoreError>;

/// Soft cap on a single page of events to keep response sizes bounded.
/// Memory + sqlite backends apply this; callers iterate via cursors.
pub const MAX_READ_BATCH: usize = 1_000;

/// Optional processors a host can plug in. Mutation hooks run inline before
/// persistence; redaction is also reapplied to public retrieval projections
/// as defense in depth for older stored data.
#[derive(Default, Clone)]
pub struct StoreHooks {
    /// Applied to event payloads and headers before persistence and again
    /// when events are read, snapshotted, or replayed.
    pub redaction: Option<SharedEventRedactor>,
    /// If set, every event is signed at append time. Without a signer
    /// only the `Receipt` event minted by [`SessionStore::close`] is
    /// signed (which is enough to verify the chain end-to-end).
    pub event_signer: Option<SessionSigner>,
    /// Required to mint receipts on `close`. Without this, `close`
    /// still finalises the chain root hash but emits an unsigned
    /// `Receipt` event.
    pub receipt_signer: Option<SessionSigner>,
    /// Default retention policy applied to new sessions when their
    /// meta does not override it.
    pub retention: RetentionPolicy,
    /// Optional durable archive destination. The default
    /// [`SessionStore::sweep_retention`] writes archived sessions and
    /// tombstones here before the rows leave primary storage; see
    /// [`super::retention::ArchiveSink`].
    pub archive_sink: Option<SharedArchiveSink>,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Plug-in processors configured for this store. The default
    /// [`Self::sweep_retention`] reads `hooks.archive_sink` so the
    /// retention loop can hand archived sessions to durable storage
    /// without callers wiring the sink explicitly.
    fn hooks(&self) -> &StoreHooks;

    async fn create(&self, request: CreateSession) -> StoreResult<SessionMeta>;
    async fn describe(&self, session_id: &str) -> StoreResult<SessionMeta>;
    async fn list(&self, filter: ListFilter) -> StoreResult<Vec<SessionMeta>>;
    async fn append(&self, session_id: &str, event: AppendEvent) -> StoreResult<StoredEvent>;
    async fn read(&self, session_id: &str, range: ReadRange) -> StoreResult<EventPage>;
    async fn fork(
        &self,
        session_id: &str,
        at_event_id: EventId,
        child_id: Option<SessionId>,
    ) -> StoreResult<ForkResult>;
    async fn truncate(&self, session_id: &str, at_event_id: EventId)
        -> StoreResult<TruncateResult>;
    async fn snapshot(&self, session_id: &str) -> StoreResult<Snapshot>;
    async fn replay(&self, snapshot_id: &SnapshotId) -> StoreResult<Snapshot>;
    async fn close(&self, session_id: &str) -> StoreResult<StoredEvent>;
    async fn soft_delete(&self, session_id: &str) -> StoreResult<SessionMeta>;
    async fn hard_delete(&self, session_id: &str) -> StoreResult<()>;
    async fn verify(&self, session_id: &str) -> StoreResult<VerifyReport>;

    /// Sweep retention. Backends with native scheduling can override
    /// to skip the default loop; the default sweeps all sessions
    /// against the configured [`RetentionPolicy`] and routes archived
    /// sessions + tombstones through `hooks().archive_sink` when set.
    async fn sweep_retention(
        &self,
        policy: &RetentionPolicy,
        now_ms: i64,
    ) -> StoreResult<SweepReport> {
        use tracing::Instrument as _;
        let span = tracing::info_span!(
            "harn.session.sweep_retention",
            harn.session.sweep.archive_sink_configured = self.hooks().archive_sink.is_some(),
            harn.session.sweep.archived = tracing::field::Empty,
            harn.session.sweep.soft_deleted = tracing::field::Empty,
            harn.session.sweep.hard_deleted = tracing::field::Empty,
        );
        let span_for_record = span.clone();
        let sink = self.hooks().archive_sink.clone();
        let result = async move {
            let mut report = SweepReport::default();
            let sessions = self.list(ListFilter::default()).await?;
            for session in sessions {
                if policy.should_hard_delete(&session, now_ms) {
                    if let Some(sink) = sink.as_ref() {
                        let tombstone = Tombstone {
                            session_id: session.id.clone(),
                            tenant_id: session.tenant_id.clone(),
                            deleted_at_ms: now_ms,
                            deleted_at: super::event::ms_to_rfc3339(now_ms),
                            final_chain_root_hash: session.chain_root_hash.clone(),
                            final_event_id: session.last_event_id,
                        };
                        sink.tombstone(&tombstone).await?;
                        report.tombstoned += 1;
                    }
                    self.hard_delete(&session.id).await?;
                    report.hard_deleted += 1;
                } else if policy.should_soft_delete(&session, now_ms) {
                    if policy.should_archive(&session, now_ms) {
                        if let Some(sink) = sink.as_ref() {
                            let events = read_all_events(self, &session.id).await?;
                            sink.archive(&session, &events).await?;
                            report.archived += 1;
                        }
                    }
                    self.soft_delete(&session.id).await?;
                    report.soft_deleted += 1;
                }
            }
            Ok::<_, StoreError>(report)
        }
        .instrument(span)
        .await?;
        span_for_record.record("harn.session.sweep.archived", result.archived as i64);
        span_for_record.record(
            "harn.session.sweep.soft_deleted",
            result.soft_deleted as i64,
        );
        span_for_record.record(
            "harn.session.sweep.hard_deleted",
            result.hard_deleted as i64,
        );
        Ok(result)
    }
}

/// Atomic, idempotent ingestion for stores that accept external session data.
///
/// This remains separate from [`SessionStore`] so downstream backends do not
/// need to implement migration semantics unless they expose import support.
#[async_trait]
pub trait SessionImporter: SessionStore {
    async fn import(&self, request: ImportSession) -> StoreResult<ImportResult>;
}

/// Drain every event for a session via repeated paginated reads. Used
/// by [`SessionStore::sweep_retention`] when shipping a session to the
/// [`super::retention::ArchiveSink`].
async fn read_all_events<S: SessionStore + ?Sized>(
    store: &S,
    session_id: &str,
) -> StoreResult<Vec<StoredEvent>> {
    let mut all = Vec::new();
    let mut cursor: Option<EventId> = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: cursor,
                    to_event_id: None,
                    limit: Some(MAX_READ_BATCH),
                },
            )
            .await?;
        let next = page.next_cursor;
        all.extend(page.events);
        match next {
            Some(next_cursor) => cursor = Some(next_cursor),
            None => break,
        }
    }
    Ok(all)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepReport {
    pub soft_deleted: usize,
    pub hard_deleted: usize,
    /// Sessions handed to [`super::retention::ArchiveSink::archive`]
    /// because they crossed `min_age_before_archive_seconds`.
    pub archived: usize,
    /// Hard-deleted sessions whose final state was emitted as a
    /// [`super::retention::Tombstone`] to the archive sink.
    pub tombstoned: usize,
}

/// Dyn-dispatch alias so adapters can keep one `Arc<dyn SessionStore>`
/// in their state without naming the concrete backend everywhere.
pub type SharedSessionStore = Arc<dyn SessionStore>;
