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
    worker_summary, worker_trigger_payload_text, worker_wait_blocks, SuspendInitiator,
    WorkerConfig, WorkerState, WorkerSuspension, WORKER_REGISTRY,
};
use self::sub_agent::{execute_sub_agent, parse_sub_agent_request};
use crate::orchestration::ArtifactRecord;
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

const AGENT_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("__host_worker_list", list_agents_builtin)
        .signature("__host_worker_list()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("List local host worker summaries."),
    SyncBuiltin::new(
        "__host_resume_conditions_parse",
        parse_resume_conditions_builtin,
    )
    .signature("__host_resume_conditions_parse(conditions?)")
    .arity(VmBuiltinArity::Range { min: 0, max: 1 })
    .doc("Validate and normalize agent resume conditions."),
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
    async_builtin!("__host_worker_resume", resume_agent_builtin)
        .signature(
            "__host_worker_resume(worker_or_snapshot, input_or_options?, continue_transcript?)",
        )
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Resume a suspended or persisted worker into the local host worker registry."),
    async_builtin!("__host_worker_suspend", suspend_agent_builtin)
        .signature("__host_worker_suspend(worker, reason, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Cooperatively suspend a host worker at the next turn boundary."),
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
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
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
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
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

/// Cooperative suspend (harn#1837 / S-01).
///
/// Sets the worker's `suspend_signal` flag — the `agent_loop` honors this
/// at the next turn boundary (S-02) — records suspension metadata,
/// persists a snapshot, and emits a `WorkerSuspended` lifecycle event.
///
/// Idempotent: calling on an already-suspended worker returns the
/// existing summary without re-emitting the event or re-persisting the
/// snapshot. Terminal states (`completed`/`failed`/`cancelled`/
/// `interrupted`) are rejected — the caller can spawn a fresh worker
/// instead.
async fn suspend_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("suspend_agent: missing worker handle".to_string()))?;
    let worker_id = worker_id_from_value(target)?;
    let reason = args.get(1).map(|value| value.display()).unwrap_or_default();
    let options = args.get(2);
    let initiator = options
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("initiator"))
        .map(|value| value.display())
        .map(|text| SuspendInitiator::parse(&text))
        .unwrap_or_default();
    let conditions = options
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("conditions"))
        .map(crate::llm::vm_value_to_json);

    let (snapshot, summary, should_emit) = with_worker_state(&worker_id, |state| {
        let mut worker = state.borrow_mut();
        if worker.status == "suspended" {
            return Ok((
                worker_event_snapshot(&worker),
                worker_summary(&worker)?,
                false,
            ));
        }
        match worker.status.as_str() {
            "completed" | "failed" | "cancelled" | "interrupted" => {
                return Err(VmError::Runtime(format!(
                    "suspend_agent: worker {} is already terminal (status={})",
                    worker.id, worker.status
                )));
            }
            _ => {}
        }
        worker.suspend_signal.store(true, Ordering::SeqCst);
        worker.status = "suspended".to_string();
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        worker.suspension = Some(WorkerSuspension {
            reason: reason.clone(),
            initiator,
            suspended_at: uuid::Uuid::now_v7().to_string(),
            snapshot_ref: worker.snapshot_path.clone(),
            conditions: conditions.clone(),
        });
        // Always persist on suspend — the cooperative-pause contract
        // demands a durable resumable handle, independent of the
        // worker's general `persist_state` carry policy.
        persist_worker_state_snapshot(&worker)?;
        Ok((
            worker_event_snapshot(&worker),
            worker_summary(&worker)?,
            true,
        ))
    })?;
    if should_emit {
        emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerSuspended).await?;
    }
    Ok(summary)
}

