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

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_parser::diagnostic_codes::Code;

use self::agents_workers::{
    emit_worker_event, ensure_worker_config_session_ids, next_worker_id, parse_worker_config,
    persist_worker_state_snapshot, reset_worker_registry, spawn_worker_task, with_worker_state,
    worker_event_snapshot, worker_id_from_value, worker_request_for_config, worker_snapshot_path,
    worker_summary, worker_trigger_payload_text, SuspendInitiator, WorkerCarryPolicy, WorkerConfig,
    WorkerExecutionProfile, WorkerInit, WorkerState, WorkerSuspension, WORKER_REGISTRY,
};
use self::sub_agent::{execute_sub_agent, parse_sub_agent_request};
use crate::agent_events::WorkerEvent;
use crate::orchestration::{ContextPolicy, MutationSessionRecord};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

mod replay;
mod resume;
mod resume_conditions;
mod stop_handoff;

#[cfg(test)]
mod suspend_tests;

use replay::*;
use resume::*;
use resume_conditions::*;
use stop_handoff::*;

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &LIST_AGENTS_BUILTIN_DEF,
    &PARSE_RESUME_CONDITIONS_BUILTIN_DEF,
    &SUB_AGENT_RUN_BUILTIN_DEF,
    &SPAWN_AGENT_BUILTIN_DEF,
    &SEND_INPUT_BUILTIN_DEF,
    &WORKER_TRIGGER_BUILTIN_DEF,
    &WAIT_AGENT_BUILTIN_DEF,
    &RESUME_AGENT_BUILTIN_DEF,
    &SUSPEND_AGENT_BUILTIN_DEF,
    &TOP_LEVEL_AGENT_SUSPEND_BUILTIN_DEF,
    &STOP_AGENT_BUILTIN_DEF,
    &CLOSE_AGENT_BUILTIN_DEF,
];

pub(crate) fn reset_agent_worker_state() {
    reset_worker_registry();
}

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
    pub(super) reminder_propagation: Vec<crate::llm::helpers::SystemReminder>,
    /// Workspace anchor applied to the child session at spawn time
    /// (#2223). `None` leaves the child anchor-less; when present, the
    /// runtime validates the anchor is inside the parent's anchor +
    /// mounted roots before launch.
    pub(super) workspace_anchor: Option<crate::workspace_anchor::WorkspaceAnchor>,
}

pub(super) struct SubAgentExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: VmValue,
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    records::register_record_builtins(vm);
    workflow::register_workflow_builtins(vm);
}

#[harn_builtin(
    sig = "__host_sub_agent_run(request: dict) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Run or spawn a normalized Harn-authored sub-agent request."
)]
async fn sub_agent_run_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let request = parse_sub_agent_request(&args)?;
    if !request.background {
        let result = execute_sub_agent(&ctx, request.spec).await?;
        return Ok(crate::stdlib::json_to_vm_value(&result.payload));
    }

    let execution = request.execution;
    let worker_policy = request.worker_policy;
    let mut carry_policy = request.carry_policy;
    carry_policy.policy = worker_policy;
    let spec = request.spec;
    let init = WorkerInit {
        name: spec.name.clone(),
        task: spec.task.clone(),
        config: WorkerConfig::SubAgent {
            spec: Box::new(spec),
        },
        wait: false,
        carry_policy,
        execution,
        audit: agents_workers::inherited_worker_audit("sub_agent"),
    };
    let state = fresh_worker_state(next_worker_id(), init);
    finalize_and_run_worker(&ctx, state, false, "sub_agent worker").await
}

fn fresh_worker_state(
    worker_id: String,
    mut init: WorkerInit,
) -> Arc<parking_lot::Mutex<WorkerState>> {
    let created_at = uuid::Uuid::now_v7().to_string();
    ensure_worker_config_session_ids(&mut init.config, &worker_id);
    let mode = worker_mode_label(&init.config).to_string();
    let mut audit = init.audit.normalize();
    audit.worker_id = Some(worker_id.clone());
    audit.execution_kind = Some(mode.clone());
    let request = worker_request_for_config(&init.task, &init.config);
    Arc::new(parking_lot::Mutex::new(WorkerState {
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
        request,
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
    }))
}

