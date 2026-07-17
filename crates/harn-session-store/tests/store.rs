//! Behaviour tests for the reusable session-store primitive.
//!
//! The runners exercise the memory and SQLite backends against the same scenarios.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use harn_session_store::*;

#[derive(Clone)]
struct TestRedactor;

impl EventRedactor for TestRedactor {
    fn redact_json_in_place(&self, value: &mut serde_json::Value) {
        if let Some(object) = value.as_object_mut() {
            if object.contains_key("api_key") {
                object.insert("api_key".to_string(), json!("[redacted]"));
            }
        }
    }

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        headers
            .iter()
            .map(|(name, value)| {
                let value = if name == "authorization" {
                    "[redacted]".to_string()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect()
    }
}

#[derive(Clone)]
struct IdentityClobberingRedactor;

impl EventRedactor for IdentityClobberingRedactor {
    fn redact_json_in_place(&self, _value: &mut serde_json::Value) {}

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut headers = headers.clone();
        headers.insert("run_id".to_string(), "[redacted]".to_string());
        headers
    }
}

#[derive(Clone)]
struct SwitchableRedactor {
    enabled: Arc<AtomicBool>,
    clobber_identity: bool,
}

impl EventRedactor for SwitchableRedactor {
    fn redact_json_in_place(&self, value: &mut serde_json::Value) {
        if self.enabled.load(Ordering::SeqCst) {
            TestRedactor.redact_json_in_place(value);
        }
    }

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        if !self.enabled.load(Ordering::SeqCst) {
            return headers.clone();
        }
        let mut headers = TestRedactor.redact_headers(headers);
        if self.clobber_identity {
            headers.insert("run_id".to_string(), "[redacted]".to_string());
        }
        headers
    }
}

fn dummy_signer(seed: u8) -> SessionSigner {
    SessionSigner::from_seed([seed; 32])
}

fn fresh_memory(hooks: StoreHooks) -> Arc<dyn SessionImporter> {
    Arc::new(MemorySessionStore::with_hooks(hooks))
}

fn fresh_sqlite(hooks: StoreHooks, dir: &TempDir) -> Arc<dyn SessionImporter> {
    let path = dir.path().join("sessions.sqlite");
    Arc::new(SqliteSessionStore::open_with_hooks(path, hooks).expect("open sqlite"))
}

async fn run_with_hooks<F, Fut>(hooks: StoreHooks, body: F)
where
    F: Fn(Arc<dyn SessionImporter>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    body(fresh_memory(hooks.clone())).await;
    let dir = TempDir::new().expect("tempdir");
    body(fresh_sqlite(hooks, &dir)).await;
}

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
async fn sqlite_upgrade_cleans_pre_foreign_key_orphans_and_records_v2() {
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
        "DROP TABLE session_imports;
         DELETE FROM schema_version;
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
    let max_version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .expect("schema version");
    assert_eq!(max_version, 2);
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
    drop(SqliteSessionStore::open(&path).expect("create sqlite"));
    let conn = rusqlite::Connection::open(&path).expect("open raw sqlite");
    conn.execute("INSERT INTO schema_version(version) VALUES (99)", [])
        .expect("mark future schema");
    drop(conn);

    let error = SqliteSessionStore::open(path)
        .err()
        .expect("reject future schema");
    assert!(error.to_string().contains("newer than supported version 2"));
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

#[tokio::test]
async fn append_assigns_monotonic_ids_and_chain_hashes() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let first = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"text": "hi"})),
            )
            .await
            .expect("append first");
        let second = store
            .append(
                &meta.id,
                AppendEvent::new(
                    SessionEventKind::ToolCall,
                    json!({"name": "shell", "args": {}}),
                ),
            )
            .await
            .expect("append second");
        assert_eq!(first.event_id, 1);
        assert_eq!(second.event_id, 2);
        assert!(first.prev_hash.is_none());
        assert_eq!(
            second.prev_hash.as_deref(),
            Some(first.record_hash.as_str())
        );
        let described = store.describe(&meta.id).await.expect("describe");
        assert_eq!(described.event_count, 2);
        assert_eq!(described.last_event_id, Some(2));
        assert!(described.chain_root_hash.is_some());
    })
    .await;
}

