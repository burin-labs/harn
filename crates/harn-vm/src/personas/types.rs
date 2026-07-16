use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

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
    pub autonomy_tier: crate::trust_graph::AutonomyTier,
    /// Persona-level authority ceiling applied to every workflow invocation.
    /// It composes with the trigger tier and ambient host policy.
    #[serde(
        default,
        skip_serializing_if = "crate::orchestration::CapabilityPolicy::is_unbounded"
    )]
    pub execution_policy: Box<crate::orchestration::CapabilityPolicy>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
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
