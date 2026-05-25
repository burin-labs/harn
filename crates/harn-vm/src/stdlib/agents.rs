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
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_parser::diagnostic_codes::Code;

use self::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, emit_worker_event,
    ensure_worker_config_session_ids, load_worker_state_snapshot, next_worker_id,
    parse_worker_config, persist_worker_state_snapshot, reset_worker_registry, spawn_worker_task,
    with_worker_state, worker_event_snapshot, worker_id_from_value, worker_request_for_config,
    worker_snapshot_path, worker_summary, worker_trigger_payload_text, worker_wait_blocks,
    SuspendInitiator, WorkerCarryPolicy, WorkerConfig, WorkerExecutionProfile, WorkerInit,
    WorkerState, WorkerSuspension, WORKER_REGISTRY,
};
use self::sub_agent::{execute_sub_agent, parse_sub_agent_request};
use crate::agent_events::WorkerEvent;
use crate::orchestration::{ArtifactRecord, ContextPolicy, MutationSessionRecord};
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
    async_builtin!(
        "__host_top_level_agent_suspend",
        top_level_agent_suspend_builtin
    )
    .signature(
        "__host_top_level_agent_suspend(session_id, task, system, options, reason, conditions?, iteration?)",
    )
    .arity(VmBuiltinArity::Range { min: 5, max: 7 })
    .doc("Persist a top-level agent_loop suspend checkpoint as a resumable worker."),
    async_builtin!("__host_worker_close", close_agent_builtin)
        .signature("__host_worker_close(worker)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Cancel a host worker and emit the cancellation lifecycle event."),
];

const AGENT_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("agent.worker")
    .sync(AGENT_SYNC_PRIMITIVES)
    .async_(AGENT_ASYNC_PRIMITIVES);

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

struct WorkerReplayCarry {
    artifacts: Vec<ArtifactRecord>,
    transcript: Option<VmValue>,
    parent_worker_id: String,
    worker_id: String,
    resume_workflow: bool,
    child_run_path: Option<String>,
    reset_sub_agent_session: bool,
}

struct LifecycleSpanGuard {
    span_id: u64,
    otel_span: tracing::Span,
}

impl LifecycleSpanGuard {
    fn start(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, true)
    }

    fn start_detached(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, false)
    }

    fn start_with_parenting(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
        inherit_parent: bool,
    ) -> Self {
        let span_id = if inherit_parent {
            crate::tracing::span_start_with_links(kind, name.clone(), links.clone())
        } else {
            crate::tracing::span_start_detached_with_links(kind, name.clone(), links.clone())
        };
        let otel_span = tracing::info_span!(
            target: "harn.vm.lifecycle",
            "harn.lifecycle",
            harn.kind = kind.as_str(),
            harn.name = %name,
        );
        for link in links {
            let trace_id = crate::TraceId(link.trace_id);
            let mut attributes: HashMap<String, String> = link.attributes.into_iter().collect();
            attributes
                .entry("harn.link.kind".to_string())
                .or_insert_with(|| "causal".to_string());
            let _ = crate::observability::otel::set_span_link(
                &otel_span,
                &trace_id,
                &link.span_id,
                Some(attributes),
            );
        }
        Self { span_id, otel_span }
    }

    fn link(&self) -> Option<crate::tracing::SpanLink> {
        crate::observability::otel::current_span_context_hex(&self.otel_span)
            .map(|(trace_id, span_id)| crate::tracing::SpanLink::new(trace_id, span_id))
            .or_else(|| crate::tracing::span_link(self.span_id))
    }

    fn set_metadata(&self, key: &str, value: serde_json::Value) {
        crate::tracing::span_set_metadata(self.span_id, key, value);
    }

    fn end(&mut self) {
        if self.span_id != 0 {
            crate::tracing::span_end(self.span_id);
            self.span_id = 0;
        }
    }
}

impl Drop for LifecycleSpanGuard {
    fn drop(&mut self) {
        self.end();
    }
}

/// Apply the canonical attribute bag to a suspension lifecycle span.
/// Kept in one helper so suspend_agent_builtin and
/// top_level_agent_suspend_builtin emit identically shaped spans for
/// OTel exporters and downstream queries.
///
/// Privacy: `reason` is a short human-facing label already broadcast on
/// WorkerSuspended events and PreSuspend/PostSuspend hook payloads, so
/// mirroring it onto the span attributes does not widen exposure.
/// Conditions are summarised as a boolean — the condition payload itself
/// is intentionally NOT placed on the span.
fn annotate_suspension_span(
    span: &LifecycleSpanGuard,
    worker_id: &str,
    reason: &str,
    initiator: SuspendInitiator,
    pipeline_span_link: Option<&crate::tracing::SpanLink>,
    parent_worker_id: Option<&str>,
    has_conditions: bool,
) {
    span.set_metadata("worker_id", serde_json::json!(worker_id));
    // `handle` is the canonical OTel attribute name for suspension spans;
    // `worker_id` remains for consumers that join against worker events.
    span.set_metadata("handle", serde_json::json!(worker_id));
    span.set_metadata("reason", serde_json::json!(reason));
    span.set_metadata(
        "initiator",
        serde_json::json!(serde_json::to_value(initiator).unwrap_or(serde_json::Value::Null)),
    );
    span.set_metadata("has_conditions", serde_json::json!(has_conditions));
    if let Some(pipeline_span_link) = pipeline_span_link {
        // Worker state does not carry a distinct pipeline identifier, so the
        // active pipeline span id is the stable cross-span join key.
        span.set_metadata(
            "pipeline_span_id",
            serde_json::json!(pipeline_span_link.span_id),
        );
        span.set_metadata("pipeline_id", serde_json::json!(pipeline_span_link.span_id));
    }
    if let Some(parent_worker_id) = parent_worker_id {
        span.set_metadata("parent_worker_id", serde_json::json!(parent_worker_id));
    }
}

/// Apply the canonical attribute bag to a resume lifecycle span.
/// Booleans describe the shape of the resume (transcript
/// continuity, fresh resume input) without exposing transcript text or
/// the resume payload itself.
fn annotate_resume_span(
    span: &LifecycleSpanGuard,
    worker_id: &str,
    initiator: &str,
    continue_transcript: bool,
    had_resume_input: bool,
    linked_suspension_count: usize,
) {
    span.set_metadata("worker_id", serde_json::json!(worker_id));
    span.set_metadata("handle", serde_json::json!(worker_id));
    span.set_metadata("initiator", serde_json::json!(initiator));
    span.set_metadata(
        "continue_transcript",
        serde_json::json!(continue_transcript),
    );
    span.set_metadata("had_resume_input", serde_json::json!(had_resume_input));
    span.set_metadata(
        "linked_suspension_count",
        serde_json::json!(linked_suspension_count),
    );
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
    finalize_and_run_worker(state, false, "sub_agent worker").await
}

