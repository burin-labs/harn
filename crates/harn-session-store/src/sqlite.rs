//! SQLite-backed [`SessionStore`].
//!
//! Single-file durable backend suitable for self-hosted deployments and
//! the TUI's persistent session DB. Schema versioning is intentionally
//! minimal. File-backed databases use the shared Harn SQLite schema marker;
//! pre-marker databases are upgraded from the original `schema_version` table
//! in the same initialization transaction. The Postgres backend (issue #2500)
//! follows the same shape so consumers can swap by config.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use harn_sqlite::{
    initialize_file, initialize_transient, require_snapshot_initialized, sqlite_contention,
    SchemaVersion, SqliteContention,
};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use uuid::Uuid;

use super::event::{
    now_ms_and_rfc3339, AppendEvent, EventId, EventSignature, SessionEventKind, StoredEvent,
};
use super::lease::{SessionLeaseError, SessionMaintenanceLease, SessionMutationLease};
use super::process_locks::{self, HeldGuard};
use super::redaction::{
    prepare_append_event, prepare_stored_events_for_persistence, redact_stored_events,
};
use super::retention::RetentionPolicy;
use super::search::{
    combined_score, fts_literal_query, ranks, redacted_search_document,
    redacted_search_document_parts, snippet, vector_blob, vector_from_blob, SearchHit, SearchMode,
    SearchQuery, SearchResponse,
};
use super::signing::{
    chain_root_fold, chain_root_hash, chain_root_init, compute_record_hash, re_anchor_events,
    verify_session_chain,
};
use super::store::{
    CreateSession, EventPage, ForkResult, ImportResult, ImportSession, ListFilter, ListOrder,
    ListSortKey, ReadRange, SessionId, SessionImporter, SessionMeta, SessionStatus, SessionStore,
    SessionType, Snapshot, SnapshotId, StoreContention, StoreError, StoreHooks, StoreResult,
    SweepReport, TruncateResult, UpdateSession, VerifyReport, MAX_READ_BATCH,
};

// v6 adds durable task admission, execution descriptors, and control events.
// Adding a table or column has to move this number:
// the shared initializer fast-paths out of schema setup entirely when the
// recorded version already matches, so an unbumped column never reaches an
// existing database.
const SCHEMA_NAME: &str = "session_store";
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_SCHEMA: SchemaVersion =
    SchemaVersion::new(SCHEMA_NAME, SqliteSessionStore::SCHEMA_VERSION);

#[derive(Clone)]
pub struct SqliteSessionStore {
    inner: Arc<StoreInner>,
}

/// The small, indexed part of a session needed to decide if it has a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionJournalPresence {
    pub tenant_id: Option<String>,
    pub has_events: bool,
}

// Clones are claims on one open store, not copies of its configuration.
// Keep the connection, hooks, admission and snapshot lifetime together so
// adding store policy does not enlarge every async caller's stack frame.
struct StoreInner {
    conn: Mutex<Connection>,
    hooks: StoreHooks,
    path: PathBuf,
    mutation_admission: MutationAdmission,
    _snapshot: Option<tempfile::TempDir>,
    task_claims: Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// Committed event-append transactions, for batching diagnostics.
    append_commits: std::sync::atomic::AtomicU64,
    /// Declared last so it drops after `conn` closes. See [`crate::process_locks`].
    _held: Option<HeldGuard>,
}

#[derive(Debug)]
enum MutationAdmission {
    InMemory,
    ReadOnly,
    Shared { store_dir: PathBuf },
    Maintenance { _lease: SessionMaintenanceLease },
}

struct MutationConnection<'a> {
    _lease: Option<SessionMutationLease>,
    connection: MutexGuard<'a, Connection>,
}

impl Deref for MutationConnection<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for MutationConnection<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl SqliteSessionStore {
    /// Current schema version supported by the session store initializer.
    pub const SCHEMA_VERSION: i64 = 6;