/// Persist, register, and spawn a freshly-built worker. Optionally block until
/// it reaches a terminal state before returning the worker summary dict.
async fn finalize_and_run_worker(
    ctx: &AsyncBuiltinCtx,
    state: Arc<parking_lot::Mutex<WorkerState>>,
    wait_for_terminal: bool,
    wait_context: &'static str,
) -> Result<VmValue, VmError> {
    {
        let worker = state.lock();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    let worker_id = state.lock().id.clone();
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(worker_id, state.clone());
    });
    spawn_worker_task(state.clone(), ctx.child_ctx());
    if wait_for_terminal {
        wait_for_worker_terminal(state.clone(), wait_context).await?;
    }
    worker_summary(&state.lock())
}

fn worker_mode_label(config: &WorkerConfig) -> &'static str {
    match config {
        WorkerConfig::Workflow { .. } => "workflow",
        WorkerConfig::Stage { .. } => "stage",
        WorkerConfig::SubAgent { .. } => "sub_agent",
    }
}

#[harn_builtin(
    sig = "__host_worker_spawn(config: dict) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Spawn a workflow, stage, or sub-agent host worker."
)]
async fn spawn_agent_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .ok_or_else(|| VmError::Runtime("spawn_agent: missing config".to_string()))?;
    let init = parse_worker_config(config)?;
    let wait = init.wait;
    let state = fresh_worker_state(next_worker_id(), init);
    finalize_and_run_worker(&ctx, state, wait, "spawn_agent worker").await
}

#[harn_builtin(
    sig = "__host_worker_send_input(worker: any, task: any) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Resume a stopped host worker with new task input."
)]
async fn send_input_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
        let mut worker = state.lock();
        if worker.status == "running" {
            return Err(VmError::Runtime(format!(
                "send_input: worker {} is still running",
                worker.id
            )));
        }
        restart_worker_run(&mut worker, &next_task, true)?;
        drop(worker);
        respawn_worker_task(state.clone(), &ctx)?;
        let summary = worker_summary(&state.lock())?;
        Ok(summary)
    })
}

#[harn_builtin(
    sig = "__host_worker_trigger(worker: any, payload: any) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Trigger an awaiting retriggerable host worker."
)]
async fn worker_trigger_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
        let mut worker = state.lock();
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
        respawn_worker_task(state.clone(), &ctx)?;
        Ok(snapshot)
    })?;
    // Emit the progressed lifecycle *after* the new cycle has been
    // spawned. Hosts see `progressed` (re-arming the run) followed
    // by `running` from the inner `WorkerSpawned` emission.
    emit_worker_event(
        Some(&ctx),
        &progressed_snapshot,
        crate::agent_events::WorkerEvent::WorkerProgressed,
    )
    .await?;
    with_worker_state(&worker_id, |state| worker_summary(&state.lock()))
}

