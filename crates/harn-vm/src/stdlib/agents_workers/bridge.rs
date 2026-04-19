use std::path::PathBuf;

use crate::agent_events::WorkerEvent;
use crate::orchestration::MutationSessionRecord;

use super::{worker_provenance, WorkerState};

fn worker_bridge_metadata(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "task": state.task,
        "mode": state.mode,
        "request": state.request,
        "provenance": worker_provenance(state),
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

pub(in super::super) struct WorkerEventSnapshot {
    pub(in super::super) worker_id: String,
    pub(in super::super) worker_name: String,
    pub(in super::super) worker_task: String,
    pub(in super::super) worker_mode: String,
    pub(in super::super) metadata: serde_json::Value,
    pub(in super::super) audit: MutationSessionRecord,
}

pub(in super::super) async fn emit_worker_event(
    snapshot: &WorkerEventSnapshot,
    event: WorkerEvent,
) -> Result<(), crate::value::VmError> {
    let status = event.as_status();
    crate::orchestration::run_lifecycle_hooks(
        crate::orchestration::HookEvent::from_worker_event(event),
        &serde_json::json!({
            "event": event.as_str(),
            "worker": {
                "id": snapshot.worker_id,
                "name": snapshot.worker_name,
                "task": snapshot.worker_task,
                "mode": snapshot.worker_mode,
                "status": status,
                "metadata": snapshot.metadata.clone(),
            },
        }),
    )
    .await?;
    if let Some(bridge) = crate::llm::current_host_bridge() {
        bridge.send_worker_update(
            &snapshot.worker_id,
            &snapshot.worker_name,
            status,
            snapshot.metadata.clone(),
            Some(&snapshot.audit),
        );
        bridge.send_progress(
            "worker",
            &format!("{} {}", snapshot.worker_name, status),
            None,
            None,
            Some(serde_json::json!({
                "worker_id": snapshot.worker_id,
                "worker_name": snapshot.worker_name,
                "status": status,
                "metadata": snapshot.metadata,
            })),
        );
    }
    Ok(())
}

pub(in super::super) fn worker_event_snapshot(state: &WorkerState) -> WorkerEventSnapshot {
    WorkerEventSnapshot {
        worker_id: state.id.clone(),
        worker_name: state.name.clone(),
        worker_task: state.task.clone(),
        worker_mode: state.mode.clone(),
        metadata: worker_bridge_metadata(state),
        audit: state.audit.clone(),
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