    /// Inspect a session without reading its events or its potentially large metadata.
    pub fn journal_presence(
        &self,
        session_id: &str,
    ) -> StoreResult<Option<SessionJournalPresence>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT tenant_id, event_count FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                let count: i64 = row.get(1)?;
                if count < 0 {
                    return Err(rusqlite::Error::IntegralValueOutOfRange(1, count));
                }
                Ok(SessionJournalPresence {
                    tenant_id: row.get(0)?,
                    has_events: count > 0,
                })
            },
        )
        .optional()
        .map_err(map_sql)
    }

    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_with_hooks(path, StoreHooks::default())
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        Self::open_in_memory_with_hooks(StoreHooks::default())
    }

    /// Open an initialized transient store with custom hooks.
    pub fn open_in_memory_with_hooks(hooks: StoreHooks) -> StoreResult<Self> {
        let conn =
            Connection::open_in_memory().map_err(|error| StoreError::Backend(error.to_string()))?;
        Self::initialize(
            conn,
            PathBuf::from(":memory:"),
            hooks,
            MutationAdmission::InMemory,
            None,
        )
    }

    /// Open an initialized file-backed store without creating or migrating it.
    ///
    /// When this process holds no connection on the store, the returned handle
    /// reads a stable temporary snapshot through SQLite's read-only connection
    /// mode, so every mutating [`SessionStore`] operation fails at the database
    /// boundary without SQLite touching source WAL/SHM bookkeeping. When it does
    /// hold one, copying would release its locks, so the handle reads the live
    /// store through a read-only connection instead (see `process_locks`).
    /// Missing, malformed, and stale-schema databases are reported without
    /// creating the database, its parent directory, or an initialization lock.
    pub fn open_read_only(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_read_only_with_hooks(path, StoreHooks::default())
    }

    /// Open an initialized file-backed store read-only with custom hooks.
    pub fn open_read_only_with_hooks(
        path: impl AsRef<Path>,
        hooks: StoreHooks,
    ) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        // Holding the registry across the copy keeps any connection from
        // opening on the file until the copy has closed its descriptors.
        let held = process_locks::lock();
        if held.holds(&path) {
            drop(held);
            return Self::open_live_read_only(path, hooks);
        }
        let (snapshot, snapshot_path) = copy_sqlite_snapshot(&path)?;
        drop(held);
        let conn = Connection::open_with_flags(&snapshot_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        require_snapshot_initialized::<StoreError>(&conn, SQLITE_SCHEMA)
            .map_err(map_initialization)?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                conn: Mutex::new(conn),
                hooks,
                path,
                mutation_admission: MutationAdmission::ReadOnly,
                _snapshot: Some(snapshot),
                task_claims: Arc::default(),
                append_commits: std::sync::atomic::AtomicU64::new(0),
                _held: None,
            }),
        })
    }

    /// Read through a SQLite connection on the live files because this process
    /// already holds locks on them; a raw copy here would release those locks.
    /// One read transaction gives every query on the handle the same frozen
    /// view, so drop the handle promptly to let WAL checkpoints proceed.
    fn open_live_read_only(path: PathBuf, hooks: StoreHooks) -> StoreResult<Self> {
        let backend = |error: rusqlite::Error| StoreError::Backend(error.to_string());
        let held = process_locks::register(&path);
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(backend)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(backend)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(backend)?;
        conn.execute_batch("BEGIN DEFERRED").map_err(backend)?;
        require_snapshot_initialized::<StoreError>(&conn, SQLITE_SCHEMA)
            .map_err(map_initialization)?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                conn: Mutex::new(conn),
                hooks,
                path,
                mutation_admission: MutationAdmission::ReadOnly,
                _snapshot: None,
                task_claims: Arc::default(),
                append_commits: std::sync::atomic::AtomicU64::new(0),
                _held: Some(held),
            }),
        })
    }

    pub fn open_with_hooks(path: impl AsRef<Path>, hooks: StoreHooks) -> StoreResult<Self> {
        let path = stable_file_path(path.as_ref())?;
        let store_dir = store_directory(&path);
        let _initialization_lease = SessionMutationLease::try_acquire(&store_dir)
            .map_err(|error| map_lease(error, StoreContention::MaintenanceActive))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| StoreError::Backend(error.to_string()))?;
            }
        }
        let held = process_locks::register(&path);
        let conn =
            Connection::open(&path).map_err(|error| StoreError::Backend(error.to_string()))?;
        Self::initialize(
            conn,
            path,
            hooks,
            MutationAdmission::Shared { store_dir },
            Some(held),
        )
    }

    /// Open a file-backed store while excluding every participating writer.
    ///
    /// The returned store holds the exclusive lease for its full lifetime and
    /// performs its own mutations directly under that ownership. Ordinary
    /// stores fail with typed contention until every maintenance clone drops.
    pub fn open_for_maintenance(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_for_maintenance_with_hooks(path, StoreHooks::default())
    }

    /// Open a file-backed store for exclusive maintenance with custom hooks.
    pub fn open_for_maintenance_with_hooks(
        path: impl AsRef<Path>,
        hooks: StoreHooks,
    ) -> StoreResult<Self> {
        let path = stable_file_path(path.as_ref())?;
        let store_dir = store_directory(&path);
        let lease = SessionMaintenanceLease::try_acquire(&store_dir)
            .map_err(|error| map_lease(error, StoreContention::ProjectLeaseHeld))?;
        let held = process_locks::register(&path);
        let conn =
            Connection::open(&path).map_err(|error| StoreError::Backend(error.to_string()))?;
        Self::initialize(
            conn,
            path,
            hooks,
            MutationAdmission::Maintenance { _lease: lease },
            Some(held),
        )
    }

    fn initialize(
        mut conn: Connection,
        path: PathBuf,
        hooks: StoreHooks,
        mutation_admission: MutationAdmission,
        held: Option<HeldGuard>,
    ) -> StoreResult<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let initialization = if path == Path::new(":memory:") {
            initialize_transient(
                &conn,
                DEFAULT_BUSY_TIMEOUT,
                SQLITE_SCHEMA,
                initialize_session_schema,
            )
        } else {
            initialize_file(
                &conn,
                DEFAULT_BUSY_TIMEOUT,
                SQLITE_SCHEMA,
                initialize_session_schema,
            )
        };
        initialization.map_err(map_initialization)?;
        rebuild_search_index_if_needed(&mut conn, &hooks)?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                conn: Mutex::new(conn),
                hooks,
                path,
                mutation_admission,
                _snapshot: None,
                task_claims: Arc::default(),
                append_commits: std::sync::atomic::AtomicU64::new(0),
                _held: held,
            }),
        })
    }

    /// Stable database path retained by this handle.
    ///
    /// Writable file-backed stores resolve relative input when they open so a
    /// later process-wide current-directory change cannot move their lock
    /// domain away from the already-open SQLite database.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Append `events` to one session, in order, as a single transaction.
    ///
    /// Every event is redacted, chained, hashed, signed, and indexed exactly
    /// as [`SessionStore::append`] would do it one at a time, so the stored
    /// rows are the same. The difference is atomicity and cost: the batch
    /// commits once, and on any error nothing in it is persisted, so a
    /// caller that keeps its queue until this returns can retry the whole
    /// batch without duplicating a prefix.
    pub async fn append_batch(
        &self,
        session_id: &str,
        events: Vec<AppendEvent>,
    ) -> StoreResult<Vec<StoredEvent>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let stored = append_events_in_tx(&tx, self.hooks(), session_id, events)?;
        tx.commit().map_err(map_sql)?;
        self.record_append_commit();
        Ok(stored)
    }

    /// Event-append transactions this store has committed since it opened,
    /// counted across clones of the handle. A diagnostic for callers that
    /// batch appends: one flush of N events should cost one transaction.
    pub fn append_transactions(&self) -> u64 {
        self.inner
            .append_commits
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn record_append_commit(&self) {
        self.inner
            .append_commits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The single admission boundary for every operation that can change a
    /// file-backed store. The lease is taken before the connection mutex and
    /// held until the returned guard drops.
    fn lock_for_mutation(&self) -> StoreResult<MutationConnection<'_>> {
        let lease = match &self.inner.mutation_admission {
            MutationAdmission::InMemory
            | MutationAdmission::ReadOnly
            | MutationAdmission::Maintenance { .. } => None,
            MutationAdmission::Shared { store_dir } => Some(
                SessionMutationLease::try_acquire(store_dir)
                    .map_err(|error| map_lease(error, StoreContention::MaintenanceActive))?,
            ),
        };
        Ok(MutationConnection {
            _lease: lease,
            connection: self.lock(),
        })
    }

    fn require_maintenance(&self, operation: &'static str) -> StoreResult<()> {
        match &self.inner.mutation_admission {
            MutationAdmission::InMemory | MutationAdmission::Maintenance { .. } => Ok(()),
            MutationAdmission::ReadOnly | MutationAdmission::Shared { .. } => {
                Err(StoreError::MaintenanceRequired { operation })
            }
        }
    }
}