/// Cooperatively suspend a running worker.
///
/// Sets the worker's `suspend_signal` flag — the `agent_loop` honors this
/// at the next turn boundary — records suspension metadata, persists a
/// snapshot, and emits a `WorkerSuspended` lifecycle event.
///
/// Idempotent: calling on an already-suspended worker returns the
/// existing summary without re-emitting the event or re-persisting the
/// snapshot. Terminal states (`completed`/`failed`/`cancelled`/
/// `interrupted`) are rejected — the caller can spawn a fresh worker
/// instead.
#[harn_builtin(
    sig = "__host_worker_suspend(worker: any, reason: any, options?: dict) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Cooperatively suspend a host worker at the next turn boundary."
)]
async fn suspend_agent_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("suspend_agent: missing worker handle".to_string()))?;
    let worker_id = worker_id_from_value(target)?;
    let mut reason = args.get(1).map(|value| value.display()).unwrap_or_default();
    let options = args.get(2);
    let initiator = options
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("initiator"))
        .map(|value| value.display())
        .map(|text| SuspendInitiator::parse(&text))
        .unwrap_or_default();
    let conditions_value = options
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("conditions"))
        .filter(|value| !is_nil(value))
        .cloned();
    let conditions = conditions_value.as_ref().map(crate::llm::vm_value_to_json);

    // PreSuspend lifecycle gate. Block cancels the suspend and leaves the
    // worker running; Modify can rewrite the public reason.
    let pre_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PreSuspend.as_str(),
        "worker": { "id": &worker_id },
        "reason": &reason,
        "initiator": serde_json::to_value(initiator).unwrap_or(serde_json::Value::Null),
        "conditions": conditions.clone().unwrap_or(serde_json::Value::Null),
    });
    match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
        Some(&ctx),
        crate::orchestration::HookEvent::PreSuspend,
        &pre_payload,
    )
    .await?
    {
        crate::orchestration::HookControl::Allow => {}
        crate::orchestration::HookControl::Block {
            reason: block_reason,
        } => {
            // Blocked suspend: leave the worker running and return its summary.
            return with_worker_state(&worker_id, |state| {
                let mut summary = worker_summary(&state.lock())?;
                if let VmValue::Dict(map) = &mut summary {
                    let mut entries = (**map).clone();
                    entries.insert(
                        "pre_suspend_denied".to_string(),
                        VmValue::String(std::sync::Arc::from(block_reason.clone())),
                    );
                    *map = std::sync::Arc::new(entries);
                }
                Ok(summary)
            });
        }
        crate::orchestration::HookControl::Modify { payload } => {
            if let Some(new_reason) = payload.get("reason").and_then(|v| v.as_str()) {
                reason = new_reason.to_string();
            }
        }
        crate::orchestration::HookControl::Decision { .. } => {
            // Decision is not a defined return for PreSuspend; treat as Allow.
        }
    }

    let mut suspension_span: Option<LifecycleSpanGuard> = None;
    let (mut snapshot, mut summary, should_emit) = with_worker_state(&worker_id, |state| {
        let mut worker = state.lock();
        if worker.status == "suspended" {
            return Ok((
                worker_event_snapshot(&worker),
                worker_summary(&worker)?,
                false,
            ));
        }
        if worker.status != "running" {
            return Err(diagnostic_error(
                Code::SuspendWorkerNotRunning,
                format!(
                    "suspend_agent: worker {} is not running (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        let pipeline_span_link = crate::tracing::current_span_link();
        let span = LifecycleSpanGuard::start(
            crate::tracing::SpanKind::Suspension,
            format!("suspend {}", worker.id),
            Vec::new(),
        );
        // OTel consumers need enough metadata to render a paused agent
        // without joining back to the worker registry. `reason` and
        // `initiator` are already public via worker events and lifecycle hook
        // payloads; transcript content, resume input, and condition payloads
        // stay out of span attributes.
        annotate_suspension_span(
            &span,
            &worker.id,
            &reason,
            initiator,
            pipeline_span_link.as_ref(),
            worker.parent_worker_id.as_deref(),
            conditions.is_some(),
        );
        let prior_span_link = span.link();
        suspension_span = Some(span);
        worker.suspend_signal.store(true, Ordering::SeqCst);
        worker.status = "suspended".to_string();
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        worker.suspension = Some(WorkerSuspension {
            reason: reason.clone(),
            initiator,
            suspended_at: uuid::Uuid::now_v7().to_string(),
            snapshot_ref: worker.snapshot_path.clone(),
            prior_span_link,
            pipeline_span_link,
            suspended_at_turn: None,
            conditions: conditions.clone(),
            auto_resume_trigger: None,
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
        if let Some(auto_resume_trigger) = super::triggers_stdlib::register_auto_resume_trigger(
            Some(&ctx),
            &worker_id,
            conditions_value.as_ref(),
        )
        .await?
        {
            (snapshot, summary) = with_worker_state(&worker_id, |state| {
                let mut worker = state.lock();
                if let Some(suspension) = worker.suspension.as_mut() {
                    suspension.auto_resume_trigger = Some(auto_resume_trigger);
                }
                persist_worker_state_snapshot(&worker)?;
                Ok((worker_event_snapshot(&worker), worker_summary(&worker)?))
            })?;
        }
        if let Some(span) = suspension_span.as_mut() {
            span.end();
        }
        emit_worker_event(
            Some(&ctx),
            &snapshot,
            crate::agent_events::WorkerEvent::WorkerSuspended,
        )
        .await?;
        // PostSuspend hooks are advisory; the snapshot has already been
        // persisted by persist_worker_state_snapshot.
        let post_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::PostSuspend.as_str(),
            "worker": { "id": &worker_id },
            "reason": &reason,
            "snapshot_path": snapshot.metadata.get("snapshot_path").cloned().unwrap_or(serde_json::Value::Null),
        });
        crate::orchestration::run_lifecycle_hooks_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::PostSuspend,
            &post_payload,
        )
        .await?;
    }
    Ok(summary)
}

/// Outcome of a single panic-broadcast suspension attempt against one
/// worker. The dispatcher rolls these up into the
/// `triggers.interrupt_and_suspend.audit` audit entries and the
/// dispatch-result blob; observers tell what happened per target without
/// re-reading the worker registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanicSuspendOutcome {
    /// Worker was running and is now suspended; `WorkerSuspended` event
    /// emitted, snapshot persisted, suspension envelope installed.
    Suspended,
    /// Worker was already in the `suspended` state — no-op, no second
    /// `WorkerSuspended` event. Avoids double-suspend audit noise when a
    /// panic broadcast races with an in-flight `suspend_agent` call.
    AlreadySuspended,
    /// Worker exists but is in a terminal state (`completed`/`failed`/
    /// `cancelled`/`interrupted`) or otherwise not running — skipped.
    NotRunning,
    /// Worker id is not registered. Skipped — closure-form scopes can
    /// return stale ids and we never want a panic to fail because one of
    /// them does not resolve.
    Unknown,
}

impl PanicSuspendOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Suspended => "suspended",
            Self::AlreadySuspended => "already_suspended",
            Self::NotRunning => "not_running",
            Self::Unknown => "unknown",
        }
    }
}