fn fresh_worker_state(worker_id: String, mut init: WorkerInit) -> Rc<RefCell<WorkerState>> {
    let created_at = uuid::Uuid::now_v7().to_string();
    ensure_worker_config_session_ids(&mut init.config, &worker_id);
    let mode = worker_mode_label(&init.config).to_string();
    let mut audit = init.audit.normalize();
    audit.worker_id = Some(worker_id.clone());
    audit.execution_kind = Some(mode.clone());
    let request = worker_request_for_config(&init.task, &init.config);
    Rc::new(RefCell::new(WorkerState {
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
    let init = parse_worker_config(config)?;
    let wait = init.wait;
    let state = fresh_worker_state(next_worker_id(), init);
    finalize_and_run_worker(state, wait, "spawn_agent worker").await
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
async fn suspend_agent_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    match crate::orchestration::run_lifecycle_hooks_with_control(
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
                let mut summary = worker_summary(&state.borrow())?;
                if let VmValue::Dict(map) = &mut summary {
                    let mut entries = (**map).clone();
                    entries.insert(
                        "pre_suspend_denied".to_string(),
                        VmValue::String(Rc::from(block_reason.clone())),
                    );
                    *map = Rc::new(entries);
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
        let mut worker = state.borrow_mut();
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
            &worker_id,
            conditions_value.as_ref(),
        )
        .await?
        {
            (snapshot, summary) = with_worker_state(&worker_id, |state| {
                let mut worker = state.borrow_mut();
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
        emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerSuspended).await?;
        // PostSuspend hooks are advisory; the snapshot has already been
        // persisted by persist_worker_state_snapshot.
        let post_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::PostSuspend.as_str(),
            "worker": { "id": &worker_id },
            "reason": &reason,
            "snapshot_path": snapshot.metadata.get("snapshot_path").cloned().unwrap_or(serde_json::Value::Null),
        });
        crate::orchestration::run_lifecycle_hooks(
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
    worker_id: &str,
    reason: &str,
) -> Result<PanicSuspendOutcome, VmError> {
    let Ok(state) = with_worker_state(worker_id, Ok) else {
        return Ok(PanicSuspendOutcome::Unknown);
    };

    let (snapshot, outcome) = {
        let mut worker = state.borrow_mut();
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

    emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerSuspended).await?;
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

async fn top_level_agent_suspend_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    let state = Rc::new(RefCell::new(worker));
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(worker_id.clone(), state);
    });
    if let Some(auto_resume_trigger) =
        super::triggers_stdlib::register_auto_resume_trigger(&worker_id, conditions_value.as_ref())
            .await?
    {
        (snapshot, summary) = with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            if let Some(suspension) = worker.suspension.as_mut() {
                suspension.auto_resume_trigger = Some(auto_resume_trigger);
            }
            persist_worker_state_snapshot(&worker)?;
            Ok((worker_event_snapshot(&worker), worker_summary(&worker)?))
        })?;
    }
    suspension_span.end();
    emit_worker_event(&snapshot, WorkerEvent::WorkerSuspended).await?;
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
    initiator: Option<String>,
    trigger_event: Option<serde_json::Value>,
}

impl Default for WorkerResumeOptions {
    fn default() -> Self {
        Self {
            resume_input: None,
            continue_transcript: true,
            initiator: None,
            trigger_event: None,
        }
    }
}

fn is_nil(value: &VmValue) -> bool {
    matches!(value, VmValue::Nil)
}

fn diagnostic_error(code: Code, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("{}: {}", code.as_str(), message.into()))
}

fn resume_rejected_error(worker: &WorkerState) -> VmError {
    if worker.status == "cancelled" {
        diagnostic_error(
            Code::ResumeWorkerClosed,
            format!(
                "resume_agent: worker {} was closed and cannot be resumed",
                worker.id
            ),
        )
    } else {
        diagnostic_error(
            Code::ResumeWorkerNotSuspended,
            format!(
                "resume_agent: worker {} is not suspended (status={})",
                worker.id, worker.status
            ),
        )
    }
}