/// Resume a suspended or persisted worker.
///
/// Accepts either:
/// - A worker handle dict or task handle currently in the local registry —
///   if it is in `suspended` state, clears the suspension and warm-resumes
///   it (transitioning back to `running` and optionally wiring resume
///   input into the next task slot).
/// - A snapshot path or worker id whose snapshot lives on disk — rehydrates
///   the snapshot into the registry. If the snapshot recorded a suspended
///   state it is preserved (the caller can then re-issue resume to warm
///   it up); otherwise it lands in the snapshot's terminal/awaiting state.
#[derive(Clone)]
struct WorkerResumeOptions {
    resume_input: Option<VmValue>,
    continue_transcript: bool,
}

impl Default for WorkerResumeOptions {
    fn default() -> Self {
        Self {
            resume_input: None,
            continue_transcript: true,
        }
    }
}

fn is_nil(value: &VmValue) -> bool {
    matches!(value, VmValue::Nil)
}

fn parse_resume_options(args: &[VmValue]) -> WorkerResumeOptions {
    let mut options = WorkerResumeOptions::default();
    let Some(raw) = args.get(1) else {
        return options;
    };
    if let VmValue::Dict(dict) = raw {
        let has_options = dict.contains_key("input")
            || dict.contains_key("resume_input")
            || dict.contains_key("continue_transcript");
        if has_options {
            options.resume_input = dict
                .get("input")
                .or_else(|| dict.get("resume_input"))
                .filter(|value| !is_nil(value))
                .cloned();
            if let Some(flag) = dict
                .get("continue_transcript")
                .filter(|value| !is_nil(value))
            {
                options.continue_transcript = flag.is_truthy();
            }
        } else {
            options.resume_input = Some(raw.clone());
        }
    } else if !is_nil(raw) {
        options.resume_input = Some(raw.clone());
    }
    if let Some(flag) = args.get(2).filter(|value| !is_nil(value)) {
        options.continue_transcript = flag.is_truthy();
    }
    options
}

fn summary_message(summary: &str) -> VmValue {
    VmValue::Dict(Rc::new(BTreeMap::from([
        (
            "role".to_string(),
            VmValue::String(Rc::from("user".to_string())),
        ),
        (
            "content".to_string(),
            VmValue::String(Rc::from(summary.to_string())),
        ),
    ])))
}

fn summary_only_resume_transcript(transcript: Option<VmValue>) -> Option<VmValue> {
    let transcript = transcript?;
    let dict = transcript.as_dict()?;
    let summary = crate::llm::helpers::transcript_summary_text(dict)?;
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }
    Some(crate::llm::helpers::new_transcript_with(
        crate::llm::helpers::transcript_id(dict),
        vec![summary_message(summary)],
        Some(summary.to_string()),
        dict.get("metadata").cloned(),
    ))
}

fn apply_resume_transcript_policy(worker: &mut WorkerState, continue_transcript: bool) {
    if continue_transcript {
        return;
    }
    worker.transcript = summary_only_resume_transcript(worker.transcript.take());
}

async fn resume_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("resume_agent: missing worker handle".to_string()))?
        .clone();
    let options = parse_resume_options(&args);

    let warm_target_id = match &target_value {
        VmValue::Dict(_) | VmValue::TaskHandle(_) => Some(worker_id_from_value(&target_value)?),
        VmValue::String(text) => {
            let text_str: &str = text.as_ref();
            if text_str.contains('/') || text_str.ends_with(".json") {
                None
            } else {
                Some(text_str.to_string())
            }
        }
        _ => None,
    };
    let registry_hit = warm_target_id
        .as_deref()
        .and_then(|id| WORKER_REGISTRY.with(|registry| registry.borrow().get(id).cloned()));

    let state = if let Some(state) = registry_hit {
        state
    } else {
        let snapshot_target = target_value.display();
        if snapshot_target.is_empty() {
            return Err(VmError::Runtime(
                "resume_agent: missing worker id or snapshot path".to_string(),
            ));
        }
        cold_load_worker(&snapshot_target)?
    };
    // A suspended worker — whether warm in the registry or just
    // cold-loaded — gets transitioned back to running. Any other state
    // is treated as "already where the caller wants it" and the resume
    // call is a no-op summary read, matching the idempotent contract on
    // the suspend side.
    if state.borrow().status == "suspended" {
        warm_resume_worker(state, options).await
    } else {
        {
            let mut worker = state.borrow_mut();
            apply_resume_transcript_policy(&mut worker, options.continue_transcript);
            apply_resume_input(&mut worker, options.resume_input.as_ref());
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
        }
        worker_summary(&state.borrow())
    }
}

