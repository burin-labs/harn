use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{params, Connection};

use super::{write_transaction, StoreContention, StoreError};

static IMMEDIATE_BUSY_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static DEFERRED_BUSY_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn version_six_store_upgrades_directory_metadata_without_losing_history() {
    use crate::{
        AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
        UpdateSession,
    };

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("version-six.sqlite");
    let original = SqliteSessionStore::open(&database).unwrap();
    original
        .create(CreateSession {
            id: Some("prior-session".into()),
            title: Some("Retained title".into()),
            ..CreateSession::default()
        })
        .await
        .unwrap();
    let event = original
        .append(
            "prior-session",
            AppendEvent::new(
                SessionEventKind::Message,
                serde_json::json!({"content":"Retained history"}),
            ),
        )
        .await
        .unwrap();
    drop(original);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE sessions DROP COLUMN additional_directories_json;
         UPDATE main._harn_sqlite_schema_versions SET version = 6 WHERE name = 'session_store';",
        )
        .unwrap();
    let marker: i64 = connection
        .query_row(
            "SELECT version FROM main._harn_sqlite_schema_versions WHERE name = 'session_store'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marker, 6);
    drop(connection);

    let upgraded = SqliteSessionStore::open(&database).unwrap();
    let metadata = upgraded.describe("prior-session").await.unwrap();
    assert_eq!(metadata.title.as_deref(), Some("Retained title"));
    assert!(metadata.additional_directories.is_empty());
    assert_eq!(
        upgraded.read_all("prior-session").await.unwrap(),
        vec![event]
    );
    let directories = vec!["/work/second".to_string(), "/work/first".to_string()];
    upgraded
        .update(
            "prior-session",
            UpdateSession {
                additional_directories: Some(directories.clone()),
                ..UpdateSession::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        upgraded
            .describe("prior-session")
            .await
            .unwrap()
            .additional_directories,
        directories
    );
}

#[tokio::test]
async fn additional_directories_survive_reopen_and_corrupt_lists_refuse() {
    use crate::{CreateSession, ListFilter, SessionStore, SqliteSessionStore};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("roots.sqlite");
    let roots = vec!["/work/second".to_string(), "/work/first".to_string()];
    {
        let store = SqliteSessionStore::open(&database).unwrap();
        store
            .create(CreateSession {
                id: Some("workspace-roots".into()),
                additional_directories: roots.clone(),
                ..CreateSession::default()
            })
            .await
            .unwrap();
    }
    let reopened = SqliteSessionStore::open(&database).unwrap();
    assert_eq!(
        reopened
            .describe("workspace-roots")
            .await
            .unwrap()
            .additional_directories,
        roots
    );
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE sessions SET additional_directories_json = '[7]' WHERE id = ?1",
            params!["workspace-roots"],
        )
        .unwrap();
    let error = reopened.describe("workspace-roots").await.unwrap_err();
    assert!(
        matches!(error, StoreError::Backend(message) if message.contains("invalid additional directories"))
    );
    reopened.list(ListFilter::default()).await.unwrap_err();
}

fn record_immediate_busy(_attempt: i32) -> bool {
    IMMEDIATE_BUSY_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    false
}

fn record_deferred_busy(_attempt: i32) -> bool {
    DEFERRED_BUSY_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    false
}

#[test]
fn deferred_read_to_write_upgrade_bypasses_busy_handler() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("deferred-upgrade.sqlite");
    let mut stale_reader = Connection::open(&database).expect("reader connection");
    let mut writer = Connection::open(&database).expect("writer connection");
    stale_reader
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE rows (value INTEGER PRIMARY KEY);
             INSERT INTO rows (value) VALUES (1);",
        )
        .expect("schema and seed");
    DEFERRED_BUSY_CALLBACKS.store(0, Ordering::SeqCst);
    stale_reader
        .busy_handler(Some(record_deferred_busy))
        .expect("install busy observer");

    let stale_tx = stale_reader.transaction().expect("deferred transaction");
    assert_eq!(
        stale_tx
            .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get::<_, i64>(0))
            .expect("establish read snapshot"),
        1
    );
    let writer_tx = write_transaction(&mut writer).expect("writer transaction");
    writer_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![2])
        .expect("concurrent write");
    writer_tx.commit().expect("concurrent commit");

    let error = stale_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![3])
        .expect_err("stale read transaction cannot upgrade");
    assert_eq!(
        harn_sqlite::sqlite_contention(&error),
        Some(harn_sqlite::SqliteContention::Busy)
    );
    assert_eq!(DEFERRED_BUSY_CALLBACKS.load(Ordering::SeqCst), 0);
}

