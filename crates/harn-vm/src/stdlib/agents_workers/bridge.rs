use std::path::PathBuf;

use super::WorkerState;

fn worker_bridge_metadata(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "task": state.task,
        "mode": state.mode,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "artifact_count": state.artifacts.len(),
        "has_transcript": state.transcript.is_some(),
        "parent_worker_id": state.parent_worker_id,
        "parent_stage_id": state.parent_stage_id,
        "child_run_id": state.child_run_id,
        "child_run_path": state.child_run_path,
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
        "error": state.latest_error,
    })
}

pub(in super::super) fn emit_worker_event(state: &WorkerState, status: &str) {
    if let Some(bridge) = crate::llm::current_host_bridge() {
        let metadata = worker_bridge_metadata(state);
        bridge.send_worker_update(
            &state.id,
            &state.name,
            status,
            metadata.clone(),
            Some(&state.audit),
        );
        bridge.send_progress(
            "worker",
            &format!("{} {}", state.name, status),
            None,
            None,
            Some(serde_json::json!({
                "worker_id": state.id,
                "worker_name": state.name,
                "status": status,
                "metadata": metadata,
            })),
        );
    }
}

fn worker_state_dir() -> PathBuf {
    std::env::var("HARN_WORKER_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".harn/workers"))
}

pub(in super::super) fn worker_snapshot_path(worker_id: &str) -> String {
    worker_state_dir()
        .join(format!("{worker_id}.json"))
        .to_string_lossy()
        .into_owned()
}
