use std::sync::Arc;

use axum::extract::{Json as ExtractJson, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dlq::{
    bulk_purge, bulk_replay, dlq_detail, export_entry, list_dlq_entries, purge_entry, replay_entry,
    DlqBulkRequest, DlqQuery, PortalDlqBulkResponse, PortalDlqEntry, PortalDlqListResponse,
};
use crate::commands::portal::dto::PortalLaunchJob;
use crate::commands::portal::errors::{bad_request_error, internal_error, not_found_error};
use crate::commands::portal::query::ErrorResponse;
use crate::commands::portal::state::PortalState;

pub(crate) async fn list_dlq_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<DlqQuery>,
) -> Result<Json<PortalDlqListResponse>, (StatusCode, Json<ErrorResponse>)> {
    list_dlq_entries(&state, &query)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn dlq_detail_handler(
    State(state): State<Arc<PortalState>>,
    Path(entry_id): Path<String>,
) -> Result<Json<PortalDlqEntry>, (StatusCode, Json<ErrorResponse>)> {
    dlq_detail(&state, &entry_id)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn replay_dlq_handler(
    State(state): State<Arc<PortalState>>,
    Path(entry_id): Path<String>,
) -> Result<Json<PortalLaunchJob>, (StatusCode, Json<ErrorResponse>)> {
    replay_entry(&state, &entry_id, false)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn replay_drift_accept_dlq_handler(
    State(state): State<Arc<PortalState>>,
    Path(entry_id): Path<String>,
) -> Result<Json<PortalLaunchJob>, (StatusCode, Json<ErrorResponse>)> {
    replay_entry(&state, &entry_id, true)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn purge_dlq_handler(
    State(state): State<Arc<PortalState>>,
    Path(entry_id): Path<String>,
) -> Result<Json<PortalDlqEntry>, (StatusCode, Json<ErrorResponse>)> {
    purge_entry(&state, &entry_id)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn export_dlq_handler(
    State(state): State<Arc<PortalState>>,
    Path(entry_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    export_entry(&state, &entry_id)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn bulk_replay_dlq_handler(
    State(state): State<Arc<PortalState>>,
    ExtractJson(request): ExtractJson<DlqBulkRequest>,
) -> Result<Json<PortalDlqBulkResponse>, (StatusCode, Json<ErrorResponse>)> {
    bulk_replay(&state, &request)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

pub(crate) async fn bulk_purge_dlq_handler(
    State(state): State<Arc<PortalState>>,
    ExtractJson(request): ExtractJson<DlqBulkRequest>,
) -> Result<Json<PortalDlqBulkResponse>, (StatusCode, Json<ErrorResponse>)> {
    bulk_purge(&state, &request)
        .await
        .map(Json)
        .map_err(map_dlq_error)
}

fn map_dlq_error(error: String) -> (StatusCode, Json<ErrorResponse>) {
    if error.contains("unknown DLQ entry") {
        not_found_error(error)
    } else if error.contains("invalid RFC3339") || error.contains("bulk operation matched") {
        bad_request_error(error)
    } else {
        internal_error(error)
    }
}