/// Resolve the file identity used by SQLite handles and their observers.
pub fn stable_file_path(path: &Path) -> StoreResult<PathBuf> {
    std::path::absolute(path).map_err(|error| StoreError::Backend(error.to_string()))
}

fn store_directory(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn map_lease(error: SessionLeaseError, kind: StoreContention) -> StoreError {
    match error {
        error @ SessionLeaseError::Contended { .. } => StoreError::Contention {
            kind,
            message: error.to_string(),
        },
        error => StoreError::Backend(error.to_string()),
    }
}

#[derive(PartialEq, Eq)]
struct SnapshotSource {
    path: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn copy_sqlite_snapshot(path: &Path) -> StoreResult<(tempfile::TempDir, PathBuf)> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 0..MAX_ATTEMPTS {
        let before = snapshot_sources(path)?;
        let snapshot = tempfile::Builder::new()
            .prefix("harn-session-store-inspect-")
            .tempdir()
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let snapshot_path = snapshot.path().join("session-store.sqlite");
        let mut copy_failed = false;
        for source in &before {
            let destination = if source.path == path {
                snapshot_path.clone()
            } else if source.path == sqlite_sidecar_path(path, "-wal") {
                sqlite_sidecar_path(&snapshot_path, "-wal")
            } else {
                sqlite_sidecar_path(&snapshot_path, "-shm")
            };
            if std::fs::copy(&source.path, destination).is_err() {
                copy_failed = true;
                break;
            }
        }
        if !copy_failed && snapshot_sources(path)? == before {
            return Ok((snapshot, snapshot_path));
        }
        if attempt + 1 == MAX_ATTEMPTS {
            return Err(StoreError::Backend(
                "session store changed while creating a read-only snapshot".to_string(),
            ));
        }
    }
    unreachable!("snapshot attempt loop returns on success or exhaustion")
}

fn snapshot_sources(path: &Path) -> StoreResult<Vec<SnapshotSource>> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(path, suffix);
        if sidecar.exists() {
            paths.push(sidecar);
        }
    }
    paths
        .into_iter()
        .map(|path| {
            let metadata =
                std::fs::metadata(&path).map_err(|error| StoreError::Backend(error.to_string()))?;
            Ok(SnapshotSource {
                path,
                len: metadata.len(),
                modified: metadata.modified().ok(),
            })
        })
        .collect()
}

pub(crate) fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

/// Database and coordination paths owned by a live session store.
pub fn session_store_runtime_paths(database: &Path) -> Vec<PathBuf> {
    let mut paths = harn_sqlite::database_runtime_paths(database);
    if let Some(directory) = database.parent() {
        paths.push(crate::session_lease_directory(directory));
    }
    paths
}

