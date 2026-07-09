//! HTTP adapter tests for the session-store router.

use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, MemorySessionStore, SessionEventKind, SessionMeta,
    SharedSessionStore,
};
use serde_json::json;

use super::api;

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
