//! SQLite-backed [`SessionStore`].
//!
//! Single-file durable backend suitable for self-hosted deployments and
//! the TUI's persistent session DB. Schema versioning is intentionally
//! minimal — one `schema_version` table; future migrations bump the
//! version and run guarded ALTERs. The Postgres backend (issue #2500)
//! follows the same shape so consumers can swap by config.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use super::event::{
    now_ms_and_rfc3339, AppendEvent, EventId, EventSignature, SessionEventKind, StoredEvent,
};
use super::signing::{
    chain_root_fold, chain_root_hash, chain_root_init, compute_record_hash, re_anchor_events,
    verify_event,
};
use super::store::{
    CreateSession, EventPage, ForkResult, ListFilter, ReadRange, SessionId, SessionMeta,
    SessionStatus, SessionStore, Snapshot, SnapshotId, StoreError, StoreHooks, StoreResult,
    TruncateResult, VerifyFailure, VerifyReport, MAX_READ_BATCH,
};

const SCHEMA_VERSION: i64 = 1;

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

    fn initialize(conn: Connection, path: PathBuf, hooks: StoreHooks) -> StoreResult<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id                  TEXT PRIMARY KEY,
                tenant_id           TEXT,
                persona             TEXT,
                parent_session_id   TEXT,
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
            );
            ",
        )
        .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_version(version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )
        .map_err(|error| StoreError::Backend(error.to_string()))?;
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

fn map_sql(error: rusqlite::Error) -> StoreError {
    StoreError::Backend(error.to_string())
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
            id, tenant_id, persona, parent_session_id, created_at_ms, created_at,
            updated_at_ms, updated_at, status, event_count, last_event_id,
            chain_root_hash, closed_at_ms, closed_at, soft_deleted_at_ms,
            ttl_seconds, tags_json, attributes_json, next_event_id
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19
         )",
        params![
            meta.id,
            meta.tenant_id,
            meta.persona,
            meta.parent_session_id,
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
    .map_err(map_sql)?;
    insert_session_tags(conn, &meta.id, &meta.tags)?;
    Ok(())
}

