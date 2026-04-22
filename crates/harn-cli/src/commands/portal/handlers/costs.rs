use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dto::PortalCostReport;
use crate::commands::portal::errors::internal_error;
use crate::commands::portal::query::ErrorResponse;
use crate::commands::portal::run_analysis::build_cost_report;
use crate::commands::portal::state::PortalState;

pub(crate) async fn cost_report_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalCostReport>, (StatusCode, Json<ErrorResponse>)> {
    build_cost_report(&state.run_dir)
        .map(Json)
        .map_err(internal_error)
}