/// Wire the optional resume input into a worker's task slot. Empty or
/// absent input is a no-op so callers don't have to special-case the
/// "resume without a new prompt" path.
fn apply_resume_input(worker: &mut WorkerState, resume_input: Option<&VmValue>) {
    let Some(input) = resume_input else {
        return;
    };
    let text = input.display();
    if text.is_empty() {
        return;
    }
    worker.task = text.clone();
    worker.history.push(text);
}

fn apply_resume_task_to_config(config: &mut WorkerConfig, task: &str) {
    if let WorkerConfig::SubAgent { spec } = config {
        spec.task = task.to_string();
    }
}

async fn join_checkpointed_worker_handle(state: Rc<RefCell<WorkerState>>) -> Result<(), VmError> {
    let handle = {
        let mut worker = state.borrow_mut();
        let Some(handle) = worker.handle.as_ref() else {
            return Ok(());
        };
        if !handle.is_finished() {
            return Err(VmError::Runtime(format!(
                "resume_agent: worker {} has not reached its suspend checkpoint yet",
                worker.id
            )));
        }
        worker.handle.take()
    };
    if let Some(handle) = handle {
        let _ = handle
            .await
            .map_err(|error| VmError::Runtime(format!("resume_agent join error: {error}")))??;
    }
    Ok(())
}

async fn warm_resume_worker(
    state: Rc<RefCell<WorkerState>>,
    options: WorkerResumeOptions,
) -> Result<VmValue, VmError> {
    join_checkpointed_worker_handle(state.clone()).await?;
    let snapshot = {
        let mut worker = state.borrow_mut();
        if worker.status != "suspended" {
            // Non-suspended warm resume is a no-op: the worker is either
            // running, awaiting input via a separate primitive, or already
            // terminal. Returning the current summary keeps the call
            // observably idempotent (same as suspend) and avoids
            // surprising state transitions in caller code that doesn't
            // pre-check the status field.
            return worker_summary(&worker);
        }
        worker.suspend_signal.store(false, Ordering::SeqCst);
        worker.suspension = None;
        worker.status = "running".to_string();
        worker.finished_at = None;
        worker.latest_error = None;
        apply_resume_transcript_policy(&mut worker, options.continue_transcript);
        apply_resume_input(&mut worker, options.resume_input.as_ref());
        let task = worker.task.clone();
        apply_resume_task_to_config(&mut worker.config, &task);
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        worker_event_snapshot(&worker)
    };
    emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerResumed).await?;
    if crate::vm::clone_async_builtin_child_vm().is_some() {
        respawn_worker_task(state.clone())?;
    }
    let summary = worker_summary(&state.borrow())?;
    Ok(summary)
}

fn cold_load_worker(snapshot_target: &str) -> Result<Rc<RefCell<WorkerState>>, VmError> {
    let state = Rc::new(RefCell::new(load_worker_state_snapshot(snapshot_target)?));
    let worker_id = state.borrow().id.clone();
    {
        let mut worker = state.borrow_mut();
        ensure_worker_config_session_ids(&mut worker.config, &worker_id);
    }
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(worker_id.clone(), state.clone());
    });
    if state.borrow().carry_policy.persist_state {
        persist_worker_state_snapshot(&state.borrow())?;
    }
    Ok(state)
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
        worker.suspend_signal.store(false, Ordering::SeqCst);
        if let Some(handle) = worker.handle.take() {
            handle.abort();
        }
        worker.status = "cancelled".to_string();
        worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        // Drop any prior suspension envelope — once the worker is
        // closed the cooperative-pause contract no longer applies, and
        // a stale `suspension` would mislead observers reading the
        // worker summary post-cancel.
        worker.suspension = None;
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

