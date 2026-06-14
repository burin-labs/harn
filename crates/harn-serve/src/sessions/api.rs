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

use super::event::{AppendEvent, EventId, EventSignature, SessionEventKind};
use super::store::{
    CreateSession, ListFilter, ReadRange, SessionId, SessionMeta, SessionStatus, SessionStore,
    SharedSessionStore, SnapshotId, StoreError,
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
        .route("/sessions/{id}/view", get(session_view))
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

// Span names and attribute keys follow the published `harn.session.*`
// vocabulary so session-store telemetry can flow through any A.10 backend.
#[tracing::instrument(
    name = "harn.session.create",
    skip_all,
    fields(
        harn.session.tenant_id = payload.tenant_id.as_deref().unwrap_or(""),
        harn.session.persona = payload.persona.as_deref().unwrap_or(""),
    ),
)]
async fn create_session(
    State(state): State<SessionsState>,
    Json(payload): Json<CreateSession>,
) -> impl IntoResponse {
    match state.store.create(payload).await {
        Ok(meta) => (StatusCode::CREATED, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.list",
    skip_all,
    fields(
        harn.session.tenant_id = filter.tenant_id.as_deref().unwrap_or(""),
    ),
)]
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

#[tracing::instrument(
    name = "harn.session.describe",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn describe_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.describe(&id).await {
        Ok(meta) => (StatusCode::OK, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.view",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn session_view(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.describe(&id).await {
        Ok(meta) => {
            let view = session_view_from_meta(&meta);
            (StatusCode::OK, Json(json!(view))).into_response()
        }
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.soft_delete",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn soft_delete_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.soft_delete(&id).await {
        Ok(meta) => (StatusCode::OK, Json(json!(meta))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.hard_delete",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn hard_delete_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.hard_delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.append",
    skip_all,
    fields(
        harn.session.id = %id,
        harn.session.event_kind = body.kind.discriminator(),
        harn.session.signed = tracing::field::Empty,
        harn.session.signature_key_id = tracing::field::Empty,
        harn.session.signature_algorithm = tracing::field::Empty,
    ),
)]
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
        Ok(stored) => {
            record_signature_fields(
                stored.signed_by.as_ref(),
                "harn.session.signed",
                "harn.session.signature_key_id",
                "harn.session.signature_algorithm",
            );
            (StatusCode::CREATED, Json(json!(stored))).into_response()
        }
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.read",
    skip_all,
    fields(harn.session.id = %id),
)]
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

fn session_view_from_meta(meta: &SessionMeta) -> harn_vm::orchestration::SessionView {
    harn_vm::orchestration::build_session_view_from_run_views(
        Vec::new(),
        harn_vm::orchestration::SessionViewOptions {
            session_id: Some(meta.id.clone()),
            parent_session_id: meta.parent_session_id.clone(),
            status: Some(session_status_string(meta.status)),
            started_at: Some(meta.created_at.clone()),
            updated_at: Some(meta.updated_at.clone()),
            last_event_id: meta.last_event_id,
            chain_root_hash: meta.chain_root_hash.clone(),
            event_count: meta.event_count,
            has_event_log: true,
            ..harn_vm::orchestration::SessionViewOptions::default()
        },
    )
}

fn session_status_string(status: SessionStatus) -> String {
    match status {
        SessionStatus::Open => "open",
        SessionStatus::Closed => "closed",
        SessionStatus::SoftDeleted => "soft_deleted",
        SessionStatus::HardDeleted => "hard_deleted",
    }
    .to_string()
}

#[tracing::instrument(
    name = "harn.session.fork",
    skip_all,
    fields(
        harn.session.id = %id,
        harn.session.fork_at_event_id = body.at_event_id,
    ),
)]
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

#[tracing::instrument(
    name = "harn.session.truncate",
    skip_all,
    fields(
        harn.session.id = %id,
        harn.session.truncate_at_event_id = body.at_event_id,
    ),
)]
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

#[tracing::instrument(
    name = "harn.session.snapshot",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn snapshot_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.snapshot(&id).await {
        Ok(snapshot) => (StatusCode::CREATED, Json(json!(snapshot))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.replay",
    skip_all,
    fields(harn.session.snapshot_id = %snapshot_id),
)]
async fn replay_snapshot(
    State(state): State<SessionsState>,
    Path(snapshot_id): Path<String>,
) -> impl IntoResponse {
    match state.store.replay(&SnapshotId(snapshot_id)).await {
        Ok(snapshot) => (StatusCode::OK, Json(json!(snapshot))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.close",
    skip_all,
    fields(
        harn.session.id = %id,
        harn.session.receipt_signed = tracing::field::Empty,
        harn.session.receipt_signature_key_id = tracing::field::Empty,
        harn.session.receipt_signature_algorithm = tracing::field::Empty,
    ),
)]
async fn close_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.close(&id).await {
        Ok(receipt) => {
            record_signature_fields(
                receipt.signed_by.as_ref(),
                "harn.session.receipt_signed",
                "harn.session.receipt_signature_key_id",
                "harn.session.receipt_signature_algorithm",
            );
            (StatusCode::OK, Json(json!(receipt))).into_response()
        }
        Err(error) => map_error(error).into_response(),
    }
}

#[tracing::instrument(
    name = "harn.session.verify",
    skip_all,
    fields(harn.session.id = %id),
)]
async fn verify_session(
    State(state): State<SessionsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.store.verify(&id).await {
        Ok(report) => (StatusCode::OK, Json(json!(report))).into_response(),
        Err(error) => map_error(error).into_response(),
    }
}

fn record_signature_fields(
    signed_by: Option<&EventSignature>,
    signed_field: &'static str,
    key_id_field: &'static str,
    algorithm_field: &'static str,
) {
    let span = tracing::Span::current();
    match signed_by {
        Some(signature) => {
            span.record(signed_field, true);
            span.record(key_id_field, signature.key_id.as_str());
            span.record(algorithm_field, signature.algorithm.as_str());
        }
        None => {
            span.record(signed_field, false);
        }
    }
}
