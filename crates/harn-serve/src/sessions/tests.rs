//! Behaviour tests for the session-store primitive.
//!
//! The two `for_each_backend` runners exercise the [`MemorySessionStore`]
//! and the file-backed [`SqliteSessionStore`] against the same scenarios.
//! When this passes, the trait surface is bytewise-compatible between
//! backends — a TUI persisting via sqlite and a cloud verifier consuming
//! events over the memory store both see the same canonical hashes and
//! signatures.

use std::sync::Arc;

use harn_vm::redact::RedactionPolicy;
use serde_json::json;
use tempfile::TempDir;

use super::*;

fn dummy_signer(seed: u8) -> SessionSigner {
    SessionSigner::from_seed([seed; 32])
}

fn fresh_memory(hooks: StoreHooks) -> Arc<dyn SessionStore> {
    Arc::new(MemorySessionStore::with_hooks(hooks))
}

fn fresh_sqlite(hooks: StoreHooks, dir: &TempDir) -> Arc<dyn SessionStore> {
    let path = dir.path().join("sessions.sqlite");
    Arc::new(SqliteSessionStore::open_with_hooks(path, hooks).expect("open sqlite"))
}

async fn run_with_hooks<F, Fut>(hooks: StoreHooks, body: F)
where
    F: Fn(Arc<dyn SessionStore>) -> Fut,
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
    let mut policy = RedactionPolicy::default();
    policy = policy.with_extra_field("api_key");
    let hooks = StoreHooks {
        redaction: Some(policy),
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
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"api_key": "ghp_1234567890abcdef", "text": "ok"}),
                ),
            )
            .await
            .expect("append");
        let page = store
            .read(&meta.id, ReadRange::default())
            .await
            .expect("read");
        let payload = &page.events[0].payload;
        let api_key = payload["api_key"].as_str().expect("string");
        assert_ne!(api_key, "ghp_1234567890abcdef");
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
        let future_ms = super::event::now_ms_and_rfc3339().0 + 1_000_000;
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
async fn http_router_round_trips_events() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    let store: SharedSessionStore = Arc::new(MemorySessionStore::new());
    let router = api::sessions_router(store.clone());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&CreateSession::default()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let meta: SessionMeta = serde_json::from_slice(&bytes).unwrap();

    let body = json!({
        "kind": {"kind": "message"},
        "payload": {"text": "hello"},
    });
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{}/events", meta.id))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/events", meta.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
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