/// Cooperatively suspend one worker as part of an `InterruptAndSuspend`
/// trigger dispatch.
///
/// Behavior parallels `suspend_agent_builtin` minus the
/// `PreSuspend`/`PostSuspend` lifecycle hooks and the auto-resume trigger
/// registration. Panic is by-design synchronous and bypasses the
/// approval/turn-boundary contract — wiring it through the hook gate would
/// let one rogue PreSuspend Deny block the org-wide emergency. The
/// suspension envelope carries the panic `reason` verbatim and uses
/// `SuspendInitiator::Triggered`, which matches the trigger-owned suspend
/// path already understood by receipts.
///
/// Returns the structured outcome so the dispatcher can build the per-worker
/// audit entries; returns `Err(...)` only on persistence/event-emission
/// failure (a real I/O fault — never on the "worker is unknown" /
/// "worker already suspended" / "worker is terminal" cases, which roll up
/// as outcomes instead).
pub(crate) async fn panic_suspend_worker(
    ctx: Option<&AsyncBuiltinCtx>,
    worker_id: &str,
    reason: &str,
) -> Result<PanicSuspendOutcome, VmError> {
    let Ok(state) = with_worker_state(worker_id, Ok) else {
        return Ok(PanicSuspendOutcome::Unknown);
    };

    let (snapshot, outcome) = {
        let mut worker = state.lock();
        if worker.status == "suspended" {
            return Ok(PanicSuspendOutcome::AlreadySuspended);
        }
        if worker.status != "running" {
            return Ok(PanicSuspendOutcome::NotRunning);
        }
        let pipeline_span_link = crate::tracing::current_span_link();
        let span = LifecycleSpanGuard::start(
            crate::tracing::SpanKind::Suspension,
            format!("panic_suspend {}", worker.id),
            Vec::new(),
        );
        annotate_suspension_span(
            &span,
            &worker.id,
            reason,
            SuspendInitiator::Triggered,
            pipeline_span_link.as_ref(),
            worker.parent_worker_id.as_deref(),
            false,
        );
        let prior_span_link = span.link();
        worker.suspend_signal.store(true, Ordering::SeqCst);
        worker.status = "suspended".to_string();
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        worker.suspension = Some(WorkerSuspension {
            reason: reason.to_string(),
            initiator: SuspendInitiator::Triggered,
            suspended_at: uuid::Uuid::now_v7().to_string(),
            snapshot_ref: worker.snapshot_path.clone(),
            prior_span_link,
            pipeline_span_link,
            suspended_at_turn: None,
            conditions: None,
            auto_resume_trigger: None,
        });
        // Same durability contract as `suspend_agent_builtin`: panic
        // suspends MUST persist a resumable handle. A panic with no
        // snapshot is worse than no panic at all — the operator has no
        // way to bring the agent back.
        persist_worker_state_snapshot(&worker)?;
        (
            worker_event_snapshot(&worker),
            PanicSuspendOutcome::Suspended,
        )
    };

    emit_worker_event(
        ctx,
        &snapshot,
        crate::agent_events::WorkerEvent::WorkerSuspended,
    )
    .await?;
    Ok(outcome)
}