#[tokio::test]
async fn typed_identity_is_normalized_and_preserved_by_every_backend() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, " run-1 ")
            .expect("run id")
            .with(EventIdentityField::TurnId, "turn-1")
            .expect("turn id")
            .with(EventIdentityField::SourceEventId, "event-7")
            .expect("source event id")
            .with(EventIdentityField::MessageId, "message-3")
            .expect("message id")
            .with(EventIdentityField::ToolCallId, "tool-2")
            .expect("tool call id");
        let event = AppendEvent::new(SessionEventKind::ToolCall, json!({"name": "shell"}))
            .with_identity(&identity)
            .expect("stamp identity");

        let stored = store.append(&meta.id, event).await.expect("append");

        assert_eq!(stored.identity().expect("stored identity"), identity);
        assert_eq!(stored.headers["run_id"], "run-1");
        let mut tampered = stored.clone();
        tampered
            .headers
            .insert("run_id".to_string(), "run-2".to_string());
        assert_ne!(compute_record_hash(&tampered), stored.record_hash);
        let replayed = store
            .replay(&store.snapshot(&meta.id).await.expect("snapshot").id)
            .await
            .expect("replay");
        assert_eq!(replayed.events[0].identity().unwrap(), identity);
    })
    .await;
}

#[tokio::test]
async fn redaction_cannot_silently_replace_producer_identity() {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(IdentityClobberingRedactor)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, "run-1")
            .expect("run id");
        let event = AppendEvent::new(SessionEventKind::Message, json!({"text": "hello"}))
            .with_identity(&identity)
            .expect("stamp identity");

        let error = store
            .append(&meta.id, event)
            .await
            .expect_err("identity clobber must fail");

        assert!(matches!(error, StoreError::InvalidInput(_)));
        assert_eq!(store.describe(&meta.id).await.unwrap().event_count, 0);
    })
    .await;
}

#[tokio::test]
async fn raw_reserved_identity_headers_are_validated_before_persistence() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let mut event = AppendEvent::new(SessionEventKind::Message, json!({"text": "hello"}));
        event
            .headers
            .insert("run_id".to_string(), " \n ".to_string());

        let error = store
            .append(&meta.id, event)
            .await
            .expect_err("blank run id must fail");

        assert!(matches!(error, StoreError::InvalidInput(_)));
        assert_eq!(store.describe(&meta.id).await.unwrap().event_count, 0);
    })
    .await;
}

#[tokio::test]
async fn read_iterates_via_cursor() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..7 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let page = store
            .read(
                &meta.id,
                ReadRange {
                    limit: Some(3),
                    ..Default::default()
                },
            )
            .await
            .expect("read");
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.next_cursor, Some(4));
        let next = store
            .read(
                &meta.id,
                ReadRange {
                    from_event_id: page.next_cursor,
                    limit: Some(10),
                    ..Default::default()
                },
            )
            .await
            .expect("read tail");
        assert_eq!(next.events.len(), 4);
        assert!(next.next_cursor.is_none());
    })
    .await;
}

#[tokio::test]
async fn fork_copies_history_up_to_event_id() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..5 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let result = store
            .fork(&meta.id, 3, Some("child".into()))
            .await
            .expect("fork");
        assert_eq!(result.child_session_id, "child");
        assert_eq!(result.copied_event_count, 3);
        let child = store.describe("child").await.expect("describe child");
        assert_eq!(child.parent_session_id.as_deref(), Some(meta.id.as_str()));
        assert_eq!(child.event_count, 3);
        let events = store
            .read("child", ReadRange::default())
            .await
            .expect("read child")
            .events;
        assert_eq!(events.last().unwrap().event_id, 3);

        let next = store
            .append(
                "child",
                AppendEvent::new(SessionEventKind::Message, json!({"branch": "child"})),
            )
            .await
            .expect("append on child");
        assert_eq!(next.event_id, 4);
    })
    .await;
}

#[tokio::test]
async fn truncate_drops_events_past_event_id() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..5 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let result = store.truncate(&meta.id, 3).await.expect("truncate");
        assert_eq!(result.kept_event_count, 3);
        assert_eq!(result.removed_event_count, 2);
        let next = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"after_truncate": true})),
            )
            .await
            .expect("append after truncate");
        assert_eq!(next.event_id, 4);
    })
    .await;
}

