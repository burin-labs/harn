//! Agent orchestration primitives.
//!
//! Provides the host execution primitives used by the Harn-authored agent
//! stdlib.

#[path = "agents_workers/mod.rs"]
pub(super) mod agents_workers;
#[path = "records.rs"]
pub(super) mod records;
#[path = "agents_sub_agent.rs"]
mod sub_agent;
#[path = "workflow/mod.rs"]
pub(super) mod workflow;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use self::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, emit_worker_event,
    ensure_worker_config_session_ids, load_worker_state_snapshot, next_worker_id,
    parse_worker_config, persist_worker_state_snapshot, spawn_worker_task, with_worker_state,
    worker_event_snapshot, worker_id_from_value, worker_request_for_config, worker_snapshot_path,
    worker_summary, worker_trigger_payload_text, worker_wait_blocks, WorkerConfig, WorkerState,
    WORKER_REGISTRY,
};
use self::sub_agent::{execute_sub_agent, parse_sub_agent_request};
use crate::orchestration::ArtifactRecord;
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

const AGENT_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("__host_worker_resume", resume_agent_builtin)
        .signature("__host_worker_resume(worker_or_snapshot)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Resume a persisted worker into the local host worker registry."),
    SyncBuiltin::new("__host_worker_list", list_agents_builtin)
        .signature("__host_worker_list()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("List local host worker summaries."),
];

const AGENT_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!("__host_sub_agent_run", sub_agent_run_builtin)
        .signature("__host_sub_agent_run(request)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Run or spawn a normalized Harn-authored sub-agent request."),
    async_builtin!("__host_worker_spawn", spawn_agent_builtin)
        .signature("__host_worker_spawn(config)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Spawn a workflow, stage, or sub-agent host worker."),
    async_builtin!("__host_worker_send_input", send_input_builtin)
        .signature("__host_worker_send_input(worker, task)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Resume a stopped host worker with new task input."),
    async_builtin!("__host_worker_trigger", worker_trigger_builtin)
        .signature("__host_worker_trigger(worker, payload)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Trigger an awaiting retriggerable host worker."),
    async_builtin!("__host_worker_wait", wait_agent_builtin)
        .signature("__host_worker_wait(worker_or_workers)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Wait for one or more host workers to reach a terminal state."),
    async_builtin!("__host_worker_close", close_agent_builtin)
        .signature("__host_worker_close(worker)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Cancel a host worker and emit the cancellation lifecycle event."),
];

const AGENT_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("agent.worker")
    .sync(AGENT_SYNC_PRIMITIVES)
    .async_(AGENT_ASYNC_PRIMITIVES);

pub(crate) use self::records::{parse_artifact_list, parse_context_policy};
fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("agents encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[derive(Clone, Debug, Default)]
pub(super) struct SubAgentRunSpec {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) system: Option<String>,
    pub(super) options: BTreeMap<String, VmValue>,
    pub(super) returns_schema: Option<VmValue>,
    pub(super) session_id: String,
    pub(super) parent_session_id: Option<String>,
}

pub(super) struct SubAgentExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: VmValue,
}

struct WorkerReplayCarry {
    artifacts: Vec<ArtifactRecord>,
    transcript: Option<VmValue>,
    parent_worker_id: String,
    worker_id: String,
    resume_workflow: bool,
    child_run_path: Option<String>,
    reset_sub_agent_session: bool,
}

fn restart_worker_run(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) -> Result<(), VmError> {
    reset_worker_for_replay(worker, next_task, clear_latest_payload)
}

fn reset_worker_for_replay(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) -> Result<(), VmError> {
    reset_worker_runtime_state(worker, next_task, clear_latest_payload);
    let carry = worker_replay_carry(worker)?;
    worker.transcript = carry.transcript.clone();
    ensure_worker_config_session_ids(&mut worker.config, &carry.worker_id);
    apply_worker_replay_config(&mut worker.config, next_task, carry);
    Ok(())
}

fn reset_worker_runtime_state(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) {
    worker.cancel_token = Arc::new(AtomicBool::new(false));
    worker.task = next_task.to_string();
    worker.history.push(next_task.to_string());
    worker.status = "running".to_string();
    worker.started_at = uuid::Uuid::now_v7().to_string();
    worker.finished_at = None;
    worker.awaiting_started_at = None;
    worker.awaiting_since = None;
    worker.latest_error = None;
    if clear_latest_payload {
        worker.latest_payload = None;
    }
}

