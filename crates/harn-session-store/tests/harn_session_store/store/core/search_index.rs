//! Canonical search over the session store.
//!
//! Lexical, semantic, and hybrid retrieval plus the index lifecycle that
//! backs them: scoping, redaction, fallback reporting, fork/truncate/delete
//! tracking, and index rebuild on reopen.

use super::*;

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
        let punctuation_search = store
            .search(SearchQuery {
                query: "...".into(),
                mode: SearchMode::Fts,
                filter: SearchFilter {
                    project_scope: Some("project-alpha".into()),
                    ..SearchFilter::default()
                },
                limit: None,
            })
            .await
            .expect("search punctuation-only query");
        assert!(punctuation_search.hits.is_empty());

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
async fn canonical_hybrid_search_fuses_lexical_and_semantic_rankings() {
    let hooks = StoreHooks {
        embedder: Arc::new(FusionTestEmbedder),
        ..StoreHooks::default()
    };
    run_with_hooks(hooks, |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("hybrid-fusion".into()),
                project_scope: Some("project-hybrid".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        let lexical_winner = store
            .append(
                &session.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "fusion query fusion query"}),
                ),
            )
            .await
            .expect("append lexical winner");
        let joint_candidate = store
            .append(
                &session.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "fusion query joint-candidate with a longer lexical tail"}),
                ),
            )
            .await
            .expect("append joint candidate");
        let semantic_winner = store
            .append(
                &session.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "semantic-only-winner"}),
                ),
            )
            .await
            .expect("append semantic winner");

        let search = |mode| SearchQuery {
            query: "fusion query".into(),
            mode,
            filter: SearchFilter {
                project_scope: Some("project-hybrid".into()),
                ..SearchFilter::default()
            },
            limit: Some(2),
        };
        let lexical = store
            .search(search(SearchMode::Fts))
            .await
            .expect("FTS search");
        let semantic = store
            .search(search(SearchMode::Semantic))
            .await
            .expect("semantic search");
        let hybrid = store
            .search(search(SearchMode::Hybrid))
            .await
            .expect("hybrid search");

        assert_eq!(lexical.hits[0].event_id, lexical_winner.event_id);
        assert_eq!(semantic.hits[0].event_id, semantic_winner.event_id);
        assert_eq!(hybrid.hits[0].event_id, joint_candidate.event_id);
        assert_eq!(hybrid.requested_mode, SearchMode::Hybrid);
        assert_eq!(hybrid.effective_mode, SearchMode::Hybrid);
        assert!(!hybrid.semantic_floor);
        assert!(hybrid.fallback_reason.is_none());
        assert!(hybrid.hits[0].fts_score.is_some());
        assert!(hybrid.hits[0].semantic_score.is_some());
    })
    .await;
}

#[tokio::test]
async fn floor_hybrid_hits_are_byte_identical_to_lexical_hits() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let session = store
            .create(CreateSession {
                id: Some("hybrid-floor".into()),
                project_scope: Some("project-floor".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        for text in [
            "alpha beta alpha beta",
            "alpha beta with a longer lexical tail",
            "alpha appears before some filler and beta",
        ] {
            store
                .append(
                    &session.id,
                    AppendEvent::new(SessionEventKind::Message, json!({"text": text})),
                )
                .await
                .expect("append searchable event");
        }

        let search = |mode| SearchQuery {
            query: "alpha beta".into(),
            mode,
            filter: SearchFilter {
                project_scope: Some("project-floor".into()),
                ..SearchFilter::default()
            },
            limit: Some(3),
        };
        let lexical = store
            .search(search(SearchMode::Fts))
            .await
            .expect("FTS search");
        let hybrid = store
            .search(search(SearchMode::Hybrid))
            .await
            .expect("hybrid search");

        assert_eq!(hybrid.requested_mode, SearchMode::Hybrid);
        assert_eq!(hybrid.effective_mode, SearchMode::Fts);
        assert!(hybrid.semantic_floor);
        assert!(hybrid.fallback_reason.is_some());
        assert_eq!(
            serde_json::to_vec(&hybrid.hits).expect("serialize hybrid hits"),
            serde_json::to_vec(&lexical.hits).expect("serialize lexical hits")
        );
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