/// Enumerate every worker id currently registered in the local worker
/// registry. Used by `InterruptAndSuspend` to materialize the
/// `AgentScope::All` target list. Ordering is the registry's stable
/// `BTreeMap` iteration order so a panic broadcast is deterministic across
/// replays.
pub(crate) fn all_registered_worker_ids() -> Vec<String> {
    WORKER_REGISTRY.with(|registry| registry.borrow().keys().cloned().collect())
}

#[harn_builtin(
    sig = "__host_top_level_agent_suspend(session_id: string, task: any, system: any, options: dict, reason: any, conditions?: any, iteration?: int) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Persist a top-level agent_loop suspend checkpoint as a resumable worker."
)]
async fn top_level_agent_suspend_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("top_level_agent_suspend: missing session_id".to_string())
        })?;
    let task = args.get(1).map(|value| value.display()).unwrap_or_default();
    let system = match args.get(2) {
        Some(VmValue::String(text)) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    };
    let options = args
        .get(3)
        .and_then(VmValue::as_dict)
        .cloned()
        .unwrap_or_default();
    let reason = args.get(4).map(|value| value.display()).unwrap_or_default();
    let conditions_value = args
        .get(5)
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let conditions = conditions_value.as_ref().map(crate::llm::vm_value_to_json);

    let worker_id = next_worker_id();
    let snapshot_path = worker_snapshot_path(&worker_id);
    let now = uuid::Uuid::now_v7().to_string();
    let name = "top-level-agent".to_string();
    let pipeline_span_link = crate::tracing::current_span_link();
    let mut suspension_span = LifecycleSpanGuard::start(
        crate::tracing::SpanKind::Suspension,
        format!("suspend {worker_id}"),
        Vec::new(),
    );
    // Mirror `suspend_agent_builtin` metadata for top-level script
    // suspensions.
    // The initiator for the top-level path is always `SelfInitiated`
    // because only the active agent loop can drive the synchronous
    // checkpoint return path.
    annotate_suspension_span(
        &suspension_span,
        &worker_id,
        &reason,
        SuspendInitiator::SelfInitiated,
        pipeline_span_link.as_ref(),
        None,
        conditions.is_some(),
    );
    let prior_span_link = suspension_span.link();
    let config = WorkerConfig::SubAgent {
        spec: Box::new(SubAgentRunSpec {
            name: name.clone(),
            task: task.clone(),
            system: system.clone(),
            options,
            returns_schema: None,
            session_id: session_id.clone(),
            parent_session_id: None,
            reminder_propagation: Vec::new(),
            workspace_anchor: None,
        }),
    };
    let request = worker_request_for_config(&task, &config);
    let transcript = crate::agent_sessions::transcript(&session_id);
    let audit = MutationSessionRecord {
        session_id: session_id.clone(),
        worker_id: Some(worker_id.clone()),
        execution_kind: Some("top_level_agent".to_string()),
        mutation_scope: "read_only".to_string(),
        ..Default::default()
    }
    .normalize();
    let worker = WorkerState {
        id: worker_id.clone(),
        name,
        task: task.clone(),
        status: "suspended".to_string(),
        created_at: now.clone(),
        started_at: now.clone(),
        finished_at: None,
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "top_level_agent".to_string(),
        history: vec![task],
        config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: Some(WorkerSuspension {
            reason,
            initiator: SuspendInitiator::SelfInitiated,
            suspended_at: now,
            snapshot_ref: snapshot_path.clone(),
            prior_span_link,
            pipeline_span_link,
            suspended_at_turn: args.get(6).and_then(VmValue::as_int),
            conditions,
            auto_resume_trigger: None,
        }),
        request,
        latest_payload: None,
        latest_error: None,
        transcript,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: None,
        child_run_id: None,
        child_run_path: None,
        carry_policy: WorkerCarryPolicy {
            artifact_mode: "inherit".to_string(),
            transcript_mode: "inherit".to_string(),
            context_policy: ContextPolicy::default(),
            resume_workflow: true,
            persist_state: true,
            retriggerable: false,
            policy: None,
        },
        execution: WorkerExecutionProfile::default(),
        snapshot_path,
        audit,
    };
    persist_worker_state_snapshot(&worker)?;
    let mut snapshot = worker_event_snapshot(&worker);
    let mut summary = worker_summary(&worker)?;
    let state = Arc::new(parking_lot::Mutex::new(worker));
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(worker_id.clone(), state);
    });
    if let Some(auto_resume_trigger) = super::triggers_stdlib::register_auto_resume_trigger(
        Some(&ctx),
        &worker_id,
        conditions_value.as_ref(),
    )
    .await?
    {
        (snapshot, summary) = with_worker_state(&worker_id, |state| {
            let mut worker = state.lock();
            if let Some(suspension) = worker.suspension.as_mut() {
                suspension.auto_resume_trigger = Some(auto_resume_trigger);
            }
            persist_worker_state_snapshot(&worker)?;
            Ok((worker_event_snapshot(&worker), worker_summary(&worker)?))
        })?;
    }
    suspension_span.end();
    emit_worker_event(Some(&ctx), &snapshot, WorkerEvent::WorkerSuspended).await?;
    Ok(summary)
}

