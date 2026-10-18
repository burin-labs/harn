use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::runtime_sqlite::{
    initialize_runtime_sqlite, require_runtime_sqlite_initialized, RuntimeSqliteSchema,
    DEFAULT_BUSY_TIMEOUT,
};

use super::util::{
    event_id_to_sqlite_i64, now_ms, prepare_event_after, sqlite_i64_to_event_id,
    sqlite_i64_to_event_id_for_row, sqlite_i64_to_usize, sqlite_json_bytes_for_row,
    sqlite_size_bytes, stream_from_broadcast, BroadcastMap,
};
use super::{
    require_expected_topic_head, AppendHeadExpectation, AppendOutcome, CompactReport, ConsumerId,
    EventId, EventLog, EventLogBackendKind, EventLogDescription, LogError, LogEvent, LogEventBytes,
    Topic,
};

const SQLITE_SCHEMA: RuntimeSqliteSchema = RuntimeSqliteSchema::new(
    "event_log",
    1,
    "CREATE TABLE IF NOT EXISTS topic_heads (
        topic TEXT PRIMARY KEY,
        last_id INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS events (
        topic TEXT NOT NULL,
        event_id INTEGER NOT NULL,
        kind TEXT NOT NULL,
        payload BLOB NOT NULL,
        headers TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL,
        PRIMARY KEY (topic, event_id)
    );
    CREATE TABLE IF NOT EXISTS consumers (
        topic TEXT NOT NULL,
        consumer_id TEXT NOT NULL,
        cursor INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (topic, consumer_id)
    );
    CREATE TABLE IF NOT EXISTS event_idempotency_keys (
        topic TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        event_id INTEGER NOT NULL,
        PRIMARY KEY (topic, key, value),
        FOREIGN KEY (topic, event_id) REFERENCES events(topic, event_id)
    );",
);

pub struct SqliteEventLog {
    path: PathBuf,
    pub(super) connection: Mutex<Connection>,
    pub(super) broadcasts: BroadcastMap,
    pub(super) queue_depth: usize,
}

impl SqliteEventLog {
    pub fn open(path: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        Self::open_inner(path, queue_depth, DEFAULT_BUSY_TIMEOUT)
    }

    #[cfg(test)]
    pub(crate) fn open_with_timeout(
        path: PathBuf,
        queue_depth: usize,
        busy_timeout: Duration,
    ) -> Result<Self, LogError> {
        Self::open_inner(path, queue_depth, busy_timeout)
    }

