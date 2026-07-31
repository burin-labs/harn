//! SQLite-backed [`SessionStore`].
//!
//! Single-file durable backend suitable for self-hosted deployments and
//! the TUI's persistent session DB. Schema versioning is intentionally
//! minimal. File-backed databases use the shared Harn SQLite schema marker;
//! pre-marker databases are upgraded from the original `schema_version` table
//! in the same initialization transaction. The Postgres backend (issue #2500)
//! follows the same shape so consumers can swap by config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harn_sqlite::{
    initialize_file, initialize_transient, sqlite_contention, SchemaVersion, SqliteContention,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use uuid::Uuid;

use super::event::{
    now_ms_and_rfc3339, AppendEvent, EventId, EventSignature, SessionEventKind, StoredEvent,
};
use super::redaction::{
    prepare_append_event, prepare_stored_events_for_persistence, redact_stored_events,
};
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
    CreateSession, EventPage, ForkResult, ImportResult, ImportSession, ListFilter, ReadRange,
    SessionId, SessionImporter, SessionMeta, SessionStatus, SessionStore, SessionType, Snapshot,
    SnapshotId, StoreContention, StoreError, StoreHooks, StoreResult, TruncateResult,
    UpdateSession, VerifyReport, MAX_READ_BATCH,
};

const SCHEMA_VERSION: i64 = 4;
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_SCHEMA: SchemaVersion = SchemaVersion::new("session_store", SCHEMA_VERSION);

#[derive(Clone)]
pub struct SqliteSessionStore {
    conn: Arc<Mutex<Connection>>,
    hooks: Arc<StoreHooks>,
    path: PathBuf,
}

