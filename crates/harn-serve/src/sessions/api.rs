//! Axum router exposing the session-store primitive at `/v1/sessions`.
//!
//! Hosted alongside the existing api/a2a/mcp adapters. The router is
//! pure axum so callers can compose it with their own auth/observability
//! middleware before mounting.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::event::{AppendEvent, EventId, SessionEventKind};
use super::store::{
    CreateSession, ListFilter, ReadRange, SessionId, SessionStore, SharedSessionStore, SnapshotId,
    StoreError,
};

/// Build an unprefixed router. Callers nest it under whichever prefix
/// fits their deployment (e.g. `Router::new().nest("/v1/session-store",
/// sessions_router(store))`). The store is shared via `Arc` and cloned
/// cheaply per request.
pub fn sessions_router(store: SharedSessionStore) -> Router {
    Router::new()
        .route("/sessions", post(create_session).get(list_sessions))
        .route(
            "/sessions/{id}",
            get(describe_session).delete(soft_delete_session),
        )
        .route("/sessions/{id}/events", post(append_event).get(read_events))
        .route("/sessions/{id}/fork", post(fork_session))
        .route("/sessions/{id}/truncate", post(truncate_session))
        .route("/sessions/{id}/snapshot", post(snapshot_session))
        .route("/snapshots/{snapshot_id}/replay", post(replay_snapshot))
        .route("/sessions/{id}/close", post(close_session))
        .route("/sessions/{id}/verify", get(verify_session))
        .route("/sessions/{id}/hard_delete", delete(hard_delete_session))
        .with_state(SessionsState { store })
}

#[derive(Clone)]
struct SessionsState {
    store: Arc<dyn SessionStore>,
}

#[derive(Debug, Deserialize)]
struct AppendRequest {
    kind: SessionEventKind,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    parent_event_id: Option<EventId>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ForkRequest {
    at_event_id: EventId,
    #[serde(default)]
    child_session_id: Option<SessionId>,
}

#[derive(Debug, Deserialize)]
struct TruncateRequest {
    at_event_id: EventId,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorBodyInner,
}

#[derive(Debug, Serialize)]
struct ErrorBodyInner {
    code: &'static str,
    message: String,
}

fn map_error(error: StoreError) -> (StatusCode, Json<ErrorBody>) {
    let (status, code) = match &error {
        StoreError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        StoreError::AlreadyExists(_) => (StatusCode::CONFLICT, "already_exists"),
        StoreError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        StoreError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "invalid_input"),
        StoreError::Tenant(_) => (StatusCode::FORBIDDEN, "tenant"),
        StoreError::Backend(_) => (StatusCode::INTERNAL_SERVER_ERROR, "backend_error"),
    };
    (
        status,
        Json(ErrorBody {
            error: ErrorBodyInner {
                code,
                message: error.to_string(),
            },
        }),
    )
}

async fn create_session(
    State(state): State<SessionsState>,
    Json(payload): Json<CreateSession>,
) -> impl IntoResponse {
    match state.store.create(payload).await {
        Ok(meta) => (StatusCode::CREATED, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn list_sessions(
    State(state): State<SessionsState>,
    Query(filter): Query<ListFilter>,
) -> impl IntoResponse {
    match state.store.list(filter).await {
        Ok(metas) => (
            StatusCode::OK,
            Json(json!({
                "object": "list",
                "data": metas,
            })),
        )
            .into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn describe_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.describe(&id).await {
        Ok(meta) => (StatusCode::OK, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn soft_delete_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.soft_delete(&id).await {
        Ok(meta) => (StatusCode::OK, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn hard_delete_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.hard_delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn append_event(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
    Json(body): Json<AppendRequest>,
) -> impl IntoResponse {
    let event = AppendEvent {
        kind: body.kind,
        payload: body.payload,
        parent_event_id: body.parent_event_id,
        actor: body.actor,
        tags: body.tags,
        headers: body.headers,
    };
    match state.store.append(&id, event).await {
        Ok(stored) => (StatusCode::CREATED, Json(json!(stored))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn read_events(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
    Query(range): Query<ReadRange>,
) -> impl IntoResponse {
    match state.store.read(&id, range).await {
        Ok(page) => (
            StatusCode::OK,
            Json(json!({
                "object": "list",
                "data": page.events,
                "next_cursor": page.next_cursor,
            })),
        )
            .into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn fork_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
    Json(body): Json<ForkRequest>,
) -> impl IntoResponse {
    match state
        .store
        .fork(&id, body.at_event_id, body.child_session_id)
        .await
    {
        Ok(result) => (StatusCode::CREATED, Json(json!(result))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn truncate_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
    Json(body): Json<TruncateRequest>,
) -> impl IntoResponse {
    match state.store.truncate(&id, body.at_event_id).await {
        Ok(result) => (StatusCode::OK, Json(json!(result))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn snapshot_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.snapshot(&id).await {
        Ok(snapshot) => (StatusCode::CREATED, Json(json!(snapshot))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn replay_snapshot(
    State(state): State<SessionsState>,
    Path(snapshot_id): Path<String>,
) -> impl IntoResponse {
    match state.store.replay(&SnapshotId(snapshot_id)).await {
        Ok(snapshot) => (StatusCode::OK, Json(json!(snapshot))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn close_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.close(&id).await {
        Ok(receipt) => (StatusCode::OK, Json(json!(receipt))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

async fn verify_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.verify(&id).await {
        Ok(report) => (StatusCode::OK, Json(json!(report))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}