#[test]
fn write_transactions_acquire_ownership_before_reading() {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = dir.path().join("write-ownership.sqlite");
    let mut owner = Connection::open(&database).expect("owner connection");
    let mut contender = Connection::open(&database).expect("contender connection");
    owner
        .execute_batch("CREATE TABLE rows (value INTEGER PRIMARY KEY);")
        .expect("schema");
    IMMEDIATE_BUSY_CALLBACKS.store(0, Ordering::SeqCst);
    contender
        .busy_handler(Some(record_immediate_busy))
        .expect("install busy observer");

    let owner_tx = write_transaction(&mut owner).expect("owner write transaction");
    owner_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![1])
        .expect("owner write");

    let error = match write_transaction(&mut contender) {
        Ok(_) => panic!("a second writer must fail at transaction entry"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::Contention {
            kind: StoreContention::DatabaseBusy,
            ..
        }
    ));
    assert_eq!(IMMEDIATE_BUSY_CALLBACKS.load(Ordering::SeqCst), 1);

    owner_tx.commit().expect("owner commit");
    let contender_tx = write_transaction(&mut contender).expect("writer after release");
    contender_tx
        .execute("INSERT INTO rows (value) VALUES (?1)", params![2])
        .expect("contender write");
    contender_tx.commit().expect("contender commit");

    assert_eq!(
        contender
            .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get::<_, i64>(0))
            .expect("row count"),
        2
    );
}

mod append_batch {
    use crate::{
        AppendEvent, CreateSession, ReadRange, SessionEventKind, SessionStore, SqliteSessionStore,
    };

    fn message(text: &str) -> AppendEvent {
        AppendEvent::new(
            SessionEventKind::Message,
            serde_json::json!({"role": "user", "content": text}),
        )
    }

    /// A batch stores the rows a run of single appends stores, chained in
    /// order, in one transaction.
    #[tokio::test]
    async fn batch_matches_single_appends_in_one_transaction() {
        let single = SqliteSessionStore::open_in_memory().expect("store");
        let batched = SqliteSessionStore::open_in_memory().expect("store");
        let request = CreateSession {
            id: Some("session".to_string()),
            ..CreateSession::default()
        };
        single.create(request.clone()).await.expect("create");
        batched.create(request).await.expect("create");
        for text in ["one", "two", "three"] {
            single
                .append("session", message(text))
                .await
                .expect("append");
        }
        let before = batched.append_transactions();
        let stored = batched
            .append_batch(
                "session",
                vec![message("one"), message("two"), message("three")],
            )
            .await
            .expect("append batch");
        assert_eq!(batched.append_transactions() - before, 1);
        assert_eq!(single.append_transactions(), 3);

        let single_events = single
            .read("session", ReadRange::default())
            .await
            .expect("read")
            .events;
        assert_eq!(
            stored,
            batched
                .read("session", ReadRange::default())
                .await
                .expect("read")
                .events
        );
        assert_eq!(stored.len(), single_events.len());
        for (left, right) in stored.iter().zip(&single_events) {
            assert_eq!(left.event_id, right.event_id);
            assert_eq!(left.payload, right.payload);
            assert_eq!(left.headers, right.headers);
        }
        assert_eq!(stored[0].prev_hash, None);
        assert_eq!(
            stored[1].prev_hash.as_deref(),
            Some(stored[0].record_hash.as_str())
        );
        assert_eq!(
            stored[2].prev_hash.as_deref(),
            Some(stored[1].record_hash.as_str())
        );
        let meta = batched.describe("session").await.expect("describe");
        assert_eq!(meta.event_count, 3);
        assert_eq!(meta.last_event_id, Some(3));
        assert!(batched
            .verify("session")
            .await
            .expect("verify")
            .failures
            .is_empty());
        let next = batched
            .append("session", message("four"))
            .await
            .expect("append after batch");
        assert_eq!(next.event_id, 4);
        assert_eq!(
            next.prev_hash.as_deref(),
            Some(stored[2].record_hash.as_str())
        );
    }

    /// A failure anywhere in the batch persists none of it, so a caller that
    /// keeps its queue until the batch commits can retry without duplicates.
    #[tokio::test]
    async fn a_failing_event_rolls_back_the_whole_batch() {
        let store = SqliteSessionStore::open_in_memory().expect("store");
        store
            .create(CreateSession {
                id: Some("session".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        let mut orphan = message("orphan");
        orphan.parent_event_id = Some(99);
        let before = store.append_transactions();
        store
            .append_batch("session", vec![message("kept?"), orphan])
            .await
            .expect_err("a missing parent must fail the batch");
        assert_eq!(store.append_transactions(), before);
        assert!(store
            .read("session", ReadRange::default())
            .await
            .expect("read")
            .events
            .is_empty());
        assert_eq!(
            store
                .describe("session")
                .await
                .expect("describe")
                .event_count,
            0
        );
    }
}