    fn open_inner(
        path: PathBuf,
        queue_depth: usize,
        busy_timeout: Duration,
    ) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        }
        let connection = Connection::open(&path).map_err(sqlite_err("open error"))?;
        initialize_runtime_sqlite(&connection, busy_timeout, &SQLITE_SCHEMA)
            .map_err(sqlite_err("sqlite setup error"))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }

    pub fn open_read_only(path: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_err("read-only open error"))?;
        require_runtime_sqlite_initialized(&connection, DEFAULT_BUSY_TIMEOUT, &SQLITE_SCHEMA)
            .map_err(sqlite_err("read-only setup error"))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }

    fn connection(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .expect("sqlite event log connection poisoned")
    }

    /// Events on `topic` after `from`, oldest first, decoded by `decode`.
    fn query_range<T>(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
        decode: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, LogError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id > ?2
                 ORDER BY event_id ASC
                 LIMIT ?3",
            )
            .map_err(sqlite_err("prepare error"))?;
        let from_sql = event_id_to_sqlite_i64(from.unwrap_or(0))?;
        let rows = statement
            .query_map(params![topic.as_str(), from_sql, limit as i64], decode)
            .map_err(sqlite_err("query error"))?;
        rows.map(|row| row.map_err(sqlite_err("row error")))
            .collect()
    }

    pub(super) fn topics(&self) -> Result<Vec<Topic>, LogError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT DISTINCT topic FROM events ORDER BY topic ASC")
            .map_err(sqlite_err("topics prepare error"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_err("topics query error"))?;
        let mut topics = Vec::new();
        for row in rows {
            topics.push(Topic::new(row.map_err(sqlite_err("topic row error"))?)?);
        }
        Ok(topics)
    }

    pub(super) fn append_idempotent(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
        expectation: AppendHeadExpectation<'_>,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        let mut connection = self.connection();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sqlite_err("transaction error"))?;

        let existing = tx
            .query_row(
                IDEMPOTENT_LOOKUP_SQL,
                params![topic.as_str(), header, value],
                decode_id_event_row,
            )
            .optional()
            .map_err(sqlite_err("idempotency read error"))?;

        if let Some((event_id, event)) = existing {
            return Ok(AppendOutcome {
                event_id,
                event,
                inserted: false,
            });
        }

        let previous = tx
            .query_row(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1
                 ORDER BY event_id DESC
                 LIMIT 1",
                params![topic.as_str()],
                decode_id_event_row,
            )
            .optional()
            .map_err(sqlite_err("previous read error"))?;
        require_expected_topic_head(
            topic,
            previous
                .as_ref()
                .map(|(previous_id, previous_event)| (*previous_id, previous_event)),
            expectation,
        )?;

        tx.execute(
            "INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES (?1, 0)",
            params![topic.as_str()],
        )
        .map_err(sqlite_err("head init error"))?;
        tx.execute(
            "UPDATE topic_heads SET last_id = last_id + 1 WHERE topic = ?1",
            params![topic.as_str()],
        )
        .map_err(sqlite_err("head update error"))?;
        let event_id = tx
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_err("head read error"))
            .and_then(sqlite_i64_to_event_id)?;
        let event_id_sql = event_id_to_sqlite_i64(event_id)?;
        let event = prepare_event_after(
            topic,
            event_id,
            previous
                .as_ref()
                .map(|(previous_id, previous_event)| (*previous_id, previous_event)),
            event,
        )?;
        tx.execute(
            "INSERT INTO events(topic, event_id, kind, payload, headers, occurred_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                topic.as_str(),
                event_id_sql,
                event.kind,
                serde_json::to_vec(&event.payload).map_err(|error| LogError::Serde(format!(
                    "event log payload encode error: {error}"
                )))?,
                serde_json::to_string(&event.headers).map_err(|error| LogError::Serde(format!(
                    "event log headers encode error: {error}"
                )))?,
                event.occurred_at_ms
            ],
        )
        .map_err(sqlite_err("insert error"))?;
        tx.execute(
            "INSERT INTO event_idempotency_keys(topic, key, value, event_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![topic.as_str(), header, value, event_id_sql],
        )
        .map_err(sqlite_err("idempotency insert error"))?;
        tx.commit().map_err(sqlite_err("commit error"))?;
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event.clone()));
        Ok(AppendOutcome {
            event_id,
            event,
            inserted: true,
        })
    }

    /// Look up the event previously appended under `(header, value)` via the
    /// indexed `event_idempotency_keys` JOIN — the read counterpart of
    /// [`Self::append_idempotent`]. O(log N) on the index instead of
    /// scanning the topic, which is what makes durable-step replay cheap.
    pub(super) fn read_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
    ) -> Result<Option<(EventId, LogEvent)>, LogError> {
        let connection = self.connection();
        connection
            .query_row(
                IDEMPOTENT_LOOKUP_SQL,
                params![topic.as_str(), header, value],
                decode_id_event_row,
            )
            .optional()
            .map_err(sqlite_err("idempotency read error"))
    }
}

/// The `(topic, header, value) -> event` JOIN against the idempotency index,
/// shared by the append-time existence check and the read counterpart.
const IDEMPOTENT_LOOKUP_SQL: &str =
    "SELECT e.event_id, e.kind, e.payload, e.headers, e.occurred_at_ms
     FROM event_idempotency_keys k
     JOIN events e ON e.topic = k.topic AND e.event_id = k.event_id
     WHERE k.topic = ?1 AND k.key = ?2 AND k.value = ?3";