fn parse_resume_options(args: &[VmValue]) -> WorkerResumeOptions {
    let mut options = WorkerResumeOptions::default();
    let Some(raw) = args.get(1) else {
        return options;
    };
    if let VmValue::Dict(dict) = raw {
        let has_options = dict.contains_key("input")
            || dict.contains_key("resume_input")
            || dict.contains_key("continue_transcript")
            || dict.contains_key("initiator")
            || dict.contains_key("resume_initiator")
            || dict.contains_key("trigger_event");
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
            options.initiator = dict
                .get("initiator")
                .or_else(|| dict.get("resume_initiator"))
                .filter(|value| !is_nil(value))
                .map(VmValue::display)
                .filter(|value| !value.trim().is_empty());
            options.trigger_event = dict
                .get("trigger_event")
                .filter(|value| !is_nil(value))
                .map(crate::llm::vm_value_to_json);
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

fn summary_only_resume_transcript(
    transcript: Option<VmValue>,
    digest_override: Option<String>,
) -> Option<VmValue> {
    let transcript = transcript?;
    let dict = transcript.as_dict()?;
    let summary = digest_override.or_else(|| crate::llm::helpers::transcript_summary_text(dict));
    let summary = summary.as_deref()?.trim();
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

async fn resume_digest_from_transcript(
    transcript: Option<&VmValue>,
) -> Result<Option<String>, VmError> {
    let Some(transcript) = transcript else {
        return Ok(None);
    };
    let Some(dict) = transcript.as_dict() else {
        return Ok(None);
    };
    if let Some(summary) = crate::llm::helpers::transcript_summary_text(dict) {
        let summary = summary.trim();
        if !summary.is_empty() {
            return Ok(Some(summary.to_string()));
        }
    }
    let messages = crate::llm::helpers::transcript_message_list(dict)?;
    if messages.is_empty() {
        return Ok(None);
    }
    let mut json_messages = messages
        .iter()
        .map(crate::llm::helpers::vm_value_to_json)
        .collect::<Vec<_>>();
    let mut config = crate::orchestration::AutoCompactConfig {
        token_threshold: 0,
        keep_last: 0,
        compact_strategy: crate::orchestration::CompactStrategy::Truncate,
        policy_strategy: crate::orchestration::compact_strategy_name(
            &crate::orchestration::CompactStrategy::Truncate,
        )
        .to_string(),
        ..Default::default()
    };
    // `ResumeDigest` mode skips hook dispatch and AgentEvent emission so
    // cold-resume digest extraction stays observably equivalent to the
    // pre-lifecycle direct `auto_compact_messages` call site.
    let lifecycle = crate::orchestration::CompactLifecycle::new(
        crate::orchestration::CompactMode::ResumeDigest,
    );
    let outcome = crate::orchestration::run_compaction_lifecycle(
        &mut json_messages,
        &mut config,
        None,
        lifecycle,
    )
    .await?;
    Ok(outcome
        .map(|outcome| outcome.summary.trim().to_string())
        .filter(|summary| !summary.is_empty()))
}

fn apply_resume_transcript_policy(
    worker: &mut WorkerState,
    continue_transcript: bool,
    digest: Option<String>,
) {
    if continue_transcript {
        return;
    }
    worker.transcript = summary_only_resume_transcript(worker.transcript.take(), digest);
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
    if state.borrow().status == "suspended" {
        warm_resume_worker(state, options).await
    } else if loaded_from_snapshot {
        // A snapshot path can be used as a restore/read operation for
        // completed or awaiting persisted workers. Keep that historical
        // contract, but only warm-resume live registry handles when they
        // are actually suspended.
        {
            let mut worker = state.borrow_mut();
            apply_resume_transcript_policy(&mut worker, options.continue_transcript, None);
            apply_resume_input(&mut worker, options.resume_input.as_ref())?;
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
        }
        worker_summary(&state.borrow())
    } else {
        Err(resume_rejected_error(&state.borrow()))
    }
}

/// Wire the optional resume input into a worker's task slot. Absent input
/// is a no-op; explicitly empty input is rejected so callers do not
/// accidentally erase the next task prompt.
fn apply_resume_input(
    worker: &mut WorkerState,
    resume_input: Option<&VmValue>,
) -> Result<(), VmError> {
    let Some(input) = resume_input else {
        return Ok(());
    };
    let text = input.display();
    if text.trim().is_empty() {
        return Err(diagnostic_error(
            Code::ResumeInputInvalid,
            "resume_agent: resume input must be nil or non-empty text",
        ));
    }
    worker.task = text.clone();
    worker.history.push(text);
    Ok(())
}

fn apply_resume_task_to_config(config: &mut WorkerConfig, task: &str) {
    if let WorkerConfig::SubAgent { spec } = config {
        spec.task = task.to_string();
    }
}

fn resume_input_rendered(input: Option<&VmValue>) -> Option<String> {
    let input = input?;
    let text = input.display();
    (!text.trim().is_empty()).then_some(text)
}

fn inferred_resume_initiator(options: &WorkerResumeOptions) -> String {
    if let Some(initiator) = options
        .initiator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return initiator.to_string();
    }
    if crate::agent_sessions::current_session_id().is_some() {
        "parent".to_string()
    } else {
        "operator".to_string()
    }
}

fn latest_payload_i64(payload: Option<&serde_json::Value>, key: &str) -> Option<i64> {
    payload.and_then(|payload| {
        payload.get(key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
    })
}

fn suspended_at_turn(worker: &WorkerState, suspension: Option<&WorkerSuspension>) -> Option<i64> {
    suspension
        .and_then(|suspension| suspension.suspended_at_turn)
        .or_else(|| latest_payload_i64(worker.latest_payload.as_ref(), "iterations_completed"))
        .or_else(|| latest_payload_i64(worker.latest_payload.as_ref(), "iterations"))
}

fn install_resume_continuity_payload(
    worker: &mut WorkerState,
    suspension: Option<&WorkerSuspension>,
    options: &WorkerResumeOptions,
    digest: Option<&str>,
) {
    let session_id = match &worker.config {
        WorkerConfig::SubAgent { spec } => spec.session_id.clone(),
        _ => return,
    };
    let suspended_turn = suspended_at_turn(worker, suspension);
    let trigger_id = worker
        .suspension
        .as_ref()
        .or(suspension)
        .and_then(|suspension| suspension.auto_resume_trigger.as_ref())
        .map(|trigger| trigger.id.clone());
    let worker_id = worker.id.clone();
    let worker_name = worker.name.clone();
    let worker_mode = worker.mode.clone();
    let reason = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.reason.clone())
        .unwrap_or_default();
    let suspension_initiator = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.initiator);
    let suspended_at = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.suspended_at.clone());
    let snapshot_ref = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.snapshot_ref.clone());
    let conditions = worker
        .suspension
        .as_ref()
        .or(suspension)
        .and_then(|suspension| suspension.conditions.clone());
    let initiator = inferred_resume_initiator(options);
    let input_rendered = resume_input_rendered(options.resume_input.as_ref());
    let input_json = options
        .resume_input
        .as_ref()
        .map(crate::llm::vm_value_to_json);
    let trigger_event = options.trigger_event.clone();
    let trigger = trigger_event.as_ref().map(|event| {
        serde_json::json!({
            "id": trigger_id,
            "event_id": event.get("id").and_then(|value| value.as_str()),
            "kind": event.get("kind").and_then(|value| value.as_str()),
            "provider": event.get("provider").and_then(|value| value.as_str()),
            "dedupe_key": event.get("dedupe_key").and_then(|value| value.as_str()),
            "timeout_action": event
                .get("provider_payload")
                .and_then(|payload| payload.get("raw"))
                .and_then(|raw| raw.get("on_timeout"))
                .and_then(|value| value.as_str()),
        })
    });
    let resume_initiator = if initiator == "timeout" {
        "timeout".to_string()
    } else if trigger.is_some() && initiator != "parent" && initiator != "operator" {
        "triggered".to_string()
    } else {
        initiator
    };
    let payload = serde_json::json!({
        "session_id": session_id,
        "session": {"id": session_id},
        "worker": {
            "id": worker_id,
            "name": worker_name,
            "mode": worker_mode,
        },
        "suspension": {
            "reason": reason,
            "initiator": suspension_initiator,
            "suspended_at": suspended_at,
            "snapshot_ref": snapshot_ref,
            "suspended_at_turn": suspended_turn,
            "conditions": conditions,
        },
        "reason": reason,
        "suspended_at_turn": suspended_turn.unwrap_or(0),
        "resume": {
            "initiator": resume_initiator,
            "input": input_json,
            "input_rendered": input_rendered,
            "input_present": options.resume_input.is_some(),
            "continue_transcript": options.continue_transcript,
            "digest": digest,
            "trigger": trigger,
        },
        "initiator": resume_initiator,
        "input": input_json,
        "input_rendered": input_rendered,
        "input_present": options.resume_input.is_some(),
        "continue_transcript": options.continue_transcript,
        "digest": digest,
        "turn": 0,
        "iteration": 0,
    });
    if let WorkerConfig::SubAgent { spec } = &mut worker.config {
        spec.options.insert(
            "_resume_continuity".to_string(),
            crate::stdlib::json_to_vm_value(&payload),
        );
    }
}

