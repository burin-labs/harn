use super::*;

#[tokio::test]
async fn typed_metadata_update_is_shared_by_memory_and_sqlite_and_reindexes_search() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("metadata-update".into()),
                project_scope: Some("project-alpha".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create session");
        store
            .append(
                &session.id,
                AppendEvent::new(SessionEventKind::Message, json!({"text": "body"})),
            )
            .await
            .expect("append event");
        let updated = store
            .update(
                &session.id,
                UpdateSession {
                    title: Some("searchable release title".into()),
                    model: Some("model-v2".into()),
                    usage_input: Some(120),
                    usage_output: Some(45),
                    usage_cost_usd_micros: Some(2_500),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("update metadata");
        assert_eq!(updated.title.as_deref(), Some("searchable release title"));
        assert_eq!(updated.model.as_deref(), Some("model-v2"));
        assert_eq!(
            (
                updated.usage_input,
                updated.usage_output,
                updated.usage_cost_usd_micros
            ),
            (120, 45, 2_500)
        );
        let search = store
            .search(SearchQuery {
                query: "searchable release title".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    project_scope: Some("project-alpha".into()),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search updated title");
        assert_eq!(search.hits.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn sqlite_10k_event_fts_query_completes_under_500ms() {
    let dir = TempDir::new().expect("tempdir");
    let store =
        SqliteSessionStore::open(dir.path().join("sessions.sqlite")).expect("open sqlite store");
    let events = (0..10_000)
        .map(|index| {
            let marker = if index == 9_999 {
                " unique-release-marker"
            } else {
                ""
            };
            AppendEvent::new(
                SessionEventKind::Message,
                json!({"text": format!("canonical transcript row {index}{marker}")}),
            )
        })
        .collect();
    store
        .import(ImportSession {
            source_id: "perf-corpus".into(),
            source_digest: "sha256:perf-corpus".into(),
            session: CreateSession {
                id: Some("perf-session".into()),
                project_scope: Some("perf-project".into()),
                ..CreateSession::default()
            },
            events,
        })
        .await
        .expect("import corpus");

    let started = std::time::Instant::now();
    let response = store
        .search(SearchQuery {
            query: "unique release marker".into(),
            mode: SearchMode::Fts,
            filter: SearchFilter {
                project_scope: Some("perf-project".into()),
                ..SearchFilter::default()
            },
            limit: Some(10),
        })
        .await
        .expect("search corpus");
    let elapsed = started.elapsed();
    eprintln!("10k-event FTS query elapsed: {elapsed:?}");

    assert_eq!(response.hits.len(), 1);
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "10k-event query took {elapsed:?}"
    );
}

#[tokio::test]
async fn canonical_search_is_scoped_redacted_and_reports_fts_fallback() {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(TestRedactor)),
        ..StoreHooks::default()
    };
    run_with_hooks(hooks, |store| async move {
        let target = store
            .create(CreateSession {
                id: Some("search-target".into()),
                tenant_id: Some("tenant-a".into()),
                title: Some("Canonical migration known-secret-value".into()),
                cwd: Some("/workspace/alpha".into()),
                model: Some("test-model".into()),
                session_type: Some(SessionType::User),
                project_scope: Some("project-alpha".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create target");
        let stored = store
            .append(
                &target.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({
                        "text": "canonical needle lives here",
                        "api_key": "known-secret-value"
                    }),
                ),
            )
            .await
            .expect("append target");
        let other = store
            .create(CreateSession {
                id: Some("search-other".into()),
                tenant_id: Some("tenant-b".into()),
                project_scope: Some("project-beta".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create other");
        store
            .append(
                &other.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "canonical needle belongs elsewhere"}),
                ),
            )
            .await
            .expect("append other");

        let response = store
            .search(SearchQuery {
                query: "canonical needle".into(),
                mode: SearchMode::Hybrid,
                filter: SearchFilter {
                    tenant_id: Some("tenant-a".into()),
                    project_scope: Some("project-alpha".into()),
                    session_id: None,
                },
                limit: None,
            })
            .await
            .expect("search");
        assert_eq!(response.requested_mode, SearchMode::Hybrid);
        assert_eq!(response.effective_mode, SearchMode::Fts);
        assert!(response.semantic_floor);
        assert_eq!(response.hits.len(), 1);
        let hit = &response.hits[0];
        assert_eq!(
            (hit.session_id.as_str(), hit.event_id),
            (target.id.as_str(), stored.event_id)
        );
        assert_eq!(
            hit.event,
            store
                .read(&target.id, ReadRange::default())
                .await
                .expect("canonical read")
                .events[0]
        );
        let projection = serde_json::to_string(&response).expect("serialize response");
        assert!(!projection.contains("known-secret-value"));
        assert!(projection.contains("[redacted]"));
        let secret_search = store
            .search(SearchQuery {
                query: "known secret value".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    project_scope: Some("project-alpha".into()),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search secret");
        assert!(secret_search.hits.is_empty());

        let outside_scope = store
            .search(SearchQuery {
                query: "canonical needle".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    tenant_id: Some("tenant-b".into()),
                    project_scope: Some("project-alpha".into()),
                    session_id: None,
                },
                limit: None,
            })
            .await
            .expect("cross-scope search");
        assert!(outside_scope.hits.is_empty());
    })
    .await;
}

#[tokio::test]
async fn canonical_search_index_tracks_fork_truncate_and_delete() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let parent = store
            .create(CreateSession {
                id: Some("search-parent".into()),
                project_scope: Some("project-alpha".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        store
            .append(
                &parent.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "inherited marker"}),
                ),
            )
            .await
            .expect("append inherited");
        let removable = store
            .append(
                &parent.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "removable marker"}),
                ),
            )
            .await
            .expect("append removable");
        let child = store
            .fork(&parent.id, removable.event_id, Some("search-child".into()))
            .await
            .expect("fork");

        let inherited = store
            .search(SearchQuery {
                query: "inherited marker".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    project_scope: Some("project-alpha".into()),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search inherited");
        assert_eq!(
            inherited
                .hits
                .iter()
                .map(|hit| hit.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["search-child", "search-parent"]
        );

        store
            .truncate(&parent.id, 1)
            .await
            .expect("truncate parent");
        let removable = store
            .search(SearchQuery {
                query: "removable marker".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    session_id: Some(parent.id.clone()),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search removed");
        assert!(removable.hits.is_empty());

        store
            .hard_delete(&child.child_session_id)
            .await
            .expect("delete child");
        let deleted = store
            .search(SearchQuery {
                query: "inherited marker".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    session_id: Some(child.child_session_id),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search deleted");
        assert!(deleted.hits.is_empty());
    })
    .await;
}

#[tokio::test]
async fn canonical_semantic_search_uses_the_injected_embedding_backend() {
    let hooks = StoreHooks {
        embedder: Arc::new(TestSemanticEmbedder),
        ..StoreHooks::default()
    };
    run_with_hooks(hooks, |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("semantic-session".into()),
                project_scope: Some("project-alpha".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        store
            .append(
                &session.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "prepare the release pipeline"}),
                ),
            )
            .await
            .expect("append related");
        store
            .append(
                &session.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "review typography spacing"}),
                ),
            )
            .await
            .expect("append unrelated");

        let response = store
            .search(SearchQuery {
                query: "shipping".into(),
                mode: SearchMode::Semantic,
                filter: SearchFilter {
                    project_scope: Some("project-alpha".into()),
                    ..SearchFilter::default()
                },
                limit: Some(1),
            })
            .await
            .expect("semantic search");
        assert_eq!(response.effective_mode, SearchMode::Semantic);
        assert_eq!(response.embedding_backend, "test-semantic");
        assert!(!response.semantic_floor);
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].event_id, 1);
        assert!(response.hits[0].fts_score.is_none());
        assert_eq!(response.hits[0].semantic_score, Some(1.0));
    })
    .await;
}

