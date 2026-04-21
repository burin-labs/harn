use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures::StreamExt;
use harn_vm::event_log::{EventLog, Topic};
use serde_json::json;

use crate::commands::portal::dto::{PortalListResponse, PortalPagination, PortalRunDetail};
use crate::commands::portal::errors::{bad_request_error, internal_error, not_found_error};
use crate::commands::portal::query::{ErrorResponse, ListRunsQuery, RunQuery};
use crate::commands::portal::run_analysis::{
    build_run_detail, filter_and_sort_runs, resolve_run_path, scan_runs, summarize_runs,
};
use crate::commands::portal::state::PortalState;

pub(crate) async fn list_runs_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<ListRunsQuery>,
) -> Result<Json<PortalListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let runs = scan_runs(&state.run_dir).map_err(internal_error)?;
    let stats = summarize_runs(&runs);
    let page_size = query.page_size.unwrap_or(25).clamp(1, 200);
    let page = query.page.unwrap_or(1).max(1);
    let filtered = filter_and_sort_runs(runs, &query);
    let filtered_count = filtered.len();
    let total_pages = usize::max(1, filtered_count.div_ceil(page_size));
    let clamped_page = page.min(total_pages);
    let start = (clamped_page - 1) * page_size;
    let end = usize::min(start + page_size, filtered_count);
    let page_runs = filtered
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>();
    Ok(Json(PortalListResponse {
        stats,
        filtered_count,
        pagination: PortalPagination {
            page: clamped_page,
            page_size,
            total_pages,
            total_runs: filtered_count,
            has_previous: clamped_page > 1,
            has_next: clamped_page < total_pages,
        },
        runs: page_runs,
    }))
}

pub(crate) async fn run_detail_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<RunQuery>,
) -> Result<Json<PortalRunDetail>, (StatusCode, Json<ErrorResponse>)> {
    let path = resolve_run_path(&state.run_dir, &query.path)?;
    let run = harn_vm::orchestration::load_run_record(&path).map_err(|error| {
        if path.exists() {
            internal_error(format!("failed to load run record: {error}"))
        } else {
            not_found_error(format!("run record not found: {}", query.path))
        }
    })?;
    Ok(Json(build_run_detail(&state.run_dir, &query.path, &run)))
}

pub(crate) async fn action_graph_stream_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<RunQuery>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<ErrorResponse>)>
{
    let path = resolve_run_path(&state.run_dir, &query.path)?;
    let run = harn_vm::orchestration::load_run_record(&path).map_err(|error| {
        if path.exists() {
            internal_error(format!("failed to load run record: {error}"))
        } else {
            not_found_error(format!("run record not found: {}", query.path))
        }
    })?;
    let observability = harn_vm::orchestration::derive_run_observability(&run, Some(&path));
    let trace_id = observability
        .action_graph_nodes
        .iter()
        .find_map(|node| node.trace_id.clone())
        .ok_or_else(|| bad_request_error("run does not have an action-graph trace_id"))?;
    let event_log = state
        .event_log
        .clone()
        .ok_or_else(|| internal_error("portal event log is unavailable for streaming"))?;
    let topic = Topic::new("observability.action_graph")
        .map_err(|error| internal_error(format!("invalid action graph topic: {error}")))?;
    let run_id = run.id.clone();
    let stream = event_log
        .subscribe(&topic, None)
        .await
        .map_err(|error| internal_error(format!("failed to subscribe to action graph: {error}")))?
        .filter_map(move |item| {
            let trace_id = trace_id.clone();
            let run_id = run_id.clone();
            async move {
                let Ok((event_id, event)) = item else {
                    return None;
                };
                let matches_trace = event.headers.get("trace_id") == Some(&trace_id)
                    || event.headers.get("run_id") == Some(&run_id)
                    || event.payload.get("trace_id").and_then(|value| value.as_str()) == Some(trace_id.as_str())
                    || event.payload.get("run_id").and_then(|value| value.as_str()) == Some(run_id.as_str());
                if !matches_trace {
                    return None;
                }
                let payload = json!({
                    "id": event_id,
                    "kind": event.kind,
                    "headers": event.headers,
                    "payload": event.payload,
                });
                let encoded = serde_json::to_string(&payload).ok()?;
                Some(Ok(Event::default().event("action_graph_update").data(encoded)))
            }
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