fn initialize_session_schema(transaction: &Transaction<'_>) -> StoreResult<()> {
    let has_legacy_schema_version = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(map_sql)?;
    let previous_schema_version = if has_legacy_schema_version {
        transaction
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(map_sql)?
            .unwrap_or_default()
    } else {
        0
    };
    if previous_schema_version > SqliteSessionStore::SCHEMA_VERSION {
        return Err(StoreError::SchemaIncompatible {
            schema: SCHEMA_NAME.to_string(),
            stored: previous_schema_version,
            supported: SqliteSessionStore::SCHEMA_VERSION,
        });
    }

    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                  TEXT PRIMARY KEY,
                tenant_id           TEXT,
                persona             TEXT,
                parent_session_id   TEXT,
                title               TEXT,
                title_pinned        INTEGER NOT NULL DEFAULT 0,
                cwd                 TEXT,
                model               TEXT,
                session_type        TEXT,
                project_scope       TEXT,
                usage_input         INTEGER NOT NULL DEFAULT 0,
                usage_output        INTEGER NOT NULL DEFAULT 0,
                usage_cost_usd_micros INTEGER NOT NULL DEFAULT 0,
                created_at_ms       INTEGER NOT NULL,
                created_at          TEXT NOT NULL,
                updated_at_ms       INTEGER NOT NULL,
                updated_at          TEXT NOT NULL,
                status              TEXT NOT NULL,
                event_count         INTEGER NOT NULL DEFAULT 0,
                last_event_id       INTEGER,
                chain_root_hash     TEXT,
                closed_at_ms        INTEGER,
                closed_at           TEXT,
                soft_deleted_at_ms  INTEGER,
                ttl_seconds         INTEGER,
                tags_json           TEXT NOT NULL DEFAULT '[]',
                attributes_json     TEXT NOT NULL DEFAULT '{}',
                next_event_id       INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS sessions_tenant_created
                ON sessions(tenant_id, created_at_ms);
            CREATE INDEX IF NOT EXISTS sessions_status
                ON sessions(status);
            CREATE INDEX IF NOT EXISTS sessions_parent
                ON sessions(parent_session_id);
            CREATE TABLE IF NOT EXISTS session_events (
                session_id          TEXT NOT NULL,
                event_id            INTEGER NOT NULL,
                tenant_id           TEXT,
                parent_event_id     INTEGER,
                actor               TEXT,
                kind                TEXT NOT NULL,
                custom_kind         TEXT,
                payload_json        TEXT NOT NULL,
                tags_json           TEXT NOT NULL DEFAULT '[]',
                headers_json        TEXT NOT NULL DEFAULT '{}',
                ts_ms               INTEGER NOT NULL,
                ts                  TEXT NOT NULL,
                record_hash         TEXT NOT NULL,
                prev_hash           TEXT,
                signature_json      TEXT,
                PRIMARY KEY (session_id, event_id),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS session_events_ts
                ON session_events(session_id, ts_ms);
            CREATE VIRTUAL TABLE IF NOT EXISTS session_events_fts USING fts5(
                session_id UNINDEXED,
                event_id UNINDEXED,
                tenant_id UNINDEXED,
                project_scope UNINDEXED,
                text,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            CREATE TABLE IF NOT EXISTS session_event_vectors (
                session_id      TEXT NOT NULL,
                event_id        INTEGER NOT NULL,
                backend         TEXT NOT NULL,
                dim             INTEGER NOT NULL,
                embedding       BLOB NOT NULL,
                PRIMARY KEY (session_id, event_id),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS session_imports (
                source_id       TEXT PRIMARY KEY,
                source_digest   TEXT NOT NULL,
                session_id      TEXT NOT NULL,
                event_count     INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_task_sessions (
                id TEXT PRIMARY KEY,
                record_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_control_event_sequence (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                next_id INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO agent_control_event_sequence (singleton, next_id) VALUES (1, 1);
            CREATE TABLE IF NOT EXISTS agent_tasks (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                status TEXT NOT NULL,
                record_json TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES agent_task_sessions(id)
            );
            CREATE INDEX IF NOT EXISTS agent_tasks_session ON agent_tasks(session_id);
            CREATE INDEX IF NOT EXISTS agent_tasks_workspace ON agent_tasks(workspace_id);
            CREATE TABLE IF NOT EXISTS task_retry_bindings (
                binding_digest TEXT PRIMARY KEY,
                request_fingerprint TEXT NOT NULL,
                task_id TEXT NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                FOREIGN KEY (task_id) REFERENCES agent_tasks(id)
            );
            CREATE TABLE IF NOT EXISTS agent_task_events (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                record_json TEXT NOT NULL,
                FOREIGN KEY (task_id) REFERENCES agent_tasks(id)
            );
            CREATE INDEX IF NOT EXISTS agent_task_events_task ON agent_task_events(task_id, id);
            CREATE INDEX IF NOT EXISTS agent_task_events_session ON agent_task_events(session_id, id);
            CREATE TABLE IF NOT EXISTS agent_task_message_projections (
                session_id TEXT PRIMARY KEY,
                record_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS session_tags (
                session_id  TEXT NOT NULL,
                tag         TEXT NOT NULL,
                PRIMARY KEY (session_id, tag),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS session_tags_by_tag
                ON session_tags(tag, session_id);
            CREATE TABLE IF NOT EXISTS session_snapshots (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL,
                captured_at_ms  INTEGER NOT NULL,
                captured_at     TEXT NOT NULL,
                body_json       TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );",
        )
        .map_err(map_sql)?;
    ensure_session_column(transaction, "title", "TEXT")?;
    // Rows written before pinning existed carry no user choice, so 0 is the
    // correct reading of their history rather than a placeholder.
    ensure_session_column(transaction, "title_pinned", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_session_column(transaction, "cwd", "TEXT")?;
    ensure_session_column(transaction, "model", "TEXT")?;
    ensure_session_column(transaction, "session_type", "TEXT")?;
    ensure_session_column(transaction, "project_scope", "TEXT")?;
    ensure_session_column(transaction, "usage_input", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_session_column(transaction, "usage_output", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_session_column(
        transaction,
        "usage_cost_usd_micros",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    transaction
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS sessions_project_updated
                ON sessions(project_scope, updated_at_ms);",
        )
        .map_err(map_sql)?;
    if previous_schema_version < SqliteSessionStore::SCHEMA_VERSION {
        // Foreign-key enforcement is connection-local and does not repair
        // child rows orphaned by v1. This guarded cleanup runs once while
        // upgrading from a pre-foreign-key schema.
        transaction
            .execute_batch(
                "DELETE FROM session_events
                   WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE sessions.id = session_events.session_id);
                 DELETE FROM session_tags
                   WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE sessions.id = session_tags.session_id);
                 DELETE FROM session_snapshots
                   WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE sessions.id = session_snapshots.session_id);
                 DELETE FROM session_event_vectors
                   WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE sessions.id = session_event_vectors.session_id);",
            )
            .map_err(map_sql)?;
    }
    if has_legacy_schema_version {
        transaction
            .execute_batch("DROP TABLE schema_version;")
            .map_err(map_sql)?;
    }
    Ok(())
}

fn ensure_session_column(
    transaction: &Transaction<'_>,
    column: &str,
    sql_type: &str,
) -> StoreResult<()> {
    let exists = transaction
        .prepare("PRAGMA table_info(sessions)")
        .map_err(map_sql)?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(map_sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sql)?
        .iter()
        .any(|name| name == column);
    if !exists {
        transaction
            .execute_batch(&format!(
                "ALTER TABLE sessions ADD COLUMN {column} {sql_type};"
            ))
            .map_err(map_sql)?;
    }
    Ok(())
}

fn map_sql(error: rusqlite::Error) -> StoreError {
    let message = error.to_string();
    match sqlite_contention(&error) {
        Some(SqliteContention::Busy) => StoreError::Contention {
            kind: StoreContention::DatabaseBusy,
            message,
        },
        Some(SqliteContention::Locked) => StoreError::Contention {
            kind: StoreContention::DatabaseLocked,
            message,
        },
        None => StoreError::Backend(message),
    }
}

fn map_initialization(error: harn_sqlite::InitializationError<StoreError>) -> StoreError {
    match error {
        harn_sqlite::InitializationError::NewerSchemaVersion {
            name,
            stored,
            supported,
        } => StoreError::SchemaIncompatible {
            schema: name.to_string(),
            stored,
            supported,
        },
        harn_sqlite::InitializationError::Initialize(error) => error,
        other => StoreError::Backend(other.to_string()),
    }
}

fn write_transaction(conn: &mut Connection) -> StoreResult<Transaction<'_>> {
    // These operations read before they write. With SQLite's default DEFERRED
    // transaction, two writers can both acquire read locks and then form an
    // upgrade deadlock; SQLite intentionally skips the busy handler in that
    // case and returns SQLITE_BUSY immediately. Acquire writer ownership at
    // the boundary so the configured busy policy can serialize contenders.
    //
    // Only a live read-only handle already holds a transaction (its frozen
    // read view); it can never write.
    if !conn.is_autocommit() {
        return Err(StoreError::Backend(
            "attempt to write a readonly database".to_string(),
        ));
    }
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sql)
}

fn map_create_sql(error: rusqlite::Error, session_id: &str) -> StoreError {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation
    ) {
        StoreError::AlreadyExists(session_id.to_string())
    } else {
        map_sql(error)
    }
}

fn kind_to_sql(kind: &SessionEventKind) -> (String, Option<String>) {
    match kind {
        SessionEventKind::Custom { custom_type } => {
            ("custom".to_string(), Some(custom_type.clone()))
        }
        other => (other.discriminator().to_string(), None),
    }
}

fn kind_from_sql(kind: &str, custom_kind: Option<String>) -> StoreResult<SessionEventKind> {
    // `custom` is the one discriminator this backend owns, because it
    // needs the separately stored custom type to reconstruct. Every
    // named variant decodes through `SessionEventKind::from_discriminator`,
    // the inverse of the `discriminator()` used on the way in, so the two
    // directions cannot drift apart.
    if kind == "custom" {
        return Ok(SessionEventKind::Custom {
            custom_type: custom_kind.unwrap_or_default(),
        });
    }
    SessionEventKind::from_discriminator(kind)
        .ok_or_else(|| StoreError::Backend(format!("unknown event kind '{kind}' in storage")))
}

fn status_to_sql(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Open => "open",
        SessionStatus::Closed => "closed",
        SessionStatus::SoftDeleted => "soft_deleted",
        SessionStatus::HardDeleted => "hard_deleted",
    }
}

fn status_from_sql(value: &str) -> StoreResult<SessionStatus> {
    Ok(match value {
        "open" => SessionStatus::Open,
        "closed" => SessionStatus::Closed,
        "soft_deleted" => SessionStatus::SoftDeleted,
        "hard_deleted" => SessionStatus::HardDeleted,
        other => {
            return Err(StoreError::Backend(format!(
                "unknown session status '{other}' in storage"
            )))
        }
    })
}

fn session_type_to_sql(session_type: SessionType) -> &'static str {
    match session_type {
        SessionType::User => "user",
        SessionType::Subagent => "subagent",
        SessionType::Scheduled => "scheduled",
    }
}

fn session_type_from_sql(value: &str) -> StoreResult<SessionType> {
    match value {
        "user" => Ok(SessionType::User),
        "subagent" => Ok(SessionType::Subagent),
        "scheduled" => Ok(SessionType::Scheduled),
        other => Err(StoreError::Backend(format!(
            "unknown session type '{other}' in storage"
        ))),
    }
}

fn insert_session_tags(conn: &Connection, session_id: &str, tags: &[String]) -> StoreResult<()> {
    if tags.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare("INSERT OR IGNORE INTO session_tags (session_id, tag) VALUES (?1, ?2)")
        .map_err(map_sql)?;
    for tag in tags {
        stmt.execute(params![session_id, tag]).map_err(map_sql)?;
    }
    Ok(())
}

fn insert_session(
    conn: &Connection,
    meta: &SessionMeta,
    next_event_id: EventId,
) -> StoreResult<()> {
    let tags_json = serde_json::to_string(&meta.tags).unwrap_or_else(|_| "[]".into());
    let attrs_json = serde_json::to_string(&meta.attributes).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO sessions (
            id, tenant_id, persona, parent_session_id, title, cwd, model,
            session_type, project_scope, usage_input, usage_output,
            usage_cost_usd_micros, created_at_ms, created_at, updated_at_ms,
            updated_at, status, event_count, last_event_id, chain_root_hash,
            closed_at_ms, closed_at, soft_deleted_at_ms, ttl_seconds, tags_json,
            attributes_json, next_event_id, title_pinned
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
            ?28
         )",
        params![
            meta.id,
            meta.tenant_id,
            meta.persona,
            meta.parent_session_id,
            meta.title,
            meta.cwd,
            meta.model,
            meta.session_type.map(session_type_to_sql),
            meta.project_scope,
            meta.usage_input as i64,
            meta.usage_output as i64,
            meta.usage_cost_usd_micros as i64,
            meta.created_at_ms,
            meta.created_at,
            meta.updated_at_ms,
            meta.updated_at,
            status_to_sql(meta.status),
            meta.event_count as i64,
            meta.last_event_id.map(|value| value as i64),
            meta.chain_root_hash,
            meta.closed_at_ms,
            meta.closed_at,
            meta.soft_deleted_at_ms,
            meta.ttl_seconds.map(|value| value as i64),
            tags_json,
            attrs_json,
            next_event_id as i64,
            meta.title_pinned,
        ],
    )
    .map_err(|error| map_create_sql(error, &meta.id))?;
    insert_session_tags(conn, &meta.id, &meta.tags)?;
    Ok(())
}

