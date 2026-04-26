use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::orchestration::{
    ArtifactRecord, CapabilityPolicy, ContextPolicy, MutationSessionRecord, WorkflowGraph,
};

mod audit;
mod bridge;
mod config;
mod execution;
mod policy;
mod worktree;

pub(super) use audit::inherited_worker_audit;
pub(super) use bridge::{emit_worker_event, worker_event_snapshot, worker_snapshot_path};
pub(super) use config::{
    load_worker_state_snapshot, parse_worker_config, parse_worker_execution_profile,
    persist_worker_state_snapshot,
};
pub(super) use execution::{
    ensure_worker_config_session_ids, execute_delegated_stage, spawn_worker_task,
};
pub(super) use policy::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, compact_worker_transcript,
    parse_worker_carry_policy, resolve_inherited_worker_policy,
};

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
    pub(super) transcript_mode: String,
    pub(super) context_policy: ContextPolicy,
    pub(super) resume_workflow: bool,
    pub(super) persist_state: bool,
    pub(super) retriggerable: bool,
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

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(super) struct WorkerRequestRecord {
    pub(super) task: String,
    pub(super) system: Option<String>,
    pub(super) payload: Option<serde_json::Value>,
    pub(super) research_questions: Vec<serde_json::Value>,
    pub(super) action_items: Vec<serde_json::Value>,
    pub(super) workflow_stages: Vec<serde_json::Value>,
    pub(super) verification_steps: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(super) struct WorkerProvenanceRecord {
    pub(super) worker_id: String,
    pub(super) worker_name: String,
    pub(super) mode: String,
    pub(super) parent_worker_id: Option<String>,
    pub(super) parent_stage_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) parent_session_id: Option<String>,
    pub(super) snapshot_path: String,
    pub(super) run_id: Option<String>,
    pub(super) run_path: Option<String>,
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
    pub(super) awaiting_started_at: Option<String>,
    pub(super) awaiting_since: Option<Instant>,
    pub(super) mode: String,
    pub(super) history: Vec<String>,
    pub(super) config: WorkerConfig,
    pub(super) handle: Option<tokio::task::JoinHandle<Result<WorkerExecutionResult, VmError>>>,
    pub(super) cancel_token: Arc<AtomicBool>,
    pub(super) request: WorkerRequestRecord,
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

pub(super) fn worker_trigger_payload_text(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.to_string(),
        _ => serde_json::to_string(&crate::llm::vm_value_to_json(value))
            .unwrap_or_else(|_| value.display()),
    }
}