impl SqliteSessionStore {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_with_hooks(path, StoreHooks::default())
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        let conn =
            Connection::open_in_memory().map_err(|error| StoreError::Backend(error.to_string()))?;
        Self::initialize(conn, PathBuf::from(":memory:"), StoreHooks::default())
    }

    pub fn open_with_hooks(path: impl AsRef<Path>, hooks: StoreHooks) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| StoreError::Backend(error.to_string()))?;
            }
        }
        let conn =
            Connection::open(&path).map_err(|error| StoreError::Backend(error.to_string()))?;
        Self::initialize(conn, path, hooks)
    }

    fn initialize(mut conn: Connection, path: PathBuf, hooks: StoreHooks) -> StoreResult<Self> {
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
        initialization.map_err(|error| StoreError::Backend(error.to_string()))?;
        rebuild_search_index_if_needed(&mut conn, &hooks)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            hooks: Arc::new(hooks),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
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
    if previous_schema_version > SCHEMA_VERSION {
        return Err(StoreError::Backend(format!(
            "session store schema version {previous_schema_version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }

    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                  TEXT PRIMARY KEY,
                tenant_id           TEXT,
                persona             TEXT,
                parent_session_id   TEXT,
                title               TEXT,
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
    if previous_schema_version < SCHEMA_VERSION {
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

fn write_transaction(conn: &mut Connection) -> StoreResult<Transaction<'_>> {
    // These operations read before they write. With SQLite's default DEFERRED
    // transaction, two writers can both acquire read locks and then form an
    // upgrade deadlock; SQLite intentionally skips the busy handler in that
    // case and returns SQLITE_BUSY immediately. Acquire writer ownership at
    // the boundary so the configured busy policy can serialize contenders.
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
    Ok(match kind {
        "message" => SessionEventKind::Message,
        "tool_call" => SessionEventKind::ToolCall,
        "tool_result" => SessionEventKind::ToolResult,
        "plan" => SessionEventKind::Plan,
        "compaction" => SessionEventKind::Compaction,
        "system_reminder" => SessionEventKind::SystemReminder,
        "hypothesis" => SessionEventKind::Hypothesis,
        "receipt" => SessionEventKind::Receipt,
        "reminder" => SessionEventKind::Reminder,
        "permission_decision" => SessionEventKind::PermissionDecision,
        "custom" => SessionEventKind::Custom {
            custom_type: custom_kind.unwrap_or_default(),
        },
        other => {
            return Err(StoreError::Backend(format!(
                "unknown event kind '{other}' in storage"
            )))
        }
    })
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
            attributes_json, next_event_id
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
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
        ],
    )
    .map_err(|error| map_create_sql(error, &meta.id))?;
    insert_session_tags(conn, &meta.id, &meta.tags)?;
    Ok(())
}

fn read_session_meta(conn: &Connection, session_id: &str) -> StoreResult<(SessionMeta, EventId)> {
    let row = conn
        .query_row(
            "SELECT tenant_id, persona, parent_session_id, title, cwd, model,
                    session_type, project_scope, usage_input, usage_output,
                    usage_cost_usd_micros, created_at_ms, created_at,
                    updated_at_ms, updated_at, status, event_count, last_event_id,
                    chain_root_hash, closed_at_ms, closed_at, soft_deleted_at_ms,
                    ttl_seconds, tags_json, attributes_json, next_event_id
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
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
                ))
            },
        )
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
    ) = row;
    let tags = serde_json::from_str(&tags_json).unwrap_or_default();
    let attributes = serde_json::from_str(&attrs_json).unwrap_or_default();
    let meta = SessionMeta {
        id: session_id.to_string(),
        tenant_id,
        persona,
        parent_session_id,
        title,
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
    conn.execute(
        "INSERT INTO session_events (
            session_id, event_id, tenant_id, parent_event_id, actor, kind, custom_kind,
            payload_json, tags_json, headers_json, ts_ms, ts, record_hash, prev_hash,
            signature_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
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
        ],
    )
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
    conn.execute(
        "INSERT INTO session_events_fts (
            session_id, event_id, tenant_id, project_scope, text
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.session_id,
            event.event_id as i64,
            event.tenant_id,
            meta.project_scope,
            document,
        ],
    )
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
    conn.execute(
        "INSERT OR REPLACE INTO session_event_vectors (
            session_id, event_id, backend, dim, embedding
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.session_id,
            event.event_id as i64,
            hooks.embedder.name(),
            hooks.embedder.dim() as i64,
            vector_blob(&vector),
        ],
    )
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
    mut event: AppendEvent,
) -> StoreResult<StoredEvent> {
    prepare_append_event(hooks, &mut event)?;
    let (mut meta, next_event_id) = read_session_meta(conn, session_id)?;
    super::memory_helpers::validate_open(&meta)?;
    if let Some(parent_event_id) = event.parent_event_id {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM session_events WHERE session_id = ?1 AND event_id = ?2",
                params![session_id, parent_event_id as i64],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sql)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::InvalidInput(format!(
                "parent_event_id {parent_event_id} not present in session"
            )));
        }
    }
    let prev_hash: Option<String> = conn
        .query_row(
            "SELECT record_hash FROM session_events
             WHERE session_id = ?1 ORDER BY event_id DESC LIMIT 1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sql)?;
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
        prev_hash,
        signed_by: None,
    };
    stored.record_hash = compute_record_hash(&stored);
    if let Some(signer) = hooks.event_signer.as_ref() {
        stored.signed_by = Some(signer.sign_event(&stored));
    }
    insert_event(conn, &stored)?;
    insert_search_rows(conn, hooks, &meta, &stored)?;
    let prev_root = meta.chain_root_hash.clone().unwrap_or_else(chain_root_init);
    let chain_root = chain_root_fold(&prev_root, &stored.record_hash);
    meta.event_count = meta.event_count.saturating_add(1);
    meta.last_event_id = Some(next_event_id);
    meta.chain_root_hash = Some(chain_root);
    meta.updated_at_ms = ts_ms;
    meta.updated_at = ts;
    conn.execute(
        "UPDATE sessions SET event_count = ?1, last_event_id = ?2,
                              chain_root_hash = ?3, updated_at_ms = ?4,
                              updated_at = ?5, next_event_id = ?6 WHERE id = ?7",
        params![
            meta.event_count as i64,
            meta.last_event_id.map(|value| value as i64),
            meta.chain_root_hash,
            meta.updated_at_ms,
            meta.updated_at,
            (next_event_id + 1) as i64,
            session_id,
        ],
    )
    .map_err(map_sql)?;
    Ok(stored)
}