pub(crate) fn read_session_meta(
    conn: &Connection,
    session_id: &str,
) -> StoreResult<(SessionMeta, EventId)> {
    let row = conn
        .prepare_cached(
            "SELECT tenant_id, persona, parent_session_id, title, cwd, model,
                    session_type, project_scope, usage_input, usage_output,
                    usage_cost_usd_micros, created_at_ms, created_at,
                    updated_at_ms, updated_at, status, event_count, last_event_id,
                    chain_root_hash, closed_at_ms, closed_at, soft_deleted_at_ms,
                    ttl_seconds, tags_json, attributes_json, next_event_id,
                    title_pinned
             FROM sessions WHERE id = ?1",
        )
        .map_err(map_sql)?
        .query_row(params![session_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, String>(15)?,
                row.get::<_, i64>(16)?,
                row.get::<_, Option<i64>>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<i64>>(19)?,
                row.get::<_, Option<String>>(20)?,
                row.get::<_, Option<i64>>(21)?,
                row.get::<_, Option<i64>>(22)?,
                row.get::<_, String>(23)?,
                row.get::<_, String>(24)?,
                row.get::<_, i64>(25)?,
                row.get::<_, bool>(26)?,
            ))
        })
        .optional()
        .map_err(map_sql)?
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
    let (
        tenant_id,
        persona,
        parent_session_id,
        title,
        cwd,
        model,
        session_type,
        project_scope,
        usage_input,
        usage_output,
        usage_cost_usd_micros,
        created_at_ms,
        created_at,
        updated_at_ms,
        updated_at,
        status,
        event_count,
        last_event_id,
        chain_root_hash,
        closed_at_ms,
        closed_at,
        soft_deleted_at_ms,
        ttl_seconds,
        tags_json,
        attrs_json,
        next_event_id,
        title_pinned,
    ) = row;
    let tags = serde_json::from_str(&tags_json).unwrap_or_default();
    let attributes = serde_json::from_str(&attrs_json).unwrap_or_default();
    let meta = SessionMeta {
        id: session_id.to_string(),
        tenant_id,
        persona,
        parent_session_id,
        title,
        title_pinned,
        cwd,
        model,
        session_type: session_type
            .as_deref()
            .map(session_type_from_sql)
            .transpose()?,
        project_scope,
        usage_input: usage_input as u64,
        usage_output: usage_output as u64,
        usage_cost_usd_micros: usage_cost_usd_micros as u64,
        created_at_ms,
        created_at,
        updated_at_ms,
        updated_at,
        status: status_from_sql(&status)?,
        event_count: event_count as usize,
        last_event_id: last_event_id.map(|value| value as EventId),
        chain_root_hash,
        closed_at_ms,
        closed_at,
        soft_deleted_at_ms,
        ttl_seconds: ttl_seconds.map(|value| value as u64),
        tags,
        attributes,
    };
    Ok((meta, next_event_id as EventId))
}