pub(super) fn worker_wait_blocks(status: &str) -> bool {
    matches!(status, "running" | "awaiting")
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

fn request_items_from_json(value: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    match value {
        Some(serde_json::Value::Array(items)) => items.clone(),
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(value) => vec![value.clone()],
    }
}

fn request_items_from_vm_value(value: Option<&VmValue>) -> Vec<serde_json::Value> {
    value
        .map(crate::llm::vm_value_to_json)
        .map(|json| request_items_from_json(Some(&json)))
        .unwrap_or_default()
}

fn request_items_from_vm_dict(
    dict: &BTreeMap<String, VmValue>,
    keys: &[&str],
) -> Vec<serde_json::Value> {
    keys.iter()
        .find_map(|key| {
            let items = request_items_from_vm_value(dict.get(*key));
            (!items.is_empty()).then_some(items)
        })
        .unwrap_or_default()
}

fn request_items_from_json_dict(
    dict: &BTreeMap<String, serde_json::Value>,
    keys: &[&str],
) -> Vec<serde_json::Value> {
    keys.iter()
        .find_map(|key| {
            let items = request_items_from_json(dict.get(*key));
            (!items.is_empty()).then_some(items)
        })
        .unwrap_or_default()
}

fn canonical_request_payload(
    research_questions: &[serde_json::Value],
    action_items: &[serde_json::Value],
    workflow_stages: &[serde_json::Value],
    verification_steps: &[serde_json::Value],
) -> Option<serde_json::Value> {
    let mut payload = serde_json::Map::new();
    if !research_questions.is_empty() {
        payload.insert(
            "research_questions".to_string(),
            serde_json::Value::Array(research_questions.to_vec()),
        );
    }
    if !action_items.is_empty() {
        payload.insert(
            "action_items".to_string(),
            serde_json::Value::Array(action_items.to_vec()),
        );
    }
    if !workflow_stages.is_empty() {
        payload.insert(
            "workflow_stages".to_string(),
            serde_json::Value::Array(workflow_stages.to_vec()),
        );
    }
    if !verification_steps.is_empty() {
        payload.insert(
            "verification_steps".to_string(),
            serde_json::Value::Array(verification_steps.to_vec()),
        );
    }
    (!payload.is_empty()).then_some(serde_json::Value::Object(payload))
}

fn worker_request_from_vm_dict(
    task: &str,
    system: Option<String>,
    dict: &BTreeMap<String, VmValue>,
) -> WorkerRequestRecord {
    let research_questions = request_items_from_vm_dict(dict, &["research_questions", "questions"]);
    let action_items = request_items_from_vm_dict(dict, &["action_items", "actions"]);
    let workflow_stages = request_items_from_vm_dict(dict, &["workflow_stages", "stages"]);
    let verification_steps =
        request_items_from_vm_dict(dict, &["verification_steps", "verification"]);
    let payload = dict
        .get("request")
        .map(crate::llm::vm_value_to_json)
        .or_else(|| {
            canonical_request_payload(
                &research_questions,
                &action_items,
                &workflow_stages,
                &verification_steps,
            )
        });
    WorkerRequestRecord {
        task: task.to_string(),
        system,
        payload,
        research_questions,
        action_items,
        workflow_stages,
        verification_steps,
    }
}

fn worker_request_from_json_dict(
    task: &str,
    system: Option<String>,
    dict: &BTreeMap<String, serde_json::Value>,
) -> WorkerRequestRecord {
    let research_questions =
        request_items_from_json_dict(dict, &["research_questions", "questions"]);
    let action_items = request_items_from_json_dict(dict, &["action_items", "actions"]);
    let workflow_stages = request_items_from_json_dict(dict, &["workflow_stages", "stages"]);
    let verification_steps =
        request_items_from_json_dict(dict, &["verification_steps", "verification"]);
    let payload = dict.get("request").cloned().or_else(|| {
        canonical_request_payload(
            &research_questions,
            &action_items,
            &workflow_stages,
            &verification_steps,
        )
    });
    WorkerRequestRecord {
        task: task.to_string(),
        system,
        payload,
        research_questions,
        action_items,
        workflow_stages,
        verification_steps,
    }
}

pub(super) fn worker_request_for_config(task: &str, config: &WorkerConfig) -> WorkerRequestRecord {
    match config {
        WorkerConfig::Workflow { graph, options, .. } => {
            let options_request = worker_request_from_vm_dict(task, None, options);
            if options_request.payload.is_some()
                || !options_request.research_questions.is_empty()
                || !options_request.action_items.is_empty()
                || !options_request.workflow_stages.is_empty()
                || !options_request.verification_steps.is_empty()
            {
                return options_request;
            }
            worker_request_from_json_dict(task, None, &graph.metadata)
        }
        WorkerConfig::Stage { node, .. } => {
            worker_request_from_json_dict(task, node.system.clone(), &node.metadata)
        }
        WorkerConfig::SubAgent { spec } => {
            worker_request_from_vm_dict(task, spec.system.clone(), &spec.options)
        }
    }
}

pub(super) fn worker_provenance(state: &WorkerState) -> WorkerProvenanceRecord {
    WorkerProvenanceRecord {
        worker_id: state.id.clone(),
        worker_name: state.name.clone(),
        mode: state.mode.clone(),
        parent_worker_id: state.parent_worker_id.clone(),
        parent_stage_id: state.parent_stage_id.clone(),
        session_id: if state.audit.session_id.is_empty() {
            None
        } else {
            Some(state.audit.session_id.clone())
        },
        parent_session_id: state.audit.parent_session_id.clone(),
        snapshot_path: state.snapshot_path.clone(),
        run_id: state.child_run_id.clone(),
        run_path: state.child_run_path.clone(),
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
        "awaiting_started_at": state.awaiting_started_at,
        "history": state.history,
        "request": state.request,
        "provenance": worker_provenance(state),
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