fn resume_conditions_error(field: &str, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!(
        "HARN-SUS-002: invalid ResumeConditions.{field}: {}",
        message.into()
    ))
}

fn parse_resume_trigger_condition(value: &VmValue) -> Result<Option<serde_json::Value>, VmError> {
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Dict(map) => {
            super::triggers_stdlib::validate_resume_trigger_spec(map)
                .map_err(|error| resume_conditions_error("trigger", error.to_string()))?;
            Ok(Some(crate::llm::vm_value_to_json(value)))
        }
        other => Err(resume_conditions_error(
            "trigger",
            format!("expected dict or nil, got {}", other.type_name()),
        )),
    }
}

fn parse_resume_timeout_condition(value: &VmValue) -> Result<Option<serde_json::Value>, VmError> {
    let VmValue::Dict(map) = value else {
        if matches!(value, VmValue::Nil) {
            return Ok(None);
        }
        return Err(resume_conditions_error(
            "timeout",
            format!("expected dict or nil, got {}", value.type_name()),
        ));
    };
    for key in map.keys() {
        if key != "duration_minutes" && key != "on_timeout" {
            return Err(resume_conditions_error(
                &format!("timeout.{key}"),
                "unknown field; expected duration_minutes or on_timeout",
            ));
        }
    }
    let duration = map
        .get("duration_minutes")
        .and_then(VmValue::as_int)
        .ok_or_else(|| {
            resume_conditions_error("timeout.duration_minutes", "must be a positive int")
        })?;
    if duration <= 0 {
        return Err(resume_conditions_error(
            "timeout.duration_minutes",
            "must be a positive int",
        ));
    }
    let on_timeout = match map.get("on_timeout") {
        Some(VmValue::Nil) | None => "resume_with_summary".to_string(),
        Some(VmValue::String(action))
            if matches!(
                action.as_ref(),
                "resume_with_summary" | "fail" | "resume_with_input"
            ) =>
        {
            action.to_string()
        }
        Some(VmValue::String(action)) => {
            return Err(resume_conditions_error(
                "timeout.on_timeout",
                format!(
                    "unsupported action `{action}`, expected resume_with_summary|fail|resume_with_input"
                ),
            ))
        }
        Some(other) => {
            return Err(resume_conditions_error(
                "timeout.on_timeout",
                format!("expected string, got {}", other.type_name()),
            ))
        }
    };
    Ok(Some(serde_json::json!({
        "duration_minutes": duration,
        "on_timeout": on_timeout,
    })))
}

fn parse_resume_event_condition(value: &VmValue) -> Result<Option<serde_json::Value>, VmError> {
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(text) if !text.trim().is_empty() => {
            let trimmed = text.trim();
            crate::event_log::Topic::new(trimmed.to_string()).map_err(|error| {
                resume_conditions_error(
                    "on_event",
                    format!("invalid runtime event channel: {error}"),
                )
            })?;
            Ok(Some(serde_json::json!(trimmed.to_string())))
        }
        VmValue::String(_) => Err(resume_conditions_error(
            "on_event",
            "must be a non-empty string",
        )),
        other => Err(resume_conditions_error(
            "on_event",
            format!("expected string or nil, got {}", other.type_name()),
        )),
    }
}