/// Decode a `(event_id, kind, payload, headers, occurred_at_ms)` row (in that
/// column order) into an `(EventId, LogEventBytes)`, leaving the payload as raw
/// JSON bytes. Every event read in this backend goes through here so the
/// row-shape decode lives in exactly one place.
fn decode_id_event_bytes_row(row: &rusqlite::Row) -> rusqlite::Result<(EventId, LogEventBytes)> {
    let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
    let headers: String = row.get(3)?;
    Ok((
        sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?,
        LogEventBytes {
            kind: row.get(1)?,
            payload: Bytes::from(payload),
            headers: serde_json::from_str(&headers).map_err(conversion_failure(
                headers.len(),
                rusqlite::types::Type::Text,
            ))?,
            occurred_at_ms: row.get(4)?,
        },
    ))
}

/// [`decode_id_event_bytes_row`] with the payload parsed into JSON.
fn decode_id_event_row(row: &rusqlite::Row) -> rusqlite::Result<(EventId, LogEvent)> {
    let (event_id, event) = decode_id_event_bytes_row(row)?;
    let payload = serde_json::from_slice(&event.payload).map_err(conversion_failure(
        event.payload.len(),
        rusqlite::types::Type::Blob,
    ))?;
    Ok((
        event_id,
        LogEvent {
            kind: event.kind,
            payload,
            headers: event.headers,
            occurred_at_ms: event.occurred_at_ms,
        },
    ))
}

fn conversion_failure(
    len: usize,
    column_type: rusqlite::types::Type,
) -> impl FnOnce(serde_json::Error) -> rusqlite::Error {
    move |error| rusqlite::Error::FromSqlConversionFailure(len, column_type, Box::new(error))
}

/// `map_err` adapter producing `LogError::Sqlite("event log {context}: {error}")`.
fn sqlite_err<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> LogError {
    move |error| LogError::Sqlite(format!("event log {context}: {error}"))
}

/// The chain hash of the newest event before `before` on `topic`.
///
/// Appended rows carry their record hash in a header, so only the headers are
/// read. A row written without one falls back to decoding the whole event and
/// hashing it, exactly as before.
fn previous_record_hash(
    tx: &rusqlite::Transaction<'_>,
    topic: &Topic,
    before: i64,
) -> Result<Option<String>, LogError> {
    let previous = tx
        .prepare_cached(
            "SELECT event_id, headers FROM events
             WHERE topic = ?1 AND event_id < ?2
             ORDER BY event_id DESC
             LIMIT 1",
        )
        .and_then(|mut statement| {
            statement
                .query_row(params![topic.as_str(), before], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .optional()
        })
        .map_err(sqlite_err("previous read error"))?;
    let Some((previous_id, headers)) = previous else {
        return Ok(None);
    };
    let headers: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&headers).map_err(sqlite_err("previous read error"))?;
    if let Some(hash) = headers
        .get(crate::provenance::HEADER_RECORD_HASH)
        .filter(|hash| !hash.trim().is_empty())
    {
        return Ok(Some(hash.clone()));
    }
    let (previous_id, previous_event) = tx
        .query_row(
            "SELECT event_id, kind, payload, headers, occurred_at_ms
             FROM events WHERE topic = ?1 AND event_id = ?2",
            params![topic.as_str(), previous_id],
            decode_id_event_row,
        )
        .map_err(sqlite_err("previous read error"))?;
    crate::provenance::event_record_hash_from_headers(topic.as_str(), previous_id, &previous_event)
        .map(Some)
}