fn worker_replay_carry(worker: &WorkerState) -> Result<WorkerReplayCarry, VmError> {
    Ok(WorkerReplayCarry {
        artifacts: apply_worker_artifact_policy(&worker.artifacts, &worker.carry_policy),
        transcript: apply_worker_transcript_policy(
            worker.transcript.clone(),
            &worker.carry_policy,
        )?,
        parent_worker_id: worker.id.clone(),
        worker_id: worker.id.clone(),
        resume_workflow: worker.carry_policy.resume_workflow,
        child_run_path: worker.child_run_path.clone(),
        reset_sub_agent_session: matches!(
            worker.carry_policy.transcript_mode.as_str(),
            "fork" | "reset"
        ),
    })
}

fn apply_worker_replay_config(
    config: &mut WorkerConfig,
    next_task: &str,
    carry: WorkerReplayCarry,
) {
    match config {
        WorkerConfig::Workflow {
            artifacts, options, ..
        } => apply_workflow_replay_config(artifacts, options, carry),
        WorkerConfig::Stage {
            artifacts,
            transcript,
            ..
        } => apply_stage_replay_config(artifacts, transcript, carry),
        WorkerConfig::SubAgent { spec } => {
            apply_sub_agent_replay_config(spec, next_task, carry.reset_sub_agent_session);
        }
    }
}

fn apply_workflow_replay_config(
    artifacts: &mut Vec<ArtifactRecord>,
    options: &mut BTreeMap<String, VmValue>,
    carry: WorkerReplayCarry,
) {
    if !carry.artifacts.is_empty() {
        *artifacts = carry.artifacts;
    }
    options.insert(
        "parent_worker_id".to_string(),
        VmValue::String(Rc::from(carry.parent_worker_id)),
    );
    if let Some(transcript) = carry.transcript {
        options.insert("transcript".to_string(), transcript);
    } else {
        options.remove("transcript");
    }
    if carry.resume_workflow {
        if let Some(child_run_path) = carry.child_run_path {
            options.insert(
                "resume_path".to_string(),
                VmValue::String(Rc::from(child_run_path)),
            );
        }
    } else {
        options.remove("resume_path");
    }
}

fn apply_stage_replay_config(
    artifacts: &mut Vec<ArtifactRecord>,
    transcript: &mut Option<VmValue>,
    carry: WorkerReplayCarry,
) {
    if !carry.artifacts.is_empty() {
        *artifacts = carry.artifacts;
    }
    *transcript = carry.transcript;
}

fn apply_sub_agent_replay_config(spec: &mut SubAgentRunSpec, next_task: &str, reset_session: bool) {
    spec.task = next_task.to_string();
    if reset_session {
        spec.session_id = format!("sub_agent_session_{}", uuid::Uuid::now_v7());
        spec.options.insert(
            "session_id".to_string(),
            VmValue::String(Rc::from(spec.session_id.clone())),
        );
    }
}

