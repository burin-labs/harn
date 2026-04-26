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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub state: PersonaLifecycleState,
    pub entry_workflow: String,
    pub last_run: Option<String>,
    pub next_scheduled_run: Option<String>,
    pub active_lease: Option<PersonaLease>,
    pub budget: PersonaBudgetStatus,
    pub last_error: Option<String>,
    pub queued_events: usize,
    pub disabled_events: usize,
    pub paused_event_policy: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

type PersonaValueSinkRegistry = RwLock<Vec<(u64, Arc<dyn PersonaValueSink>)>>;

fn persona_value_sinks() -> &'static PersonaValueSinkRegistry {
    static REGISTRY: OnceLock<PersonaValueSinkRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

fn next_persona_value_sink_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct PersonaValueSinkRegistration {
    id: u64,
}

impl Drop for PersonaValueSinkRegistration {
    fn drop(&mut self) {
        if let Ok(mut sinks) = persona_value_sinks().write() {
            sinks.retain(|(id, _)| *id != self.id);
        }
    }
}

pub fn register_persona_value_sink(
    sink: Arc<dyn PersonaValueSink>,
) -> PersonaValueSinkRegistration {
    let id = next_persona_value_sink_id();
    if let Ok(mut sinks) = persona_value_sinks().write() {
        sinks.push((id, sink));
    }
    PersonaValueSinkRegistration { id }
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

    Ok(PersonaStatus {
        name: binding.name.clone(),
        state,
        entry_workflow: binding.entry_workflow.clone(),
        last_run: last_run_ms.map(format_ms),
        next_scheduled_run: next_scheduled_run(binding, last_run_ms, now_ms),
        active_lease,
        budget,
        last_error,
        queued_events: queued.len(),
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
    for envelope in queued {
        let cost = PersonaRunCost::default();
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

async fn run_for_envelope(
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
) -> Result<Vec<PersonaTriggerEnvelope>, String> {
    let events = read_persona_events(log, persona).await?;
    let mut queued = BTreeMap::<String, PersonaTriggerEnvelope>::new();
    let mut completed = BTreeSet::<String>::new();
    for (_, event) in events {
        match event.kind.as_str() {
            "persona.trigger.queued" => {
                let Some(envelope) = event.payload.get("envelope") else {
                    continue;
                };
                let envelope: PersonaTriggerEnvelope =
                    serde_json::from_value(envelope.clone()).map_err(|error| error.to_string())?;
                queued.insert(envelope.subject_key.clone(), envelope);
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
    let sinks = persona_value_sinks()
        .read()
        .map(|sinks| {
            sinks
                .iter()
                .map(|(_, sink)| Arc::clone(sink))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for sink in sinks {
        sink.handle_value_event(event);
    }
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
    OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
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
    Ok(ts.unix_timestamp_nanos() as i64 / 1_000_000)
}

fn runtime_topic() -> Result<Topic, String> {
    Topic::new(PERSONA_RUNTIME_TOPIC).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{AnyEventLog, MemoryEventLog};
    use std::sync::Mutex;

    struct CapturingValueSink {
        events: Arc<Mutex<Vec<PersonaValueEvent>>>,
    }

    impl PersonaValueSink for CapturingValueSink {
        fn handle_value_event(&self, event: &PersonaValueEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    fn binding() -> PersonaRuntimeBinding {
        PersonaRuntimeBinding {
            name: "merge_captain".to_string(),
            template_ref: Some("software_factory@v0".to_string()),
            entry_workflow: "workflows/merge.harn#run".to_string(),
            schedules: vec!["*/30 * * * *".to_string()],
            triggers: vec!["github.pr_opened".to_string()],
            budget: PersonaBudgetPolicy {
                daily_usd: Some(0.02),
                hourly_usd: None,
                run_usd: Some(0.02),
                max_tokens: Some(100),
            },
        }
    }

    fn log() -> Arc<AnyEventLog> {
        Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)))
    }

    #[tokio::test]
    async fn schedule_tick_records_lifecycle_status_and_receipt() {
        let log = log();
        let binding = binding();
        let now = parse_rfc3339_ms("2026-04-24T12:30:00Z").unwrap();
        let receipt = fire_schedule(
            &log,
            &binding,
            PersonaRunCost {
                cost_usd: 0.01,
                tokens: 10,
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, "completed");
        assert!(receipt.lease.is_some());
        let status = persona_status(&log, &binding, now).await.unwrap();
        assert_eq!(status.state, PersonaLifecycleState::Idle);
        assert_eq!(status.last_run.as_deref(), Some("2026-04-24T12:30:00Z"));
        assert!(status.next_scheduled_run.is_some());
        assert_eq!(status.budget.spent_today_usd, 0.01);
    }

    #[tokio::test]
    async fn paused_personas_queue_and_resume_drains_once() {
        let log = log();
        let binding = binding();
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
        pause_persona(&log, &binding, now).await.unwrap();
        let receipt = fire_trigger(
            &log,
            &binding,
            "github",
            "pull_request",
            BTreeMap::from([
                ("repository".to_string(), "burin-labs/harn".to_string()),
                ("number".to_string(), "462".to_string()),
            ]),
            PersonaRunCost::default(),
            now,
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, "queued");
        assert_eq!(
            persona_status(&log, &binding, now)
                .await
                .unwrap()
                .queued_events,
            1
        );
        let status = resume_persona(&log, &binding, now + 1000).await.unwrap();
        assert_eq!(status.state, PersonaLifecycleState::Idle);
        assert_eq!(status.queued_events, 0);
    }

    #[tokio::test]
    async fn duplicate_trigger_envelope_is_not_processed_twice() {
        let log = log();
        let binding = binding();
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
        let metadata = BTreeMap::from([
            ("repository".to_string(), "burin-labs/harn".to_string()),
            ("number".to_string(), "462".to_string()),
        ]);
        let first = fire_trigger(
            &log,
            &binding,
            "github",
            "pull_request",
            metadata.clone(),
            PersonaRunCost::default(),
            now,
        )
        .await
        .unwrap();
        let second = fire_trigger(
            &log,
            &binding,
            "github",
            "pull_request",
            metadata,
            PersonaRunCost::default(),
            now + 1000,
        )
        .await
        .unwrap();
        assert_eq!(first.status, "completed");
        assert_eq!(second.status, "duplicate");
        assert!(second.lease.is_none());
    }

    #[tokio::test]
    async fn disabled_personas_dead_letter_events() {
        let log = log();
        let binding = binding();
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
        disable_persona(&log, &binding, now).await.unwrap();
        let receipt = fire_trigger(
            &log,
            &binding,
            "slack",
            "message",
            BTreeMap::from([
                ("channel".to_string(), "C123".to_string()),
                ("ts".to_string(), "1713988800.000100".to_string()),
            ]),
            PersonaRunCost::default(),
            now,
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, "dead_lettered");
        let status = persona_status(&log, &binding, now).await.unwrap();
        assert_eq!(status.state, PersonaLifecycleState::Disabled);
        assert_eq!(status.disabled_events, 1);
    }

    #[tokio::test]
    async fn budget_exhaustion_blocks_expensive_work() {
        let log = log();
        let mut binding = binding();
        binding.budget.daily_usd = Some(0.01);
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
        let receipt = fire_trigger(
            &log,
            &binding,
            "linear",
            "issue",
            BTreeMap::from([("issue_key".to_string(), "HAR-462".to_string())]),
            PersonaRunCost {
                cost_usd: 0.02,
                tokens: 1,
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, "budget_exhausted");
        let status = persona_status(&log, &binding, now).await.unwrap();
        assert_eq!(status.budget.reason.as_deref(), Some("daily_usd"));
        assert!(status.budget.exhausted);
        assert!(status.last_error.as_deref().unwrap().contains("daily_usd"));
    }

    #[tokio::test]
    async fn deterministic_predicate_hit_emits_value_event_with_avoided_cost() {
        let log = log();
        let binding = binding();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let _registration = register_persona_value_sink(Arc::new(CapturingValueSink {
            events: captured.clone(),
        }));
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();

        let receipt = fire_trigger(
            &log,
            &binding,
            "github",
            "pull_request",
            BTreeMap::from([
                ("repository".to_string(), "burin-labs/harn".to_string()),
                ("number".to_string(), "715".to_string()),
            ]),
            PersonaRunCost {
                avoided_cost_usd: 0.0042,
                deterministic_steps: 1,
                metadata: json!({
                    "predicate": "pr_already_green",
                    "would_have_called_model": "gpt-5.4-mini",
                }),
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();

        let run_id = receipt.run_id.expect("completed run has run_id");
        let events = captured.lock().unwrap().clone();
        let deterministic = events
            .iter()
            .find(|event| {
                event.kind == PersonaValueEventKind::DeterministicExecution
                    && event.run_id == Some(run_id)
            })
            .expect("deterministic execution value event");
        assert_eq!(deterministic.persona_id, "merge_captain");
        assert_eq!(
            deterministic.template_ref.as_deref(),
            Some("software_factory@v0")
        );
        assert_eq!(deterministic.run_id, Some(run_id));
        assert_eq!(deterministic.paid_cost_usd, 0.0);
        assert_eq!(deterministic.avoided_cost_usd, 0.0042);
        assert_eq!(deterministic.deterministic_steps, 1);
        assert_eq!(
            deterministic.metadata["predicate"].as_str(),
            Some("pr_already_green")
        );

        let persisted = read_persona_events(&log, &binding.name).await.unwrap();
        assert!(persisted.iter().any(|(_, event)| {
            event.kind == "persona.value.deterministic_execution"
                && event.payload["avoided_cost_usd"] == json!(0.0042)
        }));
    }

    #[tokio::test]
    async fn frontier_escalation_run_emits_value_event_with_paid_cost() {
        let log = log();
        let binding = binding();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let _registration = register_persona_value_sink(Arc::new(CapturingValueSink {
            events: captured.clone(),
        }));
        let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();

        let receipt = fire_trigger(
            &log,
            &binding,
            "linear",
            "issue",
            BTreeMap::from([("issue_key".to_string(), "HAR-715".to_string())]),
            PersonaRunCost {
                cost_usd: 0.011,
                tokens: 20,
                llm_steps: 1,
                frontier_escalations: 1,
                metadata: json!({
                    "frontier_model": "gpt-5.4",
                    "escalation_reason": "high_risk_merge",
                }),
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();

        let run_id = receipt.run_id.expect("completed run has run_id");
        let events = captured.lock().unwrap().clone();
        let escalation = events
            .iter()
            .find(|event| {
                event.kind == PersonaValueEventKind::FrontierEscalation
                    && event.run_id == Some(run_id)
            })
            .expect("frontier escalation value event");
        assert_eq!(escalation.run_id, Some(run_id));
        assert_eq!(escalation.paid_cost_usd, 0.011);
        assert_eq!(escalation.avoided_cost_usd, 0.0);
        assert_eq!(escalation.llm_steps, 1);
        assert_eq!(
            escalation.metadata["frontier_model"].as_str(),
            Some("gpt-5.4")
        );

        let completion = events
            .iter()
            .find(|event| {
                event.kind == PersonaValueEventKind::RunCompleted && event.run_id == Some(run_id)
            })
            .expect("run completed value event");
        assert_eq!(completion.paid_cost_usd, 0.0);
    }
}