#[tokio::test]
async fn sqlite_rebuilds_a_missing_search_index_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("search-rebuild.sqlite");
    let store = SqliteSessionStore::open(&path).expect("open");
    store
        .create(CreateSession {
            id: Some("rebuild-session".into()),
            project_scope: Some("project-alpha".into()),
            ..CreateSession::default()
        })
        .await
        .expect("create");
    store
        .append(
            "rebuild-session",
            AppendEvent::new(
                SessionEventKind::Message,
                json!({"text": "recoverable search marker"}),
            ),
        )
        .await
        .expect("append");
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("open raw sqlite");
    connection
        .execute("DELETE FROM session_events_fts", [])
        .expect("remove fts index");
    connection
        .execute("DELETE FROM session_event_vectors", [])
        .expect("remove vector index");
    drop(connection);

    let store = SqliteSessionStore::open(&path).expect("reopen and rebuild");
    let response = store
        .search(SearchQuery {
            query: "recoverable marker".into(),
            mode: SearchMode::Fts,
            filter: SearchFilter {
                project_scope: Some("project-alpha".into()),
                ..SearchFilter::default()
            },
            limit: None,
        })
        .await
        .expect("search rebuilt index");
    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.hits[0].session_id, "rebuild-session");
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
    assert_eq!(schema_state, (4, false));
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
        StoreError::Backend(
            "schema initialization failed: backend error: session store schema version 99 is newer than supported version 4"
                .to_string()
        )
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