fn respawn_worker_task(state: Rc<RefCell<WorkerState>>) -> Result<(), VmError> {
    {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    spawn_worker_task(state);
    Ok(())
}

async fn wait_for_worker_terminal(
    state: Rc<RefCell<WorkerState>>,
    context: &str,
) -> Result<(), VmError> {
    loop {
        let handle = state.borrow_mut().handle.take();
        if let Some(handle) = handle {
            let _ = handle
                .await
                .map_err(|error| VmError::Runtime(format!("{context} join error: {error}")))??;
            continue;
        }
        if !worker_wait_blocks(&state.borrow().status) {
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    register_builtin_group(vm, AGENT_PRIMITIVES);
    records::register_record_builtins(vm);
    workflow::register_workflow_builtins(vm);
}

async fn sub_agent_run_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let request = parse_sub_agent_request(&args)?;
    if !request.background {
        let result = execute_sub_agent(request.spec).await?;
        return Ok(crate::stdlib::json_to_vm_value(&result.payload));
    }

    let worker_id = next_worker_id();
    let created_at = uuid::Uuid::now_v7().to_string();
    let mut audit = agents_workers::inherited_worker_audit("sub_agent");
    audit.worker_id = Some(worker_id.clone());
    let execution = request.execution;
    let worker_policy = request.worker_policy;
    let mut carry_policy = request.carry_policy;
    carry_policy.policy = worker_policy;
    let spec = request.spec;
    let worker_name = spec.name.clone();
    let worker_task = spec.task.clone();
    let mut config = WorkerConfig::SubAgent {
        spec: Box::new(spec),
    };
    ensure_worker_config_session_ids(&mut config, &worker_id);
    let original_request = worker_request_for_config(&worker_task, &config);
    let state = Rc::new(RefCell::new(WorkerState {
        id: worker_id.clone(),
        name: worker_name,
        task: worker_task.clone(),
        status: "running".to_string(),
        created_at: created_at.clone(),
        started_at: created_at,
        finished_at: None,
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "sub_agent".to_string(),
        history: vec![worker_task],
        config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        request: original_request,
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: None,
        child_run_id: None,
        child_run_path: None,
        carry_policy,
        execution,
        snapshot_path: worker_snapshot_path(&worker_id),
        audit,
    }));
    finalize_and_run_worker(state, false, "sub_agent worker").await
}

/// Persist + register + spawn a freshly-built worker, optionally block until
/// it reaches a terminal state, and return the worker summary dict.
///
/// Shared by `spawn_agent_builtin` (background or `wait: true`) and
/// `sub_agent_run_builtin` (background) so persistence / registry / task
/// spawn / summary stay in one place.
async fn finalize_and_run_worker(
    state: Rc<RefCell<WorkerState>>,
    wait_for_terminal: bool,
    wait_context: &'static str,
) -> Result<VmValue, VmError> {
    {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    let worker_id = state.borrow().id.clone();
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(worker_id, state.clone());
    });
    spawn_worker_task(state.clone());
    if wait_for_terminal {
        wait_for_worker_terminal(state.clone(), wait_context).await?;
    }
    worker_summary(&state.borrow())
}

fn worker_mode_label(config: &WorkerConfig) -> &'static str {
    match config {
        WorkerConfig::Workflow { .. } => "workflow",
        WorkerConfig::Stage { .. } => "stage",
        WorkerConfig::SubAgent { .. } => "sub_agent",
    }
}

async fn spawn_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .ok_or_else(|| VmError::Runtime("spawn_agent: missing config".to_string()))?;
    let mut init = parse_worker_config(config)?;
    let worker_id = next_worker_id();
    let created_at = uuid::Uuid::now_v7().to_string();
    ensure_worker_config_session_ids(&mut init.config, &worker_id);
    let mode = worker_mode_label(&init.config).to_string();
    let mut audit = init.audit.clone().normalize();
    audit.worker_id = Some(worker_id.clone());
    audit.execution_kind = Some(mode.clone());
    let original_request = worker_request_for_config(&init.task, &init.config);
    let state = Rc::new(RefCell::new(WorkerState {
        id: worker_id.clone(),
        name: init.name,
        task: init.task.clone(),
        status: "running".to_string(),
        created_at: created_at.clone(),
        started_at: created_at,
        finished_at: None,
        awaiting_started_at: None,
        awaiting_since: None,
        mode,
        history: vec![init.task],
        config: init.config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        request: original_request,
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: None,
        child_run_id: None,
        child_run_path: None,
        carry_policy: init.carry_policy,
        execution: init.execution,
        snapshot_path: worker_snapshot_path(&worker_id),
        audit,
    }));
    finalize_and_run_worker(state, init.wait, "spawn_agent worker").await
}

async fn send_input_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "send_input: requires worker handle and task text".to_string(),
        ));
    }
    let worker_id = worker_id_from_value(&args[0])?;
    let next_task = args[1].display();
    if next_task.is_empty() {
        return Err(VmError::Runtime(
            "send_input: task text must not be empty".to_string(),
        ));
    }
    with_worker_state(&worker_id, |state| {
        let mut worker = state.borrow_mut();
        if worker.status == "running" {
            return Err(VmError::Runtime(format!(
                "send_input: worker {} is still running",
                worker.id
            )));
        }
        restart_worker_run(&mut worker, &next_task, true)?;
        drop(worker);
        respawn_worker_task(state.clone())?;
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    })
}