#[async_trait]
impl SessionImporter for SqliteSessionStore {
    async fn import(&self, request: ImportSession) -> StoreResult<ImportResult> {
        request.validate()?;
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        if let Some(existing) = read_import(&tx, &request.source_id)? {
            if existing.source_digest != request.source_digest {
                return Err(StoreError::Conflict(format!(
                    "import source '{}' changed digest",
                    request.source_id
                )));
            }
            return Ok(existing);
        }

        let meta = super::memory_helpers::meta_for_create(request.session);
        if tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![meta.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(map_sql)?
            .is_some()
        {
            return Err(StoreError::AlreadyExists(meta.id));
        }
        insert_session(&tx, &meta, 1)?;
        let event_count = request.events.len();
        for event in request.events {
            append_in_tx(&tx, &self.hooks, &meta.id, event)?;
        }
        tx.execute(
            "INSERT INTO session_imports (source_id, source_digest, session_id, event_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                request.source_id,
                request.source_digest,
                meta.id,
                event_count as i64
            ],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(ImportResult {
            source_id: request.source_id,
            source_digest: request.source_digest,
            session_id: meta.id,
            event_count,
            imported: true,
        })
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    fn hooks(&self) -> &StoreHooks {
        &self.hooks
    }

    async fn create(&self, request: CreateSession) -> StoreResult<SessionMeta> {
        let meta = super::memory_helpers::meta_for_create(request);
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        insert_session(&tx, &meta, 1)?;
        tx.commit().map_err(map_sql)?;
        Ok(meta)
    }

    async fn describe(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        Ok(meta)
    }

    async fn update(&self, session_id: &str, request: UpdateSession) -> StoreResult<SessionMeta> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (updated_at_ms, updated_at) = now_ms_and_rfc3339();
        let changed = tx
            .execute(
                "UPDATE sessions SET
                    title = COALESCE(?1, title),
                    cwd = COALESCE(?2, cwd),
                    model = COALESCE(?3, model),
                    parent_session_id = COALESCE(?4, parent_session_id),
                    session_type = COALESCE(?5, session_type),
                    project_scope = COALESCE(?6, project_scope),
                    usage_input = COALESCE(?7, usage_input),
                    usage_output = COALESCE(?8, usage_output),
                    usage_cost_usd_micros = COALESCE(?9, usage_cost_usd_micros),
                    updated_at_ms = ?10,
                    updated_at = ?11
                 WHERE id = ?12",
                params![
                    request.title,
                    request.cwd,
                    request.model,
                    request.parent_session_id,
                    request.session_type.map(session_type_to_sql),
                    request.project_scope,
                    request.usage_input.map(|value| value as i64),
                    request.usage_output.map(|value| value as i64),
                    request.usage_cost_usd_micros.map(|value| value as i64),
                    updated_at_ms,
                    updated_at,
                    session_id,
                ],
            )
            .map_err(map_sql)?;
        if changed == 0 {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        let (meta, _) = read_session_meta(&tx, session_id)?;
        let mut events = load_all_events(&tx, session_id)?;
        redact_stored_events(&self.hooks, &mut events)?;
        tx.execute(
            "DELETE FROM session_events_fts WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_event_vectors WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        for event in &events {
            insert_search_rows(&tx, &self.hooks, &meta, event)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(meta)
    }

    async fn list(&self, filter: ListFilter) -> StoreResult<Vec<SessionMeta>> {
        let conn = self.lock();
        let limit = filter.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH) as i64;
        // Pull the cursor's anchor row up front so the SQL can do
        // keyset pagination on `(created_at_ms, id)` instead of scanning
        // every prior row into memory.
        let cursor_anchor: Option<(i64, String)> = filter
            .cursor
            .as_ref()
            .map(|id| {
                conn.query_row(
                    "SELECT created_at_ms, id FROM sessions WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sql)
            })
            .transpose()?
            .flatten();

        let mut sql = String::from("SELECT s.id FROM sessions s");
        if filter.tag.is_some() {
            sql.push_str(" INNER JOIN session_tags t ON t.session_id = s.id AND t.tag = :tag");
        }
        sql.push_str(" WHERE 1=1");
        let mut args: Vec<(&'static str, rusqlite::types::Value)> = Vec::new();
        if let Some(tag) = filter.tag {
            args.push((":tag", tag.into()));
        }
        if let Some(tenant) = filter.tenant_id {
            sql.push_str(" AND s.tenant_id = :tenant");
            args.push((":tenant", tenant.into()));
        }
        if let Some(persona) = filter.persona {
            sql.push_str(" AND s.persona = :persona");
            args.push((":persona", persona.into()));
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND s.status = :status");
            args.push((":status", status_to_sql(status).to_string().into()));
        }
        if let Some(parent_session_id) = filter.parent_session_id {
            sql.push_str(" AND s.parent_session_id = :parent_session_id");
            args.push((":parent_session_id", parent_session_id.into()));
        }
        if let Some(session_type) = filter.session_type {
            sql.push_str(" AND s.session_type = :session_type");
            args.push((
                ":session_type",
                session_type_to_sql(session_type).to_string().into(),
            ));
        }
        if let Some(project_scope) = filter.project_scope {
            sql.push_str(" AND s.project_scope = :project_scope");
            args.push((":project_scope", project_scope.into()));
        }
        if let Some(after) = filter.created_after_ms {
            sql.push_str(" AND s.created_at_ms >= :after");
            args.push((":after", after.into()));
        }
        if let Some(before) = filter.created_before_ms {
            sql.push_str(" AND s.created_at_ms <= :before");
            args.push((":before", before.into()));
        }
        if let Some((anchor_ms, anchor_id)) = cursor_anchor {
            sql.push_str(
                " AND (s.created_at_ms > :cursor_ms OR (s.created_at_ms = :cursor_ms AND s.id > :cursor_id))",
            );
            args.push((":cursor_ms", anchor_ms.into()));
            args.push((":cursor_id", anchor_id.into()));
        }
        sql.push_str(" ORDER BY s.created_at_ms ASC, s.id ASC LIMIT :limit");
        args.push((":limit", limit.into()));

        let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
            .iter()
            .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
            .collect();
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
        let ids: Vec<String> = stmt
            .query_map(named_args.as_slice(), |row| row.get(0))
            .map_err(map_sql)?
            .collect::<Result<_, _>>()
            .map_err(map_sql)?;
        let mut metas = Vec::with_capacity(ids.len());
        for id in ids {
            let (meta, _) = read_session_meta(&conn, &id)?;
            metas.push(meta);
        }
        Ok(metas)
    }

    async fn append(&self, session_id: &str, event: AppendEvent) -> StoreResult<StoredEvent> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let stored = append_in_tx(&tx, &self.hooks, session_id, event)?;
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    async fn read(&self, session_id: &str, range: ReadRange) -> StoreResult<EventPage> {
        let conn = self.lock();
        let from = range.from_event_id.unwrap_or(1) as i64;
        // SQLite stores event_id as INTEGER (signed i64); use i64::MAX as
        // the unbounded upper sentinel rather than casting EventId::MAX,
        // which silently wraps to -1.
        let to = range
            .to_event_id
            .map(|value| value as i64)
            .unwrap_or(i64::MAX);
        let limit = range.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH) as i64;
        let mut stmt = conn
            .prepare(
                "SELECT session_id, event_id, tenant_id, parent_event_id, actor, kind,
                        custom_kind, payload_json, tags_json, headers_json, ts_ms, ts,
                        record_hash, prev_hash, signature_json
                 FROM session_events
                 WHERE session_id = ?1 AND event_id >= ?2 AND event_id <= ?3
                 ORDER BY event_id ASC LIMIT ?4",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map(params![session_id, from, to, limit], read_event)
            .map_err(map_sql)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(map_sql)?);
        }
        redact_stored_events(&self.hooks, &mut events)?;
        let next_cursor = if events.len() as i64 == limit {
            events.last().map(|tail| tail.event_id + 1)
        } else {
            None
        };
        Ok(EventPage {
            events,
            next_cursor,
        })
    }

    async fn fork(
        &self,
        session_id: &str,
        at_event_id: EventId,
        child_id: Option<SessionId>,
    ) -> StoreResult<ForkResult> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (parent_meta, _) = read_session_meta(&tx, session_id)?;
        let parent_events = load_all_events(&tx, session_id)?;
        if !parent_events
            .iter()
            .any(|event| event.event_id == at_event_id)
        {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let new_id = child_id.unwrap_or_else(|| Uuid::now_v7().to_string());
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![new_id],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sql)?
            .unwrap_or(false);
        if exists {
            return Err(StoreError::AlreadyExists(new_id));
        }
        let (ms, text) = now_ms_and_rfc3339();
        let mut child_meta = parent_meta.clone();
        child_meta.id = new_id.clone();
        child_meta.parent_session_id = Some(parent_meta.id);
        child_meta.created_at_ms = ms;
        child_meta.created_at = text.clone();
        child_meta.updated_at_ms = ms;
        child_meta.updated_at = text;
        child_meta.status = SessionStatus::Open;
        child_meta.closed_at_ms = None;
        child_meta.closed_at = None;
        child_meta.soft_deleted_at_ms = None;
        let mut inherited: Vec<StoredEvent> = parent_events
            .into_iter()
            .filter(|event| event.event_id <= at_event_id)
            .collect();
        prepare_stored_events_for_persistence(&self.hooks, &mut inherited)?;
        let copied = re_anchor_events(&inherited, &new_id);
        child_meta.event_count = copied.len();
        child_meta.last_event_id = copied.last().map(|tail| tail.event_id);
        child_meta.chain_root_hash = Some(chain_root_hash(&copied));
        let next_event_id = copied.last().map(|tail| tail.event_id + 1).unwrap_or(1);
        insert_session(&tx, &child_meta, next_event_id)?;
        for event in &copied {
            insert_event(&tx, event)?;
            insert_search_rows(&tx, &self.hooks, &child_meta, event)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(ForkResult {
            child_session_id: new_id,
            forked_from_event_id: at_event_id,
            copied_event_count: copied.len(),
        })
    }

    async fn truncate(
        &self,
        session_id: &str,
        at_event_id: EventId,
    ) -> StoreResult<TruncateResult> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (mut meta, _) = read_session_meta(&tx, session_id)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM session_events WHERE session_id = ?1 AND event_id = ?2",
                params![session_id, at_event_id as i64],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sql)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let removed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM session_events
                 WHERE session_id = ?1 AND event_id > ?2",
                params![session_id, at_event_id as i64],
                |row| row.get(0),
            )
            .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_events WHERE session_id = ?1 AND event_id > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_events_fts
             WHERE session_id = ?1 AND CAST(event_id AS INTEGER) > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_event_vectors
             WHERE session_id = ?1 AND event_id > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        let remaining_hashes: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT record_hash FROM session_events
                     WHERE session_id = ?1 ORDER BY event_id ASC",
                )
                .map_err(map_sql)?;
            let rows = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))
                .map_err(map_sql)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_sql)?);
            }
            out
        };
        let new_root = remaining_hashes
            .iter()
            .fold(chain_root_init(), |root, hash| chain_root_fold(&root, hash));
        let (ms, text) = now_ms_and_rfc3339();
        meta.event_count = remaining_hashes.len();
        meta.last_event_id = Some(at_event_id);
        meta.chain_root_hash = Some(new_root);
        meta.updated_at_ms = ms;
        meta.updated_at = text;
        tx.execute(
            "UPDATE sessions SET event_count = ?1, last_event_id = ?2,
                                  chain_root_hash = ?3, updated_at_ms = ?4,
                                  updated_at = ?5, next_event_id = ?6 WHERE id = ?7",
            params![
                meta.event_count as i64,
                meta.last_event_id.map(|value| value as i64),
                meta.chain_root_hash,
                meta.updated_at_ms,
                meta.updated_at,
                (at_event_id + 1) as i64,
                session_id,
            ],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(TruncateResult {
            kept_event_count: meta.event_count,
            removed_event_count: removed as usize,
            new_tip_event_id: meta.last_event_id,
        })
    }

    async fn snapshot(&self, session_id: &str) -> StoreResult<Snapshot> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        let mut events = load_all_events(&conn, session_id)?;
        redact_stored_events(&self.hooks, &mut events)?;
        let (ms, text) = now_ms_and_rfc3339();
        let snapshot = Snapshot {
            id: SnapshotId(format!("snap-{}", Uuid::now_v7())),
            session: meta,
            events,
            captured_at_ms: ms,
            captured_at: text,
        };
        let body = serde_json::to_string(&snapshot)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.execute(
            "INSERT INTO session_snapshots (id, session_id, captured_at_ms, captured_at, body_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                snapshot.id.0,
                snapshot.session.id,
                snapshot.captured_at_ms,
                snapshot.captured_at,
                body,
            ],
        )
        .map_err(map_sql)?;
        Ok(snapshot)
    }

    async fn replay(&self, snapshot_id: &SnapshotId) -> StoreResult<Snapshot> {
        let conn = self.lock();
        let body: Option<String> = conn
            .query_row(
                "SELECT body_json FROM session_snapshots WHERE id = ?1",
                params![snapshot_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        let body = body.ok_or_else(|| StoreError::NotFound(snapshot_id.0.clone()))?;
        let mut snapshot: Snapshot =
            serde_json::from_str(&body).map_err(|error| StoreError::Backend(error.to_string()))?;
        redact_stored_events(&self.hooks, &mut snapshot.events)?;
        Ok(snapshot)
    }

    async fn close(&self, session_id: &str) -> StoreResult<StoredEvent> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        // Read the pre-receipt chain root inside the transaction so the
        // root we sign is exactly the chain the receipt finalises, with
        // no window for a concurrent append to move the tip.
        let (meta, _) = read_session_meta(&tx, session_id)?;
        super::memory_helpers::validate_open(&meta)?;
        let record_root = match meta.chain_root_hash.clone() {
            Some(root) => root,
            None => chain_root_hash(&load_all_events(&tx, session_id)?),
        };
        let last_event_id = meta.last_event_id.unwrap_or(0);
        let payload =
            super::signing::canonical_receipt_payload(session_id, last_event_id, &record_root);
        let mut append = AppendEvent::new(SessionEventKind::Receipt, payload);
        append.actor = Some("session_store".into());
        let mut stored = append_in_tx(&tx, &self.hooks, session_id, append)?;
        // Intentionally replace the receipt's append-time per-event
        // signature with a receipt-root signature. The receipt's purpose
        // is to attest the chain root, so `verify()` special-cases it via
        // `verify_receipt_root` against the pre-receipt root rather than
        // the receipt event's own canonical bytes.
        if let Some(signer) = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
        {
            let signature = signer.sign_receipt(&record_root);
            let signature_json =
                serde_json::to_string(&signature).unwrap_or_else(|_| "null".into());
            tx.execute(
                "UPDATE session_events SET signature_json = ?1
                 WHERE session_id = ?2 AND event_id = ?3",
                params![signature_json, session_id, stored.event_id as i64],
            )
            .map_err(map_sql)?;
            stored.signed_by = Some(signature);
        }
        let (ms, text) = now_ms_and_rfc3339();
        tx.execute(
            "UPDATE sessions SET status = ?1, closed_at_ms = ?2, closed_at = ?3,
                                  updated_at_ms = ?2, updated_at = ?3 WHERE id = ?4",
            params![status_to_sql(SessionStatus::Closed), ms, text, session_id,],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    async fn soft_delete(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let conn = self.lock();
        let (mut meta, _) = read_session_meta(&conn, session_id)?;
        match meta.status {
            SessionStatus::HardDeleted => return Err(StoreError::NotFound(session_id.to_string())),
            SessionStatus::SoftDeleted => return Ok(meta),
            _ => {}
        }
        let (ms, text) = now_ms_and_rfc3339();
        conn.execute(
            "UPDATE sessions SET status = ?1, soft_deleted_at_ms = ?2,
                                  updated_at_ms = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                status_to_sql(SessionStatus::SoftDeleted),
                ms,
                text,
                session_id,
            ],
        )
        .map_err(map_sql)?;
        meta.status = SessionStatus::SoftDeleted;
        meta.soft_deleted_at_ms = Some(ms);
        meta.updated_at_ms = ms;
        meta.updated_at = text;
        Ok(meta)
    }

    async fn hard_delete(&self, session_id: &str) -> StoreResult<()> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        tx.execute(
            "DELETE FROM session_events_fts WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        let removed = tx
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(map_sql)?;
        if removed == 0 {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        tx.commit().map_err(map_sql)?;
        Ok(())
    }

    async fn verify(&self, session_id: &str) -> StoreResult<VerifyReport> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        let events = load_all_events(&conn, session_id)?;
        let event_verifier = self
            .hooks
            .event_signer
            .as_ref()
            .map(|signer| signer.verifying_key());
        let receipt_verifier = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
            .map(|signer| signer.verifying_key());
        Ok(verify_session_chain(
            &meta,
            &events,
            event_verifier.as_ref(),
            receipt_verifier.as_ref(),
        ))
    }

    async fn search(&self, query: SearchQuery) -> StoreResult<SearchResponse> {
        query.validate().map_err(StoreError::InvalidInput)?;
        let conn = self.lock();
        let embedder = self.hooks.embedder.clone();
        let semantic_available = embedder.is_semantic();
        let effective_mode = if semantic_available {
            query.mode
        } else {
            SearchMode::Fts
        };

        let literal_query = fts_literal_query(&query.query);
        let mut fts_scores = BTreeMap::new();
        if !literal_query.is_empty() {
            let mut sql = String::from(
                "SELECT f.session_id, CAST(f.event_id AS INTEGER),
                        -bm25(session_events_fts)
                 FROM session_events_fts f
                 INNER JOIN sessions s ON s.id = f.session_id
                 WHERE session_events_fts MATCH :match
                   AND s.status NOT IN ('soft_deleted', 'hard_deleted')",
            );
            let mut args: Vec<(&'static str, rusqlite::types::Value)> =
                vec![(":match", literal_query.clone().into())];
            append_search_scope(&mut sql, &mut args, &query);
            let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
                .iter()
                .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
                .collect();
            let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
            let rows = stmt
                .query_map(named_args.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as EventId,
                        row.get::<_, f64>(2)? as f32,
                    ))
                })
                .map_err(map_sql)?;
            for row in rows {
                let (session_id, event_id, score) = row.map_err(map_sql)?;
                fts_scores.insert((session_id, event_id), score.max(f32::MIN_POSITIVE));
            }
        }

        let mut sql = String::from(
            "SELECT e.session_id, e.event_id, e.tenant_id, e.parent_event_id,
                    e.actor, e.kind, e.custom_kind, e.payload_json, e.tags_json,
                    e.headers_json, e.ts_ms, e.ts, e.record_hash, e.prev_hash,
                    e.signature_json, s.title, s.cwd, s.model, s.project_scope,
                    v.backend, v.dim, v.embedding
             FROM session_events e
             INNER JOIN sessions s ON s.id = e.session_id
             LEFT JOIN session_event_vectors v
               ON v.session_id = e.session_id AND v.event_id = e.event_id
             WHERE s.status NOT IN ('soft_deleted', 'hard_deleted')",
        );
        let mut args: Vec<(&'static str, rusqlite::types::Value)> = Vec::new();
        append_search_scope(&mut sql, &mut args, &query);
        if effective_mode == SearchMode::Fts {
            if literal_query.is_empty() {
                sql.push_str(" AND 1 = 0");
            } else {
                sql.push_str(
                    " AND EXISTS (
                        SELECT 1 FROM session_events_fts matched
                        WHERE matched.session_id = e.session_id
                          AND matched.event_id = e.event_id
                          AND session_events_fts MATCH :candidate_match
                    )",
                );
                args.push((":candidate_match", literal_query.into()));
            }
        }
        sql.push_str(" ORDER BY e.session_id ASC, e.event_id ASC");
        let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
            .iter()
            .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
            .collect();
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
        let rows = stmt
            .query_map(named_args.as_slice(), |row| {
                Ok((
                    read_event(row)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, Option<String>>(19)?,
                    row.get::<_, Option<i64>>(20)?,
                    row.get::<_, Option<Vec<u8>>>(21)?,
                ))
            })
            .map_err(map_sql)?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row.map_err(map_sql)?);
        }
        drop(stmt);
        drop(conn);

        let mut redacted = candidates
            .iter()
            .map(|(event, ..)| event.clone())
            .collect::<Vec<_>>();
        redact_stored_events(&self.hooks, &mut redacted)?;
        for ((event, ..), redacted_event) in candidates.iter_mut().zip(redacted) {
            *event = redacted_event;
        }
        let documents = candidates
            .iter()
            .map(|(event, title, cwd, model, project_scope, ..)| {
                redacted_search_document_parts(
                    self.hooks.redaction.as_ref(),
                    title.as_deref(),
                    cwd.as_deref(),
                    model.as_deref(),
                    project_scope.as_deref(),
                    event,
                )
            })
            .collect::<Vec<_>>();
        let aligned_fts_scores = candidates
            .iter()
            .map(|(event, ..)| {
                fts_scores
                    .get(&(event.session_id.clone(), event.event_id))
                    .copied()
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let semantic_scores = if semantic_available {
            let query_vector = embedder.embed(&query.query);
            candidates
                .iter()
                .enumerate()
                .map(|(index, (_, _, _, _, _, backend, dim, blob))| {
                    let stored = backend
                        .as_deref()
                        .filter(|backend| *backend == embedder.name())
                        .zip(dim.and_then(|dim| usize::try_from(dim).ok()))
                        .filter(|(_, dim)| *dim == embedder.dim())
                        .zip(blob.as_deref())
                        .and_then(|((_, dim), blob)| vector_from_blob(blob, dim));
                    let vector =
                        stored.unwrap_or_else(|| embedder.embed(documents[index].as_str()));
                    super::search::cosine(&query_vector, &vector).max(0.0)
                })
                .collect::<Vec<_>>()
        } else {
            vec![0.0; candidates.len()]
        };

        let fts_ranks = ranks(&aligned_fts_scores);
        let semantic_ranks = ranks(&semantic_scores);
        let mut hits = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(index, (event, ..))| {
                let fts_rank = fts_ranks.get(&index).copied();
                let semantic_rank = semantic_ranks.get(&index).copied();
                let fts_score =
                    (aligned_fts_scores[index] > 0.0).then_some(aligned_fts_scores[index]);
                let semantic_score =
                    (semantic_scores[index] > 0.0).then_some(semantic_scores[index]);
                let included = match effective_mode {
                    SearchMode::Fts => fts_rank.is_some(),
                    SearchMode::Semantic => semantic_rank.is_some(),
                    SearchMode::Hybrid => fts_rank.is_some() || semantic_rank.is_some(),
                };
                included.then(|| SearchHit {
                    session_id: event.session_id.clone(),
                    event_id: event.event_id,
                    kind: event.kind.clone(),
                    score: combined_score(
                        effective_mode,
                        fts_rank,
                        semantic_rank,
                        fts_score,
                        semantic_score,
                    ),
                    fts_score,
                    semantic_score,
                    snippet: snippet(&documents[index], &query.query, 240),
                    event,
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        hits.truncate(query.limit());
        let semantic_floor = !semantic_available;
        Ok(SearchResponse {
            requested_mode: query.mode,
            effective_mode,
            embedding_backend: embedder.name().to_string(),
            semantic_floor,
            fallback_reason: (semantic_floor && query.mode != SearchMode::Fts)
                .then(|| "semantic model unavailable; FTS-only fallback active".into()),
            hits,
        })
    }
}

fn append_search_scope(
    sql: &mut String,
    args: &mut Vec<(&'static str, rusqlite::types::Value)>,
    query: &SearchQuery,
) {
    if let Some(tenant_id) = query.filter.tenant_id.as_ref() {
        sql.push_str(" AND s.tenant_id = :search_tenant");
        args.push((":search_tenant", tenant_id.clone().into()));
    }
    if let Some(project_scope) = query.filter.project_scope.as_ref() {
        sql.push_str(" AND s.project_scope = :search_project");
        args.push((":search_project", project_scope.clone().into()));
    }
    if let Some(session_id) = query.filter.session_id.as_ref() {
        sql.push_str(" AND s.id = :search_session");
        args.push((":search_session", session_id.clone().into()));
    }
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