#[tokio::test]
async fn snapshot_and_replay_round_trip() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..3 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let snapshot = store.snapshot(&meta.id).await.expect("snapshot");
        let replayed = store.replay(&snapshot.id).await.expect("replay");
        assert_eq!(replayed.id, snapshot.id);
        assert_eq!(replayed.events.len(), 3);
        assert_eq!(replayed.session.id, meta.id);
    })
    .await;
}

#[tokio::test]
async fn close_emits_signed_receipt_event() {
    let hooks = StoreHooks {
        receipt_signer: Some(dummy_signer(1)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"text": "hi"})),
            )
            .await
            .expect("append");
        let receipt = store.close(&meta.id).await.expect("close");
        assert!(matches!(receipt.kind, SessionEventKind::Receipt));
        let signature = receipt.signed_by.expect("receipt is signed");
        assert_eq!(signature.algorithm, SIGNATURE_ALGORITHM);
        // Closed sessions reject further appends.
        let err = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({})),
            )
            .await
            .expect_err("append to closed");
        assert!(matches!(err, StoreError::Conflict(_)));
    })
    .await;
}

#[tokio::test]
async fn event_signer_only_close_verifies_receipt_against_chain_root() {
    // Regression for the receipt-signature clobber: with an event signer
    // configured (and no separate receipt signer), `append` signs every
    // event including the receipt minted by `close`. `close` then replaces
    // that per-event signature with a receipt-root signature. `verify`
    // must recognise the receipt and check it against the pre-receipt
    // chain root — previously it applied `verify_event` to the receipt and
    // reported a spurious `BadSignature` on a correctly closed session.
    let signer = dummy_signer(9);
    let hooks = StoreHooks {
        event_signer: Some(signer.clone()),
        ..Default::default()
    };
    run_with_hooks(hooks, move |store| {
        let signer = signer.clone();
        async move {
            let meta = store
                .create(CreateSession::default())
                .await
                .expect("create");
            for i in 0..3 {
                store
                    .append(
                        &meta.id,
                        AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                    )
                    .await
                    .expect("append");
            }
            let receipt = store.close(&meta.id).await.expect("close");
            assert!(matches!(receipt.kind, SessionEventKind::Receipt));

            let report = store.verify(&meta.id).await.expect("verify");
            assert!(
                report.failures.is_empty(),
                "closed signed session reported failures: {:?}",
                report.failures
            );
            // 3 messages + 1 receipt, all signed and all verified.
            assert_eq!(report.event_count, 4);
            assert_eq!(report.signed_event_count, 4);

            // The receipt's signature attests the chain root over the
            // events that preceded it, not the receipt event's own bytes.
            let events = store
                .read(&meta.id, ReadRange::default())
                .await
                .expect("read")
                .events;
            let (index, receipt_event) = events
                .iter()
                .enumerate()
                .find(|(_, event)| matches!(event.kind, SessionEventKind::Receipt))
                .expect("receipt present");
            let pre_receipt_root = chain_root_hash(&events[..index]);
            let signature = receipt_event.signed_by.as_ref().expect("receipt signed");
            verify_receipt_root(signature, &signer.verifying_key(), &pre_receipt_root)
                .expect("receipt attests the pre-receipt chain root");

            // close() leaves the session atomically closed.
            let described = store.describe(&meta.id).await.expect("describe");
            assert_eq!(described.status, SessionStatus::Closed);
            let err = store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({})),
                )
                .await
                .expect_err("append to closed");
            assert!(matches!(err, StoreError::Conflict(_)));
        }
    })
    .await;
}

#[tokio::test]
async fn receipt_signer_only_close_verifies_receipt() {
    // With only a receipt signer, the receipt is the sole signed event.
    // `verify` now actually attests it (previously it counted the receipt
    // as signed without ever calling into the signature check).
    let signer = dummy_signer(11);
    let hooks = StoreHooks {
        receipt_signer: Some(signer.clone()),
        ..Default::default()
    };
    run_with_hooks(hooks, move |store| {
        let signer = signer.clone();
        async move {
            let meta = store
                .create(CreateSession::default())
                .await
                .expect("create");
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"text": "hi"})),
                )
                .await
                .expect("append");
            store.close(&meta.id).await.expect("close");

            let report = store.verify(&meta.id).await.expect("verify");
            assert!(
                report.failures.is_empty(),
                "receipt-signed session reported failures: {:?}",
                report.failures
            );
            // Message is unsigned (no event signer); only the receipt is.
            assert_eq!(report.signed_event_count, 1);

            let events = store
                .read(&meta.id, ReadRange::default())
                .await
                .expect("read")
                .events;
            let (index, receipt_event) = events
                .iter()
                .enumerate()
                .find(|(_, event)| matches!(event.kind, SessionEventKind::Receipt))
                .expect("receipt present");
            let pre_receipt_root = chain_root_hash(&events[..index]);
            verify_receipt_root(
                receipt_event.signed_by.as_ref().expect("receipt signed"),
                &signer.verifying_key(),
                &pre_receipt_root,
            )
            .expect("receipt attests the pre-receipt chain root");
        }
    })
    .await;
}