fn insert_event(conn: &Connection, event: &StoredEvent) -> StoreResult<()> {
    let (kind, custom_kind) = kind_to_sql(&event.kind);
    let payload_json = serde_json::to_string(&event.payload)
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    let tags_json = serde_json::to_string(&event.tags).unwrap_or_else(|_| "[]".into());
    let headers_json = serde_json::to_string(&event.headers).unwrap_or_else(|_| "{}".into());
    let signature_json = event
        .signed_by
        .as_ref()
        .map(|sig| serde_json::to_string(sig).unwrap_or_else(|_| "null".into()));
    conn.prepare_cached(
        "INSERT INTO session_events (
            session_id, event_id, tenant_id, parent_event_id, actor, kind, custom_kind,
            payload_json, tags_json, headers_json, ts_ms, ts, record_hash, prev_hash,
            signature_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )
    .map_err(map_sql)?
    .execute(params![
        event.session_id,
        event.event_id as i64,
        event.tenant_id,
        event.parent_event_id.map(|value| value as i64),
        event.actor,
        kind,
        custom_kind,
        payload_json,
        tags_json,
        headers_json,
        event.ts_ms,
        event.ts,
        event.record_hash,
        event.prev_hash,
        signature_json,
    ])
    .map_err(map_sql)?;
    Ok(())
}

