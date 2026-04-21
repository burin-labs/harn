use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::commands::portal::dto::PortalTrustGraphResponse;
use crate::commands::portal::errors::internal_error;
use crate::commands::portal::query::{ErrorResponse, TrustGraphQuery};
use crate::commands::portal::state::PortalState;

pub(crate) async fn trust_graph_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<TrustGraphQuery>,
) -> Result<Json<PortalTrustGraphResponse>, (StatusCode, Json<ErrorResponse>)> {
    let event_log = state
        .event_log
        .clone()
        .ok_or_else(|| internal_error("portal is not attached to an event log"))?;
    let filters = harn_vm::TrustQueryFilters {
        agent: query.agent,
        action: query.action,
        limit: query.limit,
        grouped_by_trace: query.grouped_by_trace.unwrap_or(false),
        ..harn_vm::TrustQueryFilters::default()
    };
    let records = harn_vm::query_trust_records(&event_log, &filters)
        .await
        .map_err(|error| internal_error(format!("failed to query trust graph: {error}")))?;
    let chain = harn_vm::verify_trust_chain(&event_log)
        .await
        .map_err(|error| internal_error(format!("failed to verify trust graph: {error}")))?;
    let groups = filters
        .grouped_by_trace
        .then(|| harn_vm::group_trust_records_by_trace(&records));
    Ok(Json(PortalTrustGraphResponse {
        summary: harn_vm::summarize_trust_records(&records),
        records,
        groups,
        chain,
        topics: vec![
            harn_vm::TRUST_GRAPH_GLOBAL_TOPIC.to_string(),
            harn_vm::TRUST_GRAPH_LEGACY_GLOBAL_TOPIC.to_string(),
        ],
    }))
}
