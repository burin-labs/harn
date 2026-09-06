//! Session creation, import atomicity, and SQLite schema versioning.

use super::super::*;

#[tokio::test]
async fn create_assigns_meta_and_open_status() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession {
                tenant_id: Some("acme".into()),
                persona: Some("planner".into()),
                tags: vec!["primary".into()],
                ..Default::default()
            })
            .await
            .expect("create");
        assert!(!meta.id.is_empty());
        assert_eq!(meta.tenant_id.as_deref(), Some("acme"));
        assert_eq!(meta.persona.as_deref(), Some("planner"));
        assert_eq!(meta.status, SessionStatus::Open);
        assert_eq!(meta.event_count, 0);
        assert!(meta.chain_root_hash.is_none());

        let described = store.describe(&meta.id).await.expect("describe");
        assert_eq!(described, meta);
    })
    .await;
}

#[tokio::test]
async fn import_is_atomic_idempotent_and_survives_session_deletion() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let request = ImportSession {
            source_id: "legacy:session-a".to_string(),
            source_digest: "sha256:source-a".to_string(),
            session: CreateSession {
                id: Some("session-a".to_string()),
                ..CreateSession::default()
            },
            events: vec![AppendEvent::new(
                SessionEventKind::Message,
                json!({"text": "imported"}),
            )],
        };
        let first = store.import(request.clone()).await.expect("first import");
        assert!(first.imported);
        assert_eq!(first.event_count, 1);

        let second = store.import(request.clone()).await.expect("repeat import");
        assert!(!second.imported);
        assert_eq!(second.event_count, 1);
        assert_eq!(store.describe("session-a").await.unwrap().event_count, 1);

        let mut changed = request.clone();
        changed.source_digest = "sha256:changed".to_string();
        assert!(matches!(
            store.import(changed).await,
            Err(StoreError::Conflict(_))
        ));

        store.hard_delete("session-a").await.expect("hard delete");
        let after_delete = store.import(request).await.expect("receipt survives");
        assert!(!after_delete.imported);
        assert!(matches!(
            store.describe("session-a").await,
            Err(StoreError::NotFound(_))
        ));
    })
    .await;
}

#[tokio::test]
async fn failed_import_rolls_back_before_retry() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let mut invalid = ImportSession {
            source_id: "legacy:rollback".to_string(),
            source_digest: "sha256:rollback".to_string(),
            session: CreateSession {
                id: Some("rollback".to_string()),
                ..CreateSession::default()
            },
            events: vec![AppendEvent::new(SessionEventKind::Message, json!({})).with_parent(99)],
        };
        assert!(matches!(
            store.import(invalid.clone()).await,
            Err(StoreError::InvalidInput(_))
        ));
        assert!(matches!(
            store.describe("rollback").await,
            Err(StoreError::NotFound(_))
        ));

        invalid.events = vec![AppendEvent::new(SessionEventKind::Message, json!({}))];
        assert!(store.import(invalid).await.expect("retry import").imported);
    })
    .await;
}

#[test]
fn sqlite_import_serializes_independent_connections() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("concurrent.sqlite");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let request = ImportSession {
        source_id: "legacy:concurrent".to_string(),
        source_digest: "sha256:concurrent".to_string(),
        session: CreateSession {
            id: Some("concurrent".to_string()),
            ..CreateSession::default()
        },
        events: vec![AppendEvent::new(SessionEventKind::Message, json!({}))],
    };
    let stores = [
        SqliteSessionStore::open(&path).expect("open first sqlite"),
        SqliteSessionStore::open(&path).expect("open second sqlite"),
    ];
    let workers = stores
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(store.import(request))
                    .expect("concurrent import")
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.imported).count(), 1);
    assert_eq!(results.iter().filter(|result| !result.imported).count(), 1);
}

#[test]
fn sqlite_create_maps_an_independent_connection_race_to_already_exists() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("concurrent-create.sqlite");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let stores = [
        SqliteSessionStore::open(&path).expect("open first sqlite"),
        SqliteSessionStore::open(&path).expect("open second sqlite"),
    ];
    let workers = stores
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(store.create(CreateSession {
                        id: Some("concurrent-create".to_string()),
                        ..CreateSession::default()
                    }))
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::AlreadyExists(_))))
            .count(),
        1
    );
}

