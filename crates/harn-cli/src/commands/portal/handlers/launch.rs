use std::sync::Arc;

use axum::extract::{Json as ExtractJson, State};
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dto::{
    PortalLaunchJob, PortalLaunchJobList, PortalLaunchRequest, PortalLaunchTargetList,
};
use crate::commands::portal::errors::internal_error;
use crate::commands::portal::launch::{create_launch_job, scan_launch_targets};
use crate::commands::portal::query::ErrorResponse;
use crate::commands::portal::state::PortalState;

pub(crate) async fn list_launch_targets_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalLaunchTargetList>, (StatusCode, Json<ErrorResponse>)> {
    let targets = scan_launch_targets(&state.workspace_root).map_err(internal_error)?;
    Ok(Json(PortalLaunchTargetList { targets }))
}

pub(crate) async fn list_launch_jobs_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalLaunchJobList>, (StatusCode, Json<ErrorResponse>)> {
    let jobs = state
        .launch_jobs
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    Ok(Json(PortalLaunchJobList { jobs }))
}

pub(crate) async fn launch_run_handler(
    State(state): State<Arc<PortalState>>,
    ExtractJson(request): ExtractJson<PortalLaunchRequest>,
) -> Result<Json<PortalLaunchJob>, (StatusCode, Json<ErrorResponse>)> {
    let job = create_launch_job(&state, request).await?;
    Ok(Json(job))
}