fn insert_search_rows(
    conn: &Connection,
    hooks: &StoreHooks,
    meta: &SessionMeta,
    event: &StoredEvent,
) -> StoreResult<()> {
    let document = redacted_search_document(hooks.redaction.as_ref(), meta, event);
    conn.prepare_cached(
        "INSERT INTO session_events_fts (
            session_id, event_id, tenant_id, project_scope, text
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .map_err(map_sql)?
    .execute(params![
        event.session_id,
        event.event_id as i64,
        event.tenant_id,
        meta.project_scope,
        document,
    ])
    .map_err(map_sql)?;
    let vector = hooks.embedder.embed(&document);
    if vector.len() != hooks.embedder.dim() {
        return Err(StoreError::Backend(format!(
            "embedding backend '{}' returned dimension {}, expected {}",
            hooks.embedder.name(),
            vector.len(),
            hooks.embedder.dim()
        )));
    }
    conn.prepare_cached(
        "INSERT OR REPLACE INTO session_event_vectors (
            session_id, event_id, backend, dim, embedding
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .map_err(map_sql)?
    .execute(params![
        event.session_id,
        event.event_id as i64,
        hooks.embedder.name(),
        hooks.embedder.dim() as i64,
        vector_blob(&vector),
    ])
    .map_err(map_sql)?;
    Ok(())
}

fn rebuild_search_index_if_needed(conn: &mut Connection, hooks: &StoreHooks) -> StoreResult<()> {
    let event_count = conn
        .query_row("SELECT COUNT(*) FROM session_events", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(map_sql)?;
    let fts_count = conn
        .query_row("SELECT COUNT(*) FROM session_events_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(map_sql)?;
    let compatible_vector_count = conn
        .query_row(
            "SELECT COUNT(*) FROM session_event_vectors
             WHERE backend = ?1 AND dim = ?2",
            params![hooks.embedder.name(), hooks.embedder.dim() as i64],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sql)?;
    if event_count == fts_count && event_count == compatible_vector_count {
        return Ok(());
    }

    let session_ids = {
        let mut stmt = conn
            .prepare("SELECT id FROM sessions ORDER BY id ASC")
            .map_err(map_sql)?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sql)?;
        ids
    };
    let mut rows = Vec::new();
    for session_id in session_ids {
        let (meta, _) = read_session_meta(conn, &session_id)?;
        let mut events = load_all_events(conn, &session_id)?;
        redact_stored_events(hooks, &mut events)?;
        rows.extend(events.into_iter().map(|event| (meta.clone(), event)));
    }

    let tx = write_transaction(conn)?;
    tx.execute("DELETE FROM session_events_fts", [])
        .map_err(map_sql)?;
    tx.execute("DELETE FROM session_event_vectors", [])
        .map_err(map_sql)?;
    for (meta, event) in &rows {
        insert_search_rows(&tx, hooks, meta, event)?;
    }
    tx.commit().map_err(map_sql)
}

fn read_event(row: &rusqlite::Row) -> Result<StoredEvent, rusqlite::Error> {
    let session_id: String = row.get(0)?;
    let event_id: i64 = row.get(1)?;
    let tenant_id: Option<String> = row.get(2)?;
    let parent_event_id: Option<i64> = row.get(3)?;
    let actor: Option<String> = row.get(4)?;
    let kind: String = row.get(5)?;
    let custom_kind: Option<String> = row.get(6)?;
    let payload_json: String = row.get(7)?;
    let tags_json: String = row.get(8)?;
    let headers_json: String = row.get(9)?;
    let ts_ms: i64 = row.get(10)?;
    let ts: String = row.get(11)?;
    let record_hash: String = row.get(12)?;
    let prev_hash: Option<String> = row.get(13)?;
    let signature_json: Option<String> = row.get(14)?;
    let resolved_kind = kind_from_sql(&kind, custom_kind).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })?;
    let payload = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })?;
    let tags = serde_json::from_str(&tags_json).unwrap_or_default();
    let headers = serde_json::from_str(&headers_json).unwrap_or_default();
    let signed_by = signature_json
        .as_ref()
        .map(|value| serde_json::from_str::<EventSignature>(value))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
        })?;
    Ok(StoredEvent {
        event_id: event_id as EventId,
        session_id,
        tenant_id,
        parent_event_id: parent_event_id.map(|value| value as EventId),
        actor,
        kind: resolved_kind,
        payload,
        tags,
        headers,
        ts_ms,
        ts,
        record_hash,
        prev_hash,
        signed_by,
    })
}