#[tokio::test]
async fn sqlite_upgrade_cleans_pre_foreign_key_orphans_and_records_current_version() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("upgrade.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open sqlite");
    store
        .create(CreateSession {
            id: Some("reused".to_string()),
            tags: vec!["old".to_string()],
            ..CreateSession::default()
        })
        .await
        .expect("create old session");
    store
        .append(
            "reused",
            AppendEvent::new(SessionEventKind::Message, json!({"generation": 1})),
        )
        .await
        .expect("append old event");
    store
        .snapshot("reused")
        .await
        .expect("snapshot old session");
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("open legacy connection");
    conn.execute_batch(
        "DROP TABLE _harn_sqlite_schema_versions;
         DROP TABLE session_imports;
         CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);
         INSERT INTO schema_version(version) VALUES (1);
         DELETE FROM sessions WHERE id = 'reused';",
    )
    .expect("simulate v1 hard delete");
    drop(conn);

    let store = SqliteSessionStore::open(&path).expect("upgrade sqlite");
    store
        .create(CreateSession {
            id: Some("reused".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("recreate cleaned session");
    let event = store
        .append(
            "reused",
            AppendEvent::new(SessionEventKind::Message, json!({"generation": 2})),
        )
        .await
        .expect("append after cleanup");
    assert_eq!(event.event_id, 1);
    drop(store);

    let conn = rusqlite::Connection::open(path).expect("inspect upgraded sqlite");
    let schema_state = (
        conn.query_row(
            "SELECT version FROM _harn_sqlite_schema_versions WHERE name = 'session_store'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("shared schema version"),
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .expect("legacy schema table state"),
    );
    assert_eq!(schema_state, (5, false));
    let stale_children: i64 = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM session_tags WHERE tag = 'old') +
                (SELECT COUNT(*) FROM session_snapshots WHERE session_id = 'reused')",
            [],
            |row| row.get(0),
        )
        .expect("orphan count");
    assert_eq!(stale_children, 0);
}

#[test]
fn sqlite_rejects_a_newer_schema_version() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("future.sqlite");
    let conn = rusqlite::Connection::open(&path).expect("open raw sqlite");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);
         INSERT INTO schema_version(version) VALUES (99);",
    )
    .expect("mark future legacy schema");
    drop(conn);

    let error = SqliteSessionStore::open(path)
        .err()
        .expect("reject future schema");
    assert_eq!(
        error,
        StoreError::SchemaIncompatible {
            schema: "session_store".to_string(),
            stored: 99,
            supported: 5,
        }
    );
}

#[test]
fn sqlite_rejects_a_newer_shared_schema_marker_without_erasing_its_type() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("future-shared.sqlite");
    drop(SqliteSessionStore::open(&path).expect("initialize sqlite"));
    let conn = rusqlite::Connection::open(&path).expect("open raw sqlite");
    conn.execute(
        "UPDATE _harn_sqlite_schema_versions SET version = 99 WHERE name = 'session_store'",
        [],
    )
    .expect("mark future shared schema");
    drop(conn);

    let error = SqliteSessionStore::open(path)
        .err()
        .expect("reject future shared schema");
    assert_eq!(
        error,
        StoreError::SchemaIncompatible {
            schema: "session_store".to_string(),
            stored: 99,
            supported: 5,
        }
    );
}

#[tokio::test]
async fn sqlite_verify_detects_a_deleted_middle_event() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tamper.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open sqlite");
    store
        .create(CreateSession {
            id: Some("tamper".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create");
    for index in 0..3 {
        store
            .append(
                "tamper",
                AppendEvent::new(SessionEventKind::Message, json!({"index": index})),
            )
            .await
            .expect("append");
    }
    drop(store);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "DELETE FROM session_events WHERE session_id = 'tamper' AND event_id = 2",
            [],
        )
        .unwrap();

    let store = SqliteSessionStore::open(path).expect("reopen sqlite");
    let report = store.verify("tamper").await.expect("verify");
    assert!(!report.failures.is_empty());
    assert!(report
        .failures
        .iter()
        .any(|failure| failure.reason.contains("sequence gap")));
    assert!(report
        .failures
        .iter()
        .any(|failure| failure.reason.contains("event_count mismatch")));
}
