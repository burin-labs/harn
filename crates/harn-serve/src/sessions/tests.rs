//! HTTP adapter tests for the session-store router.

use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, MemorySessionStore, SessionEventKind, SessionMeta,
    SharedSessionStore, StoreHooks,
};
use serde_json::json;

use super::api;

#[tokio::test]
async fn http_boundary_read_drives_an_acknowledged_fork() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use harn_session_store::{CanonicalHistoryBoundaries, CanonicalSessionBoundary, ReadRange};
    use tower::ServiceExt as _;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("saved-session.sqlite");
    let store: SharedSessionStore =
        Arc::new(harn_session_store::SqliteSessionStore::open(&database).unwrap());
    let router = api::sessions_router(store.clone());
    let get_boundaries = |id: &str| {
        Request::builder()
            .uri(format!("/sessions/{id}/boundaries"))
            .body(Body::empty())
            .unwrap()
    };
    let missing = router
        .clone()
        .oneshot(get_boundaries("missing"))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let parent = store.create(CreateSession::default()).await.unwrap();
    let response = router
        .clone()
        .oneshot(get_boundaries(&parent.id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let empty: CanonicalHistoryBoundaries = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(empty.tip, CanonicalSessionBoundary::empty(&parent.id));
    assert!(empty.positions.is_empty());

    let mut message = AppendEvent::new(
        SessionEventKind::Message,
        json!({"raw_message":{"role":"user","content":"first context"}}),
    );
    message
        .headers
        .insert("source_event_id".into(), "opaque-source-first".into());
    let first = store.append(&parent.id, message).await.unwrap();
    let receipt = store
        .append(
            &parent.id,
            AppendEvent::new(SessionEventKind::Receipt, json!({"status":"completed"})),
        )
        .await
        .unwrap();
    // Reopen the saved session without an active producer or in-memory journal.
    drop(router);
    drop(store);
    let store: SharedSessionStore =
        Arc::new(harn_session_store::SqliteSessionStore::open(&database).unwrap());
    let router = api::sessions_router(store.clone());
    let response = router
        .clone()
        .oneshot(get_boundaries(&parent.id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let acknowledged: CanonicalHistoryBoundaries = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        acknowledged.tip,
        CanonicalSessionBoundary::acknowledged(&receipt)
    );
    assert_eq!(acknowledged.positions.len(), 1);
    assert_eq!(
        acknowledged.positions[0].source_event_id,
        "opaque-source-first"
    );
    assert_eq!(
        acknowledged.positions[0].boundary,
        CanonicalSessionBoundary::acknowledged(&first)
    );

    let fork_request = |child: &str, boundary: CanonicalSessionBoundary| {
        Request::builder()
            .method("POST")
            .uri(format!("/sessions/{}/fork", parent.id))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"canonical_boundary":boundary,"child_session_id":child}).to_string(),
            ))
            .unwrap()
    };
    let mut wrong_hash = acknowledged.positions[0].boundary.clone();
    wrong_hash.record_hash = Some("unacknowledged-hash".into());
    let refused = router
        .clone()
        .oneshot(fork_request("refused-child", wrong_hash))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(matches!(
        store.describe("refused-child").await,
        Err(harn_session_store::StoreError::NotFound(_))
    ));

    let mut foreign = acknowledged.positions[0].boundary.clone();
    foreign.session_id = "foreign-parent".into();
    let refused = router
        .clone()
        .oneshot(fork_request("foreign-child", foreign))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(matches!(
        store.describe("foreign-child").await,
        Err(harn_session_store::StoreError::NotFound(_))
    ));

    let response = router
        .oneshot(fork_request(
            "acknowledged-child",
            acknowledged.positions[0].boundary.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let child = store.describe("acknowledged-child").await.unwrap();
    assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()));
    let copied = store.read(&child.id, ReadRange::default()).await.unwrap();
    assert_eq!(copied.events.len(), 1);
    assert_eq!(
        copied.events[0].payload["raw_message"]["content"],
        "first context"
    );
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

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/sessions/{}", meta.id))
                .header("content-type", "application/json")
                .body(Body::from(json!({"title": "Canonical title"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let updated: SessionMeta = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated.title.as_deref(), Some("Canonical title"));

    let body = json!({
        "kind": {"kind": "message"},
        "payload": {
            "transcript_event": {
                "kind": "message",
                "role": "user",
                "visibility": "public",
                "text": "hello",
                "metadata": {}
            }
        },
        "headers": {"run_id": "run-http", "turn_id": "turn-http"},
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
                .uri(format!("/sessions/{}/timeline", meta.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["nodes"][0]["children"], json!([]));
    assert_eq!(
        wire["nodes"][0]["links"],
        json!([
            {"kind": "run", "targetId": "run-http"},
            {"kind": "turn", "targetId": "turn-http"}
        ])
    );
    assert!(wire["nodes"][0].get("attributes").is_some());
    let timeline: harn_vm::session_timeline::SessionTimelineSnapshot =
        serde_json::from_slice(&bytes).unwrap();
    assert_eq!(timeline.query.session_id.as_deref(), Some(meta.id.as_str()));
    assert_eq!(timeline.nodes.len(), 1);
    assert_eq!(timeline.nodes[0].category, "message");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/sessions/{}/recap?limit=10", meta.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let recap: harn_vm::session_recap::SessionRecapAvailability =
        serde_json::from_slice(&bytes).unwrap();
    let harn_vm::session_recap::SessionRecapAvailability::Available { snapshot } = recap else {
        panic!("existing session must return an available recap")
    };
    assert_eq!(snapshot.coverage.scanned, 1);
    assert_eq!(snapshot.coverage.matched, 1);
    assert_eq!(snapshot.turns[0].prompts[0].text, "hello");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/missing/recap")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        json!({"state": "unavailable", "reason": "session_missing"})
    );

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
async fn http_router_returns_session_view() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    let store: SharedSessionStore = Arc::new(MemorySessionStore::new());
    let router = api::sessions_router(store.clone());
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create");
    store
        .append(
            &meta.id,
            AppendEvent::new(SessionEventKind::Message, json!({"text": "hello"})),
        )
        .await
        .expect("append");

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/view", meta.id))
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
    assert_eq!(body["schema"], "harn.session_view.v1");
    assert_eq!(body["session"]["session_id"], meta.id);
    assert_eq!(body["session"]["last_event_id"], 1);
    assert_eq!(body["metadata"]["event_count"], 1);
    assert!(body["projection"]["projection_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
}

#[tokio::test]
async fn http_router_searches_the_canonical_store() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    let store: SharedSessionStore = Arc::new(MemorySessionStore::with_hooks(StoreHooks::default()));
    let router = api::sessions_router(store.clone());
    let meta = store
        .create(CreateSession {
            tenant_id: Some("tenant-a".to_string()),
            project_scope: Some("project-a".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create");
    store
        .append(
            &meta.id,
            AppendEvent::new(
                SessionEventKind::Message,
                json!({"text": "canonical transcript needle"}),
            ),
        )
        .await
        .expect("append");

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/search")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "query": "transcript needle",
                        "mode": "hybrid",
                        "filter": {
                            "tenant_id": "tenant-a",
                            "project_scope": "project-a",
                            "session_id": meta.id,
                        },
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["requested_mode"], "hybrid");
    assert_eq!(body["effective_mode"], "fts");
    assert_eq!(body["semantic_floor"], true);
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);
    assert_eq!(body["hits"][0]["session_id"], meta.id);
    assert_eq!(body["hits"][0]["event_id"], 1);
}