impl EventLog for SqliteEventLog {
    fn describe(&self) -> EventLogDescription {
        EventLogDescription {
            backend: EventLogBackendKind::Sqlite,
            location: Some(self.path.clone()),
            size_bytes: Some(sqlite_size_bytes(&self.path)),
            queue_depth: self.queue_depth,
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        let mut connection = self.connection();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sqlite_err("transaction error"))?;
        // Every append runs the same five statements; reuse their compiled
        // forms instead of re-parsing the SQL on each event.
        tx.prepare_cached("INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES (?1, 0)")
            .and_then(|mut statement| statement.execute(params![topic.as_str()]))
            .map_err(sqlite_err("head init error"))?;
        tx.prepare_cached("UPDATE topic_heads SET last_id = last_id + 1 WHERE topic = ?1")
            .and_then(|mut statement| statement.execute(params![topic.as_str()]))
            .map_err(sqlite_err("head update error"))?;
        let event_id = tx
            .prepare_cached("SELECT last_id FROM topic_heads WHERE topic = ?1")
            .and_then(|mut statement| {
                statement.query_row(params![topic.as_str()], |row| row.get::<_, i64>(0))
            })
            .map_err(sqlite_err("head read error"))
            .and_then(sqlite_i64_to_event_id)?;
        let event_id_sql = event_id_to_sqlite_i64(event_id)?;
        let previous_hash = previous_record_hash(&tx, topic, event_id_sql)?;
        let event = crate::provenance::prepare_event_for_append(
            topic.as_str(),
            event_id,
            previous_hash,
            event,
        )?;
        let payload = serde_json::to_vec(&event.payload)
            .map_err(|error| LogError::Serde(format!("event log payload encode error: {error}")))?;
        let headers = serde_json::to_string(&event.headers)
            .map_err(|error| LogError::Serde(format!("event log headers encode error: {error}")))?;
        tx.prepare_cached(
            "INSERT INTO events(topic, event_id, kind, payload, headers, occurred_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .and_then(|mut statement| {
            statement.execute(params![
                topic.as_str(),
                event_id_sql,
                event.kind,
                payload,
                headers,
                event.occurred_at_ms
            ])
        })
        .map_err(sqlite_err("insert error"))?;
        tx.commit().map_err(sqlite_err("commit error"))?;
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event));
        Ok(event_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        let connection = self.connection();
        connection
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .map_err(sqlite_err("checkpoint error"))?;
        Ok(())
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        self.query_range(topic, from, limit, decode_id_event_row)
    }

    async fn read_range_bytes(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEventBytes)>, LogError> {
        self.query_range(topic, from, limit, decode_id_event_bytes_row)
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        let history = self.read_range(topic, from, usize::MAX).await?;
        Ok(stream_from_broadcast(history, from, rx, self.queue_depth))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        let connection = self.connection();
        let up_to_sql = event_id_to_sqlite_i64(up_to)?;
        connection
            .execute(
                "INSERT INTO consumers(topic, consumer_id, cursor, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(topic, consumer_id)
                 DO UPDATE SET cursor = excluded.cursor, updated_at_ms = excluded.updated_at_ms",
                params![topic.as_str(), consumer.as_str(), up_to_sql, now_ms()],
            )
            .map_err(sqlite_err("ack error"))?;
        Ok(())
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT cursor FROM consumers WHERE topic = ?1 AND consumer_id = ?2",
                params![topic.as_str(), consumer.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(sqlite_err("consumer cursor error"))
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(sqlite_err("latest error"))
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let connection = self.connection();
        let before_sql = event_id_to_sqlite_i64(before)?;
        connection
            .execute(
                "DELETE FROM event_idempotency_keys WHERE topic = ?1 AND event_id <= ?2",
                params![topic.as_str(), before_sql],
            )
            .map_err(sqlite_err("idempotency compact error"))?;
        let removed = connection
            .execute(
                "DELETE FROM events WHERE topic = ?1 AND event_id <= ?2",
                params![topic.as_str(), before_sql],
            )
            .map_err(sqlite_err("compact delete error"))?;
        let remaining = connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_err("compact count error"))
            .and_then(sqlite_i64_to_usize)?;
        let latest = connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(sqlite_err("latest error"))?;
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(sqlite_err("checkpoint error"))?;
        Ok(CompactReport {
            removed,
            remaining,
            latest,
            checkpointed: true,
        })
    }
}
