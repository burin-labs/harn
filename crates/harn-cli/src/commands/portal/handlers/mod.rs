use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::errors::forbidden_error;
use crate::commands::portal::query::ErrorResponse;
use crate::commands::portal::state::PortalState;

pub(super) mod compare;
pub(super) mod costs;
pub(super) mod dlq;
pub(super) mod launch;
pub(super) mod meta;
pub(super) mod personas;
pub(super) mod runs;
pub(super) mod trust;

pub(super) fn ensure_mutation_enabled(
    state: &PortalState,
    disabled_message: &'static str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if state.mutation_endpoints_enabled {
        Ok(())
    } else {
        Err(forbidden_error(disabled_message))
    }
}
