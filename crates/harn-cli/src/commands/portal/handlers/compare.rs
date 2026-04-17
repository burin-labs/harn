use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dto::PortalRunDiff;
use crate::commands::portal::errors::{internal_error, not_found_error};
use crate::commands::portal::query::{CompareQuery, ErrorResponse};
use crate::commands::portal::run_analysis::resolve_run_path;
use crate::commands::portal::state::PortalState;

pub(crate) async fn compare_runs_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<CompareQuery>,
) -> Result<Json<PortalRunDiff>, (StatusCode, Json<ErrorResponse>)> {
    let left_path = resolve_run_path(&state.run_dir, &query.left)?;
    let right_path = resolve_run_path(&state.run_dir, &query.right)?;
    let left = harn_vm::orchestration::load_run_record(&left_path).map_err(|error| {
        if left_path.exists() {
            internal_error(format!("failed to load left run: {error}"))
        } else {
            not_found_error(format!("left run not found: {}", query.left))
        }
    })?;
    let right = harn_vm::orchestration::load_run_record(&right_path).map_err(|error| {
        if right_path.exists() {
            internal_error(format!("failed to load right run: {error}"))
        } else {
            not_found_error(format!("right run not found: {}", query.right))
        }
    })?;
    let diff = harn_vm::orchestration::diff_run_records(&left, &right);
    Ok(Json(PortalRunDiff {
        left_path: query.left,
        right_path: query.right,
        identical: diff.identical,
        status_changed: diff.status_changed,
        left_status: diff.left_status,
        right_status: diff.right_status,
        stage_diffs: diff.stage_diffs,
        transition_count_delta: diff.transition_count_delta,
        artifact_count_delta: diff.artifact_count_delta,
        checkpoint_count_delta: diff.checkpoint_count_delta,
    }))
}
