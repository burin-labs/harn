use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::stream::BoxStream;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::util::{
    event_id_to_sqlite_i64, now_ms, prepare_event_after, sqlite_i64_to_event_id,
    sqlite_i64_to_event_id_for_row, sqlite_i64_to_usize, sqlite_json_bytes_for_row,
    sqlite_size_bytes, stream_from_broadcast, BroadcastMap,
};
use super::{
    AppendOutcome, CompactReport, ConsumerId, EventId, EventLog, EventLogBackendKind,
    EventLogDescription, LogError, LogEvent, LogEventBytes, Topic,
};

pub struct SqliteEventLog {
    path: PathBuf,
    pub(super) connection: Mutex<Connection>,
    pub(super) broadcasts: BroadcastMap,
    pub(super) queue_depth: usize,
}

impl SqliteEventLog {
    pub fn open(path: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        }
        let connection = Connection::open(&path)
            .map_err(|error| LogError::Sqlite(format!("event log open error: {error}")))?;
        // Set busy_timeout BEFORE the WAL pragma so SQLite waits out transient
        // SQLITE_BUSY from a previous test's connection that hasn't finished
        // dropping yet (parallel `cargo test` on the same process, distinct
        // paths, still contends on SQLite's own global mutex under WAL-mode
        // promotion). Without this, `journal_mode = WAL` fails fast with
        // "database is locked" instead of retrying.
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| LogError::Sqlite(format!("event log busy-timeout error: {error}")))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| LogError::Sqlite(format!("event log WAL pragma error: {error}")))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| LogError::Sqlite(format!("event log sync pragma error: {error}")))?;
        connection
            .execute_batch(
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
            )
            .map_err(|error| LogError::Sqlite(format!("event log schema error: {error}")))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }

    pub fn open_read_only(path: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| {
                LogError::Sqlite(format!("event log read-only open error: {error}"))
            })?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| LogError::Sqlite(format!("event log busy-timeout error: {error}")))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }

    pub(super) fn topics(&self) -> Result<Vec<Topic>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let mut statement = connection
            .prepare("SELECT DISTINCT topic FROM events ORDER BY topic ASC")
            .map_err(|error| {
                LogError::Sqlite(format!("event log topics prepare error: {error}"))
            })?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| LogError::Sqlite(format!("event log topics query error: {error}")))?;
        let mut topics = Vec::new();
        for row in rows {
            topics.push(Topic::new(row.map_err(|error| {
                LogError::Sqlite(format!("event log topic row error: {error}"))
            })?)?);
        }
        Ok(topics)
    }

    pub(super) fn append_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        let mut connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| LogError::Sqlite(format!("event log transaction error: {error}")))?;

        let existing = tx
            .query_row(
                "SELECT e.event_id, e.kind, e.payload, e.headers, e.occurred_at_ms
                 FROM event_idempotency_keys k
                 JOIN events e ON e.topic = k.topic AND e.event_id = k.event_id
                 WHERE k.topic = ?1 AND k.key = ?2 AND k.value = ?3",
                params![topic.as_str(), header, value],
                |row| {
                    let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
                    let headers: String = row.get(3)?;
                    Ok((
                        sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?,
                        LogEvent {
                            kind: row.get(1)?,
                            payload: serde_json::from_slice(&payload).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    payload.len(),
                                    rusqlite::types::Type::Blob,
                                    Box::new(error),
                                )
                            })?,
                            headers: serde_json::from_str(&headers).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    headers.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            occurred_at_ms: row.get(4)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|error| {
                LogError::Sqlite(format!("event log idempotency read error: {error}"))
            })?;

        if let Some((event_id, event)) = existing {
            return Ok(AppendOutcome {
                event_id,
                event,
                inserted: false,
            });
        }

        tx.execute(
            "INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES (?1, 0)",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head init error: {error}")))?;
        tx.execute(
            "UPDATE topic_heads SET last_id = last_id + 1 WHERE topic = ?1",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head update error: {error}")))?;
        let event_id = tx
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| LogError::Sqlite(format!("event log head read error: {error}")))
            .and_then(sqlite_i64_to_event_id)?;
        let event_id_sql = event_id_to_sqlite_i64(event_id)?;
        let previous = tx
            .query_row(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id < ?2
                 ORDER BY event_id DESC
                 LIMIT 1",
                params![topic.as_str(), event_id_sql],
                |row| {
                    let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
                    let headers: String = row.get(3)?;
                    Ok((
                        sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?,
                        LogEvent {
                            kind: row.get(1)?,
                            payload: serde_json::from_slice(&payload).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    payload.len(),
                                    rusqlite::types::Type::Blob,
                                    Box::new(error),
                                )
                            })?,
                            headers: serde_json::from_str(&headers).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    headers.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            occurred_at_ms: row.get(4)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log previous read error: {error}")))?;
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
        .map_err(|error| LogError::Sqlite(format!("event log insert error: {error}")))?;
        tx.execute(
            "INSERT INTO event_idempotency_keys(topic, key, value, event_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![topic.as_str(), header, value, event_id_sql],
        )
        .map_err(|error| {
            LogError::Sqlite(format!("event log idempotency insert error: {error}"))
        })?;
        tx.commit()
            .map_err(|error| LogError::Sqlite(format!("event log commit error: {error}")))?;
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event.clone()));
        Ok(AppendOutcome {
            event_id,
            event,
            inserted: true,
        })
    }
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
        let mut connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| LogError::Sqlite(format!("event log transaction error: {error}")))?;
        tx.execute(
            "INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES (?1, 0)",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head init error: {error}")))?;
        tx.execute(
            "UPDATE topic_heads SET last_id = last_id + 1 WHERE topic = ?1",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head update error: {error}")))?;
        let event_id = tx
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| LogError::Sqlite(format!("event log head read error: {error}")))
            .and_then(sqlite_i64_to_event_id)?;
        let event_id_sql = event_id_to_sqlite_i64(event_id)?;
        let previous = tx
            .query_row(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id < ?2
                 ORDER BY event_id DESC
                 LIMIT 1",
                params![topic.as_str(), event_id_sql],
                |row| {
                    let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
                    let headers: String = row.get(3)?;
                    Ok((
                        sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?,
                        LogEvent {
                            kind: row.get(1)?,
                            payload: serde_json::from_slice(&payload).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    payload.len(),
                                    rusqlite::types::Type::Blob,
                                    Box::new(error),
                                )
                            })?,
                            headers: serde_json::from_str(&headers).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    headers.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            occurred_at_ms: row.get(4)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log previous read error: {error}")))?;
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
        .map_err(|error| LogError::Sqlite(format!("event log insert error: {error}")))?;
        tx.commit()
            .map_err(|error| LogError::Sqlite(format!("event log commit error: {error}")))?;
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event));
        Ok(event_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .map_err(|error| LogError::Sqlite(format!("event log checkpoint error: {error}")))?;
        Ok(())
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id > ?2
                 ORDER BY event_id ASC
                 LIMIT ?3",
            )
            .map_err(|error| LogError::Sqlite(format!("event log prepare error: {error}")))?;
        let from_sql = event_id_to_sqlite_i64(from.unwrap_or(0))?;
        let rows = statement
            .query_map(params![topic.as_str(), from_sql, limit as i64], |row| {
                let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
                let headers: String = row.get(3)?;
                let event_id = sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?;
                Ok((
                    event_id,
                    LogEvent {
                        kind: row.get(1)?,
                        payload: serde_json::from_slice(&payload).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                payload.len(),
                                rusqlite::types::Type::Blob,
                                Box::new(error),
                            )
                        })?,
                        headers: serde_json::from_str(&headers).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                headers.len(),
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        occurred_at_ms: row.get(4)?,
                    },
                ))
            })
            .map_err(|error| LogError::Sqlite(format!("event log query error: {error}")))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(
                row.map_err(|error| LogError::Sqlite(format!("event log row error: {error}")))?,
            );
        }
        Ok(events)
    }

    async fn read_range_bytes(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEventBytes)>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id > ?2
                 ORDER BY event_id ASC
                 LIMIT ?3",
            )
            .map_err(|error| LogError::Sqlite(format!("event log prepare error: {error}")))?;
        let from_sql = event_id_to_sqlite_i64(from.unwrap_or(0))?;
        let rows = statement
            .query_map(params![topic.as_str(), from_sql, limit as i64], |row| {
                let payload = sqlite_json_bytes_for_row(row, 2, "payload")?;
                let headers: String = row.get(3)?;
                let event_id = sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?)?;
                Ok((
                    event_id,
                    LogEventBytes {
                        kind: row.get(1)?,
                        payload: Bytes::from(payload),
                        headers: serde_json::from_str(&headers).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                headers.len(),
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        occurred_at_ms: row.get(4)?,
                    },
                ))
            })
            .map_err(|error| LogError::Sqlite(format!("event log query error: {error}")))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(
                row.map_err(|error| LogError::Sqlite(format!("event log row error: {error}")))?,
            );
        }
        Ok(events)
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
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let up_to_sql = event_id_to_sqlite_i64(up_to)?;
        connection
            .execute(
                "INSERT INTO consumers(topic, consumer_id, cursor, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(topic, consumer_id)
                 DO UPDATE SET cursor = excluded.cursor, updated_at_ms = excluded.updated_at_ms",
                params![topic.as_str(), consumer.as_str(), up_to_sql, now_ms()],
            )
            .map_err(|error| LogError::Sqlite(format!("event log ack error: {error}")))?;
        Ok(())
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .query_row(
                "SELECT cursor FROM consumers WHERE topic = ?1 AND consumer_id = ?2",
                params![topic.as_str(), consumer.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log consumer cursor error: {error}")))
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log latest error: {error}")))
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let before_sql = event_id_to_sqlite_i64(before)?;
        connection
            .execute(
                "DELETE FROM event_idempotency_keys WHERE topic = ?1 AND event_id <= ?2",
                params![topic.as_str(), before_sql],
            )
            .map_err(|error| {
                LogError::Sqlite(format!("event log idempotency compact error: {error}"))
            })?;
        let removed = connection
            .execute(
                "DELETE FROM events WHERE topic = ?1 AND event_id <= ?2",
                params![topic.as_str(), before_sql],
            )
            .map_err(|error| {
                LogError::Sqlite(format!("event log compact delete error: {error}"))
            })?;
        let remaining = connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| LogError::Sqlite(format!("event log compact count error: {error}")))
            .and_then(sqlite_i64_to_usize)?;
        let latest = connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| sqlite_i64_to_event_id_for_row(row.get::<_, i64>(0)?),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log latest error: {error}")))?;
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| LogError::Sqlite(format!("event log checkpoint error: {error}")))?;
        Ok(CompactReport {
            removed,
            remaining,
            latest,
            checkpointed: true,
        })
    }
}