fn load_all_events(conn: &Connection, session_id: &str) -> StoreResult<Vec<StoredEvent>> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, event_id, tenant_id, parent_event_id, actor, kind,
                    custom_kind, payload_json, tags_json, headers_json, ts_ms, ts,
                    record_hash, prev_hash, signature_json
             FROM session_events WHERE session_id = ?1 ORDER BY event_id ASC",
        )
        .map_err(map_sql)?;
    let rows = stmt
        .query_map(params![session_id], read_event)
        .map_err(map_sql)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_sql)?);
    }
    Ok(out)
}

fn read_import(conn: &Connection, source_id: &str) -> StoreResult<Option<ImportResult>> {
    conn.query_row(
        "SELECT source_digest, session_id, event_count
         FROM session_imports WHERE source_id = ?1",
        params![source_id],
        |row| {
            Ok(ImportResult {
                source_id: source_id.to_string(),
                source_digest: row.get(0)?,
                session_id: row.get(1)?,
                event_count: row.get::<_, i64>(2)? as usize,
                imported: false,
            })
        },
    )
    .optional()
    .map_err(map_sql)
}

/// Core append logic, operating on a caller-owned connection (typically a
/// transaction). Redacts, validates, links, signs (when an event signer
/// is configured), inserts the event, and advances the session counters —
/// but does **not** commit. `append` wraps this in its own transaction;
/// `close` reuses it so the receipt insert, its signature, and the status
/// flip all land in a single atomic transaction.
fn append_in_tx(
    conn: &Connection,
    hooks: &StoreHooks,
    session_id: &str,
    event: AppendEvent,
) -> StoreResult<StoredEvent> {
    let mut stored = append_events_in_tx(conn, hooks, session_id, vec![event])?;
    Ok(stored
        .pop()
        .expect("appending one event stores exactly one event"))
}

/// Append `events` in order inside the caller's transaction.
///
/// Each event is redacted, chained to its predecessor, hashed, signed, and
/// indexed exactly as a lone append would do it. The session row and chain
/// tip are read once and written once, because every event in the batch is
/// appended by this one writer inside one transaction.
fn append_events_in_tx(
    conn: &Connection,
    hooks: &StoreHooks,
    session_id: &str,
    mut events: Vec<AppendEvent>,
) -> StoreResult<Vec<StoredEvent>> {
    for event in &mut events {
        prepare_append_event(hooks, event)?;
    }
    let (mut meta, mut next_event_id) = read_session_meta(conn, session_id)?;
    super::memory_helpers::validate_open(&meta)?;
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let mut prev_hash: Option<String> = conn
        .prepare_cached(
            "SELECT record_hash FROM session_events
             WHERE session_id = ?1 ORDER BY event_id DESC LIMIT 1",
        )
        .and_then(|mut stmt| {
            stmt.query_row(params![session_id], |row| row.get(0))
                .optional()
        })
        .map_err(map_sql)?;
    let mut chain_root = meta.chain_root_hash.clone().unwrap_or_else(chain_root_init);
    let mut stored_events = Vec::with_capacity(events.len());
    for event in events {
        if let Some(parent_event_id) = event.parent_event_id {
            let exists: bool = conn
                .prepare_cached(
                    "SELECT 1 FROM session_events WHERE session_id = ?1 AND event_id = ?2",
                )
                .and_then(|mut stmt| {
                    stmt.query_row(params![session_id, parent_event_id as i64], |_| Ok(true))
                        .optional()
                })
                .map_err(map_sql)?
                .unwrap_or(false);
            if !exists {
                return Err(StoreError::InvalidInput(format!(
                    "parent_event_id {parent_event_id} not present in session"
                )));
            }
        }
        let (ts_ms, ts) = now_ms_and_rfc3339();
        let mut stored = StoredEvent {
            event_id: next_event_id,
            session_id: session_id.to_string(),
            tenant_id: meta.tenant_id.clone(),
            parent_event_id: event.parent_event_id,
            actor: event.actor,
            kind: event.kind,
            payload: event.payload,
            tags: event.tags,
            headers: event.headers,
            ts_ms,
            ts: ts.clone(),
            record_hash: String::new(),
            prev_hash: prev_hash.take(),
            signed_by: None,
        };
        stored.bind_canonical_origin(session_id);
        stored.record_hash = compute_record_hash(&stored);
        if let Some(signer) = hooks.event_signer.as_ref() {
            stored.signed_by = Some(signer.sign_event(&stored));
        }
        insert_event(conn, &stored)?;
        insert_search_rows(conn, hooks, &meta, &stored)?;
        chain_root = chain_root_fold(&chain_root, &stored.record_hash);
        meta.event_count = meta.event_count.saturating_add(1);
        meta.last_event_id = Some(next_event_id);
        meta.updated_at_ms = ts_ms;
        meta.updated_at = ts;
        prev_hash = Some(stored.record_hash.clone());
        next_event_id += 1;
        stored_events.push(stored);
    }
    meta.chain_root_hash = Some(chain_root);
    conn.prepare_cached(
        "UPDATE sessions SET event_count = ?1, last_event_id = ?2,
                              chain_root_hash = ?3, updated_at_ms = ?4,
                              updated_at = ?5, next_event_id = ?6 WHERE id = ?7",
    )
    .and_then(|mut stmt| {
        stmt.execute(params![
            meta.event_count as i64,
            meta.last_event_id.map(|value| value as i64),
            meta.chain_root_hash,
            meta.updated_at_ms,
            meta.updated_at,
            next_event_id as i64,
            session_id,
        ])
    })
    .map_err(map_sql)?;
    Ok(stored_events)
}

#[path = "sqlite/operations.rs"]
mod operations;
mod tasks;

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
