use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dto::{PortalHighlightKeywords, PortalLlmOptions, PortalMeta};
use crate::commands::portal::highlight::build_highlight_keywords;
use crate::commands::portal::llm::build_llm_options;
use crate::commands::portal::query::ErrorResponse;
use crate::commands::portal::state::PortalState;

pub(crate) async fn portal_meta_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalMeta>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(PortalMeta {
        workspace_root: state.workspace_root.display().to_string(),
        run_dir: state.run_dir.display().to_string(),
    }))
}

pub(crate) async fn highlight_keywords_handler(
) -> Result<Json<PortalHighlightKeywords>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(build_highlight_keywords()))
}

pub(crate) async fn llm_options_handler(
) -> Result<Json<PortalLlmOptions>, (StatusCode, Json<ErrorResponse>)> {
    let options = build_llm_options().await;
    Ok(Json(options))
}