fn is_nil(value: &VmValue) -> bool {
    matches!(value, VmValue::Nil)
}

fn diagnostic_error(code: Code, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("{}: {}", code.as_str(), message.into()))
}

#[harn_builtin(
    sig = "__host_worker_resume(worker_or_snapshot: any, input_or_options?: any, continue_transcript?: bool) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Resume a suspended or persisted worker into the local host worker registry."
)]
async fn resume_agent_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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

    let (state, loaded_from_snapshot) = if let Some(state) = registry_hit {
        (state, false)
    } else {
        let snapshot_target = target_value.display();
        if snapshot_target.is_empty() {
            return Err(diagnostic_error(
                Code::ResumeSnapshotInvalid,
                "resume_agent: missing worker id or snapshot path",
            ));
        }
        (cold_load_worker(&snapshot_target)?, true)
    };
    if state.lock().status == "suspended" {
        warm_resume_worker(&ctx, state, options).await
    } else if loaded_from_snapshot {
        // A snapshot path can be used as a restore/read operation for
        // completed or awaiting persisted workers. Keep that historical
        // contract, but only warm-resume live registry handles when they
        // are actually suspended.
        {
            let mut worker = state.lock();
            apply_resume_transcript_policy(&mut worker, options.continue_transcript, None);
            apply_resume_input(&mut worker, options.resume_input.as_ref())?;
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
        }
        worker_summary(&state.lock())
    } else {
        Err(resume_rejected_error(&state.lock()))
    }
}