fn read_session_meta(conn: &Connection, session_id: &str) -> StoreResult<(SessionMeta, EventId)> {
    let row = conn
        .query_row(
            "SELECT tenant_id, persona, parent_session_id, created_at_ms, created_at,
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
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, Option<i64>>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, i64>(17)?,
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

fn apply_redaction(hooks: &StoreHooks, event: &mut AppendEvent) {
    let Some(policy) = hooks.redaction.as_ref() else {
        return;
    };
    policy.redact_json_in_place(&mut event.payload);
    event.headers = policy.redact_headers(&event.headers);
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    fn hooks(&self) -> &StoreHooks {
        &self.hooks
    }

    async fn create(&self, request: CreateSession) -> StoreResult<SessionMeta> {
        let meta = super::memory_helpers::meta_for_create(request);
        let conn = self.lock();
        if conn
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
        insert_session(&conn, &meta, 1)?;
        Ok(meta)
    }

    async fn describe(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
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
        let mut event = event;
        apply_redaction(&self.hooks, &mut event);
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_sql)?;
        let (mut meta, next_event_id) = read_session_meta(&tx, session_id)?;
        super::memory_helpers::validate_open(&meta)?;
        if let Some(parent_event_id) = event.parent_event_id {
            let exists: bool = tx
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
        let prev_hash: Option<String> = tx
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
        if let Some(signer) = self.hooks.event_signer.as_ref() {
            stored.signed_by = Some(signer.sign_event(&stored));
        }
        insert_event(&tx, &stored)?;
        let prev_root = meta.chain_root_hash.clone().unwrap_or_else(chain_root_init);
        let chain_root = chain_root_fold(&prev_root, &stored.record_hash);
        meta.event_count = meta.event_count.saturating_add(1);
        meta.last_event_id = Some(next_event_id);
        meta.chain_root_hash = Some(chain_root);
        meta.updated_at_ms = ts_ms;
        meta.updated_at = ts;
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
                (next_event_id + 1) as i64,
                session_id,
            ],
        )
        .map_err(map_sql)?;
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
        if let Some(policy) = self.hooks.redaction.as_ref() {
            for event in events.iter_mut() {
                policy.redact_json_in_place(&mut event.payload);
            }
        }
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
        let tx = conn.transaction().map_err(map_sql)?;
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
        let inherited: Vec<StoredEvent> = parent_events
            .into_iter()
            .filter(|event| event.event_id <= at_event_id)
            .collect();
        let copied = re_anchor_events(&inherited, &new_id);
        child_meta.event_count = copied.len();
        child_meta.last_event_id = copied.last().map(|tail| tail.event_id);
        child_meta.chain_root_hash = Some(chain_root_hash(&copied));
        let next_event_id = copied.last().map(|tail| tail.event_id + 1).unwrap_or(1);
        insert_session(&tx, &child_meta, next_event_id)?;
        for event in &copied {
            insert_event(&tx, event)?;
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
        let tx = conn.transaction().map_err(map_sql)?;
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
        let events = load_all_events(&conn, session_id)?;
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
        serde_json::from_str(&body).map_err(|error| StoreError::Backend(error.to_string()))
    }

    async fn close(&self, session_id: &str) -> StoreResult<StoredEvent> {
        let (record_root, last_event_id) = {
            let conn = self.lock();
            let (meta, _) = read_session_meta(&conn, session_id)?;
            super::memory_helpers::validate_open(&meta)?;
            let events = load_all_events(&conn, session_id)?;
            (
                meta.chain_root_hash
                    .clone()
                    .unwrap_or_else(|| chain_root_hash(&events)),
                meta.last_event_id.unwrap_or(0),
            )
        };
        let payload =
            super::signing::canonical_receipt_payload(session_id, last_event_id, &record_root);
        let mut append = AppendEvent::new(SessionEventKind::Receipt, payload);
        append.actor = Some("session_store".into());
        let mut stored = self.append(session_id, append).await?;
        let signature = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
            .map(|signer| signer.sign_receipt(&record_root));
        let (ms, text) = now_ms_and_rfc3339();
        let conn = self.lock();
        if let Some(ref signature) = signature {
            stored.signed_by = Some(signature.clone());
            let signature_json = serde_json::to_string(signature).unwrap_or_else(|_| "null".into());
            conn.execute(
                "UPDATE session_events SET signature_json = ?1
                 WHERE session_id = ?2 AND event_id = ?3",
                params![signature_json, session_id, stored.event_id as i64],
            )
            .map_err(map_sql)?;
        }
        conn.execute(
            "UPDATE sessions SET status = ?1, closed_at_ms = ?2, closed_at = ?3,
                                  updated_at_ms = ?2, updated_at = ?3 WHERE id = ?4",
            params![status_to_sql(SessionStatus::Closed), ms, text, session_id,],
        )
        .map_err(map_sql)?;
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
        let conn = self.lock();
        let removed = conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(map_sql)?;
        if removed == 0 {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        Ok(())
    }

    async fn verify(&self, session_id: &str) -> StoreResult<VerifyReport> {
        let conn = self.lock();
        let events = load_all_events(&conn, session_id)?;
        let chain_root = chain_root_hash(&events);
        let mut signed = 0usize;
        let mut failures = Vec::new();
        let verifier = self
            .hooks
            .event_signer
            .as_ref()
            .map(|signer| signer.verifying_key());
        for event in &events {
            let recomputed = compute_record_hash(event);
            if recomputed != event.record_hash {
                failures.push(VerifyFailure {
                    event_id: event.event_id,
                    reason: format!(
                        "record_hash mismatch: stored '{stored}' vs computed '{recomputed}'",
                        stored = event.record_hash
                    ),
                });
                continue;
            }
            if let Some(verifying_key) = verifier.as_ref() {
                if event.signed_by.is_some() {
                    match verify_event(event, verifying_key) {
                        Ok(()) => signed += 1,
                        Err(error) => failures.push(VerifyFailure {
                            event_id: event.event_id,
                            reason: error.to_string(),
                        }),
                    }
                }
            } else if event.signed_by.is_some() {
                signed += 1;
            }
        }
        Ok(VerifyReport {
            session_id: session_id.to_string(),
            chain_root_hash: chain_root,
            event_count: events.len(),
            signed_event_count: signed,
            failures,
        })
    }
}