async fn worker_trigger_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "worker_trigger: requires worker handle and payload".to_string(),
        ));
    }
    let worker_id = worker_id_from_value(&args[0])?;
    let next_task = worker_trigger_payload_text(&args[1]);
    if next_task.trim().is_empty() {
        return Err(VmError::Runtime(
            "worker_trigger: payload must not be empty".to_string(),
        ));
    }
    // Snapshot the about-to-resume worker state and validate
    // preconditions in a `with_worker_state` borrow that also
    // restarts the run. The borrow has to be dropped before we
    // await the lifecycle event emission, so we pull the snapshot
    // out and run `emit_worker_event` after the closure returns.
    let progressed_snapshot = with_worker_state(&worker_id, |state| {
        let mut worker = state.borrow_mut();
        if !worker.carry_policy.retriggerable {
            return Err(VmError::Runtime(format!(
                "worker_trigger: worker {} is not retriggerable",
                worker.id
            )));
        }
        if worker.status == "running" {
            return Err(VmError::Runtime(format!(
                "worker_trigger: worker {} is still running",
                worker.id
            )));
        }
        if worker.status != "awaiting" {
            return Err(VmError::Runtime(format!(
                "worker_trigger: worker {} is not awaiting (status={})",
                worker.id, worker.status
            )));
        }
        restart_worker_run(&mut worker, &next_task, false)?;
        let snapshot = worker_event_snapshot(&worker);
        drop(worker);
        respawn_worker_task(state.clone())?;
        Ok(snapshot)
    })?;
    // Emit the progressed lifecycle *after* the new cycle has been
    // spawned. Hosts see `progressed` (re-arming the run) followed
    // by `running` from the inner `WorkerSpawned` emission.
    emit_worker_event(
        &progressed_snapshot,
        crate::agent_events::WorkerEvent::WorkerProgressed,
    )
    .await?;
    with_worker_state(&worker_id, |state| worker_summary(&state.borrow()))
}

fn resume_agent_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("resume_agent: missing worker id or snapshot path".to_string())
        })?;
    let state = Rc::new(RefCell::new(load_worker_state_snapshot(&target)?));
    let worker_id = state.borrow().id.clone();
    {
        let mut worker = state.borrow_mut();
        ensure_worker_config_session_ids(&mut worker.config, &worker_id);
    }
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(worker_id, state.clone());
    });
    if state.borrow().carry_policy.persist_state {
        persist_worker_state_snapshot(&state.borrow())?;
    }
    let summary = worker_summary(&state.borrow())?;
    Ok(summary)
}

async fn wait_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("wait_agent: missing worker handle".to_string()))?;
    if let VmValue::List(list) = target {
        let mut results = Vec::new();
        for item in list.iter() {
            let worker_id = worker_id_from_value(item)?;
            let state = with_worker_state(&worker_id, Ok)?;
            wait_for_worker_terminal(state.clone(), "wait_agent").await?;
            results.push(worker_summary(&state.borrow())?);
        }
        return Ok(VmValue::List(Rc::new(results)));
    }
    let worker_id = worker_id_from_value(target)?;
    let state = with_worker_state(&worker_id, Ok)?;
    wait_for_worker_terminal(state.clone(), "wait_agent").await?;
    let summary = worker_summary(&state.borrow())?;
    Ok(summary)
}

async fn close_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("close_agent: missing worker handle".to_string()))?;
    let worker_id = worker_id_from_value(target)?;
    let state = with_worker_state(&worker_id, |state| Ok(state.clone()))?;
    let (snapshot, summary) = {
        let mut worker = state.borrow_mut();
        worker.cancel_token.store(true, Ordering::SeqCst);
        if let Some(handle) = worker.handle.take() {
            handle.abort();
        }
        worker.status = "cancelled".to_string();
        worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        worker.latest_error = Some("worker cancelled".to_string());
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        let snapshot = worker_event_snapshot(&worker);
        let summary = worker_summary(&worker)?;
        (snapshot, summary)
    };
    emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerCancelled).await?;
    Ok(summary)
}

fn list_agents_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let workers = WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .values()
            .map(|state| worker_summary(&state.borrow()))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(VmValue::List(Rc::new(workers)))
}

pub(crate) fn snapshot_suspended_subagents() -> Vec<serde_json::Value> {
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .values()
            .filter_map(|state| {
                let worker = state.borrow();
                if worker.status != "awaiting" || worker.mode != "sub_agent" {
                    return None;
                }
                let age_ms = worker
                    .awaiting_since
                    .map(|started| started.elapsed().as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                Some(serde_json::json!({
                    "handle": worker.id.clone(),
                    "session_id": if worker.audit.session_id.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(worker.audit.session_id.clone())
                    },
                    "reason": "awaiting_input",
                    "conditions": {
                        "status": worker.status.clone(),
                        "retriggerable": worker.carry_policy.retriggerable,
                        "awaiting_started_at": worker.awaiting_started_at.clone(),
                    },
                    "age_ms": age_ms,
                    "initiator": worker
                        .parent_worker_id
                        .clone()
                        .unwrap_or_else(|| "pipeline".to_string()),
                }))
            })
            .collect()
    })
}