#[tokio::test]
async fn verify_reports_chain_hash_mismatch() {
    let hooks = StoreHooks {
        event_signer: Some(dummy_signer(2)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..3 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let report = store.verify(&meta.id).await.expect("verify");
        assert_eq!(report.failures, vec![]);
        assert_eq!(report.event_count, 3);
        assert_eq!(report.signed_event_count, 3);
    })
    .await;
}

#[tokio::test]
async fn redaction_hook_scrubs_payload_on_append() {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(TestRedactor)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let mut event = AppendEvent::new(
            SessionEventKind::Message,
            json!({"api_key": "sensitive", "text": "ok"}),
        );
        event
            .headers
            .insert("authorization".to_string(), "Bearer sensitive".to_string());
        store.append(&meta.id, event).await.expect("append");
        let page = store
            .read(&meta.id, ReadRange::default())
            .await
            .expect("read");
        let payload = &page.events[0].payload;
        let api_key = payload["api_key"].as_str().expect("string");
        assert_eq!(api_key, "[redacted]");
        assert_eq!(page.events[0].headers["authorization"], "[redacted]");
    })
    .await;
}

#[tokio::test]
async fn retrieval_reapplies_redaction_to_stored_data_and_marks_projection() {
    let enabled = Arc::new(AtomicBool::new(false));
    let hooks = StoreHooks {
        redaction: Some(Arc::new(SwitchableRedactor {
            enabled: Arc::clone(&enabled),
            clobber_identity: false,
        })),
        event_signer: Some(dummy_signer(9)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| {
        let enabled = Arc::clone(&enabled);
        async move {
            enabled.store(false, Ordering::SeqCst);
            let meta = store
                .create(CreateSession::default())
                .await
                .expect("create");
            let identity = EventIdentity::new()
                .with(EventIdentityField::RunId, "run-1")
                .expect("run id");
            let mut event = AppendEvent::new(
                SessionEventKind::Message,
                json!({"api_key": "sensitive", "text": "ok"}),
            )
            .with_identity(&identity)
            .expect("stamp identity");
            event
                .headers
                .insert("authorization".to_string(), "Bearer sensitive".to_string());
            let stored = store.append(&meta.id, event).await.expect("append");
            assert_eq!(stored.payload["api_key"], "sensitive");
            assert_eq!(stored.headers["authorization"], "Bearer sensitive");
            assert!(stored.signed_by.is_some());
            let raw_snapshot = store.snapshot(&meta.id).await.expect("raw snapshot");
            store.close(&meta.id).await.expect("close");

            enabled.store(true, Ordering::SeqCst);
            let page = store
                .read(&meta.id, ReadRange::default())
                .await
                .expect("read");

            assert_eq!(page.events[0].payload["api_key"], "[redacted]");
            assert_eq!(page.events[0].headers["authorization"], "[redacted]");
            assert_eq!(page.events[0].headers["run_id"], "run-1");
            assert!(page.events[0].is_redacted_projection());
            assert_eq!(page.events[0].source_record_hash(), stored.record_hash);
            assert!(page.events[0].signed_by.is_none());
            let verify_error = verify_event(&page.events[0], &dummy_signer(9).verifying_key())
                .expect_err("projection must not authenticate as canonical bytes");
            assert!(matches!(verify_error, VerifyError::InvalidShape(_)));

            let forked = store
                .fork(&meta.id, 1, Some("redacted-child".to_string()))
                .await
                .expect("fork");
            let child_page = store
                .read(&forked.child_session_id, ReadRange::default())
                .await
                .expect("read child");
            assert_eq!(child_page.events[0].payload["api_key"], "[redacted]");
            assert_eq!(child_page.events[0].headers["authorization"], "[redacted]");
            assert!(!child_page.events[0].is_redacted_projection());
            assert!(store
                .verify(&forked.child_session_id)
                .await
                .expect("verify child")
                .failures
                .is_empty());

            let replayed_raw = store.replay(&raw_snapshot.id).await.expect("replay raw");
            assert_eq!(replayed_raw.events[0], page.events[0]);

            let protected_snapshot = store.snapshot(&meta.id).await.expect("protected snapshot");
            assert_eq!(protected_snapshot.events[0], page.events[0]);
            let projected_root = chain_root_hash(&protected_snapshot.events);
            assert_eq!(
                Some(projected_root.as_str()),
                protected_snapshot.session.chain_root_hash.as_deref()
            );
            let verifying_key = dummy_signer(9).verifying_key();
            let (signed, failures) = verify_event_chain(
                &protected_snapshot.events,
                Some(&verifying_key),
                Some(&verifying_key),
            );
            assert_eq!(signed, 1, "the unchanged receipt remains valid");
            assert_eq!(failures.len(), 1);
            assert!(failures[0].1.contains("redacted projection"));
            let replayed_protected = store
                .replay(&protected_snapshot.id)
                .await
                .expect("replay protected");
            assert_eq!(replayed_protected.events[0], page.events[0]);
        }
    })
    .await;
}

#[tokio::test]
async fn read_rejects_a_policy_that_clobbers_stored_identity() {
    let enabled = Arc::new(AtomicBool::new(false));
    let hooks = StoreHooks {
        redaction: Some(Arc::new(SwitchableRedactor {
            enabled: Arc::clone(&enabled),
            clobber_identity: true,
        })),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| {
        let enabled = Arc::clone(&enabled);
        async move {
            enabled.store(false, Ordering::SeqCst);
            let meta = store
                .create(CreateSession::default())
                .await
                .expect("create");
            let identity = EventIdentity::new()
                .with(EventIdentityField::RunId, "run-1")
                .expect("run id");
            let event = AppendEvent::new(SessionEventKind::Message, json!({"text": "ok"}))
                .with_identity(&identity)
                .expect("stamp identity");
            store.append(&meta.id, event).await.expect("append");

            enabled.store(true, Ordering::SeqCst);
            let error = store
                .read(&meta.id, ReadRange::default())
                .await
                .expect_err("identity clobber must fail");

            assert!(matches!(error, StoreError::Backend(_)));
            assert!(error
                .to_string()
                .contains("redaction policy changed producer identity"));
        }
    })
    .await;
}

#[tokio::test]
async fn list_filters_by_tenant_and_persona() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let _ = store
            .create(CreateSession {
                tenant_id: Some("acme".into()),
                persona: Some("planner".into()),
                ..Default::default()
            })
            .await
            .expect("create one");
        let _ = store
            .create(CreateSession {
                tenant_id: Some("acme".into()),
                persona: Some("coder".into()),
                ..Default::default()
            })
            .await
            .expect("create two");
        let _ = store
            .create(CreateSession {
                tenant_id: Some("globex".into()),
                persona: Some("planner".into()),
                ..Default::default()
            })
            .await
            .expect("create three");
        let acme_only = store
            .list(ListFilter {
                tenant_id: Some("acme".into()),
                ..Default::default()
            })
            .await
            .expect("list acme");
        assert_eq!(acme_only.len(), 2);
        let acme_planner = store
            .list(ListFilter {
                tenant_id: Some("acme".into()),
                persona: Some("planner".into()),
                ..Default::default()
            })
            .await
            .expect("list acme planner");
        assert_eq!(acme_planner.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn soft_delete_marks_session_and_hard_delete_removes_it() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"generation": 1})),
            )
            .await
            .expect("append before delete");
        let soft = store.soft_delete(&meta.id).await.expect("soft delete");
        assert_eq!(soft.status, SessionStatus::SoftDeleted);
        assert!(soft.soft_deleted_at_ms.is_some());
        let err = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({})),
            )
            .await
            .expect_err("append to soft-deleted");
        assert!(matches!(err, StoreError::NotFound(_)));
        store.hard_delete(&meta.id).await.expect("hard delete");
        let missing = store.describe(&meta.id).await.expect_err("describe gone");
        assert!(matches!(missing, StoreError::NotFound(_)));

        store
            .create(CreateSession {
                id: Some(meta.id.clone()),
                ..CreateSession::default()
            })
            .await
            .expect("recreate same id");
        let event = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"generation": 2})),
            )
            .await
            .expect("append after recreate");
        assert_eq!(event.event_id, 1);
    })
    .await;
}