pub(crate) async fn resume_worker_from_auto_resume_trigger(
    ctx: &AsyncBuiltinCtx,
    worker_id: &str,
    event: &crate::triggers::TriggerEvent,
) -> Result<VmValue, VmError> {
    let payload = auto_resume_event_payload(event);
    let timeout_action = auto_resume_timeout_action(&payload);
    if timeout_action.as_deref() == Some("fail") {
        return fail_suspended_worker_from_auto_resume_timeout(ctx, worker_id).await;
    }
    let resume_input = match timeout_action.as_deref() {
        Some("resume_with_summary") => None,
        _ => Some(crate::stdlib::json_to_vm_value(&payload)),
    };
    let options = WorkerResumeOptions {
        resume_input,
        continue_transcript: timeout_action.as_deref() != Some("resume_with_summary"),
        initiator: Some(
            if timeout_action.as_deref() == Some("resume_with_summary") {
                "timeout"
            } else {
                "triggered"
            }
            .to_string(),
        ),
        trigger_event: Some(serde_json::to_value(event).unwrap_or(serde_json::Value::Null)),
    };
    let state = with_worker_state(worker_id, Ok)?;
    warm_resume_worker(ctx, state, options).await
}

#[harn_builtin(
    sig = "__host_worker_wait(worker_or_workers: any) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Wait for one or more host workers to reach a terminal state."
)]
async fn wait_agent_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("wait_agent: missing worker handle".to_string()))?;
    if let VmValue::List(list) = target {
        let mut results = Vec::new();
        for item in list.iter() {
            let worker_id = worker_id_from_value(item)?;
            let state = with_worker_state(&worker_id, Ok)?;
            wait_for_worker_terminal(state.clone(), "wait_agent").await?;
            results.push(worker_summary(&state.lock())?);
        }
        return Ok(VmValue::List(std::sync::Arc::new(results)));
    }
    let worker_id = worker_id_from_value(target)?;
    let state = with_worker_state(&worker_id, Ok)?;
    wait_for_worker_terminal(state.clone(), "wait_agent").await?;
    let summary = worker_summary(&state.lock())?;
    Ok(summary)
}

#[harn_builtin(
    sig = "__host_worker_stop(worker: any, options: any) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Stop a host worker. With {graceful:true}, return a recursive typed handoff summary before terminating the worker."
)]
async fn stop_agent_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("agent_stop: missing worker handle".to_string()))?;
    let options = parse_stop_options(&args, "agent_stop")?;
    let worker_id = worker_id_from_value(target)?;
    let state = with_worker_state(&worker_id, Ok)?;
    if !options.graceful {
        return finish_worker_with_event(
            &ctx,
            state,
            "cancelled",
            WorkerEvent::WorkerCancelled,
            Some("worker cancelled".to_string()),
            None,
        )
        .await;
    }

    let source = source_from_worker(&state.lock());
    let tree = build_stop_handoff_tree(source, &options.reason, &mut HashSet::new())?;
    stop_descendant_workers(&ctx, tree.descendant_worker_payloads).await?;
    let worker_summary = finish_worker_with_event(
        &ctx,
        state,
        "stopped",
        WorkerEvent::WorkerStopped,
        None,
        Some(tree.payload.clone()),
    )
    .await?;
    let mut payload = tree.payload;
    if let Some(map) = payload.as_object_mut() {
        map.insert(
            "worker".to_string(),
            crate::llm::vm_value_to_json(&worker_summary),
        );
    }
    Ok(crate::stdlib::json_to_vm_value(&payload))
}

