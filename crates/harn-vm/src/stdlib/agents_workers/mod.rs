use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

mod audit;
mod bridge;
mod config;
mod execution;
mod policy;
mod worktree;

pub(super) use audit::inherited_worker_audit;
pub(super) use bridge::{emit_worker_event, worker_snapshot_path};
pub(super) use config::{
    load_worker_state_snapshot, parse_worker_config, parse_worker_execution_profile,
    persist_worker_state_snapshot,
};
pub(super) use execution::{execute_delegated_stage, spawn_worker_task};
pub(super) use policy::{apply_worker_artifact_policy, resolve_inherited_worker_policy};

#[derive(Clone)]
pub(super) enum WorkerConfig {
    Workflow {
        graph: Box<WorkflowGraph>,
        artifacts: Vec<ArtifactRecord>,
        options: BTreeMap<String, VmValue>,
    },
    Stage {
        node: Box<crate::orchestration::WorkflowNode>,
        artifacts: Vec<ArtifactRecord>,
        transcript: Option<VmValue>,
    },
    SubAgent {
        spec: Box<SubAgentRunSpec>,
    },
}

#[derive(Clone)]
pub(super) struct WorkerExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) execution: WorkerExecutionProfile,
}

#[derive(Clone, Default)]
pub(super) struct WorkerCarryPolicy {
    pub(super) artifact_mode: String,
    pub(super) context_policy: ContextPolicy,
    pub(super) resume_workflow: bool,
    pub(super) persist_state: bool,
    /// Capability policy scoped to this worker. Pushed onto the policy stack
    /// during execution and popped when the worker completes.
    pub(super) policy: Option<CapabilityPolicy>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct WorkerWorktreeSpec {
    pub(super) repo: String,
    pub(super) path: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) base_ref: Option<String>,
    pub(super) cleanup: Option<String>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct WorkerExecutionProfile {
    pub(super) cwd: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) worktree: Option<WorkerWorktreeSpec>,
}

pub(super) struct WorkerInit {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) config: WorkerConfig,
    pub(super) wait: bool,
    pub(super) carry_policy: WorkerCarryPolicy,
    pub(super) execution: WorkerExecutionProfile,
    pub(super) audit: MutationSessionRecord,
}

pub(super) struct WorkerState {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) task: String,
    pub(super) status: String,
    pub(super) created_at: String,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) mode: String,
    pub(super) history: Vec<String>,
    pub(super) config: WorkerConfig,
    pub(super) handle: Option<tokio::task::JoinHandle<Result<WorkerExecutionResult, VmError>>>,
    pub(super) cancel_token: Arc<AtomicBool>,
    pub(super) latest_payload: Option<serde_json::Value>,
    pub(super) latest_error: Option<String>,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) parent_worker_id: Option<String>,
    pub(super) parent_stage_id: Option<String>,
    pub(super) child_run_id: Option<String>,
    pub(super) child_run_path: Option<String>,
    pub(super) carry_policy: WorkerCarryPolicy,
    pub(super) execution: WorkerExecutionProfile,
    pub(super) snapshot_path: String,
    pub(super) audit: MutationSessionRecord,
}

thread_local! {
    pub(super) static WORKER_REGISTRY: RefCell<BTreeMap<String, Rc<RefCell<WorkerState>>>> = const { RefCell::new(BTreeMap::new()) };
    static WORKER_COUNTER: Cell<u64> = const { Cell::new(0) };
}

pub(super) fn next_worker_id() -> String {
    WORKER_COUNTER.with(|counter| {
        let next = counter.get() + 1;
        counter.set(next);
        format!("worker_{}", uuid::Uuid::now_v7())
    })
}

pub(super) fn worker_id_from_value(value: &VmValue) -> Result<String, VmError> {
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Dict(map) => match map.get("id") {
            Some(VmValue::String(id)) => Ok(id.to_string()),
            Some(other) => Ok(other.display()),
            None => Err(VmError::Runtime(
                "agent handle dict is missing an id field".to_string(),
            )),
        },
        VmValue::TaskHandle(id) => Ok(id.clone()),
        _ => Err(VmError::Runtime(
            "expected agent handle or worker id".to_string(),
        )),
    }
}

pub(super) fn clone_worker_state(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "_type": "agent_handle",
        "id": state.id,
        "name": state.name,
        "task": state.task,
        "mode": state.mode,
        "status": state.status,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "history": state.history,
        "result": state.latest_payload,
        "error": state.latest_error,
        "artifact_count": state.artifacts.len(),
        "has_transcript": state.transcript.is_some(),
        "parent_worker_id": state.parent_worker_id,
        "parent_stage_id": state.parent_stage_id,
        "child_run_id": state.child_run_id,
        "child_run_path": state.child_run_path,
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
    })
}

pub(super) fn worker_summary(state: &WorkerState) -> Result<VmValue, VmError> {
    to_vm(&clone_worker_state(state))
}

pub(super) fn with_worker_state<T>(
    worker_id: &str,
    f: impl FnOnce(Rc<RefCell<WorkerState>>) -> Result<T, VmError>,
) -> Result<T, VmError> {
    WORKER_REGISTRY.with(|registry| {
        let state = registry
            .borrow()
            .get(worker_id)
            .cloned()
            .ok_or_else(|| VmError::Runtime(format!("unknown worker: {worker_id}")))?;
        f(state)
    })
}

#[cfg(test)]
mod tests;