fn parse_resume_conditions_value(value: Option<&VmValue>) -> Result<VmValue, VmError> {
    let Some(value) = value else {
        return Ok(VmValue::Nil);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(VmValue::Nil);
    }
    let VmValue::Dict(map) = value else {
        return Err(resume_conditions_error(
            "root",
            format!("expected dict or nil, got {}", value.type_name()),
        ));
    };
    let valid_keys = ["trigger", "timeout", "on_event"];
    for key in map.keys() {
        if !valid_keys.contains(&key.as_str()) {
            return Err(resume_conditions_error(
                key,
                "unknown field; expected trigger, timeout, or on_event",
            ));
        }
    }

    let mut normalized = serde_json::Map::new();
    if let Some(trigger) = map
        .get("trigger")
        .map(parse_resume_trigger_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("trigger".to_string(), trigger);
    }
    if let Some(timeout) = map
        .get("timeout")
        .map(parse_resume_timeout_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("timeout".to_string(), timeout);
    }
    if let Some(event) = map
        .get("on_event")
        .map(parse_resume_event_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("on_event".to_string(), event);
    }
    Ok(crate::stdlib::json_to_vm_value(&serde_json::Value::Object(
        normalized,
    )))
}

fn parse_resume_conditions_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    parse_resume_conditions_value(args.first())
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

#[cfg(test)]
mod suspend_tests {
    use super::*;
    use crate::orchestration::MutationSessionRecord;
    use std::path::Path;
    use std::sync::OnceLock;
    use tokio::sync::{Mutex, MutexGuard};