async fn unregister_suspension_auto_resume(
    suspension: Option<WorkerSuspension>,
) -> Result<(), VmError> {
    let Some(handle) = suspension.and_then(|suspension| suspension.auto_resume_trigger) else {
        return Ok(());
    };
    super::triggers_stdlib::unregister_auto_resume_trigger(&handle).await
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
    mut options: WorkerResumeOptions,
) -> Result<VmValue, VmError> {
    join_checkpointed_worker_handle(state.clone()).await?;
    // PreResume lifecycle gate. Block keeps the worker suspended; Modify can
    // amend the resume input before the transcript policy runs.
    let pre_worker_id = state.borrow().id.clone();
    let pre_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PreResume.as_str(),
        "worker": { "id": &pre_worker_id },
        "resume_input": options
            .resume_input
            .as_ref()
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(serde_json::Value::Null),
        "continue_transcript": options.continue_transcript,
    });
    match crate::orchestration::run_lifecycle_hooks_with_control(
        crate::orchestration::HookEvent::PreResume,
        &pre_payload,
    )
    .await?
    {
        crate::orchestration::HookControl::Allow => {}
        crate::orchestration::HookControl::Block { reason } => {
            // Blocked resume: do not transition out of suspended.
            let mut summary = worker_summary(&state.borrow())?;
            if let VmValue::Dict(map) = &mut summary {
                let mut entries = (**map).clone();
                entries.insert(
                    "pre_resume_denied".to_string(),
                    VmValue::String(Rc::from(reason)),
                );
                *map = Rc::new(entries);
            }
            return Ok(summary);
        }
        crate::orchestration::HookControl::Modify { payload } => {
            if let Some(new_input) = payload.get("resume_input") {
                if !new_input.is_null() {
                    options.resume_input = Some(crate::stdlib::json_to_vm_value(new_input));
                }
            }
        }
        crate::orchestration::HookControl::Decision { .. } => {}
    }
    let resume_digest = if options.continue_transcript {
        None
    } else {
        let transcript = state.borrow().transcript.clone();
        resume_digest_from_transcript(transcript.as_ref()).await?
    };
    let (worker_id, span_links) = {
        let worker = state.borrow();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "resume_agent: worker {} changed before resume could complete (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        let mut span_links = Vec::new();
        if let Some(suspension) = &worker.suspension {
            if let Some(link) = suspension.prior_span_link.clone() {
                span_links.push(link.with_attributes(BTreeMap::from([(
                    "harn.link.kind".to_string(),
                    "suspension".to_string(),
                )])));
            }
            if let Some(link) = suspension.pipeline_span_link.clone() {
                span_links.push(link.with_attributes(BTreeMap::from([(
                    "harn.link.kind".to_string(),
                    "pipeline".to_string(),
                )])));
            }
        }
        (worker.id.clone(), span_links)
    };
    let link_count = span_links.len();
    let mut resume_span = LifecycleSpanGuard::start_detached(
        crate::tracing::SpanKind::Resume,
        format!("resume {worker_id}"),
        span_links,
    );
    // Annotate the resume span with its shape (initiator, transcript
    // continuity, fresh input) and prior-span link count. The resume input can
    // contain transcript text or user data, so the span records only whether
    // fresh input exists.
    annotate_resume_span(
        &resume_span,
        &worker_id,
        &inferred_resume_initiator(&options),
        options.continue_transcript,
        options.resume_input.is_some(),
        link_count,
    );
    let (snapshot, suspension) = {
        let mut worker = state.borrow_mut();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "resume_agent: worker {} changed before resume could complete (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        worker.suspend_signal.store(false, Ordering::SeqCst);
        let suspension = worker.suspension.take();
        worker.status = "running".to_string();
        worker.finished_at = None;
        worker.latest_error = None;
        apply_resume_transcript_policy(
            &mut worker,
            options.continue_transcript,
            resume_digest.clone(),
        );
        apply_resume_input(&mut worker, options.resume_input.as_ref())?;
        let task = worker.task.clone();
        apply_resume_task_to_config(&mut worker.config, &task);
        install_resume_continuity_payload(
            &mut worker,
            suspension.as_ref(),
            &options,
            resume_digest.as_deref(),
        );
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        (worker_event_snapshot(&worker), suspension)
    };
    unregister_suspension_auto_resume(suspension).await?;
    emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerResumed).await?;
    resume_span.end();
    if crate::vm::clone_async_builtin_child_vm().is_some() {
        respawn_worker_task(state.clone())?;
    }
    // PostResume hooks are advisory; the resumed turn is about to start.
    let post_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PostResume.as_str(),
        "worker": { "id": &worker_id },
    });
    crate::orchestration::run_lifecycle_hooks(
        crate::orchestration::HookEvent::PostResume,
        &post_payload,
    )
    .await?;
    let summary = worker_summary(&state.borrow())?;
    Ok(summary)
}

