use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::commands::persona;
use crate::commands::portal::errors::{bad_request_error, internal_error};
use crate::commands::portal::query::{ErrorResponse, PersonaStatusQuery};
use crate::commands::portal::state::PortalState;

pub(crate) async fn list_personas_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<ErrorResponse>)> {
    persona::list_payload(state.persona_manifest.as_deref())
        .map(Json)
        .map_err(internal_error)
}

pub(crate) async fn persona_status_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<PersonaStatusQuery>,
) -> Result<Json<harn_vm::PersonaStatus>, (StatusCode, Json<ErrorResponse>)> {
    if query.name.trim().is_empty() {
        return Err(bad_request_error("persona status requires non-empty name"));
    }
    persona::status_payload(
        state.persona_manifest.as_deref(),
        &state.persona_state_dir,
        &query.name,
        query.at.as_deref(),
    )
    .await
    .map(Json)
    .map_err(internal_error)
}