    /// Suspend tests mutate the process-wide `HARN_WORKER_STATE_DIR`
    /// env var. Serialize them to keep snapshot paths from one test
    /// from leaking into another's worker construction. Held across
    /// `.await` points, so a tokio-aware mutex is required.
    async fn suspend_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    /// Set up an isolated `.harn/workers/...` directory and seed a single
    /// worker into the local registry. Returns the seeded worker id and
    /// the test directory (callers clean both up on completion).
    fn seed_test_worker(name: &str) -> (String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("harn-suspend-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };
        let worker_id = format!("worker_{}", uuid::Uuid::now_v7());
        let snapshot_path = worker_snapshot_path(&worker_id);
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: name.to_string(),
            task: "do the thing".to_string(),
            status: "running".to_string(),
            created_at: uuid::Uuid::now_v7().to_string(),
            started_at: uuid::Uuid::now_v7().to_string(),
            finished_at: None,
            awaiting_started_at: None,
            awaiting_since: None,
            mode: "sub_agent".to_string(),
            history: vec!["do the thing".to_string()],
            config: WorkerConfig::SubAgent {
                spec: Box::new(SubAgentRunSpec {
                    name: name.to_string(),
                    task: "do the thing".to_string(),
                    session_id: format!("session_{worker_id}"),
                    ..Default::default()
                }),
            },
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            suspend_signal: Arc::new(AtomicBool::new(false)),
            suspension: None,
            request: Default::default(),
            latest_payload: None,
            latest_error: None,
            transcript: None,
            artifacts: Vec::new(),
            parent_worker_id: None,
            parent_stage_id: None,
            child_run_id: None,
            child_run_path: None,
            carry_policy: agents_workers::WorkerCarryPolicy {
                artifact_mode: "inherit".to_string(),
                transcript_mode: "inherit".to_string(),
                persist_state: true,
                ..Default::default()
            },
            execution: Default::default(),
            snapshot_path,
            audit: MutationSessionRecord::default().normalize(),
        }));
        WORKER_REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(worker_id.clone(), state.clone());
        });
        (worker_id, dir)
    }

    fn teardown(dir: &Path, worker_id: &str) {
        WORKER_REGISTRY.with(|registry| {
            registry.borrow_mut().remove(worker_id);
        });
        let _ = std::fs::remove_dir_all(dir);
        unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
    }

    fn handle_value(worker_id: &str) -> VmValue {
        VmValue::TaskHandle(worker_id.to_string())
    }

    fn message_value(role: &str, content: &str) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "role".to_string(),
                VmValue::String(Rc::from(role.to_string())),
            ),
            (
                "content".to_string(),
                VmValue::String(Rc::from(content.to_string())),
            ),
        ])))
    }

    fn summary_status(summary: &VmValue) -> String {
        summary
            .as_dict()
            .and_then(|dict| dict.get("status"))
            .map(VmValue::display)
            .unwrap_or_default()
    }

    #[test]
    fn resume_conditions_parse_round_trips_each_shape() {
        let trigger = VmValue::Dict(Rc::new(BTreeMap::from([(
            "trigger".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("id".to_string(), VmValue::String(Rc::from("resume-review"))),
                (
                    "kind".to_string(),
                    VmValue::String(Rc::from("review.approved")),
                ),
                ("provider".to_string(), VmValue::String(Rc::from("github"))),
                (
                    "handler".to_string(),
                    VmValue::String(Rc::from("worker://auto-resume")),
                ),
                (
                    "match".to_string(),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "events".to_string(),
                        VmValue::List(Rc::new(vec![VmValue::String(Rc::from("review.approved"))])),
                    )]))),
                ),
            ]))),
        )])));
        let trigger_json = crate::llm::vm_value_to_json(
            &parse_resume_conditions_value(Some(&trigger)).expect("parse trigger"),
        );
        assert_eq!(trigger_json["trigger"]["kind"], "review.approved");

        let timeout = VmValue::Dict(Rc::new(BTreeMap::from([(
            "timeout".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("duration_minutes".to_string(), VmValue::Int(15)),
                (
                    "on_timeout".to_string(),
                    VmValue::String(Rc::from("resume_with_input")),
                ),
            ]))),
        )])));
        let timeout_json = crate::llm::vm_value_to_json(
            &parse_resume_conditions_value(Some(&timeout)).expect("parse timeout"),
        );
        assert_eq!(timeout_json["timeout"]["duration_minutes"], 15);
        assert_eq!(timeout_json["timeout"]["on_timeout"], "resume_with_input");

        let event = VmValue::Dict(Rc::new(BTreeMap::from([(
            "on_event".to_string(),
            VmValue::String(Rc::from("operator.resume")),
        )])));
        let event_json = crate::llm::vm_value_to_json(
            &parse_resume_conditions_value(Some(&event)).expect("parse event"),
        );
        assert_eq!(event_json["on_event"], "operator.resume");
    }

    #[test]
    fn resume_conditions_parse_reports_harn_sus_002_field() {
        let invalid = VmValue::Dict(Rc::new(BTreeMap::from([(
            "timeout".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "duration_minutes".to_string(),
                VmValue::Int(0),
            )]))),
        )])));
        let error = parse_resume_conditions_value(Some(&invalid)).expect_err("invalid timeout");
        assert!(
            error.to_string().contains("HARN-SUS-002")
                && error.to_string().contains("timeout.duration_minutes"),
            "expected HARN-SUS-002 timeout field error, got: {error}"
        );

        let unknown_timeout = VmValue::Dict(Rc::new(BTreeMap::from([(
            "timeout".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("duration_minutes".to_string(), VmValue::Int(1)),
                ("extra".to_string(), VmValue::Bool(true)),
            ]))),
        )])));
        let unknown_timeout_error =
            parse_resume_conditions_value(Some(&unknown_timeout)).expect_err("unknown timeout key");
        assert!(
            unknown_timeout_error.to_string().contains("timeout.extra"),
            "expected HARN-SUS-002 timeout.extra field error, got: {unknown_timeout_error}"
        );

        let invalid_event = VmValue::Dict(Rc::new(BTreeMap::from([(
            "on_event".to_string(),
            VmValue::String(Rc::from("bad channel")),
        )])));
        let event_error =
            parse_resume_conditions_value(Some(&invalid_event)).expect_err("invalid event topic");
        assert!(
            event_error.to_string().contains("HARN-SUS-002")
                && event_error.to_string().contains("on_event"),
            "expected HARN-SUS-002 on_event field error, got: {event_error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspend_then_resume_then_close_is_idempotent_and_emits_events() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-suspend");

        // Idempotency: second suspend on an already-suspended worker is
        // a no-op summary read, and the suspension metadata recorded on
        // the first call is preserved verbatim.
        let first = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting on external review")),
        ])
        .await
        .expect("first suspend");
        assert_eq!(summary_status(&first), "suspended");
        let first_suspension = first.as_dict().unwrap().get("suspension").cloned();
        assert!(
            first_suspension.is_some() && first_suspension.as_ref().unwrap().display() != "nil"
        );

        let second = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("different reason — should be ignored")),
        ])
        .await
        .expect("second suspend");
        assert_eq!(summary_status(&second), "suspended");
        let second_suspension = second.as_dict().unwrap().get("suspension").cloned();
        // `VmValue` does not implement PartialEq for the dict branch, so
        // compare the JSON projection — the same coast-to-coast equality
        // contract clients see on the wire.
        let to_json = |value: Option<VmValue>| value.map(|v| crate::llm::vm_value_to_json(&v));
        assert_eq!(
            to_json(first_suspension),
            to_json(second_suspension),
            "idempotent suspend must preserve the original suspension metadata"
        );

        // Resume transitions back to running and wires the resume input
        // into the worker's task slot.
        let resumed = resume_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("next: write the report")),
        ])
        .await
        .expect("resume");
        assert_eq!(summary_status(&resumed), "running");
        assert!(
            resumed.as_dict().unwrap().get("suspension").is_none()
                || resumed
                    .as_dict()
                    .unwrap()
                    .get("suspension")
                    .unwrap()
                    .display()
                    == "nil"
        );
        assert_eq!(
            resumed.as_dict().unwrap().get("task").map(VmValue::display),
            Some("next: write the report".to_string())
        );

        // Closing a (re-)running worker still works the same way as
        // before — proves that the close path is not gated on a
        // specific upstream status.
        let closed = close_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("close");
        assert_eq!(summary_status(&closed), "cancelled");

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_can_drop_transcript_history_to_summary() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-resume-summary-only");

        WORKER_REGISTRY.with(|registry| {
            let state = registry.borrow().get(&worker_id).cloned().unwrap();
            state.borrow_mut().transcript = Some(crate::llm::helpers::new_transcript_with(
                Some(format!("session_{worker_id}")),
                vec![
                    message_value("user", "old request"),
                    message_value("assistant", "old answer"),
                ],
                Some("Prior digest".to_string()),
                None,
            ));
        });
        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("park before fresh prompt")),
        ])
        .await
        .expect("suspend");

        let resumed = resume_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "input".to_string(),
                    VmValue::String(Rc::from("fresh prompt")),
                ),
                ("continue_transcript".to_string(), VmValue::Bool(false)),
            ]))),
        ])
        .await
        .expect("resume with transcript reset");
        assert_eq!(summary_status(&resumed), "running");
        assert_eq!(
            resumed.as_dict().unwrap().get("task").map(VmValue::display),
            Some("fresh prompt".to_string())
        );

        let transcript = WORKER_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&worker_id)
                .unwrap()
                .borrow()
                .transcript
                .clone()
        });
        let transcript = transcript.expect("summary-only transcript");
        let dict = transcript.as_dict().expect("transcript dict");
        let messages = crate::llm::helpers::transcript_message_list(dict).expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]
                .as_dict()
                .and_then(|message| message.get("content"))
                .map(VmValue::display),
            Some("Prior digest".to_string())
        );

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspend_then_close_transitions_to_cancelled() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-suspend-close");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("park me")),
        ])
        .await
        .expect("suspend");

        let summary = close_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("close");
        assert_eq!(summary_status(&summary), "cancelled");

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspended_worker_survives_process_restart_via_snapshot() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-suspend-restart");
        let snapshot_path = WORKER_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&worker_id)
                .unwrap()
                .borrow()
                .snapshot_path
                .clone()
        });

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("checkpoint before restart")),
        ])
        .await
        .expect("suspend");

        // Simulate process restart by evicting the in-memory registry
        // entry and cold-loading the persisted snapshot. The wire
        // contract is that suspended status survives the round trip;
        // `interrupted` would be a regression because cooperative
        // suspends are not synonymous with crash-while-running.
        WORKER_REGISTRY.with(|registry| {
            registry.borrow_mut().remove(&worker_id);
        });
        let reloaded =
            agents_workers::load_worker_state_snapshot(&snapshot_path).expect("reload snapshot");
        assert_eq!(reloaded.status, "suspended");
        assert!(
            reloaded.suspension.is_some(),
            "snapshot must preserve suspension metadata"
        );
        assert_eq!(
            reloaded.suspension.as_ref().unwrap().reason,
            "checkpoint before restart"
        );

        // Rehydrate into the registry and warm-resume — the operator
        // wakes the worker back up after restart.
        WORKER_REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(worker_id.clone(), Rc::new(RefCell::new(reloaded)));
        });
        let resumed = resume_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("resume after restart");
        assert_eq!(summary_status(&resumed), "running");

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_loop_returns_suspended_checkpoint_for_current_worker() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-loop-suspend");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("pause before another model call")),
        ])
        .await
        .expect("suspend");

        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let _vm_context = crate::vm::install_async_builtin_child_vm(vm);
        let _runtime_context = crate::runtime_context::install_runtime_context_overlay(
            crate::runtime_context::RuntimeContextOverlay {
                worker_id: Some(worker_id.clone()),
                ..Default::default()
            },
        );

        let result = crate::stdlib::harn_entry::call_harn_export_by_name(
            "std/agent/loop",
            "agent_loop",
            "agent_loop_suspend_test",
            &[
                VmValue::String(Rc::from("continue the task")),
                VmValue::Nil,
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "max_iterations".to_string(),
                    VmValue::Int(3),
                )]))),
            ],
        )
        .await
        .expect("agent loop returns suspended checkpoint");
        let json = crate::llm::vm_value_to_json(&result);

        assert_eq!(json["status"], "suspended");
        assert_eq!(json["final_status"], "suspended");
        assert_eq!(json["reason"], "pause before another model call");
        assert_eq!(json["initiator"], "operator");
        assert_eq!(json["iterations_completed"], 0);
        assert_eq!(json["handle"]["id"], worker_id);
        assert!(
            serde_json::to_string(&json)
                .expect("serialize result")
                .contains("Worker suspended before the next turn: pause before another model call"),
            "suspend checkpoint should inject a resume-visible reminder"
        );

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sub_agent_execution_preserves_suspended_loop_payload() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-sub-agent-suspend");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("checkpoint the child loop")),
        ])
        .await
        .expect("suspend");

        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let _vm_context = crate::vm::install_async_builtin_child_vm(vm);
        let _runtime_context = crate::runtime_context::install_runtime_context_overlay(
            crate::runtime_context::RuntimeContextOverlay {
                worker_id: Some(worker_id.clone()),
                ..Default::default()
            },
        );

        let result = execute_sub_agent(SubAgentRunSpec {
            name: "worker-sub-agent-suspend".to_string(),
            task: "continue the task".to_string(),
            session_id: format!("session_{worker_id}"),
            options: BTreeMap::from([("max_iterations".to_string(), VmValue::Int(3))]),
            ..Default::default()
        })
        .await
        .expect("sub-agent execution returns suspended checkpoint");

        assert_eq!(result.payload["status"], "suspended");
        assert_eq!(result.payload["reason"], "checkpoint the child loop");
        assert_eq!(result.payload["handle"]["id"], worker_id);

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspend_rejects_terminal_workers() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-suspend-terminal");
        WORKER_REGISTRY.with(|registry| {
            let state = registry.borrow().get(&worker_id).cloned().unwrap();
            state.borrow_mut().status = "completed".to_string();
        });

        let err = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("late suspend")),
        ])
        .await
        .expect_err("terminal worker should reject suspend");
        match err {
            VmError::Runtime(message) => assert!(
                message.contains("terminal"),
                "expected terminal-rejection message, got: {message}"
            ),
            other => panic!("expected Runtime error, got {other:?}"),
        }

        teardown(&dir, &worker_id);
    }
}