pub(crate) async fn resume_worker_from_auto_resume_trigger(
    worker_id: &str,
    event: &crate::triggers::TriggerEvent,
) -> Result<VmValue, VmError> {
    let payload = auto_resume_event_payload(event);
    let timeout_action = auto_resume_timeout_action(&payload);
    if timeout_action.as_deref() == Some("fail") {
        return fail_suspended_worker_from_auto_resume_timeout(worker_id).await;
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
    warm_resume_worker(state, options).await
}

fn auto_resume_event_payload(event: &crate::triggers::TriggerEvent) -> serde_json::Value {
    let payload = serde_json::to_value(&event.provider_payload).unwrap_or(serde_json::Value::Null);
    payload.get("raw").cloned().unwrap_or(payload)
}

fn auto_resume_timeout_action(payload: &serde_json::Value) -> Option<String> {
    if payload.get("type").and_then(|value| value.as_str()) != Some("auto_resume.timeout") {
        return None;
    }
    payload
        .get("on_timeout")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

async fn fail_suspended_worker_from_auto_resume_timeout(
    worker_id: &str,
) -> Result<VmValue, VmError> {
    let state = with_worker_state(worker_id, Ok)?;
    let (snapshot, summary, suspension) = {
        let mut worker = state.borrow_mut();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "auto-resume timeout: worker {} is no longer suspended (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        worker.suspend_signal.store(false, Ordering::SeqCst);
        let suspension = worker.suspension.take();
        worker.status = "failed".to_string();
        worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
        worker.latest_error = Some("auto-resume timeout expired".to_string());
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        (
            worker_event_snapshot(&worker),
            worker_summary(&worker)?,
            suspension,
        )
    };
    unregister_suspension_auto_resume(suspension).await?;
    emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerFailed).await?;
    Ok(summary)
}

fn cold_load_worker(snapshot_target: &str) -> Result<Rc<RefCell<WorkerState>>, VmError> {
    let state = Rc::new(RefCell::new(
        load_worker_state_snapshot(snapshot_target).map_err(|error| {
            diagnostic_error(
                Code::ResumeSnapshotInvalid,
                format!("resume_agent: failed to load snapshot `{snapshot_target}`: {error}"),
            )
        })?,
    ));
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
    let state = with_worker_state(&worker_id, Ok)?;
    let (snapshot, summary, suspension) = {
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
        let suspension = worker.suspension.take();
        worker.latest_error = Some("worker cancelled".to_string());
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        let snapshot = worker_event_snapshot(&worker);
        let summary = worker_summary(&worker)?;
        (snapshot, summary, suspension)
    };
    unregister_suspension_auto_resume(suspension).await?;
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
    diagnostic_error(
        Code::ResumeConditionsInvalid,
        format!("invalid ResumeConditions.{field}: {}", message.into()),
    )
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
        VmValue::task_handle(worker_id.to_string())
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

    fn auto_resume_conditions(kind: &str) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([(
            "trigger".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "kind".to_string(),
                    VmValue::String(Rc::from(kind.to_string())),
                ),
                ("provider".to_string(), VmValue::String(Rc::from("github"))),
            ]))),
        )])))
    }

    fn auto_resume_conditions_with_timeout(kind: &str, on_timeout: &str) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "trigger".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([
                    (
                        "kind".to_string(),
                        VmValue::String(Rc::from(kind.to_string())),
                    ),
                    ("provider".to_string(), VmValue::String(Rc::from("github"))),
                ]))),
            ),
            (
                "timeout".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([
                    ("duration_minutes".to_string(), VmValue::Int(1)),
                    (
                        "on_timeout".to_string(),
                        VmValue::String(Rc::from(on_timeout.to_string())),
                    ),
                ]))),
            ),
        ])))
    }

    fn suspend_options(conditions: VmValue) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("initiator".to_string(), VmValue::String(Rc::from("self"))),
            ("conditions".to_string(), conditions),
        ])))
    }

    fn auto_resume_trigger_id(summary: &VmValue) -> String {
        let json = crate::llm::vm_value_to_json(summary);
        json["suspension"]["auto_resume_trigger"]["id"]
            .as_str()
            .expect("auto-resume trigger id")
            .to_string()
    }

    fn worker_status_and_task(worker_id: &str) -> (String, String) {
        WORKER_REGISTRY.with(|registry| {
            let state = registry.borrow().get(worker_id).cloned().unwrap();
            let worker = state.borrow();
            (worker.status.clone(), worker.task.clone())
        })
    }

    fn assert_error_code(error: VmError, code: Code) {
        let message = error.to_string();
        assert!(
            message.contains(code.as_str()),
            "expected {}, got: {message}",
            code.as_str()
        );
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
    async fn suspended_subagent_snapshot_includes_active_suspension_metadata() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-suspend-snapshot");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting on external review")),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "initiator".to_string(),
                VmValue::String(Rc::from("parent")),
            )]))),
        ])
        .await
        .expect("suspend worker");

        let snapshot = snapshot_suspended_subagents();
        let item = snapshot
            .iter()
            .find(|item| item["handle"] == worker_id)
            .expect("suspended worker should be in unsettled snapshot");
        assert_eq!(item["status"], "suspended");
        assert_eq!(item["reason"], "waiting on external review");
        assert_eq!(item["initiator"], "parent");
        assert!(
            item["age_ms"].as_i64().unwrap_or(-1) >= 0,
            "snapshot age must be a non-negative duration"
        );
        assert!(
            item["snapshot_ref"]
                .as_str()
                .is_some_and(|path| !path.is_empty()),
            "suspended workers should expose their durable snapshot path"
        );

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panic_suspend_worker_suspends_running_worker_and_emits_event() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-panic-running");

        let outcome = panic_suspend_worker(&worker_id, "build_storm")
            .await
            .expect("panic suspend running worker");
        assert_eq!(outcome, PanicSuspendOutcome::Suspended);

        // Verify the worker is in the suspended state with the panic
        // reason recorded and the WorkerSuspension envelope installed.
        with_worker_state(&worker_id, |state| {
            let worker = state.borrow();
            assert_eq!(worker.status, "suspended");
            assert!(worker.suspend_signal.load(Ordering::SeqCst));
            let suspension = worker.suspension.as_ref().expect("suspension envelope");
            assert_eq!(suspension.reason, "build_storm");
            assert_eq!(suspension.initiator, SuspendInitiator::Triggered);
            Ok(())
        })
        .unwrap();

        // Verify the unsettled snapshot also exposes the panic suspension
        // (the same observability surface operators use to see paused
        // workers via `harn agents list --suspended`).
        let snapshot = snapshot_suspended_subagents();
        let item = snapshot
            .iter()
            .find(|item| item["handle"] == worker_id)
            .expect("panic-suspended worker visible in unsettled snapshot");
        assert_eq!(item["status"], "suspended");
        assert_eq!(item["reason"], "build_storm");
        assert_eq!(item["initiator"], "triggered");

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panic_suspend_worker_skips_already_suspended_and_terminal_and_unknown() {
        let _guard = suspend_test_lock().await;

        // Already-suspended: first suspend via the normal path, then
        // verify the panic broadcast is a no-op (no double-suspend, no
        // overwritten reason, no second event).
        let (already_suspended, dir1) = seed_test_worker("worker-panic-already-suspended");
        suspend_agent_builtin(vec![
            handle_value(&already_suspended),
            VmValue::String(Rc::from("operator pause")),
        ])
        .await
        .expect("operator-suspend first");

        let outcome = panic_suspend_worker(&already_suspended, "build_storm")
            .await
            .expect("panic against already-suspended");
        assert_eq!(outcome, PanicSuspendOutcome::AlreadySuspended);

        // Reason from the original suspension MUST be preserved — the
        // panic broadcast must never silently rewrite a pre-existing
        // suspension reason.
        with_worker_state(&already_suspended, |state| {
            let suspension = state
                .borrow()
                .suspension
                .clone()
                .expect("original suspension still present");
            assert_eq!(suspension.reason, "operator pause");
            Ok(())
        })
        .unwrap();

        // Terminal (manually flipped): a worker that is no longer running
        // must be skipped with a `not_running` outcome.
        let (terminal, dir2) = seed_test_worker("worker-panic-terminal");
        with_worker_state(&terminal, |state| {
            state.borrow_mut().status = "completed".to_string();
            Ok(())
        })
        .unwrap();
        let outcome = panic_suspend_worker(&terminal, "build_storm")
            .await
            .expect("panic against terminal worker");
        assert_eq!(outcome, PanicSuspendOutcome::NotRunning);

        // Unknown: stale worker id never crashes the broadcast.
        let outcome = panic_suspend_worker("worker-does-not-exist", "build_storm")
            .await
            .expect("panic against unknown worker");
        assert_eq!(outcome, PanicSuspendOutcome::Unknown);

        teardown(&dir1, &already_suspended);
        teardown(&dir2, &terminal);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn all_registered_worker_ids_returns_btreemap_order() {
        let _guard = suspend_test_lock().await;
        let (a, dir_a) = seed_test_worker("worker-broadcast-A");
        let (b, dir_b) = seed_test_worker("worker-broadcast-B");

        let ids = all_registered_worker_ids();
        // BTreeMap iteration order is sorted by key — deterministic
        // across replay, which the dispatcher relies on for the
        // panic-broadcast per-worker audit order.
        assert!(ids.contains(&a), "registry must include seeded worker A");
        assert!(ids.contains(&b), "registry must include seeded worker B");
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "all_registered_worker_ids must return a deterministic (sorted) order"
        );

        teardown(&dir_a, &a);
        teardown(&dir_b, &b);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reset_agent_worker_state_clears_suspended_snapshot() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-reset-snapshot");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting on external review")),
        ])
        .await
        .expect("suspend worker");
        assert!(
            snapshot_suspended_subagents()
                .iter()
                .any(|item| item["handle"] == worker_id),
            "seeded suspension should be visible before reset"
        );

        reset_agent_worker_state();

        assert!(
            snapshot_suspended_subagents().is_empty(),
            "stdlib reset should not leave workers visible to later lifecycle snapshots"
        );
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
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
    async fn suspend_registers_auto_resume_trigger_and_operator_resume_unregisters() {
        let _guard = suspend_test_lock().await;
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        let (worker_id, dir) = seed_test_worker("worker-auto-resume-register");

        let suspended = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting for review")),
            suspend_options(auto_resume_conditions("review.approved")),
        ])
        .await
        .expect("suspend with auto-resume trigger");
        assert_eq!(summary_status(&suspended), "suspended");
        let trigger_id = auto_resume_trigger_id(&suspended);
        let binding = crate::triggers::resolve_live_trigger_binding(&trigger_id, None)
            .expect("registered auto-resume binding");
        assert_eq!(binding.kind, "auto_resume");
        assert_eq!(binding.handler.kind(), "auto_resume");
        assert_eq!(binding.match_events, vec!["review.approved".to_string()]);

        let resumed = resume_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("operator resume");
        assert_eq!(summary_status(&resumed), "running");
        let snapshot = crate::triggers::snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == trigger_id)
            .expect("auto-resume binding snapshot");
        assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

        teardown(&dir, &worker_id);
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn top_level_suspend_registers_auto_resume_trigger() {
        let _guard = suspend_test_lock().await;
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        let dir = std::env::temp_dir().join(format!(
            "harn-top-level-suspend-test-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };

        let suspended = top_level_agent_suspend_builtin(vec![
            VmValue::String(Rc::from("session-top-level-auto-resume")),
            VmValue::String(Rc::from("continue the top-level task")),
            VmValue::Nil,
            VmValue::Dict(Rc::new(BTreeMap::new())),
            VmValue::String(Rc::from("waiting for review")),
            auto_resume_conditions("review.approved"),
        ])
        .await
        .expect("top-level suspend with auto-resume trigger");
        assert_eq!(summary_status(&suspended), "suspended");
        let worker_id = suspended
            .as_dict()
            .and_then(|dict| dict.get("id"))
            .map(VmValue::display)
            .expect("worker id");
        let trigger_id = auto_resume_trigger_id(&suspended);
        let binding = crate::triggers::resolve_live_trigger_binding(&trigger_id, None)
            .expect("registered top-level auto-resume binding");
        assert_eq!(binding.kind, "auto_resume");
        assert_eq!(binding.handler.kind(), "auto_resume");
        assert_eq!(binding.match_events, vec!["review.approved".to_string()]);

        teardown(&dir, &worker_id);
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matching_trigger_event_auto_resumes_worker_and_unregisters() {
        let _guard = suspend_test_lock().await;
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        let log = crate::event_log::install_memory_for_current_thread(128);
        let (worker_id, dir) = seed_test_worker("worker-auto-resume-dispatch");

        let suspended = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting for review")),
            suspend_options(auto_resume_conditions("review.approved")),
        ])
        .await
        .expect("suspend with auto-resume trigger");
        let trigger_id = auto_resume_trigger_id(&suspended);
        let event = crate::triggers::TriggerEvent::new(
            crate::triggers::ProviderId::from("github"),
            "review.approved",
            None,
            "delivery-auto-resume",
            None,
            BTreeMap::new(),
            crate::triggers::ProviderPayload::Extension(
                crate::triggers::ExtensionProviderPayload {
                    provider: "github".to_string(),
                    schema_name: "ReviewEvent".to_string(),
                    raw: serde_json::json!({"decision": "approved"}),
                },
            ),
            crate::triggers::SignatureStatus::Unsigned,
        );
        let dispatcher = crate::triggers::Dispatcher::with_event_log(crate::Vm::new(), log);
        let outcomes = dispatcher
            .dispatch_event(event)
            .await
            .expect("dispatch auto-resume event");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].status,
            crate::triggers::DispatchStatus::Succeeded
        );
        assert_eq!(outcomes[0].handler_kind, "auto_resume");

        let (status, task) = worker_status_and_task(&worker_id);
        assert_eq!(status, "running");
        assert!(
            task.contains("approved"),
            "resume input should carry trigger payload, got: {task}"
        );
        let snapshot = crate::triggers::snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == trigger_id)
            .expect("auto-resume binding snapshot");
        assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

        teardown(&dir, &worker_id);
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_resume_timeout_dispatches_synthetic_resume_input() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let _guard = suspend_test_lock().await;
                crate::triggers::clear_trigger_registry();
                crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
                let log = crate::event_log::install_memory_for_current_thread(128);
                drop(log);
                let clock = crate::triggers::test_util::clock::MockClock::at_wall_ms(0);
                let _clock_guard =
                    crate::triggers::test_util::clock::install_override(clock.clone());
                let (worker_id, dir) = seed_test_worker("worker-auto-resume-timeout");

                let suspended = suspend_agent_builtin(vec![
                    handle_value(&worker_id),
                    VmValue::String(Rc::from("waiting for review or timeout")),
                    suspend_options(auto_resume_conditions_with_timeout(
                        "review.approved",
                        "resume_with_input",
                    )),
                ])
                .await
                .expect("suspend with auto-resume timeout");
                let trigger_id = auto_resume_trigger_id(&suspended);

                tokio::task::yield_now().await;
                clock.advance_std(std::time::Duration::from_mins(1)).await;
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;

                let (status, task) = worker_status_and_task(&worker_id);
                assert_eq!(status, "running");
                assert!(
                    task.contains("auto_resume.timeout"),
                    "timeout resume input should name synthetic event, got: {task}"
                );
                let snapshot = crate::triggers::snapshot_trigger_bindings()
                    .into_iter()
                    .find(|binding| binding.id == trigger_id)
                    .expect("auto-resume binding snapshot");
                assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

                teardown(&dir, &worker_id);
                crate::triggers::clear_trigger_registry();
                crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
            })
            .await;
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
        let resume_context = WORKER_REGISTRY
            .with(|registry| {
                let state = registry.borrow().get(&worker_id).cloned().unwrap();
                let worker = state.borrow();
                match &worker.config {
                    WorkerConfig::SubAgent { spec } => {
                        spec.options.get("_resume_continuity").cloned()
                    }
                    _ => None,
                }
            })
            .expect("resume continuity payload");
        let resume_context = crate::llm::vm_value_to_json(&resume_context);
        assert_eq!(
            resume_context["continue_transcript"],
            serde_json::json!(false)
        );
        assert_eq!(resume_context["reason"], "park before fresh prompt");
        assert_eq!(resume_context["suspended_at_turn"], 0);
        assert_eq!(resume_context["digest"], "Prior digest");
        assert_eq!(resume_context["input_rendered"], "fresh prompt");

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
    async fn suspend_resume_links_lifecycle_spans_across_snapshot_reload() {
        let _guard = suspend_test_lock().await;
        crate::tracing::reset_tracing();
        crate::tracing::set_tracing_enabled(true);
        let (worker_id, dir) = seed_test_worker("worker-suspend-resume-trace-link");
        let snapshot_path = WORKER_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&worker_id)
                .unwrap()
                .borrow()
                .snapshot_path
                .clone()
        });

        let pipeline_span_id =
            crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "pipeline".to_string());
        let pipeline_span_link =
            crate::tracing::span_link(pipeline_span_id).expect("open pipeline span link");
        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("checkpoint before restart")),
        ])
        .await
        .expect("suspend");
        crate::tracing::span_end(pipeline_span_id);

        WORKER_REGISTRY.with(|registry| {
            registry.borrow_mut().remove(&worker_id);
        });
        let reloaded =
            agents_workers::load_worker_state_snapshot(&snapshot_path).expect("reload snapshot");
        let suspension_record = reloaded.suspension.as_ref().expect("snapshot suspension");
        let prior_span_link = suspension_record
            .prior_span_link
            .clone()
            .expect("snapshot preserves prior span link");
        let reloaded_pipeline_span_link = suspension_record
            .pipeline_span_link
            .clone()
            .expect("snapshot preserves pipeline span link");
        assert!(!prior_span_link.trace_id.is_empty());
        assert!(!prior_span_link.span_id.is_empty());
        assert_eq!(
            reloaded_pipeline_span_link.trace_id,
            pipeline_span_link.trace_id
        );
        assert_eq!(
            reloaded_pipeline_span_link.span_id,
            pipeline_span_link.span_id
        );

        WORKER_REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(worker_id.clone(), Rc::new(RefCell::new(reloaded)));
        });
        resume_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("resume after restart");

        let spans = crate::tracing::peek_spans();
        let suspension = spans
            .iter()
            .find(|span| span.kind == crate::tracing::SpanKind::Suspension)
            .expect("suspension span");
        let resume = spans
            .iter()
            .find(|span| span.kind == crate::tracing::SpanKind::Resume)
            .expect("resume span");
        assert_eq!(suspension.name, format!("suspend {worker_id}"));
        assert_eq!(resume.name, format!("resume {worker_id}"));
        assert_eq!(resume.parent_id, None);
        assert_eq!(resume.links.len(), 2);
        let suspension_link = resume
            .links
            .iter()
            .find(|link| {
                link.attributes.get("harn.link.kind").map(String::as_str) == Some("suspension")
            })
            .expect("resume links to suspension");
        let pipeline_link = resume
            .links
            .iter()
            .find(|link| {
                link.attributes.get("harn.link.kind").map(String::as_str) == Some("pipeline")
            })
            .expect("resume links to pipeline");
        assert_eq!(suspension_link.trace_id, prior_span_link.trace_id);
        assert_eq!(suspension_link.span_id, prior_span_link.span_id);
        assert_eq!(pipeline_link.trace_id, pipeline_span_link.trace_id);
        assert_eq!(pipeline_link.span_id, pipeline_span_link.span_id);

        teardown(&dir, &worker_id);
        crate::tracing::set_tracing_enabled(false);
        crate::tracing::reset_tracing();
    }

    /// Verify the suspension and resume spans carry the documented attribute
    /// bag (`handle`, `reason`, `initiator`,
    /// `pipeline_id` on suspend; `handle`, `initiator`,
    /// `continue_transcript`, `had_resume_input`,
    /// `linked_suspension_count` on resume). These attributes let OTel
    /// consumers slice paused agents without joining back to the worker
    /// registry. The test also doubles as a privacy check: resume_input
    /// content must not appear on the resume span.
    #[tokio::test(flavor = "current_thread")]
    async fn suspend_resume_spans_carry_canonical_attribute_bag() {
        let _guard = suspend_test_lock().await;
        crate::tracing::reset_tracing();
        crate::tracing::set_tracing_enabled(true);
        let (worker_id, dir) = seed_test_worker("worker-suspend-resume-attributes");

        let pipeline_span_id =
            crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "pipeline".to_string());
        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("waiting on review")),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "initiator".to_string(),
                VmValue::String(Rc::from("triggered")),
            )]))),
        ])
        .await
        .expect("suspend");
        crate::tracing::span_end(pipeline_span_id);

        resume_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "input".to_string(),
                    VmValue::String(Rc::from("CONFIDENTIAL_DO_NOT_LEAK_INTO_SPAN_ATTRS")),
                ),
                ("continue_transcript".to_string(), VmValue::Bool(false)),
            ]))),
        ])
        .await
        .expect("resume");

        let spans = crate::tracing::peek_spans();
        let suspension = spans
            .iter()
            .find(|span| span.kind == crate::tracing::SpanKind::Suspension)
            .expect("suspension span");
        assert_eq!(
            suspension.metadata.get("handle").and_then(|v| v.as_str()),
            Some(worker_id.as_str()),
            "suspend span exposes handle"
        );
        assert_eq!(
            suspension
                .metadata
                .get("worker_id")
                .and_then(|v| v.as_str()),
            Some(worker_id.as_str()),
            "suspend span exposes worker_id"
        );
        assert_eq!(
            suspension.metadata.get("reason").and_then(|v| v.as_str()),
            Some("waiting on review")
        );
        assert_eq!(
            suspension
                .metadata
                .get("initiator")
                .and_then(|v| v.as_str()),
            Some("triggered"),
            "initiator parsed and exposed"
        );
        assert_eq!(
            suspension
                .metadata
                .get("pipeline_id")
                .and_then(|v| v.as_str()),
            Some(pipeline_span_id.to_string()).as_deref(),
            "pipeline_id alias of pipeline_span_id is present"
        );
        assert_eq!(
            suspension
                .metadata
                .get("pipeline_span_id")
                .and_then(|v| v.as_str()),
            Some(pipeline_span_id.to_string()).as_deref(),
            "pipeline_span_id is present"
        );
        assert_eq!(
            suspension
                .metadata
                .get("has_conditions")
                .and_then(|v| v.as_bool()),
            Some(false)
        );

        let resume = spans
            .iter()
            .find(|span| span.kind == crate::tracing::SpanKind::Resume)
            .expect("resume span");
        assert_eq!(
            resume.metadata.get("handle").and_then(|v| v.as_str()),
            Some(worker_id.as_str())
        );
        assert!(
            resume.metadata.contains_key("initiator"),
            "resume span has initiator"
        );
        assert_eq!(
            resume
                .metadata
                .get("continue_transcript")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            resume
                .metadata
                .get("had_resume_input")
                .and_then(|v| v.as_bool()),
            Some(true),
            "resume span flags presence of resume_input without leaking value"
        );
        assert_eq!(
            resume
                .metadata
                .get("linked_suspension_count")
                .and_then(|v| v.as_u64()),
            Some(2)
        );

        // Privacy guard: the resume_input value itself must not be
        // serialised into any resume-span attribute.
        let serialized = serde_json::to_string(&resume.metadata).expect("serialize metadata");
        assert!(
            !serialized.contains("CONFIDENTIAL_DO_NOT_LEAK_INTO_SPAN_ATTRS"),
            "resume_input content leaked into span metadata: {serialized}"
        );

        teardown(&dir, &worker_id);
        crate::tracing::set_tracing_enabled(false);
        crate::tracing::reset_tracing();
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
    async fn resume_rejects_live_non_suspended_workers() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-resume-not-suspended");

        let err = resume_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect_err("running worker should reject warm resume");
        assert_error_code(err, Code::ResumeWorkerNotSuspended);

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_rejects_closed_suspended_workers() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-resume-closed");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("park before close")),
        ])
        .await
        .expect("suspend");
        close_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect("close");

        let err = resume_agent_builtin(vec![handle_value(&worker_id)])
            .await
            .expect_err("closed worker should reject resume");
        assert_error_code(err, Code::ResumeWorkerClosed);

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_rejects_empty_resume_input() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-empty-resume-input");

        suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("park before empty input")),
        ])
        .await
        .expect("suspend");
        let err = resume_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("")),
        ])
        .await
        .expect_err("empty resume input should be rejected");
        assert_error_code(err, Code::ResumeInputInvalid);

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_missing_snapshot_reports_sus_004() {
        let _guard = suspend_test_lock().await;
        let dir = std::env::temp_dir().join(format!(
            "harn-missing-snapshot-test-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing-worker.json");

        let err = resume_agent_builtin(vec![VmValue::String(Rc::from(
            missing.display().to_string(),
        ))])
        .await
        .expect_err("missing snapshot should reject resume");
        assert_error_code(err, Code::ResumeSnapshotInvalid);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_warm_resume_reports_sus_006() {
        let _guard = suspend_test_lock().await;
        let (worker_id, dir) = seed_test_worker("worker-concurrent-resume");
        let state = with_worker_state(&worker_id, Ok).unwrap();

        let err = warm_resume_worker(state, WorkerResumeOptions::default())
            .await
            .expect_err("non-suspended warm resume should be a conflict");
        assert_error_code(err, Code::ConcurrentResumeConflict);

        teardown(&dir, &worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn raw_suspend_trigger_registration_reports_sus_007() {
        let _guard = suspend_test_lock().await;
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        let (worker_id, dir) = seed_test_worker("worker-trigger-registration-error");
        let invalid_trigger = VmValue::Dict(Rc::new(BTreeMap::from([(
            "trigger".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::new())),
        )])));

        let err = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("bad trigger")),
            suspend_options(invalid_trigger),
        ])
        .await
        .expect_err("invalid raw trigger registration should fail");
        assert_error_code(err, Code::ResumeTriggerRegistrationFailed);

        teardown(&dir, &worker_id);
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn raw_suspend_timeout_action_reports_sus_008() {
        let _guard = suspend_test_lock().await;
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        let (worker_id, dir) = seed_test_worker("worker-timeout-action-error");

        let err = suspend_agent_builtin(vec![
            handle_value(&worker_id),
            VmValue::String(Rc::from("bad timeout")),
            suspend_options(auto_resume_conditions_with_timeout(
                "review.approved",
                "explode",
            )),
        ])
        .await
        .expect_err("unsupported raw timeout action should fail");
        assert_error_code(err, Code::ResumeTimeoutUnsupported);

        teardown(&dir, &worker_id);
        crate::triggers::clear_trigger_registry();
        crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
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
                message.contains(Code::SuspendWorkerNotRunning.as_str()),
                "expected HARN-SUS-001 terminal-rejection message, got: {message}"
            ),
            other => panic!("expected Runtime error, got {other:?}"),
        }

        teardown(&dir, &worker_id);
    }
}