#[tokio::test]
async fn sweep_retention_hard_deletes_after_grace_window() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        store.soft_delete(&meta.id).await.expect("soft delete");
        let policy = RetentionPolicy {
            grace_seconds: 0,
            ..RetentionPolicy::default()
        };
        let soft_deleted = store
            .describe(&meta.id)
            .await
            .expect("describe soft-deleted");
        let future_ms = soft_deleted
            .soft_deleted_at_ms
            .expect("soft-deleted timestamp")
            + 1;
        let report = store
            .sweep_retention(&policy, future_ms)
            .await
            .expect("sweep");
        assert_eq!(report.hard_deleted, 1);
        let missing = store.describe(&meta.id).await.expect_err("gone");
        assert!(matches!(missing, StoreError::NotFound(_)));
    })
    .await;
}

#[tokio::test]
async fn signed_event_roundtrips_via_verify() {
    let signer = dummy_signer(7);
    let hooks = StoreHooks {
        event_signer: Some(signer.clone()),
        ..Default::default()
    };
    let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::with_hooks(hooks));
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create");
    let event = store
        .append(
            &meta.id,
            AppendEvent::new(SessionEventKind::Message, json!({"text": "hello"})),
        )
        .await
        .expect("append");
    assert!(event.signed_by.is_some());
    let verifying_key = signer.verifying_key();
    verify_event(&event, &verifying_key).expect("verify ok");
}

