use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{TimeZone, Utc};
use croner::Cron;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};

mod run;
mod types;

pub(crate) use run::{
    begin_persona_trigger, complete_persona_run, fail_persona_run, PersonaRunAdmission,
};
pub use run::{fire_schedule, fire_trigger, record_persona_spend};
pub use types::*;

use run::run_for_envelope;

pub const PERSONA_RUNTIME_TOPIC: &str = "persona.runtime.events";

struct TypedSinkRegistry<T: ?Sized + Send + Sync> {
    sinks: RwLock<Vec<(u64, Arc<T>)>>,
    next_id: AtomicU64,
}

impl<T: ?Sized + Send + Sync> TypedSinkRegistry<T> {
    const fn new() -> Self {
        Self {
            sinks: RwLock::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn register(&self, sink: Arc<T>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut sinks) = self.sinks.write() {
            sinks.push((id, sink));
        }
        id
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut sinks) = self.sinks.write() {
            sinks.retain(|(existing, _)| *existing != id);
        }
    }

    fn snapshot(&self) -> Vec<Arc<T>> {
        self.sinks
            .read()
            .map(|sinks| sinks.iter().map(|(_, sink)| Arc::clone(sink)).collect())
            .unwrap_or_default()
    }
}

fn persona_value_sinks() -> &'static TypedSinkRegistry<dyn PersonaValueSink> {
    static REGISTRY: OnceLock<TypedSinkRegistry<dyn PersonaValueSink>> = OnceLock::new();
    REGISTRY.get_or_init(TypedSinkRegistry::new)
}

fn persona_supervision_sinks() -> &'static TypedSinkRegistry<dyn PersonaSupervisionSink> {
    static REGISTRY: OnceLock<TypedSinkRegistry<dyn PersonaSupervisionSink>> = OnceLock::new();
    REGISTRY.get_or_init(TypedSinkRegistry::new)
}

#[must_use = "dropping the registration immediately unregisters the sink"]
pub struct PersonaValueSinkRegistration {
    id: u64,
}

impl Drop for PersonaValueSinkRegistration {
    fn drop(&mut self) {
        persona_value_sinks().unregister(self.id);
    }
}

pub fn register_persona_value_sink(
    sink: Arc<dyn PersonaValueSink>,
) -> PersonaValueSinkRegistration {
    PersonaValueSinkRegistration {
        id: persona_value_sinks().register(sink),
    }
}

#[must_use = "dropping the registration immediately unregisters the sink"]
pub struct PersonaSupervisionSinkRegistration {
    id: u64,
}

impl Drop for PersonaSupervisionSinkRegistration {
    fn drop(&mut self) {
        persona_supervision_sinks().unregister(self.id);
    }
}

pub fn register_persona_supervision_sink(
    sink: Arc<dyn PersonaSupervisionSink>,
) -> PersonaSupervisionSinkRegistration {
    PersonaSupervisionSinkRegistration {
        id: persona_supervision_sinks().register(sink),
    }
}

