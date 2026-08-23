//! Read-only helpers for watching a file-backed session store from another
//! process.
//!
//! The writer connection is mutexed and already publishes in-process. A
//! second process cannot hear that hook, so a watcher keeps its own reader
//! and asks SQLite whether the file changed (`PRAGMA data_version`) before
//! re-reading titles. Nothing here takes the store mutex or writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use super::sqlite::{read_session_meta, sqlite_sidecar_path};
use super::store::{SessionMeta, StoreError, StoreResult};

const READER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Snapshot a watcher diffs without loading every session column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTitleSnapshot {
    pub id: String,
    pub title: Option<String>,
    pub title_pinned: bool,
}

/// `{database}-wal` beside a file-backed store.
pub fn wal_sidecar_path(database: &Path) -> PathBuf {
    sqlite_sidecar_path(database, "-wal")
}

/// Open a reader that will never write and can see WAL commits from others.
pub fn open_watch_reader(database: &Path) -> StoreResult<Connection> {
    let conn =
        Connection::open(database).map_err(|error| StoreError::Backend(error.to_string()))?;
    conn.busy_timeout(READER_BUSY_TIMEOUT)
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    Ok(conn)
}

/// `PRAGMA data_version` on an already-open reader.
///
/// The value is per-connection. Keep the reader and re-query it; opening a
/// fresh connection each time cannot see another writer's commit.
pub fn data_version(conn: &Connection) -> StoreResult<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(|error| StoreError::Backend(error.to_string()))
}

pub fn list_title_snapshots(conn: &Connection) -> StoreResult<Vec<SessionTitleSnapshot>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, title_pinned FROM sessions
             WHERE soft_deleted_at_ms IS NULL
             ORDER BY id",
        )
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SessionTitleSnapshot {
                id: row.get(0)?,
                title: row.get(1)?,
                title_pinned: row.get(2)?,
            })
        })
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::Backend(error.to_string()))
}

pub fn describe_session(conn: &Connection, session_id: &str) -> StoreResult<SessionMeta> {
    read_session_meta(conn, session_id).map(|(meta, _)| meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CreateSession, SessionStore, SqliteSessionStore, UpdateSession};
    use tempfile::TempDir;

    #[tokio::test]
    async fn reader_data_version_moves_when_another_connection_commits() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("store.sqlite");
        let store = SqliteSessionStore::open(&path).expect("open store");
        store
            .create(CreateSession {
                id: Some("s1".to_string()),
                title: Some("before".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");

        let reader = open_watch_reader(&path).expect("open reader");
        let before = data_version(&reader).expect("version before");
        store
            .update(
                "s1",
                UpdateSession {
                    title: Some("after".to_string()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("rename");
        let after = data_version(&reader).expect("version after");
        assert_ne!(
            before, after,
            "a kept reader must see another connection's commit"
        );

        let titles = list_title_snapshots(&reader).expect("titles");
        assert_eq!(
            titles,
            vec![SessionTitleSnapshot {
                id: "s1".to_string(),
                title: Some("after".to_string()),
                title_pinned: false,
            }]
        );
        assert_eq!(
            describe_session(&reader, "s1")
                .expect("describe")
                .title
                .as_deref(),
            Some("after")
        );
    }
}
