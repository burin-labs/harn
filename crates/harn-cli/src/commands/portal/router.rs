use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use super::assets::{asset, index};
use super::handlers::compare::compare_runs_handler;
use super::handlers::costs::cost_report_handler;
use super::handlers::dlq::{
    bulk_purge_dlq_handler, bulk_replay_dlq_handler, dlq_detail_handler, export_dlq_handler,
    list_dlq_handler, purge_dlq_handler, replay_dlq_handler, replay_drift_accept_dlq_handler,
};
use super::handlers::launch::{
    launch_run_handler, list_launch_jobs_handler, list_launch_targets_handler,
    trigger_replay_handler,
};
use super::handlers::meta::{highlight_keywords_handler, llm_options_handler, portal_meta_handler};
use super::handlers::runs::{action_graph_stream_handler, list_runs_handler, run_detail_handler};
use super::handlers::trust::trust_graph_handler;
use super::state::PortalState;

pub(super) fn build_router(state: Arc<PortalState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/meta", get(portal_meta_handler))
        .route("/api/highlight/keywords", get(highlight_keywords_handler))
        .route("/api/llm/options", get(llm_options_handler))
        .route("/api/costs", get(cost_report_handler))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/trust-graph", get(trust_graph_handler))
        .route("/api/run", get(run_detail_handler))
        .route(
            "/api/run/action-graph/stream",
            get(action_graph_stream_handler),
        )
        .route("/api/compare", get(compare_runs_handler))
        .route("/api/launch/targets", get(list_launch_targets_handler))
        .route("/api/launch/jobs", get(list_launch_jobs_handler))
        .route("/api/launch", post(launch_run_handler))
        .route("/api/trigger/replay", post(trigger_replay_handler))
        .route("/api/dlq", get(list_dlq_handler))
        .route("/api/dlq/bulk/replay", post(bulk_replay_dlq_handler))
        .route("/api/dlq/bulk/purge", post(bulk_purge_dlq_handler))
        .route("/api/dlq/{entry_id}", get(dlq_detail_handler))
        .route("/api/dlq/{entry_id}/replay", post(replay_dlq_handler))
        .route(
            "/api/dlq/{entry_id}/replay-drift-accept",
            post(replay_drift_accept_dlq_handler),
        )
        .route("/api/dlq/{entry_id}/purge", post(purge_dlq_handler))
        .route("/api/dlq/{entry_id}/export", get(export_dlq_handler))
        .route("/{*path}", get(index))
        .with_state(state)
}