#[tokio::test]
async fn list_tag_filter_matches_sessions_with_tag() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let primary = store
            .create(CreateSession {
                tags: vec!["alpha".into(), "primary".into()],
                ..Default::default()
            })
            .await
            .expect("create primary");
        let secondary = store
            .create(CreateSession {
                tags: vec!["beta".into()],
                ..Default::default()
            })
            .await
            .expect("create secondary");
        let _ = store
            .create(CreateSession {
                tags: vec![],
                ..Default::default()
            })
            .await
            .expect("create untagged");
        let alpha = store
            .list(ListFilter {
                tag: Some("alpha".into()),
                ..Default::default()
            })
            .await
            .expect("list alpha");
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].id, primary.id);
        let beta = store
            .list(ListFilter {
                tag: Some("beta".into()),
                ..Default::default()
            })
            .await
            .expect("list beta");
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].id, secondary.id);
        let none = store
            .list(ListFilter {
                tag: Some("gamma".into()),
                ..Default::default()
            })
            .await
            .expect("list gamma");
        assert!(none.is_empty());
    })
    .await;
}

#[tokio::test]
async fn list_cursor_paginates_in_creation_order() {
    // Zero-pad ids so lexical ordering matches insertion order even
    // when every session lands in the same wall-clock millisecond.
    // Both backends order by `(created_at_ms, id)`, so same-ms sessions
    // fall through to id ASC — the cursor walk is deterministic
    // without any sleeps.
    run_with_hooks(StoreHooks::default(), |store| async move {
        let mut ids = Vec::new();
        for i in 0..5 {
            let meta = store
                .create(CreateSession {
                    id: Some(format!("session-{i:02}")),
                    ..Default::default()
                })
                .await
                .expect("create");
            ids.push(meta.id);
        }
        let first_page = store
            .list(ListFilter {
                limit: Some(2),
                ..Default::default()
            })
            .await
            .expect("list page 1");
        assert_eq!(first_page.len(), 2);
        assert_eq!(first_page[0].id, ids[0]);
        assert_eq!(first_page[1].id, ids[1]);
        let cursor = first_page.last().unwrap().id.clone();
        let second_page = store
            .list(ListFilter {
                limit: Some(2),
                cursor: Some(cursor),
                ..Default::default()
            })
            .await
            .expect("list page 2");
        assert_eq!(second_page.len(), 2);
        assert_eq!(second_page[0].id, ids[2]);
        assert_eq!(second_page[1].id, ids[3]);
    })
    .await;
}