pub async fn persona_status(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    now_ms: i64,
) -> Result<PersonaStatus, String> {
    let events = read_persona_events(log, &binding.name).await?;
    let mut state = PersonaLifecycleState::Idle;
    let mut last_run_ms = None;
    let mut active_lease = None;
    let mut last_error = None;
    let mut queued = BTreeSet::<String>::new();
    let mut completed = BTreeSet::<String>::new();
    let mut disabled_events = 0usize;
    let mut budget_receipt = None;
    let mut budget_exhaustion_reason = None;
    let mut spent = Vec::<(i64, f64, u64)>::new();
    let mut queued_work = BTreeMap::<String, PersonaQueuedWork>::new();
    let mut value_receipts = Vec::<PersonaValueReceipt>::new();

    for (_, event) in events {
        match event.kind.as_str() {
            "persona.control.paused" => state = PersonaLifecycleState::Paused,
            "persona.control.resumed" => state = PersonaLifecycleState::Idle,
            "persona.control.disabled" => state = PersonaLifecycleState::Disabled,
            "persona.control.draining" => state = PersonaLifecycleState::Draining,
            "persona.lease.acquired" => {
                if let Ok(lease) = serde_json::from_value::<PersonaLease>(event.payload.clone()) {
                    active_lease = Some(lease);
                    state = PersonaLifecycleState::Running;
                }
            }
            "persona.lease.released" => {
                active_lease = None;
                if !matches!(
                    state,
                    PersonaLifecycleState::Paused | PersonaLifecycleState::Disabled
                ) {
                    state = PersonaLifecycleState::Idle;
                }
            }
            "persona.lease.expired" => {
                active_lease = None;
                if !matches!(
                    state,
                    PersonaLifecycleState::Paused | PersonaLifecycleState::Disabled
                ) {
                    state = PersonaLifecycleState::Idle;
                }
            }
            "persona.run.started" => state = PersonaLifecycleState::Running,
            "persona.run.completed" => {
                last_run_ms = event
                    .payload
                    .get("completed_at_ms")
                    .and_then(serde_json::Value::as_i64)
                    .or(Some(event.occurred_at_ms));
                if let Some(work_key) = event
                    .payload
                    .get("work_key")
                    .and_then(serde_json::Value::as_str)
                {
                    completed.insert(work_key.to_string());
                }
                if !matches!(
                    state,
                    PersonaLifecycleState::Paused | PersonaLifecycleState::Disabled
                ) {
                    state = PersonaLifecycleState::Idle;
                }
            }
            "persona.run.failed" => {
                state = PersonaLifecycleState::Failed;
                last_error = event
                    .payload
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string);
            }
            "persona.trigger.queued" => {
                if let Some(work_key) = event
                    .payload
                    .get("work_key")
                    .and_then(serde_json::Value::as_str)
                {
                    queued.insert(work_key.to_string());
                }
                if let Some(item) = queued_work_from_event(&event)? {
                    queued_work.insert(item.work_key.clone(), item);
                }
            }
            "persona.trigger.dead_lettered" => disabled_events += 1,
            "persona.budget.recorded" => {
                budget_receipt = event
                    .payload
                    .get("receipt_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string);
                spent.push((
                    event.occurred_at_ms,
                    event
                        .payload
                        .get("cost_usd")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or_default(),
                    event
                        .payload
                        .get("tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                ));
            }
            "persona.budget.exhausted" => {
                budget_exhaustion_reason = event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string);
                last_error = budget_exhaustion_reason
                    .as_ref()
                    .map(|reason| format!("persona budget exhausted: {reason}"));
                budget_receipt = event
                    .payload
                    .get("receipt_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string);
            }
            kind if kind.starts_with("persona.value.") => {
                if let Some(receipt) = value_receipt_from_event(&event)? {
                    value_receipts.push(receipt);
                }
            }
            _ => {}
        }
    }

    if let Some(lease) = active_lease.as_ref() {
        if lease.expires_at_ms <= now_ms {
            active_lease = None;
            if !matches!(
                state,
                PersonaLifecycleState::Paused | PersonaLifecycleState::Disabled
            ) {
                state = PersonaLifecycleState::Idle;
            }
        }
    }

    queued.retain(|work_key| !completed.contains(work_key));
    queued_work.retain(|work_key, _| !completed.contains(work_key));
    let queued_work = queued_work.into_values().collect::<Vec<_>>();
    let handoff_inbox = queued_work
        .iter()
        .filter_map(handoff_inbox_item)
        .collect::<Vec<_>>();

    let mut budget = budget_status(&binding.budget, &spent, now_ms);
    if budget.reason.is_none() {
        if let Some(reason) = budget_exhaustion_reason {
            budget.exhausted = true;
            budget.reason = Some(reason);
        }
    }
    if budget.last_receipt_id.is_none() {
        budget.last_receipt_id = budget_receipt;
    }

    let current_assignment = active_lease.as_ref().map(assignment_status_from_lease);

    Ok(PersonaStatus {
        name: binding.name.clone(),
        template_ref: binding.template_ref.clone(),
        state,
        entry_workflow: binding.entry_workflow.clone(),
        role: binding.name.clone(),
        current_assignment,
        last_run: last_run_ms.map(format_ms),
        next_scheduled_run: next_scheduled_run(binding, last_run_ms, now_ms),
        active_lease,
        budget,
        last_error,
        queued_events: queued.len(),
        queued_work,
        handoff_inbox,
        value_receipts,
        disabled_events,
        paused_event_policy: "queue_then_drain_on_resume".to_string(),
    })
}

pub async fn pause_persona(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    now_ms: i64,
) -> Result<PersonaStatus, String> {
    append_persona_event(
        log,
        &binding.name,
        "persona.control.paused",
        json!({"paused_at_ms": now_ms, "policy": "queue_then_drain_on_resume"}),
        now_ms,
    )
    .await?;
    persona_status(log, binding, now_ms).await
}

pub async fn resume_persona(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    now_ms: i64,
) -> Result<PersonaStatus, String> {
    append_persona_event(
        log,
        &binding.name,
        "persona.control.resumed",
        json!({"resumed_at_ms": now_ms, "drain": true}),
        now_ms,
    )
    .await?;
    let queued = queued_events(log, &binding.name).await?;
    for (envelope, cost) in queued {
        let _ = run_for_envelope(log, binding, envelope, cost, now_ms).await?;
    }
    persona_status(log, binding, now_ms).await
}

pub async fn disable_persona(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    now_ms: i64,
) -> Result<PersonaStatus, String> {
    append_persona_event(
        log,
        &binding.name,
        "persona.control.disabled",
        json!({"disabled_at_ms": now_ms}),
        now_ms,
    )
    .await?;
    persona_status(log, binding, now_ms).await
}

/// Report a `repair_worker_status` lifecycle transition for a sandboxed PR
/// repair run.
///
/// Append-only and idempotent on `(repair_worker_id, lifecycle)`: replaying
/// the same lifecycle is a no-op (`Ok(false)` indicates the event was already
/// recorded). Hosted runtimes call this when a repair-worker job created by
/// the persona transitions states.
pub async fn report_repair_worker_status(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    status: PersonaRepairWorkerStatusUpdate,
    now_ms: i64,
) -> Result<bool, String> {
    let mut status = status;
    if status.persona_id.is_empty() {
        status.persona_id = binding.name.clone();
    }
    if status.template_ref.is_none() {
        status.template_ref = binding.template_ref.clone();
    }
    if status.occurred_at_ms == 0 {
        status.occurred_at_ms = now_ms;
    }
    if status.last_heartbeat_ms == 0 {
        status.last_heartbeat_ms = now_ms;
    }

    if repair_worker_status_recorded(log, &binding.name, &status).await? {
        return Ok(false);
    }
    append_persona_event(
        log,
        &binding.name,
        "persona.repair_worker.status",
        serde_json::to_value(&status).map_err(|error| error.to_string())?,
        status.occurred_at_ms,
    )
    .await?;
    record_persona_supervision_event(
        log,
        &binding.name,
        PersonaSupervisionEvent::RepairWorkerStatus(status),
    )
    .await?;
    Ok(true)
}

/// Acknowledge a checkpoint-restore request initiated by the supervision API.
///
/// Idempotent on `checkpoint_id`: a repeated restore request resolves to the
/// same ack (returns `Ok(false)`). The runtime emits a typed
/// `Checkpoint(action: RestoreAcked)` supervision event carrying the
/// coordinates the runtime resumed from.
pub async fn restore_persona_checkpoint(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    request: PersonaCheckpointRestoreRequest,
    now_ms: i64,
) -> Result<PersonaCheckpointRestoreOutcome, String> {
    let PersonaCheckpointRestoreRequest {
        checkpoint_id,
        work_key,
        resumed_from,
    } = request;
    let status = persona_status(log, binding, now_ms).await?;

    if let Some(prior) = find_checkpoint_restore_ack(log, &binding.name, &checkpoint_id).await? {
        return Ok(PersonaCheckpointRestoreOutcome {
            acked: false,
            update: prior,
        });
    }

    let resume_coordinates = resumed_from.unwrap_or_else(|| PersonaCheckpointResume {
        run_id: None,
        lease_id: status.active_lease.as_ref().map(|lease| lease.id.clone()),
        last_run_ms: status
            .last_run
            .as_deref()
            .and_then(|value| parse_rfc3339_ms(value).ok()),
        queued_work_keys: status
            .queued_work
            .iter()
            .map(|item| item.work_key.clone())
            .collect(),
        note: None,
    });

    let update = PersonaCheckpointUpdate {
        persona_id: binding.name.clone(),
        template_ref: binding.template_ref.clone(),
        action: PersonaCheckpointAction::RestoreAcked,
        checkpoint_id: checkpoint_id.clone(),
        work_key,
        resumed_from: Some(resume_coordinates),
        occurred_at_ms: now_ms,
    };

    append_persona_event(
        log,
        &binding.name,
        "persona.checkpoint.restore_acked",
        serde_json::to_value(&update).map_err(|error| error.to_string())?,
        now_ms,
    )
    .await?;
    record_persona_supervision_event(
        log,
        &binding.name,
        PersonaSupervisionEvent::Checkpoint(update.clone()),
    )
    .await?;
    Ok(PersonaCheckpointRestoreOutcome {
        acked: true,
        update,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCheckpointRestoreRequest {
    pub checkpoint_id: String,
    #[serde(default)]
    pub work_key: Option<String>,
    #[serde(default)]
    pub resumed_from: Option<PersonaCheckpointResume>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCheckpointRestoreOutcome {
    pub acked: bool,
    pub update: PersonaCheckpointUpdate,
}

async fn repair_worker_status_recorded(
    log: &Arc<AnyEventLog>,
    persona: &str,
    update: &PersonaRepairWorkerStatusUpdate,
) -> Result<bool, String> {
    let events = read_persona_events(log, persona).await?;
    Ok(events.into_iter().any(|(_, event)| {
        event.kind == "persona.repair_worker.status"
            && event
                .payload
                .get("repair_worker_id")
                .and_then(serde_json::Value::as_str)
                == Some(update.repair_worker_id.as_str())
            && event
                .payload
                .get("lifecycle")
                .and_then(serde_json::Value::as_str)
                == Some(update.lifecycle.as_str())
    }))
}

async fn find_checkpoint_restore_ack(
    log: &Arc<AnyEventLog>,
    persona: &str,
    checkpoint_id: &str,
) -> Result<Option<PersonaCheckpointUpdate>, String> {
    let events = read_persona_events(log, persona).await?;
    for (_, event) in events.into_iter().rev() {
        if event.kind != "persona.checkpoint.restore_acked" {
            continue;
        }
        if event
            .payload
            .get("checkpoint_id")
            .and_then(serde_json::Value::as_str)
            != Some(checkpoint_id)
        {
            continue;
        }
        let update: PersonaCheckpointUpdate =
            serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
        return Ok(Some(update));
    }
    Ok(None)
}

async fn queued_events(
    log: &Arc<AnyEventLog>,
    persona: &str,
) -> Result<Vec<(PersonaTriggerEnvelope, PersonaRunCost)>, String> {
    let events = read_persona_events(log, persona).await?;
    let mut queued = BTreeMap::<String, (PersonaTriggerEnvelope, PersonaRunCost)>::new();
    let mut completed = BTreeSet::<String>::new();
    for (_, event) in events {
        match event.kind.as_str() {
            "persona.trigger.queued" => {
                let Some(envelope) = event.payload.get("envelope") else {
                    continue;
                };
                let envelope: PersonaTriggerEnvelope =
                    serde_json::from_value(envelope.clone()).map_err(|error| error.to_string())?;
                let cost = event
                    .payload
                    .get("cost")
                    .cloned()
                    .map(serde_json::from_value::<PersonaRunCost>)
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .unwrap_or_default();
                queued.insert(envelope.subject_key.clone(), (envelope, cost));
            }
            "persona.run.completed" => {
                if let Some(work_key) = event
                    .payload
                    .get("work_key")
                    .and_then(serde_json::Value::as_str)
                {
                    completed.insert(work_key.to_string());
                }
            }
            _ => {}
        }
    }
    queued.retain(|work_key, _| !completed.contains(work_key));
    Ok(queued.into_values().collect())
}

fn assignment_status_from_lease(lease: &PersonaLease) -> PersonaAssignmentStatus {
    PersonaAssignmentStatus {
        work_key: lease.work_key.clone(),
        lease_id: lease.id.clone(),
        holder: lease.holder.clone(),
        acquired_at: format_ms(lease.acquired_at_ms),
        expires_at: format_ms(lease.expires_at_ms),
    }
}

fn queued_work_from_event(event: &LogEvent) -> Result<Option<PersonaQueuedWork>, String> {
    let Some(envelope) = event.payload.get("envelope") else {
        return Ok(None);
    };
    let envelope: PersonaTriggerEnvelope =
        serde_json::from_value(envelope.clone()).map_err(|error| error.to_string())?;
    Ok(Some(PersonaQueuedWork {
        work_key: envelope.subject_key,
        provider: envelope.provider,
        kind: envelope.kind,
        queued_at: format_ms(event.occurred_at_ms),
        reason: event
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("queued")
            .to_string(),
        source_event_id: envelope.source_event_id,
        metadata: envelope.metadata,
    }))
}

fn handoff_inbox_item(work: &PersonaQueuedWork) -> Option<PersonaHandoffInboxItem> {
    if work.provider != "handoff" && !work.metadata.contains_key("handoff_id") {
        return None;
    }
    Some(PersonaHandoffInboxItem {
        work_key: work.work_key.clone(),
        handoff_id: work.metadata.get("handoff_id").cloned(),
        handoff_kind: work
            .metadata
            .get("handoff_kind")
            .or_else(|| work.metadata.get("kind"))
            .cloned(),
        source_persona: work.metadata.get("source_persona").cloned(),
        task: work.metadata.get("task").cloned(),
        queued_at: work.queued_at.clone(),
        reason: work.reason.clone(),
    })
}

fn value_receipt_from_event(event: &LogEvent) -> Result<Option<PersonaValueReceipt>, String> {
    let Ok(value_event) = serde_json::from_value::<PersonaValueEvent>(event.payload.clone()) else {
        return Ok(None);
    };
    Ok(Some(PersonaValueReceipt {
        kind: value_event.kind,
        run_id: value_event.run_id,
        occurred_at: value_event
            .occurred_at
            .format(&Rfc3339)
            .map_err(|error| error.to_string())?,
        paid_cost_usd: value_event.paid_cost_usd,
        avoided_cost_usd: value_event.avoided_cost_usd,
        deterministic_steps: value_event.deterministic_steps,
        llm_steps: value_event.llm_steps,
        metadata: value_event.metadata,
    }))
}

async fn work_completed(
    log: &Arc<AnyEventLog>,
    persona: &str,
    work_key: &str,
) -> Result<bool, String> {
    let events = read_persona_events(log, persona).await?;
    Ok(events.into_iter().any(|(_, event)| {
        event.kind == "persona.run.completed"
            && event
                .payload
                .get("work_key")
                .and_then(serde_json::Value::as_str)
                == Some(work_key)
    }))
}

async fn read_persona_events(
    log: &Arc<AnyEventLog>,
    persona: &str,
) -> Result<Vec<(u64, LogEvent)>, String> {
    let topic = runtime_topic()?;
    Ok(log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|(_, event)| {
            event
                .headers
                .get("persona")
                .is_some_and(|name| name == persona)
        })
        .collect())
}

async fn append_persona_event(
    log: &Arc<AnyEventLog>,
    persona: &str,
    kind: &str,
    payload: serde_json::Value,
    now_ms: i64,
) -> Result<u64, String> {
    let mut headers = BTreeMap::new();
    headers.insert("persona".to_string(), persona.to_string());
    forward_persona_run_event(persona, kind, &payload);
    let event = LogEvent {
        kind: kind.to_string(),
        payload,
        headers,
        occurred_at_ms: now_ms,
    };
    log.append(&runtime_topic()?, event)
        .await
        .map_err(|error| error.to_string())
}

/// Mirror persona-stage transitions onto the run-events sink for
/// `harn run --json`. Both `persona.stage.*` (per-stage moves) and
/// `persona.run.*` (whole-run lifecycle) kinds are surfaced as
/// [`crate::run_events::RunEvent::PersonaStage`]; the `transition`
/// field carries the suffix (`started`, `completed`, `handoff_started`,
/// `failed`, ...) and `stage` carries the named stage when present.
fn forward_persona_run_event(persona: &str, kind: &str, payload: &serde_json::Value) {
    if !crate::run_events::sink_active() {
        return;
    }
    let transition = kind
        .strip_prefix("persona.stage.")
        .or_else(|| kind.strip_prefix("persona.run."));
    let Some(transition) = transition else {
        return;
    };
    let stage = payload
        .get("stage")
        .or_else(|| payload.get("to"))
        .or_else(|| payload.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    crate::run_events::emit(crate::run_events::RunEvent::PersonaStage {
        persona: persona.to_string(),
        stage,
        transition: transition.to_string(),
    });
}

struct PersonaValueEventDelta {
    kind: PersonaValueEventKind,
    paid_cost_usd: f64,
    avoided_cost_usd: f64,
    deterministic_steps: i64,
    llm_steps: i64,
    metadata: serde_json::Value,
}

impl Default for PersonaValueEventDelta {
    fn default() -> Self {
        Self {
            kind: PersonaValueEventKind::RunCompleted,
            paid_cost_usd: 0.0,
            avoided_cost_usd: 0.0,
            deterministic_steps: 0,
            llm_steps: 0,
            metadata: serde_json::Value::Null,
        }
    }
}

async fn emit_persona_value_event(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    run_id: Uuid,
    delta: PersonaValueEventDelta,
    now_ms: i64,
) -> Result<(), String> {
    let event = PersonaValueEvent {
        persona_id: binding.name.clone(),
        template_ref: binding.template_ref.clone(),
        run_id: Some(run_id),
        kind: delta.kind,
        paid_cost_usd: delta.paid_cost_usd.max(0.0),
        avoided_cost_usd: delta.avoided_cost_usd.max(0.0),
        deterministic_steps: delta.deterministic_steps.max(0),
        llm_steps: delta.llm_steps.max(0),
        metadata: delta.metadata,
        occurred_at: offset_datetime_from_ms(now_ms),
    };
    append_persona_event(
        log,
        &binding.name,
        &format!("persona.value.{}", event.kind.as_str()),
        serde_json::to_value(&event).map_err(|error| error.to_string())?,
        now_ms,
    )
    .await?;
    emit_persona_value_sink_event(&event);
    Ok(())
}

fn emit_persona_value_sink_event(event: &PersonaValueEvent) {
    for sink in persona_value_sinks().snapshot() {
        sink.handle_value_event(event);
    }
}

fn emit_persona_supervision_sink_event(event: &PersonaSupervisionEvent) {
    for sink in persona_supervision_sinks().snapshot() {
        sink.handle_supervision_event(event);
    }
}

async fn record_persona_supervision_event(
    log: &Arc<AnyEventLog>,
    persona: &str,
    event: PersonaSupervisionEvent,
) -> Result<(), String> {
    let update_kind = event.update_kind();
    let occurred_at_ms = event.occurred_at_ms();
    append_persona_event(
        log,
        persona,
        &format!("persona.supervision.{update_kind}"),
        serde_json::to_value(&event).map_err(|error| error.to_string())?,
        occurred_at_ms,
    )
    .await?;
    emit_persona_supervision_sink_event(&event);
    Ok(())
}

#[derive(Clone, Debug)]
struct QueueEntry {
    work_key: String,
    queued_at_ms: i64,
}

async fn queue_snapshot(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    now_ms: i64,
) -> Result<Vec<QueueEntry>, String> {
    let status = persona_status(log, binding, now_ms).await?;
    Ok(status
        .queued_work
        .into_iter()
        .map(|item| QueueEntry {
            queued_at_ms: parse_rfc3339_ms(&item.queued_at).unwrap_or(now_ms),
            work_key: item.work_key,
        })
        .collect())
}

async fn emit_queue_position_supervision(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    before: &[QueueEntry],
    after: &[QueueEntry],
    now_ms: i64,
) -> Result<(), String> {
    use std::collections::HashSet;
    let before_keys: HashSet<&str> = before.iter().map(|e| e.work_key.as_str()).collect();
    let after_keys: HashSet<&str> = after.iter().map(|e| e.work_key.as_str()).collect();
    let after_depth = after.len() as i64;

    for (index, entry) in after.iter().enumerate() {
        if !before_keys.contains(entry.work_key.as_str()) {
            record_persona_supervision_event(
                log,
                &binding.name,
                PersonaSupervisionEvent::QueuePosition(PersonaQueuePositionUpdate {
                    persona_id: binding.name.clone(),
                    template_ref: binding.template_ref.clone(),
                    work_key: entry.work_key.clone(),
                    queue_depth: after_depth,
                    position: (index + 1) as i64,
                    queued_at_ms: entry.queued_at_ms,
                    occurred_at_ms: now_ms,
                }),
            )
            .await?;
        }
    }
    for entry in before {
        if !after_keys.contains(entry.work_key.as_str()) {
            record_persona_supervision_event(
                log,
                &binding.name,
                PersonaSupervisionEvent::QueuePosition(PersonaQueuePositionUpdate {
                    persona_id: binding.name.clone(),
                    template_ref: binding.template_ref.clone(),
                    work_key: entry.work_key.clone(),
                    queue_depth: after_depth,
                    position: 0,
                    queued_at_ms: entry.queued_at_ms,
                    occurred_at_ms: now_ms,
                }),
            )
            .await?;
        }
    }
    Ok(())
}

async fn emit_receipt_supervision(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    receipt: &PersonaRunReceipt,
    now_ms: i64,
) -> Result<(), String> {
    record_persona_supervision_event(
        log,
        &binding.name,
        PersonaSupervisionEvent::Receipt(PersonaReceiptUpdate {
            persona_id: binding.name.clone(),
            template_ref: binding.template_ref.clone(),
            receipt: receipt.clone(),
            occurred_at_ms: now_ms,
        }),
    )
    .await
}

fn run_value_metadata(
    envelope: &PersonaTriggerEnvelope,
    lease: &PersonaLease,
    cost: &PersonaRunCost,
) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert("work_key".to_string(), json!(envelope.subject_key));
    metadata.insert("trigger_provider".to_string(), json!(envelope.provider));
    metadata.insert("trigger_kind".to_string(), json!(envelope.kind));
    metadata.insert("lease_id".to_string(), json!(lease.id));
    metadata.insert("tokens".to_string(), json!(cost.tokens));
    if cost.frontier_escalations > 0 {
        metadata.insert(
            "frontier_escalations".to_string(),
            json!(cost.frontier_escalations),
        );
    }
    match &cost.metadata {
        serde_json::Value::Null => {}
        serde_json::Value::Object(extra) => {
            metadata.extend(
                extra
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        extra => {
            metadata.insert("run_cost_metadata".to_string(), extra.clone());
        }
    }
    serde_json::Value::Object(metadata)
}

fn budget_status(
    policy: &PersonaBudgetPolicy,
    spent: &[(i64, f64, u64)],
    now_ms: i64,
) -> PersonaBudgetStatus {
    let day_start = now_ms - (now_ms.rem_euclid(86_400_000));
    let hour_start = now_ms - (now_ms.rem_euclid(3_600_000));
    let mut spent_today_usd = 0.0;
    let mut spent_this_hour_usd = 0.0;
    let mut tokens_today = 0u64;
    let mut spent_last_run_usd = 0.0;
    for (at_ms, cost, tokens) in spent {
        spent_last_run_usd = *cost;
        if *at_ms >= day_start {
            spent_today_usd += cost;
            tokens_today += tokens;
        }
        if *at_ms >= hour_start {
            spent_this_hour_usd += cost;
        }
    }

    let remaining_today_usd = policy
        .daily_usd
        .map(|limit| (limit - spent_today_usd).max(0.0));
    let remaining_hour_usd = policy
        .hourly_usd
        .map(|limit| (limit - spent_this_hour_usd).max(0.0));
    let reason = if policy
        .daily_usd
        .is_some_and(|limit| spent_today_usd >= limit && limit >= 0.0)
    {
        Some("daily_usd".to_string())
    } else if policy
        .hourly_usd
        .is_some_and(|limit| spent_this_hour_usd >= limit && limit >= 0.0)
    {
        Some("hourly_usd".to_string())
    } else if policy
        .max_tokens
        .is_some_and(|limit| tokens_today >= limit && limit > 0)
    {
        Some("max_tokens".to_string())
    } else {
        None
    };

    PersonaBudgetStatus {
        daily_usd: policy.daily_usd,
        hourly_usd: policy.hourly_usd,
        run_usd: policy.run_usd,
        max_tokens: policy.max_tokens,
        spent_today_usd,
        spent_this_hour_usd,
        spent_last_run_usd,
        tokens_today,
        remaining_today_usd,
        remaining_hour_usd,
        exhausted: reason.is_some(),
        reason,
        last_receipt_id: None,
    }
}

fn next_scheduled_run(
    binding: &PersonaRuntimeBinding,
    last_run_ms: Option<i64>,
    now_ms: i64,
) -> Option<String> {
    binding
        .schedules
        .iter()
        .filter_map(|schedule| next_cron_ms(schedule, last_run_ms.unwrap_or(now_ms)).ok())
        .min()
        .map(format_ms)
}

fn next_cron_ms(schedule: &str, after_ms: i64) -> Result<i64, String> {
    let cron = schedule
        .parse::<Cron>()
        .map_err(|error| error.to_string())?;
    let after = Utc
        .timestamp_millis_opt(after_ms)
        .single()
        .ok_or_else(|| "invalid timestamp".to_string())?;
    let next = cron
        .find_next_occurrence(&after, false)
        .map_err(|error| error.to_string())?;
    Ok(next.timestamp_millis())
}

pub fn now_ms() -> i64 {
    harn_clock::offset_datetime_to_ms(OffsetDateTime::now_utc())
}

fn offset_datetime_from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

pub fn format_ms(ms: i64) -> String {
    offset_datetime_from_ms(ms)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub fn parse_rfc3339_ms(value: &str) -> Result<i64, String> {
    let ts = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| format!("invalid RFC3339 timestamp '{value}': {error}"))?;
    Ok(harn_clock::offset_datetime_to_ms(ts))
}

fn runtime_topic() -> Result<Topic, String> {
    Topic::new(PERSONA_RUNTIME_TOPIC).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
