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

pub const PERSONA_RUNTIME_TOPIC: &str = "persona.runtime.events";

const DEFAULT_LEASE_TTL_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaLifecycleState {
    Inactive,
    Starting,
    #[default]
    Idle,
    Running,
    Paused,
    Draining,
    Failed,
    Disabled,
}

impl PersonaLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Starting => "starting",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaBudgetPolicy {
    pub daily_usd: Option<f64>,
    pub hourly_usd: Option<f64>,
    pub run_usd: Option<f64>,
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersonaRuntimeBinding {
    pub name: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub entry_workflow: String,
    #[serde(default)]
    pub schedules: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub budget: PersonaBudgetPolicy,
    /// Ordered stage declarations. Each stage names a `@step` and narrows the
    /// runtime capability surface for the duration of that step (see
    /// `crates/harn-vm/src/step_runtime.rs`). Empty means the persona keeps
    /// the ambient `CapabilityPolicy`.
    #[serde(default)]
    pub stages: Vec<StageDecl>,
}

/// Per-stage narrowing of the runtime tool surface.
///
/// `name` matches a `@step` declaration on the persona. When that step's
/// frame is entered the runtime pushes a `CapabilityPolicy` whose `tools`
/// allowlist is `allowed_tools` (when set) and whose side-effect ceiling is
/// `side_effect_level` (when set). Both narrow the ambient policy — the
/// existing ceiling still applies, this just adds a tighter scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageDecl {
    pub name: String,
    /// Tool allowlist for the stage. `None` (omitted) inherits the
    /// persona-level allowlist; `Some(vec![])` denies every tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Optional side-effect ceiling override (`none` / `read_only` /
    /// `workspace_write` / `process_exec` / `network`). Tightens, never
    /// loosens, the ambient ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect_level: Option<String>,
    /// Optional agent-loop iteration cap for the stage. Surfaced for
    /// downstream consumers; `agent_loop` reads it via the persona binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Exit transitions that name the next stage on success/failure plus an
    /// optional explicit policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<StageExit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_override: Option<crate::orchestration::CapabilityPolicy>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaLease {
    pub id: String,
    pub holder: String,
    pub work_key: String,
    pub acquired_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaBudgetStatus {
    pub daily_usd: Option<f64>,
    pub hourly_usd: Option<f64>,
    pub run_usd: Option<f64>,
    pub max_tokens: Option<u64>,
    pub spent_today_usd: f64,
    pub spent_this_hour_usd: f64,
    pub spent_last_run_usd: f64,
    pub tokens_today: u64,
    pub remaining_today_usd: Option<f64>,
    pub remaining_hour_usd: Option<f64>,
    pub exhausted: bool,
    pub reason: Option<String>,
    pub last_receipt_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaStatus {
    pub name: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub state: PersonaLifecycleState,
    pub entry_workflow: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub current_assignment: Option<PersonaAssignmentStatus>,
    pub last_run: Option<String>,
    pub next_scheduled_run: Option<String>,
    pub active_lease: Option<PersonaLease>,
    pub budget: PersonaBudgetStatus,
    pub last_error: Option<String>,
    pub queued_events: usize,
    #[serde(default)]
    pub queued_work: Vec<PersonaQueuedWork>,
    #[serde(default)]
    pub handoff_inbox: Vec<PersonaHandoffInboxItem>,
    #[serde(default)]
    pub value_receipts: Vec<PersonaValueReceipt>,
    pub disabled_events: usize,
    pub paused_event_policy: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaAssignmentStatus {
    pub work_key: String,
    pub lease_id: String,
    pub holder: String,
    pub acquired_at: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaQueuedWork {
    pub work_key: String,
    pub provider: String,
    pub kind: String,
    pub queued_at: String,
    pub reason: String,
    pub source_event_id: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaHandoffInboxItem {
    pub work_key: String,
    pub handoff_id: Option<String>,
    pub handoff_kind: Option<String>,
    pub source_persona: Option<String>,
    pub task: Option<String>,
    pub queued_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersonaValueReceipt {
    pub kind: PersonaValueEventKind,
    pub run_id: Option<Uuid>,
    pub occurred_at: String,
    pub paid_cost_usd: f64,
    pub avoided_cost_usd: f64,
    pub deterministic_steps: i64,
    pub llm_steps: i64,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaTriggerEnvelope {
    pub provider: String,
    pub kind: String,
    pub subject_key: String,
    pub source_event_id: Option<String>,
    pub received_at_ms: i64,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub raw: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaRunReceipt {
    pub status: String,
    pub persona: String,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    pub work_key: String,
    pub lease: Option<PersonaLease>,
    pub queued: bool,
    pub error: Option<String>,
    pub budget_receipt_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaRunCost {
    pub cost_usd: f64,
    pub tokens: u64,
    #[serde(default)]
    pub avoided_cost_usd: f64,
    #[serde(default)]
    pub deterministic_steps: i64,
    #[serde(default)]
    pub llm_steps: i64,
    #[serde(default)]
    pub frontier_escalations: i64,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaValueEventKind {
    RunStarted,
    RunCompleted,
    AcceptedOutcome,
    FrontierEscalation,
    DeterministicExecution,
    PromotionSavings,
    ApprovalWait,
}

impl PersonaValueEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::RunCompleted => "run_completed",
            Self::AcceptedOutcome => "accepted_outcome",
            Self::FrontierEscalation => "frontier_escalation",
            Self::DeterministicExecution => "deterministic_execution",
            Self::PromotionSavings => "promotion_savings",
            Self::ApprovalWait => "approval_wait",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersonaValueEvent {
    pub persona_id: String,
    pub template_ref: Option<String>,
    pub run_id: Option<Uuid>,
    pub kind: PersonaValueEventKind,
    pub paid_cost_usd: f64,
    pub avoided_cost_usd: f64,
    pub deterministic_steps: i64,
    pub llm_steps: i64,
    pub metadata: serde_json::Value,
    pub occurred_at: OffsetDateTime,
}

pub trait PersonaValueSink: Send + Sync {
    fn handle_value_event(&self, event: &PersonaValueEvent);
}

/// Append-only multiplexed feed mirroring the cloud platform's
/// `persona_supervision_events` projection. Sinks see the runtime-sourced
/// `queue_position`, `repair_worker_status`, `receipt`, and
/// `checkpoint` (restore-ack) variants; supervisor-sourced variants
/// (`control`, `feedback`, `approval`, `handoff`, and checkpoint writes)
/// originate on the hosted side and aren't routed here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "update_kind", rename_all = "snake_case")]
pub enum PersonaSupervisionEvent {
    QueuePosition(PersonaQueuePositionUpdate),
    RepairWorkerStatus(PersonaRepairWorkerStatusUpdate),
    Receipt(PersonaReceiptUpdate),
    Checkpoint(PersonaCheckpointUpdate),
}

impl PersonaSupervisionEvent {
    pub fn update_kind(&self) -> &'static str {
        match self {
            Self::QueuePosition(_) => "queue_position",
            Self::RepairWorkerStatus(_) => "repair_worker_status",
            Self::Receipt(_) => "receipt",
            Self::Checkpoint(_) => "checkpoint",
        }
    }

    pub fn persona_id(&self) -> &str {
        match self {
            Self::QueuePosition(update) => &update.persona_id,
            Self::RepairWorkerStatus(update) => &update.persona_id,
            Self::Receipt(update) => &update.persona_id,
            Self::Checkpoint(update) => &update.persona_id,
        }
    }

    pub fn template_ref(&self) -> Option<&str> {
        match self {
            Self::QueuePosition(update) => update.template_ref.as_deref(),
            Self::RepairWorkerStatus(update) => update.template_ref.as_deref(),
            Self::Receipt(update) => update.template_ref.as_deref(),
            Self::Checkpoint(update) => update.template_ref.as_deref(),
        }
    }

    pub fn occurred_at_ms(&self) -> i64 {
        match self {
            Self::QueuePosition(update) => update.occurred_at_ms,
            Self::RepairWorkerStatus(update) => update.occurred_at_ms,
            Self::Receipt(update) => update.occurred_at_ms,
            Self::Checkpoint(update) => update.occurred_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaQueuePositionUpdate {
    pub persona_id: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub work_key: String,
    pub queue_depth: i64,
    /// 1-indexed position of `work_key`; `0` means the item left the queue.
    pub position: i64,
    pub queued_at_ms: i64,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaRepairWorkerLifecycle {
    Pending,
    Running,
    Verifying,
    Pushing,
    Succeeded,
    Failed,
    Cancelled,
}

impl PersonaRepairWorkerLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Pushing => "pushing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaRepairWorkerStatusUpdate {
    pub persona_id: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub repair_worker_id: String,
    pub lifecycle: PersonaRepairWorkerLifecycle,
    #[serde(default)]
    pub work_key: Option<String>,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub scratchpad_url: Option<String>,
    pub last_heartbeat_ms: i64,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaReceiptUpdate {
    pub persona_id: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub receipt: PersonaRunReceipt,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCheckpointUpdate {
    pub persona_id: String,
    #[serde(default)]
    pub template_ref: Option<String>,
    pub action: PersonaCheckpointAction,
    pub checkpoint_id: String,
    #[serde(default)]
    pub work_key: Option<String>,
    /// Coordinates the runtime actually resumed from when acking a restore.
    #[serde(default)]
    pub resumed_from: Option<PersonaCheckpointResume>,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaCheckpointAction {
    RestoreAcked,
}

impl PersonaCheckpointAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RestoreAcked => "restore_acked",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCheckpointResume {
    pub run_id: Option<Uuid>,
    pub lease_id: Option<String>,
    pub last_run_ms: Option<i64>,
    pub queued_work_keys: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub trait PersonaSupervisionSink: Send + Sync {
    fn handle_supervision_event(&self, event: &PersonaSupervisionEvent);
}

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

pub async fn fire_schedule(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let schedule = binding
        .schedules
        .first()
        .cloned()
        .unwrap_or_else(|| "manual".to_string());
    let envelope = PersonaTriggerEnvelope {
        provider: "schedule".to_string(),
        kind: "cron.tick".to_string(),
        subject_key: format!("schedule:{}:{schedule}:{}", binding.name, format_ms(now_ms)),
        source_event_id: None,
        received_at_ms: now_ms,
        metadata: BTreeMap::from([
            ("persona".to_string(), binding.name.clone()),
            ("schedule".to_string(), schedule),
            ("fired_at".to_string(), format_ms(now_ms)),
        ]),
        raw: json!({}),
    };
    append_persona_event(
        log,
        &binding.name,
        "persona.schedule.fired",
        json!({"persona": binding.name, "envelope": envelope}),
        now_ms,
    )
    .await?;
    run_for_envelope(log, binding, envelope, cost, now_ms).await
}

pub async fn fire_trigger(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    provider: &str,
    kind: &str,
    metadata: BTreeMap<String, String>,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let envelope = normalize_trigger_envelope(provider, kind, metadata, now_ms);
    append_persona_event(
        log,
        &binding.name,
        "persona.trigger.received",
        json!({"persona": binding.name, "envelope": envelope}),
        now_ms,
    )
    .await?;
    run_for_envelope(log, binding, envelope, cost, now_ms).await
}

pub async fn record_persona_spend(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaBudgetStatus, String> {
    enforce_budget(log, binding, &cost, now_ms).await?;
    append_budget_record(log, &binding.name, &cost, None, now_ms).await?;
    persona_status(log, binding, now_ms)
        .await
        .map(|status| status.budget)
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

async fn run_for_envelope(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    envelope: PersonaTriggerEnvelope,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let pre_queue = queue_snapshot(log, binding, now_ms).await?;
    let receipt = run_for_envelope_inner(log, binding, envelope, cost, now_ms).await?;
    let post_queue = queue_snapshot(log, binding, now_ms).await?;
    emit_queue_position_supervision(log, binding, &pre_queue, &post_queue, now_ms).await?;
    emit_receipt_supervision(log, binding, &receipt, now_ms).await?;
    Ok(receipt)
}

async fn run_for_envelope_inner(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    envelope: PersonaTriggerEnvelope,
    cost: PersonaRunCost,
    now_ms: i64,
) -> Result<PersonaRunReceipt, String> {
    let status = persona_status(log, binding, now_ms).await?;
    match status.state {
        PersonaLifecycleState::Paused => {
            append_persona_event(
                log,
                &binding.name,
                "persona.trigger.queued",
                json!({
                    "work_key": envelope.subject_key,
                    "envelope": envelope,
                    "cost": cost,
                    "reason": "paused",
                }),
                now_ms,
            )
            .await?;
            return Ok(PersonaRunReceipt {
                status: "queued".to_string(),
                persona: binding.name.clone(),
                run_id: None,
                work_key: envelope.subject_key,
                lease: None,
                queued: true,
                error: None,
                budget_receipt_id: None,
            });
        }
        PersonaLifecycleState::Disabled => {
            append_persona_event(
                log,
                &binding.name,
                "persona.trigger.dead_lettered",
                json!({
                    "work_key": envelope.subject_key,
                    "envelope": envelope,
                    "reason": "disabled",
                }),
                now_ms,
            )
            .await?;
            return Ok(PersonaRunReceipt {
                status: "dead_lettered".to_string(),
                persona: binding.name.clone(),
                run_id: None,
                work_key: envelope.subject_key,
                lease: None,
                queued: false,
                error: Some("persona is disabled".to_string()),
                budget_receipt_id: None,
            });
        }
        _ => {}
    }

    if let Err(error) = enforce_budget(log, binding, &cost, now_ms).await {
        return Ok(PersonaRunReceipt {
            status: "budget_exhausted".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: None,
            queued: false,
            error: Some(error),
            budget_receipt_id: None,
        });
    }

    if work_completed(log, &binding.name, &envelope.subject_key).await? {
        append_persona_event(
            log,
            &binding.name,
            "persona.trigger.duplicate",
            json!({
                "work_key": envelope.subject_key,
                "envelope": envelope,
                "reason": "already_completed",
            }),
            now_ms,
        )
        .await?;
        return Ok(PersonaRunReceipt {
            status: "duplicate".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: None,
            queued: false,
            error: None,
            budget_receipt_id: None,
        });
    }

    let Some(lease) = acquire_lease(
        log,
        binding,
        &envelope.subject_key,
        "persona-runtime",
        DEFAULT_LEASE_TTL_MS,
        now_ms,
    )
    .await?
    else {
        return Ok(PersonaRunReceipt {
            status: "lease_busy".to_string(),
            persona: binding.name.clone(),
            run_id: None,
            work_key: envelope.subject_key,
            lease: status.active_lease,
            queued: false,
            error: Some("active lease already owns persona work".to_string()),
            budget_receipt_id: None,
        });
    };

    let run_id = Uuid::now_v7();
    let value_metadata = run_value_metadata(&envelope, &lease, &cost);
    append_persona_event(
        log,
        &binding.name,
        "persona.run.started",
        json!({
            "work_key": envelope.subject_key,
            "run_id": run_id,
            "started_at_ms": now_ms,
            "entry_workflow": binding.entry_workflow,
            "lease_id": lease.id,
        }),
        now_ms,
    )
    .await?;
    emit_persona_value_event(
        log,
        binding,
        run_id,
        PersonaValueEventDelta {
            kind: PersonaValueEventKind::RunStarted,
            metadata: value_metadata.clone(),
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    let budget_receipt_id =
        append_budget_record(log, &binding.name, &cost, Some(&lease.id), now_ms).await?;
    if cost.avoided_cost_usd > 0.0 || cost.deterministic_steps > 0 {
        emit_persona_value_event(
            log,
            binding,
            run_id,
            PersonaValueEventDelta {
                kind: PersonaValueEventKind::DeterministicExecution,
                avoided_cost_usd: cost.avoided_cost_usd,
                deterministic_steps: cost.deterministic_steps.max(1),
                metadata: value_metadata.clone(),
                ..Default::default()
            },
            now_ms,
        )
        .await?;
    }
    if cost.frontier_escalations > 0 {
        emit_persona_value_event(
            log,
            binding,
            run_id,
            PersonaValueEventDelta {
                kind: PersonaValueEventKind::FrontierEscalation,
                paid_cost_usd: cost.cost_usd,
                llm_steps: cost.llm_steps.max(cost.frontier_escalations),
                metadata: value_metadata.clone(),
                ..Default::default()
            },
            now_ms,
        )
        .await?;
    }
    let completion_paid_cost = if cost.frontier_escalations > 0 {
        0.0
    } else {
        cost.cost_usd
    };
    let completion_llm_steps = if cost.frontier_escalations > 0 {
        0
    } else {
        cost.llm_steps
    };
    emit_persona_value_event(
        log,
        binding,
        run_id,
        PersonaValueEventDelta {
            kind: PersonaValueEventKind::RunCompleted,
            paid_cost_usd: completion_paid_cost,
            llm_steps: completion_llm_steps,
            metadata: value_metadata,
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.run.completed",
        json!({
            "work_key": envelope.subject_key,
            "run_id": run_id,
            "completed_at_ms": now_ms,
            "entry_workflow": binding.entry_workflow,
            "lease_id": lease.id,
        }),
        now_ms,
    )
    .await?;
    append_persona_event(
        log,
        &binding.name,
        "persona.lease.released",
        json!({
            "id": lease.id,
            "work_key": envelope.subject_key,
            "released_at_ms": now_ms,
        }),
        now_ms,
    )
    .await?;
    Ok(PersonaRunReceipt {
        status: "completed".to_string(),
        persona: binding.name.clone(),
        run_id: Some(run_id),
        work_key: envelope.subject_key,
        lease: Some(lease),
        queued: false,
        error: None,
        budget_receipt_id: Some(budget_receipt_id),
    })
}

async fn acquire_lease(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    work_key: &str,
    holder: &str,
    ttl_ms: i64,
    now_ms: i64,
) -> Result<Option<PersonaLease>, String> {
    let status = persona_status(log, binding, now_ms).await?;
    if let Some(lease) = status.active_lease {
        if lease.expires_at_ms > now_ms {
            append_persona_event(
                log,
                &binding.name,
                "persona.lease.conflict",
                json!({
                    "active_lease": lease,
                    "requested_work_key": work_key,
                    "at_ms": now_ms,
                }),
                now_ms,
            )
            .await?;
            return Ok(None);
        }
        append_persona_event(
            log,
            &binding.name,
            "persona.lease.expired",
            json!({
                "id": lease.id,
                "work_key": lease.work_key,
                "expired_at_ms": now_ms,
            }),
            now_ms,
        )
        .await?;
    }

    let lease = PersonaLease {
        id: format!("persona_lease_{}", Uuid::now_v7()),
        holder: holder.to_string(),
        work_key: work_key.to_string(),
        acquired_at_ms: now_ms,
        expires_at_ms: now_ms + ttl_ms,
    };
    append_persona_event(
        log,
        &binding.name,
        "persona.lease.acquired",
        serde_json::to_value(&lease).map_err(|error| error.to_string())?,
        now_ms,
    )
    .await?;
    Ok(Some(lease))
}

async fn enforce_budget(
    log: &Arc<AnyEventLog>,
    binding: &PersonaRuntimeBinding,
    cost: &PersonaRunCost,
    now_ms: i64,
) -> Result<(), String> {
    let status = persona_status(log, binding, now_ms).await?;
    let reason = if binding
        .budget
        .run_usd
        .is_some_and(|limit| cost.cost_usd > limit)
    {
        Some("run_usd")
    } else if binding
        .budget
        .daily_usd
        .is_some_and(|limit| status.budget.spent_today_usd + cost.cost_usd > limit)
    {
        Some("daily_usd")
    } else if binding
        .budget
        .hourly_usd
        .is_some_and(|limit| status.budget.spent_this_hour_usd + cost.cost_usd > limit)
    {
        Some("hourly_usd")
    } else if binding
        .budget
        .max_tokens
        .is_some_and(|limit| status.budget.tokens_today + cost.tokens > limit)
    {
        Some("max_tokens")
    } else {
        None
    };

    if let Some(reason) = reason {
        let receipt_id = format!("persona_budget_{}", Uuid::now_v7());
        append_persona_event(
            log,
            &binding.name,
            "persona.budget.exhausted",
            json!({
                "receipt_id": receipt_id,
                "reason": reason,
                "attempted_cost_usd": cost.cost_usd,
                "attempted_tokens": cost.tokens,
                "persona": binding.name,
            }),
            now_ms,
        )
        .await?;
        return Err(format!("persona budget exhausted: {reason}"));
    }

    Ok(())
}

async fn append_budget_record(
    log: &Arc<AnyEventLog>,
    persona: &str,
    cost: &PersonaRunCost,
    lease_id: Option<&str>,
    now_ms: i64,
) -> Result<String, String> {
    let receipt_id = format!("persona_budget_{}", Uuid::now_v7());
    append_persona_event(
        log,
        persona,
        "persona.budget.recorded",
        json!({
            "receipt_id": receipt_id,
            "persona": persona,
            "cost_usd": cost.cost_usd,
            "tokens": cost.tokens,
            "lease_id": lease_id,
        }),
        now_ms,
    )
    .await?;
    Ok(receipt_id)
}

fn normalize_trigger_envelope(
    provider: &str,
    kind: &str,
    metadata: BTreeMap<String, String>,
    now_ms: i64,
) -> PersonaTriggerEnvelope {
    let provider = provider.to_ascii_lowercase();
    let kind = kind.to_string();
    let source_event_id = metadata
        .get("event_id")
        .or_else(|| metadata.get("id"))
        .cloned();
    let subject_key = match provider.as_str() {
        "github" => {
            let repo = metadata
                .get("repository")
                .or_else(|| metadata.get("repository.full_name"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(number) = metadata
                .get("pr")
                .or_else(|| metadata.get("pull_request.number"))
                .or_else(|| metadata.get("number"))
            {
                format!("github:{repo}:pr:{number}")
            } else if let Some(check) = metadata
                .get("check_run.name")
                .or_else(|| metadata.get("check_name"))
            {
                format!("github:{repo}:check:{check}")
            } else {
                format!("github:{repo}:{kind}")
            }
        }
        "linear" => {
            let issue = metadata
                .get("issue_key")
                .or_else(|| metadata.get("issue.identifier"))
                .or_else(|| metadata.get("issue_id"))
                .or_else(|| metadata.get("id"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            format!("linear:issue:{issue}")
        }
        "slack" => {
            let channel = metadata
                .get("channel")
                .or_else(|| metadata.get("channel_id"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let ts = metadata
                .get("ts")
                .or_else(|| metadata.get("event_ts"))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            format!("slack:{channel}:{ts}")
        }
        "webhook" => metadata
            .get("dedupe_key")
            .or_else(|| metadata.get("event_id"))
            .map(|value| format!("webhook:{value}"))
            .unwrap_or_else(|| format!("webhook:{kind}:{}", Uuid::now_v7())),
        _ => metadata
            .get("dedupe_key")
            .or_else(|| metadata.get("event_id"))
            .map(|value| format!("{provider}:{kind}:{value}"))
            .unwrap_or_else(|| format!("{provider}:{kind}:{}", Uuid::now_v7())),
    };

    PersonaTriggerEnvelope {
        provider,
        kind,
        subject_key,
        source_event_id,
        received_at_ms: now_ms,
        raw: json!({"metadata": metadata}),
        metadata,
    }
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