#[tokio::test]
async fn sweep_retention_archives_closed_sessions_before_soft_delete() {
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSink {
        archived: Mutex<Vec<(String, usize)>>,
        tombstones: Mutex<Vec<Tombstone>>,
    }
    #[async_trait::async_trait]
    impl ArchiveSink for RecordingSink {
        async fn archive(&self, session: &SessionMeta, events: &[StoredEvent]) -> StoreResult<()> {
            self.archived
                .lock()
                .unwrap()
                .push((session.id.clone(), events.len()));
            Ok(())
        }
        async fn tombstone(&self, tombstone: &Tombstone) -> StoreResult<()> {
            self.tombstones.lock().unwrap().push(tombstone.clone());
            Ok(())
        }
    }

    let sink = Arc::new(RecordingSink::default());
    let hooks = StoreHooks {
        archive_sink: Some(sink.clone() as SharedArchiveSink),
        ..Default::default()
    };
    let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::with_hooks(hooks));
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create");
    store
        .append(
            &meta.id,
            AppendEvent::new(SessionEventKind::Message, json!({"text": "hi"})),
        )
        .await
        .expect("append");
    store.close(&meta.id).await.expect("close");

    let now_ms = meta.created_at_ms + 60_000;
    let policy = RetentionPolicy {
        min_age_before_archive_seconds: Some(1),
        grace_seconds: 1,
        ..RetentionPolicy::default()
    };
    let report = store
        .sweep_retention(&policy, now_ms)
        .await
        .expect("sweep archive");
    assert_eq!(report.archived, 1);
    assert_eq!(report.soft_deleted, 1);
    assert_eq!(report.hard_deleted, 0);
    let archived = sink.archived.lock().unwrap().clone();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].0, meta.id);
    // Message + Receipt event from close = 2 events.
    assert_eq!(archived[0].1, 2);

    // Second sweep, past the grace window, hard-deletes and emits tombstone.
    let later_ms = now_ms + 60_000;
    let report = store
        .sweep_retention(&policy, later_ms)
        .await
        .expect("sweep tombstone");
    assert_eq!(report.hard_deleted, 1);
    assert_eq!(report.tombstoned, 1);
    let tombstones = sink.tombstones.lock().unwrap().clone();
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].session_id, meta.id);
    assert!(tombstones[0].final_chain_root_hash.is_some());
}

#[tokio::test]
async fn fork_produces_self_contained_verifiable_chain() {
    // Regression: prior sqlite fork rewrote `session_id` on copied
    // events but kept the old `record_hash`, so `verify` on the child
    // failed with HashMismatch (the stored hash no longer matched
    // `compute_record_hash` against the new canonical bytes). Both
    // backends now re-anchor copied events on the child's id so the
    // child's chain stands alone and verify passes cleanly.
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create parent");
        for i in 0..4 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let _ = store
            .fork(&meta.id, 3, Some("forked-child".into()))
            .await
            .expect("fork");

        let report = store.verify("forked-child").await.expect("verify child");
        assert_eq!(report.event_count, 3);
        assert!(
            report.failures.is_empty(),
            "child chain failed verification: {:?}",
            report.failures
        );

        // The child's stored chain root must equal a from-scratch
        // recompute over its own events — same invariant the append
        // path enforces for non-forked sessions.
        let described = store
            .describe("forked-child")
            .await
            .expect("describe child");
        assert_eq!(
            described.chain_root_hash.as_deref(),
            Some(report.chain_root_hash.as_str())
        );

        // Each copied event must report the child's session_id, not
        // the parent's — otherwise downstream consumers (TUI session
        // continuation, cloud verifier) would see events that look
        // like they belong to a different session.
        let page = store
            .read("forked-child", ReadRange::default())
            .await
            .expect("read child");
        for event in &page.events {
            assert_eq!(event.session_id, "forked-child");
        }
    })
    .await;
}

#[tokio::test]
async fn append_chain_root_matches_full_recompute() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        for i in 0..10 {
            store
                .append(
                    &meta.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"i": i})),
                )
                .await
                .expect("append");
        }
        let described = store.describe(&meta.id).await.expect("describe");
        // The verify path replays the chain from genesis; the stored
        // incremental root must equal it byte-for-byte.
        let report = store.verify(&meta.id).await.expect("verify");
        assert_eq!(
            described.chain_root_hash.as_deref(),
            Some(report.chain_root_hash.as_str())
        );
        assert!(report.failures.is_empty());
    })
    .await;
}