#[harn_builtin(
    sig = "__host_worker_close(worker: any) -> any",
    kind = "async",
    category = "agent.worker",
    runtime_only = true,
    doc = "Cancel a host worker and emit the cancellation lifecycle event."
)]
async fn close_agent_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("close_agent: missing worker handle".to_string()))?;
    let worker_id = worker_id_from_value(target)?;
    let state = with_worker_state(&worker_id, Ok)?;
    finish_worker_with_event(
        &ctx,
        state,
        "cancelled",
        WorkerEvent::WorkerCancelled,
        Some("worker cancelled".to_string()),
        None,
    )
    .await
}

#[harn_builtin(
    sig = "__host_worker_list() -> list",
    category = "agent.worker",
    runtime_only = true,
    doc = "List local host worker summaries."
)]
fn list_agents_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let workers = WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .values()
            .map(|state| worker_summary(&state.lock()))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(VmValue::List(std::sync::Arc::new(workers)))
}

#[harn_builtin(
    sig = "__host_resume_conditions_parse(conditions?: any) -> any",
    category = "agent.worker",
    runtime_only = true,
    doc = "Validate and normalize agent resume conditions."
)]
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
                let worker = state.lock();
                if worker.status == "suspended" {
                    let suspension = worker.suspension.as_ref();
                    let suspended_at_ms =
                        suspension.and_then(|value| uuid_v7_unix_ms(&value.suspended_at));
                    let age_ms = suspended_at_ms
                        .map(|started| crate::stdlib::clock::now_wall_ms().saturating_sub(started))
                        .unwrap_or(0)
                        .max(0);
                    return Some(serde_json::json!({
                        "handle": worker.id.clone(),
                        "session_id": worker_session_id_json(&worker),
                        "reason": suspension
                            .map(|value| value.reason.clone())
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| "suspended".to_string()),
                        "conditions": suspension
                            .and_then(|value| value.conditions.clone())
                            .unwrap_or(serde_json::Value::Null),
                        "age_ms": age_ms,
                        "initiator": suspension
                            .and_then(|value| serde_json::to_value(value.initiator).ok())
                            .unwrap_or_else(|| serde_json::json!("operator")),
                        "mode": worker.mode.clone(),
                        "status": worker.status.clone(),
                        "suspended_at": suspension
                            .map(|value| value.suspended_at.clone())
                            .unwrap_or_default(),
                        "snapshot_ref": suspension
                            .map(|value| value.snapshot_ref.clone())
                            .unwrap_or_else(|| worker.snapshot_path.clone()),
                        "auto_resume_trigger": suspension
                            .and_then(|value| value.auto_resume_trigger.as_ref())
                            .and_then(|value| serde_json::to_value(value).ok()),
                    }));
                }
                if worker.status != "awaiting" || worker.mode != "sub_agent" {
                    return None;
                }
                let age_ms = worker
                    .awaiting_since
                    .map(|started| started.elapsed().as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                Some(serde_json::json!({
                    "handle": worker.id.clone(),
                    "session_id": worker_session_id_json(&worker),
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
                    "mode": worker.mode.clone(),
                    "status": worker.status.clone(),
                }))
            })
            .collect()
    })
}

fn worker_session_id_json(worker: &WorkerState) -> serde_json::Value {
    let session_id = if worker.audit.session_id.is_empty() {
        match &worker.config {
            WorkerConfig::SubAgent { spec } if !spec.session_id.is_empty() => {
                Some(spec.session_id.clone())
            }
            _ => None,
        }
    } else {
        Some(worker.audit.session_id.clone())
    };
    session_id
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null)
}

fn uuid_v7_unix_ms(value: &str) -> Option<i64> {
    let uuid = uuid::Uuid::parse_str(value).ok()?;
    let (seconds, nanos) = uuid.get_timestamp()?.to_unix();
    let millis = seconds
        .checked_mul(1_000)?
        .checked_add(u64::from(nanos / 1_000_000))?;
    i64::try_from(millis).ok()
}
