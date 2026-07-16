use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogEvent, Topic};
use crate::llm::trigger_predicate::TriggerPredicateBudget;
use crate::secrets::{configured_default_chain, SecretProvider};
use crate::triggers::test_util::clock;
use crate::trust_graph::AutonomyTier;
use crate::value::VmClosure;

use super::aggregation::TriggerAggregationConfig;
use super::dispatcher::TriggerRetryConfig;
use super::flow_control::TriggerFlowControlConfig;
use super::ProviderId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerId(String);

impl TriggerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TriggerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerState {
    Registering,
    Active,
    Paused,
    Draining,
    Terminated,
}

impl TriggerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registering => "registering",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Terminated => "terminated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerBindingSource {
    Manifest,
    Dynamic,
}

impl TriggerBindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Dynamic => "dynamic",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerBudgetExhaustionStrategy {
    #[default]
    False,
    RetryLater,
    Fail,
    Warn,
}

impl TriggerBudgetExhaustionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::False => "false",
            Self::RetryLater => "retry_later",
            Self::Fail => "fail",
            Self::Warn => "warn",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorBudgetConfig {
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorBudgetSnapshot {
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
    pub cost_today_usd_micros: u64,
    pub cost_hour_usd_micros: u64,
    pub day_utc: i32,
    pub hour_utc: i64,
}

#[derive(Debug)]
struct OrchestratorBudgetState {
    config: OrchestratorBudgetConfig,
    day_utc: i32,
    hour_utc: i64,
    cost_today_usd_micros: u64,
    cost_hour_usd_micros: u64,
}

impl Default for OrchestratorBudgetState {
    fn default() -> Self {
        Self {
            config: OrchestratorBudgetConfig::default(),
            day_utc: utc_day_key(),
            hour_utc: utc_hour_key(),
            cost_today_usd_micros: 0,
            cost_hour_usd_micros: 0,
        }
    }
}

#[derive(Clone)]
pub enum TriggerHandlerSpec {
    Local {
        raw: String,
        callable: crate::value::VmCallable,
    },
    A2a {
        target: String,
        allow_cleartext: bool,
    },
    Worker {
        queue: String,
    },
    Persona {
        binding: crate::PersonaRuntimeBinding,
        callable: crate::value::VmCallable,
    },
    EvalPack {
        target: String,
        manifest: Box<crate::orchestration::EvalPackManifest>,
        ledger_options: Option<serde_json::Value>,
    },
    AutoResume {
        worker_id: String,
    },
    /// Composes triggers (#1870) with named agent pools (#1883). On match,
    /// the dispatcher resolves `pool` by name, invokes `task_factory(event)`
    /// to build the per-event closure, and submits it to the pool with the
    /// pool's queue strategy + backpressure policy applied (PL-04 / #1889).
    SpawnToPool {
        pool: String,
        priority_from: Option<String>,
        key_from: Option<String>,
        task_factory: Arc<VmClosure>,
    },
    /// Composes triggers (#1870) with the reminder pipeline (#1815) by
    /// injecting a `SystemReminder` into a target agent session's transcript
    /// when the trigger matches (CH-05 / #1876). The reminder appears at the
    /// session's next turn boundary — same as any other reminder. No spawn,
    /// no resume, no signal. `body` is a prompt-template string rendered
    /// against `{event}` / `{batch.events}` per match. `target` resolution
    /// happens at dispatch time via [`TargetExpr`]; missing-target dispatches
    /// are recorded as audit events rather than crashing the trigger.
    ReminderInject {
        target: TargetExpr,
        body: String,
        tags: Vec<String>,
        ttl_turns: Option<i64>,
        dedupe_key: Option<String>,
        propagate: crate::llm::helpers::ReminderPropagate,
        role_hint: crate::llm::helpers::ReminderRoleHint,
        preserve_on_compact: bool,
    },
    /// CH-10 (#1910): emergency "panic" broadcast. When the trigger fires,
    /// every running worker matched by `target_agents` is suspended
    /// synchronously via the cooperative suspend pipeline used by
    /// `suspend_agent` (#1837), bypassing the normal turn-boundary
    /// delivery contract. Already-suspended or terminal workers are skipped
    /// (no double-suspend, no error). `reason` is propagated to the
    /// suspension envelope, the `WorkerSuspended` event, and the
    /// `triggers.interrupt_and_suspend.audit` audit entry per suspension.
    /// A no-target dispatch records a single roll-up audit entry and
    /// returns a `broadcast` result with `suspended_count: 0` — graceful
    /// no-op rather than dispatch failure, mirroring the `ReminderInject`
    /// missing-target contract.
    InterruptAndSuspend {
        target_agents: AgentScope,
        reason: String,
    },
}

/// Resolution mode for the target worker set of a
/// [`TriggerHandlerSpec::InterruptAndSuspend`] dispatch. `All` enumerates
/// every entry in the local worker registry (the org-scoped "panic"
/// broadcast); `Concrete` carries an explicit worker-id list from the
/// registration; `Closure` defers resolution to a user closure that runs at
/// dispatch time with the event payload bound to its first argument and
/// must return a list of worker-id strings.
#[derive(Clone)]
pub enum AgentScope {
    All,
    Concrete(Vec<String>),
    Closure(Arc<VmClosure>),
}

impl AgentScope {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Concrete(_) => "concrete",
            Self::Closure(_) => "closure",
        }
    }
}

impl std::fmt::Debug for AgentScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => f.write_str("All"),
            Self::Concrete(ids) => f.debug_tuple("Concrete").field(ids).finish(),
            Self::Closure(_) => f.write_str("Closure(<vm_closure>)"),
        }
    }
}

/// Resolution mode for the target session of a [`TriggerHandlerSpec::ReminderInject`]
/// dispatch. `Current` walks the current-session thread-local; `Parent` resolves
/// the active session's parent in the agent-session lineage; `Concrete` carries
/// a literal session id from the registration; `Closure` defers resolution to
/// a user closure that runs at dispatch time with the event payload bound to
/// its first argument and must return the session id as a string.
#[derive(Clone)]
pub enum TargetExpr {
    Current,
    Parent,
    Concrete(String),
    Closure(Arc<VmClosure>),
}

impl TargetExpr {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Parent => "parent",
            Self::Concrete(_) => "concrete",
            Self::Closure(_) => "closure",
        }
    }
}

impl std::fmt::Debug for TargetExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Current => f.write_str("Current"),
            Self::Parent => f.write_str("Parent"),
            Self::Concrete(id) => f.debug_tuple("Concrete").field(id).finish(),
            Self::Closure(_) => f.write_str("Closure(<vm_closure>)"),
        }
    }
}

impl std::fmt::Debug for TriggerHandlerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { raw, .. } => f.debug_struct("Local").field("raw", raw).finish(),
            Self::A2a {
                target,
                allow_cleartext,
            } => f
                .debug_struct("A2a")
                .field("target", target)
                .field("allow_cleartext", allow_cleartext)
                .finish(),
            Self::Worker { queue } => f.debug_struct("Worker").field("queue", queue).finish(),
            Self::Persona { binding, .. } => f
                .debug_struct("Persona")
                .field("name", &binding.name)
                .finish(),
            Self::EvalPack {
                target,
                manifest,
                ledger_options,
            } => f
                .debug_struct("EvalPack")
                .field("target", target)
                .field("pack_id", &manifest.id)
                .field("ledger_options", ledger_options)
                .finish(),
            Self::AutoResume { worker_id } => f
                .debug_struct("AutoResume")
                .field("worker_id", worker_id)
                .finish(),
            Self::SpawnToPool {
                pool,
                priority_from,
                key_from,
                ..
            } => f
                .debug_struct("SpawnToPool")
                .field("pool", pool)
                .field("priority_from", priority_from)
                .field("key_from", key_from)
                .finish(),
            Self::ReminderInject {
                target,
                body,
                tags,
                ttl_turns,
                dedupe_key,
                propagate,
                role_hint,
                preserve_on_compact,
            } => f
                .debug_struct("ReminderInject")
                .field("target", target)
                .field("body", body)
                .field("tags", tags)
                .field("ttl_turns", ttl_turns)
                .field("dedupe_key", dedupe_key)
                .field("propagate", propagate)
                .field("role_hint", role_hint)
                .field("preserve_on_compact", preserve_on_compact)
                .finish(),
            Self::InterruptAndSuspend {
                target_agents,
                reason,
            } => f
                .debug_struct("InterruptAndSuspend")
                .field("target_agents", target_agents)
                .field("reason", reason)
                .finish(),
        }
    }
}

impl TriggerHandlerSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::A2a { .. } => "a2a",
            Self::Worker { .. } => "worker",
            Self::Persona { .. } => "persona",
            Self::EvalPack { .. } => "eval_pack",
            Self::AutoResume { .. } => "auto_resume",
            Self::SpawnToPool { .. } => "spawn_to_pool",
            Self::ReminderInject { .. } => "reminder_inject",
            Self::InterruptAndSuspend { .. } => "interrupt_and_suspend",
        }
    }
}

#[derive(Clone)]
pub struct TriggerPredicateSpec {
    pub raw: String,
    pub callable: crate::value::VmCallable,
}

impl std::fmt::Debug for TriggerPredicateSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerPredicateSpec")
            .field("raw", &self.raw)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TriggerBindingSpec {
    pub id: String,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: ProviderId,
    pub autonomy_tier: AutonomyTier,
    pub handler: TriggerHandlerSpec,
    pub dispatch_priority: super::worker_queue::WorkerQueuePriority,
    pub when: Option<TriggerPredicateSpec>,
    pub when_budget: Option<TriggerPredicateBudget>,
    pub retry: TriggerRetryConfig,
    pub match_events: Vec<String>,
    pub dedupe_key: Option<String>,
    pub dedupe_retention_days: u32,
    pub filter: Option<String>,
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
    pub max_autonomous_decisions_per_hour: Option<u64>,
    pub max_autonomous_decisions_per_day: Option<u64>,
    pub on_budget_exhausted: TriggerBudgetExhaustionStrategy,
    pub max_concurrent: Option<u32>,
    pub flow_control: TriggerFlowControlConfig,
    /// CH-04 (#1875): optional aggregation buffer. When set, the
    /// dispatcher accumulates matching events into a per-(binding,
    /// partition_key) buffer and dispatches the handler with a batched
    /// event when `count` is reached or the `window` elapses.
    pub aggregation: Option<TriggerAggregationConfig>,
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub definition_fingerprint: String,
}

#[derive(Debug)]
pub struct TriggerMetrics {
    pub received: AtomicU64,
    pub dispatched: AtomicU64,
    pub failed: AtomicU64,
    pub dlq: AtomicU64,
    pub last_received_ms: Mutex<Option<i64>>,
    pub cost_total_usd_micros: AtomicU64,
    pub cost_today_usd_micros: AtomicU64,
    pub cost_hour_usd_micros: AtomicU64,
    pub autonomous_decisions_total: AtomicU64,
    pub autonomous_decisions_today: AtomicU64,
    pub autonomous_decisions_hour: AtomicU64,
}

impl Default for TriggerMetrics {
    fn default() -> Self {
        Self {
            received: AtomicU64::new(0),
            dispatched: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            dlq: AtomicU64::new(0),
            last_received_ms: Mutex::new(None),
            cost_total_usd_micros: AtomicU64::new(0),
            cost_today_usd_micros: AtomicU64::new(0),
            cost_hour_usd_micros: AtomicU64::new(0),
            autonomous_decisions_total: AtomicU64::new(0),
            autonomous_decisions_today: AtomicU64::new(0),
            autonomous_decisions_hour: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerMetricsSnapshot {
    pub received: u64,
    pub dispatched: u64,
    pub failed: u64,
    pub dlq: u64,
    pub in_flight: u64,
    pub last_received_ms: Option<i64>,
    pub cost_total_usd_micros: u64,
    pub cost_today_usd_micros: u64,
    pub cost_hour_usd_micros: u64,
    pub autonomous_decisions_total: u64,
    pub autonomous_decisions_today: u64,
    pub autonomous_decisions_hour: u64,
}

pub struct TriggerBinding {
    pub id: TriggerId,
    pub version: u32,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: ProviderId,
    pub autonomy_tier: AutonomyTier,
    pub handler: TriggerHandlerSpec,
    pub dispatch_priority: super::worker_queue::WorkerQueuePriority,
    pub when: Option<TriggerPredicateSpec>,
    pub when_budget: Option<TriggerPredicateBudget>,
    pub retry: TriggerRetryConfig,
    pub match_events: Vec<String>,
    pub dedupe_key: Option<String>,
    pub dedupe_retention_days: u32,
    pub filter: Option<String>,
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
    pub max_autonomous_decisions_per_hour: Option<u64>,
    pub max_autonomous_decisions_per_day: Option<u64>,
    pub on_budget_exhausted: TriggerBudgetExhaustionStrategy,
    pub max_concurrent: Option<u32>,
    pub flow_control: TriggerFlowControlConfig,
    /// CH-04 (#1875): see `TriggerBindingSpec::aggregation`. Cloned at
    /// registration so the dispatcher does not need to lock the registry
    /// for every emit.
    pub aggregation: Option<TriggerAggregationConfig>,
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub definition_fingerprint: String,
    pub state: Mutex<TriggerState>,
    pub metrics: TriggerMetrics,
    pub in_flight: AtomicU64,
    pub cancel_token: Arc<AtomicBool>,
    pub predicate_state: Mutex<TriggerPredicateState>,
}

#[derive(Clone, Debug, Default)]
pub struct TriggerPredicateState {
    pub budget_day_utc: Option<i32>,
    pub budget_hour_utc: Option<i64>,
    pub consecutive_failures: u32,
    pub breaker_open_until_ms: Option<i64>,
    pub recent_cost_usd_micros: VecDeque<u64>,
}

impl std::fmt::Debug for TriggerBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerBinding")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("source", &self.source)
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .field("handler_kind", &self.handler.kind())
            .field("state", &self.state_snapshot())
            .finish()
    }
}

impl TriggerBinding {
    pub fn snapshot(&self) -> TriggerBindingSnapshot {
        TriggerBindingSnapshot {
            id: self.id.as_str().to_string(),
            version: self.version,
            source: self.source,
            kind: self.kind.clone(),
            provider: self.provider.as_str().to_string(),
            autonomy_tier: self.autonomy_tier,
            handler_kind: self.handler.kind().to_string(),
            state: self.state_snapshot(),
            metrics: self.metrics_snapshot(),
            daily_cost_usd: self.daily_cost_usd,
            hourly_cost_usd: self.hourly_cost_usd,
            max_autonomous_decisions_per_hour: self.max_autonomous_decisions_per_hour,
            max_autonomous_decisions_per_day: self.max_autonomous_decisions_per_day,
            on_budget_exhausted: self.on_budget_exhausted,
        }
    }

    fn new(spec: TriggerBindingSpec, version: u32) -> Self {
        Self {
            id: TriggerId::new(spec.id),
            version,
            source: spec.source,
            kind: spec.kind,
            provider: spec.provider,
            autonomy_tier: spec.autonomy_tier,
            handler: spec.handler,
            dispatch_priority: spec.dispatch_priority,
            when: spec.when,
            when_budget: spec.when_budget,
            retry: spec.retry,
            match_events: spec.match_events,
            dedupe_key: spec.dedupe_key,
            dedupe_retention_days: spec.dedupe_retention_days,
            filter: spec.filter,
            daily_cost_usd: spec.daily_cost_usd,
            hourly_cost_usd: spec.hourly_cost_usd,
            max_autonomous_decisions_per_hour: spec.max_autonomous_decisions_per_hour,
            max_autonomous_decisions_per_day: spec.max_autonomous_decisions_per_day,
            on_budget_exhausted: spec.on_budget_exhausted,
            max_concurrent: spec.max_concurrent,
            flow_control: spec.flow_control,
            aggregation: spec.aggregation,
            manifest_path: spec.manifest_path,
            package_name: spec.package_name,
            definition_fingerprint: spec.definition_fingerprint,
            state: Mutex::new(TriggerState::Registering),
            metrics: TriggerMetrics::default(),
            in_flight: AtomicU64::new(0),
            cancel_token: Arc::new(AtomicBool::new(false)),
            predicate_state: Mutex::new(TriggerPredicateState::default()),
        }
    }

    pub fn binding_key(&self) -> String {
        format!("{}@v{}", self.id.as_str(), self.version)
    }

    pub fn state_snapshot(&self) -> TriggerState {
        *self.state.lock().expect("trigger state poisoned")
    }

    pub fn metrics_snapshot(&self) -> TriggerMetricsSnapshot {
        TriggerMetricsSnapshot {
            received: self.metrics.received.load(Ordering::Relaxed),
            dispatched: self.metrics.dispatched.load(Ordering::Relaxed),
            failed: self.metrics.failed.load(Ordering::Relaxed),
            dlq: self.metrics.dlq.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            last_received_ms: *self
                .metrics
                .last_received_ms
                .lock()
                .expect("trigger metrics poisoned"),
            cost_total_usd_micros: self.metrics.cost_total_usd_micros.load(Ordering::Relaxed),
            cost_today_usd_micros: self.metrics.cost_today_usd_micros.load(Ordering::Relaxed),
            cost_hour_usd_micros: self.metrics.cost_hour_usd_micros.load(Ordering::Relaxed),
            autonomous_decisions_total: self
                .metrics
                .autonomous_decisions_total
                .load(Ordering::Relaxed),
            autonomous_decisions_today: self
                .metrics
                .autonomous_decisions_today
                .load(Ordering::Relaxed),
            autonomous_decisions_hour: self
                .metrics
                .autonomous_decisions_hour
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerBindingSnapshot {
    pub id: String,
    pub version: u32,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: String,
    pub autonomy_tier: AutonomyTier,
    pub handler_kind: String,
    pub state: TriggerState,
    pub metrics: TriggerMetricsSnapshot,
    pub daily_cost_usd: Option<f64>,
    pub hourly_cost_usd: Option<f64>,
    pub max_autonomous_decisions_per_hour: Option<u64>,
    pub max_autonomous_decisions_per_day: Option<u64>,
    pub on_budget_exhausted: TriggerBudgetExhaustionStrategy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerDispatchOutcome {
    Dispatched,
    Failed,
    Dlq,
}

#[derive(Debug)]
pub enum TriggerRegistryError {
    DuplicateId(String),
    InvalidSpec(String),
    UnknownId(String),
    UnknownBindingVersion { id: String, version: u32 },
    EventLog(String),
}

impl std::fmt::Display for TriggerRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate trigger id '{id}'"),
            Self::InvalidSpec(message) | Self::EventLog(message) => f.write_str(message),
            Self::UnknownId(id) => write!(f, "unknown trigger id '{id}'"),
            Self::UnknownBindingVersion { id, version } => {
                write!(f, "unknown trigger binding '{id}' version {version}")
            }
        }
    }
}

impl std::error::Error for TriggerRegistryError {}

#[derive(Default)]
pub struct TriggerRegistry {
    bindings: BTreeMap<String, Vec<Arc<TriggerBinding>>>,
    by_provider: BTreeMap<String, BTreeSet<String>>,
    event_log: Option<Arc<AnyEventLog>>,
    secret_provider: Option<Arc<dyn SecretProvider>>,
}

thread_local! {
    static TRIGGER_REGISTRY: RefCell<TriggerRegistry> = RefCell::new(TriggerRegistry::default());
}

thread_local! {
    static ORCHESTRATOR_BUDGET: RefCell<OrchestratorBudgetState> =
        RefCell::new(OrchestratorBudgetState::default());
}

const TERMINATED_VERSION_RETENTION_LIMIT: usize = 2;

const TRIGGERS_LIFECYCLE_TOPIC: &str = "triggers.lifecycle";
const PREDICATE_COST_WINDOW: usize = 100;

#[derive(Clone, Debug, Deserialize)]
struct LifecycleStateTransitionRecord {
    id: String,
    version: u32,
    #[serde(default)]
    definition_fingerprint: Option<String>,
    to_state: TriggerState,
}

#[derive(Clone, Debug)]
struct HistoricalLifecycleRecord {
    occurred_at_ms: i64,
    transition: LifecycleStateTransitionRecord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedTriggerBinding {
    pub version: u32,
    pub received_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Default)]
struct HistoricalVersionLookup {
    matching_version: Option<u32>,
    max_version: Option<u32>,
}

pub fn clear_trigger_registry() {
    TRIGGER_REGISTRY.with(|slot| {
        *slot.borrow_mut() = TriggerRegistry::default();
    });
    clear_orchestrator_budget();
    super::aggregation::clear_aggregation_state();
}

pub fn install_orchestrator_budget(config: OrchestratorBudgetConfig) {
    ORCHESTRATOR_BUDGET.with(|slot| {
        let mut state = slot.borrow_mut();
        rollover_orchestrator_budget(&mut state);
        state.config = config;
    });
}

pub fn clear_orchestrator_budget() {
    ORCHESTRATOR_BUDGET.with(|slot| {
        *slot.borrow_mut() = OrchestratorBudgetState::default();
    });
}

pub fn snapshot_orchestrator_budget() -> OrchestratorBudgetSnapshot {
    ORCHESTRATOR_BUDGET.with(|slot| {
        let mut state = slot.borrow_mut();
        rollover_orchestrator_budget(&mut state);
        OrchestratorBudgetSnapshot {
            daily_cost_usd: state.config.daily_cost_usd,
            hourly_cost_usd: state.config.hourly_cost_usd,
            cost_today_usd_micros: state.cost_today_usd_micros,
            cost_hour_usd_micros: state.cost_hour_usd_micros,
            day_utc: state.day_utc,
            hour_utc: state.hour_utc,
        }
    })
}

pub fn note_orchestrator_budget_cost(cost_usd_micros: u64) {
    if cost_usd_micros == 0 {
        return;
    }
    ORCHESTRATOR_BUDGET.with(|slot| {
        let mut state = slot.borrow_mut();
        rollover_orchestrator_budget(&mut state);
        state.cost_today_usd_micros = state.cost_today_usd_micros.saturating_add(cost_usd_micros);
        state.cost_hour_usd_micros = state.cost_hour_usd_micros.saturating_add(cost_usd_micros);
    });
}

pub(crate) fn note_binding_budget_cost(binding: &TriggerBinding, cost_usd_micros: u64) {
    if cost_usd_micros == 0 {
        return;
    }
    reset_binding_budget_windows(binding);
    binding
        .metrics
        .cost_total_usd_micros
        .fetch_add(cost_usd_micros, Ordering::Relaxed);
    binding
        .metrics
        .cost_today_usd_micros
        .fetch_add(cost_usd_micros, Ordering::Relaxed);
    binding
        .metrics
        .cost_hour_usd_micros
        .fetch_add(cost_usd_micros, Ordering::Relaxed);
}

pub fn orchestrator_budget_would_exceed(expected_cost_usd_micros: u64) -> Option<&'static str> {
    ORCHESTRATOR_BUDGET.with(|slot| {
        let mut state = slot.borrow_mut();
        rollover_orchestrator_budget(&mut state);
        if state.config.hourly_cost_usd.is_some_and(|limit| {
            micros_to_usd(
                state
                    .cost_hour_usd_micros
                    .saturating_add(expected_cost_usd_micros),
            ) > limit
        }) {
            return Some("orchestrator_hourly_budget_exceeded");
        }
        if state.config.daily_cost_usd.is_some_and(|limit| {
            micros_to_usd(
                state
                    .cost_today_usd_micros
                    .saturating_add(expected_cost_usd_micros),
            ) > limit
        }) {
            return Some("orchestrator_daily_budget_exceeded");
        }
        None
    })
}

pub fn reset_binding_budget_windows(binding: &TriggerBinding) {
    let today = utc_day_key();
    let hour = utc_hour_key();
    let mut state = binding
        .predicate_state
        .lock()
        .expect("trigger predicate state poisoned");
    if state.budget_day_utc != Some(today) {
        state.budget_day_utc = Some(today);
        binding
            .metrics
            .cost_today_usd_micros
            .store(0, Ordering::Relaxed);
        binding
            .metrics
            .autonomous_decisions_today
            .store(0, Ordering::Relaxed);
    }
    if state.budget_hour_utc != Some(hour) {
        state.budget_hour_utc = Some(hour);
        binding
            .metrics
            .cost_hour_usd_micros
            .store(0, Ordering::Relaxed);
        binding
            .metrics
            .autonomous_decisions_hour
            .store(0, Ordering::Relaxed);
    }
}

pub fn binding_budget_would_exceed(
    binding: &TriggerBinding,
    expected_cost_usd_micros: u64,
) -> Option<&'static str> {
    reset_binding_budget_windows(binding);
    if binding.hourly_cost_usd.is_some_and(|limit| {
        micros_to_usd(
            binding
                .metrics
                .cost_hour_usd_micros
                .load(Ordering::Relaxed)
                .saturating_add(expected_cost_usd_micros),
        ) > limit
    }) {
        return Some("hourly_budget_exceeded");
    }
    if binding.daily_cost_usd.is_some_and(|limit| {
        micros_to_usd(
            binding
                .metrics
                .cost_today_usd_micros
                .load(Ordering::Relaxed)
                .saturating_add(expected_cost_usd_micros),
        ) > limit
    }) {
        return Some("daily_budget_exceeded");
    }
    None
}

pub fn binding_autonomy_budget_would_exceed(binding: &TriggerBinding) -> Option<&'static str> {
    reset_binding_budget_windows(binding);
    if binding
        .max_autonomous_decisions_per_hour
        .is_some_and(|limit| {
            binding
                .metrics
                .autonomous_decisions_hour
                .load(Ordering::Relaxed)
                .saturating_add(1)
                > limit
        })
    {
        return Some("hourly_autonomy_budget_exceeded");
    }
    if binding
        .max_autonomous_decisions_per_day
        .is_some_and(|limit| {
            binding
                .metrics
                .autonomous_decisions_today
                .load(Ordering::Relaxed)
                .saturating_add(1)
                > limit
        })
    {
        return Some("daily_autonomy_budget_exceeded");
    }
    None
}

pub fn note_autonomous_decision(binding: &TriggerBinding) {
    reset_binding_budget_windows(binding);
    binding
        .metrics
        .autonomous_decisions_total
        .fetch_add(1, Ordering::Relaxed);
    binding
        .metrics
        .autonomous_decisions_today
        .fetch_add(1, Ordering::Relaxed);
    binding
        .metrics
        .autonomous_decisions_hour
        .fetch_add(1, Ordering::Relaxed);
}

pub fn expected_predicate_cost_usd_micros(binding: &TriggerBinding) -> u64 {
    let state = binding
        .predicate_state
        .lock()
        .expect("trigger predicate state poisoned");
    if let Some(average) = average_cost_sample_micros(&state.recent_cost_usd_micros) {
        return average;
    }
    binding
        .when_budget
        .as_ref()
        .and_then(|budget| budget.max_cost_usd)
        .map(usd_to_micros)
        .unwrap_or_default()
}

pub fn record_predicate_cost_sample(binding: &TriggerBinding, cost_usd_micros: u64) {
    let mut state = binding
        .predicate_state
        .lock()
        .expect("trigger predicate state poisoned");
    state.recent_cost_usd_micros.push_back(cost_usd_micros);
    while state.recent_cost_usd_micros.len() > PREDICATE_COST_WINDOW {
        state.recent_cost_usd_micros.pop_front();
    }
}

fn average_cost_sample_micros(samples: &VecDeque<u64>) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let total: u128 = samples.iter().map(|sample| u128::from(*sample)).sum();
    Some((total / samples.len() as u128) as u64)
}

pub fn usd_to_micros(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (value * 1_000_000.0).ceil() as u64
}

pub fn micros_to_usd(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

fn rollover_orchestrator_budget(state: &mut OrchestratorBudgetState) {
    let today = utc_day_key();
    let hour = utc_hour_key();
    if state.day_utc != today {
        state.day_utc = today;
        state.cost_today_usd_micros = 0;
    }
    if state.hour_utc != hour {
        state.hour_utc = hour;
        state.cost_hour_usd_micros = 0;
    }
}

fn utc_day_key() -> i32 {
    (clock::now_utc().date()
        - time::Date::from_calendar_date(1970, time::Month::January, 1).expect("valid epoch date"))
    .whole_days() as i32
}

fn utc_hour_key() -> i64 {
    clock::now_utc().unix_timestamp() / 3_600
}

pub fn snapshot_trigger_bindings() -> Vec<TriggerBindingSnapshot> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let mut snapshots = Vec::new();
        for bindings in registry.bindings.values() {
            for binding in bindings {
                snapshots.push(binding.snapshot());
            }
        }
        snapshots.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then(left.version.cmp(&right.version))
                .then(left.state.as_str().cmp(right.state.as_str()))
        });
        snapshots
    })
}

#[allow(clippy::arc_with_non_send_sync)]
pub fn resolve_trigger_binding_as_of(
    id: &str,
    as_of: OffsetDateTime,
) -> Result<Arc<TriggerBinding>, TriggerRegistryError> {
    let version = binding_version_as_of(id, as_of)?;
    resolve_trigger_binding_version(id, version)
}

#[allow(clippy::arc_with_non_send_sync)]
pub fn resolve_live_or_as_of(
    id: &str,
    recorded: RecordedTriggerBinding,
) -> Result<Arc<TriggerBinding>, TriggerRegistryError> {
    match resolve_live_trigger_binding(id, Some(recorded.version)) {
        Ok(binding) => Ok(binding),
        Err(TriggerRegistryError::UnknownBindingVersion { .. }) => {
            let binding = resolve_trigger_binding_as_of(id, recorded.received_at)?;
            let mut metadata = BTreeMap::new();
            metadata.insert("trigger_id".to_string(), serde_json::json!(id));
            metadata.insert(
                "recorded_version".to_string(),
                serde_json::json!(recorded.version),
            );
            metadata.insert(
                "received_at".to_string(),
                serde_json::json!(recorded
                    .received_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| recorded.received_at.to_string())),
            );
            metadata.insert(
                "resolved_version".to_string(),
                serde_json::json!(binding.version),
            );
            crate::events::log_warn_meta(
                "replay.binding_version_gc_fallback",
                "trigger replay fell back to lifecycle history after binding version GC",
                metadata,
            );
            Ok(binding)
        }
        Err(error) => Err(error),
    }
}

pub fn binding_version_as_of(id: &str, as_of: OffsetDateTime) -> Result<u32, TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        registry.binding_version_as_of(id, as_of)
    })
}

#[allow(clippy::arc_with_non_send_sync)]
fn resolve_trigger_binding_version(
    id: &str,
    version: u32,
) -> Result<Arc<TriggerBinding>, TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        registry
            .binding(id, version)
            .ok_or_else(|| TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            })
    })
}

#[allow(clippy::arc_with_non_send_sync)]
pub fn resolve_live_trigger_binding(
    id: &str,
    version: Option<u32>,
) -> Result<Arc<TriggerBinding>, TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        if let Some(version) = version {
            let binding = registry.binding(id, version).ok_or_else(|| {
                TriggerRegistryError::UnknownBindingVersion {
                    id: id.to_string(),
                    version,
                }
            })?;
            if binding.state_snapshot() == TriggerState::Terminated {
                return Err(TriggerRegistryError::UnknownBindingVersion {
                    id: id.to_string(),
                    version,
                });
            }
            return Ok(binding);
        }

        registry
            .live_bindings_any_source(id)
            .into_iter()
            .max_by_key(|binding| binding.version)
            .ok_or_else(|| TriggerRegistryError::UnknownId(id.to_string()))
    })
}

/// Snapshot active channel-source trigger bindings whose `channel:` selector
/// in `match_events[0]` matches the supplied (scope, scope_id, name) tuple.
///
/// Used by [`crate::channels`] fan-out (CH-02 / #1872). Returns bindings
/// sorted by id then version so dispatch ordering is deterministic.
pub(crate) fn channel_bindings_matching(
    scope: &str,
    scope_id: &str,
    name: &str,
) -> Vec<Arc<TriggerBinding>> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let Some(binding_ids) = registry.by_provider.get("channel") else {
            return Vec::new();
        };
        let mut bindings = Vec::new();
        for id in binding_ids {
            let Some(versions) = registry.bindings.get(id) else {
                continue;
            };
            for binding in versions {
                if binding.state_snapshot() != TriggerState::Active {
                    continue;
                }
                let Some(selector_raw) = binding.match_events.first() else {
                    continue;
                };
                let Ok(selector) = crate::channels::ChannelSelector::parse(selector_raw) else {
                    continue;
                };
                // For tenant-default ("Current") selectors we resolve "current"
                // to the scope_id of the emit, since the trigger and the
                // emitter share the same registry / runtime context.
                if !selector.matches(scope, scope_id, name, scope_id) {
                    continue;
                }
                bindings.push(binding.clone());
            }
        }
        bindings.sort_by(|left, right| {
            left.id
                .as_str()
                .cmp(right.id.as_str())
                .then(left.version.cmp(&right.version))
        });
        bindings
    })
}

pub(crate) fn matching_bindings(event: &super::TriggerEvent) -> Vec<Arc<TriggerBinding>> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let Some(binding_ids) = registry.by_provider.get(event.provider.as_str()) else {
            return Vec::new();
        };

        let mut bindings = Vec::new();
        for id in binding_ids {
            let Some(versions) = registry.bindings.get(id) else {
                continue;
            };
            for binding in versions {
                if binding.state_snapshot() != TriggerState::Active {
                    continue;
                }
                if !binding.match_events.is_empty()
                    && !binding
                        .match_events
                        .iter()
                        .any(|kind| trigger_event_kind_matches(event, kind))
                {
                    continue;
                }
                bindings.push(binding.clone());
            }
        }

        bindings.sort_by(|left, right| {
            left.id
                .as_str()
                .cmp(right.id.as_str())
                .then(left.version.cmp(&right.version))
        });
        bindings
    })
}

fn trigger_event_kind_matches(event: &super::TriggerEvent, expected: &str) -> bool {
    if expected == event.kind {
        return true;
    }
    expected
        .strip_prefix(event.provider.as_str())
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|kind| kind == event.kind)
}

pub async fn install_manifest_triggers(
    specs: Vec<TriggerBindingSpec>,
) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        registry.refresh_runtime_context();
        let mut touched_ids = BTreeSet::new();

        let mut incoming = BTreeMap::new();
        for spec in specs {
            let spec_id = spec.id.clone();
            if spec.source != TriggerBindingSource::Manifest {
                return Err(TriggerRegistryError::InvalidSpec(format!(
                    "manifest install received non-manifest trigger '{spec_id}'"
                )));
            }
            if spec_id.trim().is_empty() {
                return Err(TriggerRegistryError::InvalidSpec(
                    "manifest trigger id cannot be empty".to_string(),
                ));
            }
            if incoming.insert(spec_id.clone(), spec).is_some() {
                return Err(TriggerRegistryError::DuplicateId(spec_id));
            }
        }

        let mut lifecycle = Vec::new();
        let existing_ids: Vec<String> = registry
            .bindings
            .iter()
            .filter(|(_, bindings)| {
                bindings.iter().any(|binding| {
                    binding.source == TriggerBindingSource::Manifest
                        && binding.state_snapshot() != TriggerState::Terminated
                })
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in existing_ids {
            let live_manifest = registry.live_bindings(&id, TriggerBindingSource::Manifest);
            let Some(spec) = incoming.remove(&id) else {
                for binding in live_manifest {
                    registry.transition_binding_to_draining(&binding, &mut lifecycle);
                }
                touched_ids.insert(id.clone());
                continue;
            };

            let has_matching_active = live_manifest.iter().any(|binding| {
                binding.definition_fingerprint == spec.definition_fingerprint
                    && matches!(
                        binding.state_snapshot(),
                        TriggerState::Registering | TriggerState::Active
                    )
            });
            if has_matching_active {
                continue;
            }

            for binding in live_manifest {
                registry.transition_binding_to_draining(&binding, &mut lifecycle);
            }

            let version = registry.next_version_for_spec(&spec);
            registry.register_binding(spec, version, &mut lifecycle);
            touched_ids.insert(id.clone());
        }

        for spec in incoming.into_values() {
            touched_ids.insert(spec.id.clone());
            let version = registry.next_version_for_spec(&spec);
            registry.register_binding(spec, version, &mut lifecycle);
        }

        for id in touched_ids {
            registry.gc_terminated_versions(&id);
        }

        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn dynamic_register(
    mut spec: TriggerBindingSpec,
) -> Result<TriggerId, TriggerRegistryError> {
    if spec.id.trim().is_empty() {
        spec.id = format!("dynamic_trigger_{}", Uuid::now_v7());
    }
    spec.source = TriggerBindingSource::Dynamic;
    let id = spec.id.clone();
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        registry.refresh_runtime_context();

        if registry.bindings.contains_key(id.as_str()) {
            return Err(TriggerRegistryError::DuplicateId(id.clone()));
        }

        let mut lifecycle = Vec::new();
        let version = registry.next_version_for_spec(&spec);
        registry.register_binding(spec, version, &mut lifecycle);
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await?;
    Ok(TriggerId::new(id))
}

pub async fn dynamic_deregister(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live_dynamic = registry.live_bindings(id, TriggerBindingSource::Dynamic);
        if live_dynamic.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live_dynamic {
            registry.transition_binding_to_draining(&binding, &mut lifecycle);
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn drain(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live = registry.live_bindings_any_source(id);
        if live.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live {
            registry.transition_binding_to_draining(&binding, &mut lifecycle);
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn pause(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live = registry.live_bindings_any_source(id);
        if live.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live {
            match binding.state_snapshot() {
                TriggerState::Registering | TriggerState::Active => {
                    registry.transition_binding_state(
                        &binding,
                        TriggerState::Paused,
                        &mut lifecycle,
                    );
                }
                TriggerState::Paused | TriggerState::Draining | TriggerState::Terminated => {}
            }
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn resume(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live = registry.live_bindings_any_source(id);
        if live.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live {
            if binding.state_snapshot() == TriggerState::Paused {
                registry.transition_binding_state(&binding, TriggerState::Active, &mut lifecycle);
            }
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

fn pin_trigger_binding_inner(
    id: &str,
    version: u32,
    allow_terminated: bool,
) -> Result<(), TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        match binding.state_snapshot() {
            TriggerState::Paused => Err(TriggerRegistryError::InvalidSpec(format!(
                "trigger binding '{id}' version {version} is paused"
            ))),
            TriggerState::Terminated if !allow_terminated => {
                Err(TriggerRegistryError::InvalidSpec(format!(
                    "trigger binding '{id}' version {version} is terminated"
                )))
            }
            _ => {
                binding.in_flight.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    })
}

pub fn pin_trigger_binding(id: &str, version: u32) -> Result<(), TriggerRegistryError> {
    pin_trigger_binding_inner(id, version, false)
}

pub async fn unpin_trigger_binding(id: &str, version: u32) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        let current = binding.in_flight.load(Ordering::Relaxed);
        if current == 0 {
            return Err(TriggerRegistryError::InvalidSpec(format!(
                "trigger binding '{id}' version {version} has no in-flight events"
            )));
        }
        binding.in_flight.fetch_sub(1, Ordering::Relaxed);

        let mut lifecycle = Vec::new();
        registry.maybe_finalize_draining(&binding, &mut lifecycle);
        registry.gc_terminated_versions(binding.id.as_str());
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub fn begin_in_flight(id: &str, version: u32) -> Result<(), TriggerRegistryError> {
    begin_in_flight_inner(id, version, false)
}

pub(crate) fn begin_replay_in_flight(id: &str, version: u32) -> Result<(), TriggerRegistryError> {
    begin_in_flight_inner(id, version, true)
}

fn begin_in_flight_inner(
    id: &str,
    version: u32,
    allow_terminated: bool,
) -> Result<(), TriggerRegistryError> {
    pin_trigger_binding_inner(id, version, allow_terminated)?;
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        binding.metrics.received.fetch_add(1, Ordering::Relaxed);
        *binding
            .metrics
            .last_received_ms
            .lock()
            .expect("trigger metrics poisoned") = Some(now_ms());
        Ok(())
    })
}

pub async fn finish_in_flight(
    id: &str,
    version: u32,
    outcome: TriggerDispatchOutcome,
) -> Result<(), TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        let current = binding.in_flight.load(Ordering::Relaxed);
        if current == 0 {
            return Err(TriggerRegistryError::InvalidSpec(format!(
                "trigger binding '{id}' version {version} has no in-flight events"
            )));
        }
        match outcome {
            TriggerDispatchOutcome::Dispatched => {
                binding.metrics.dispatched.fetch_add(1, Ordering::Relaxed);
            }
            TriggerDispatchOutcome::Failed => {
                binding.metrics.failed.fetch_add(1, Ordering::Relaxed);
            }
            TriggerDispatchOutcome::Dlq => {
                binding.metrics.dlq.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    })?;

    unpin_trigger_binding(id, version).await
}

impl TriggerRegistry {
    fn refresh_runtime_context(&mut self) {
        if self.event_log.is_none() {
            self.event_log = active_event_log();
        }
        if self.secret_provider.is_none() {
            self.secret_provider = default_secret_provider();
        }
    }

    fn binding(&self, id: &str, version: u32) -> Option<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .and_then(|bindings| bindings.iter().find(|binding| binding.version == version))
            .cloned()
    }

    fn live_bindings(&self, id: &str, source: TriggerBindingSource) -> Vec<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(|binding| {
                binding.source == source && binding.state_snapshot() != TriggerState::Terminated
            })
            .cloned()
            .collect()
    }

    fn live_bindings_any_source(&self, id: &str) -> Vec<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(|binding| binding.state_snapshot() != TriggerState::Terminated)
            .cloned()
            .collect()
    }

    fn next_version_for_spec(&self, spec: &TriggerBindingSpec) -> u32 {
        if let Some(version) = self
            .bindings
            .get(spec.id.as_str())
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .find(|binding| binding.definition_fingerprint == spec.definition_fingerprint)
            .map(|binding| binding.version)
        {
            return version;
        }

        let historical =
            self.historical_versions_for(spec.id.as_str(), spec.definition_fingerprint.as_str());
        if let Some(version) = historical.matching_version {
            return version;
        }

        self.bindings
            .get(spec.id.as_str())
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .map(|binding| binding.version)
            .chain(historical.max_version)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn gc_terminated_versions(&mut self, id: &str) {
        let Some(bindings) = self.bindings.get_mut(id) else {
            return;
        };

        let mut newest_versions: Vec<u32> =
            bindings.iter().map(|binding| binding.version).collect();
        newest_versions.sort_unstable_by(|left, right| right.cmp(left));
        newest_versions.truncate(TERMINATED_VERSION_RETENTION_LIMIT);
        let retained_versions: BTreeSet<u32> = newest_versions.into_iter().collect();

        bindings.retain(|binding| {
            binding.state_snapshot() != TriggerState::Terminated
                || retained_versions.contains(&binding.version)
        });

        if bindings.is_empty() {
            self.bindings.remove(id);
        }
    }

    fn historical_versions_for(&self, id: &str, fingerprint: &str) -> HistoricalVersionLookup {
        let mut lookup = HistoricalVersionLookup::default();
        for record in self.lifecycle_records_for(id) {
            lookup.max_version = Some(
                lookup
                    .max_version
                    .unwrap_or(0)
                    .max(record.transition.version),
            );
            if record.transition.definition_fingerprint.as_deref() == Some(fingerprint) {
                lookup.matching_version = Some(record.transition.version);
            }
        }
        lookup
    }

    fn binding_version_as_of(
        &self,
        id: &str,
        as_of: OffsetDateTime,
    ) -> Result<u32, TriggerRegistryError> {
        let cutoff_ms = harn_clock::offset_datetime_to_ms(as_of);
        let mut active_version = None;
        for record in self.lifecycle_records_for(id) {
            if record.occurred_at_ms > cutoff_ms {
                break;
            }
            match record.transition.to_state {
                TriggerState::Active => active_version = Some(record.transition.version),
                TriggerState::Paused | TriggerState::Draining | TriggerState::Terminated => {
                    if active_version == Some(record.transition.version) {
                        active_version = None;
                    }
                }
                TriggerState::Registering => {}
            }
        }

        active_version.ok_or_else(|| {
            TriggerRegistryError::InvalidSpec(format!(
                "no active trigger binding '{}' at {}",
                id,
                as_of
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| as_of.to_string())
            ))
        })
    }

    fn lifecycle_records_for(&self, id: &str) -> Vec<HistoricalLifecycleRecord> {
        let Some(event_log) = self.event_log.as_ref() else {
            return Vec::new();
        };
        let topic = Topic::new(TRIGGERS_LIFECYCLE_TOPIC)
            .expect("static triggers.lifecycle topic should always be valid");
        futures::executor::block_on(event_log.read_range(&topic, None, usize::MAX))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, event)| {
                let occurred_at_ms = event.occurred_at_ms;
                let transition: LifecycleStateTransitionRecord =
                    serde_json::from_value(event.payload).ok()?;
                (transition.id == id).then_some(HistoricalLifecycleRecord {
                    occurred_at_ms,
                    transition,
                })
            })
            .collect()
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn register_binding(
        &mut self,
        spec: TriggerBindingSpec,
        version: u32,
        lifecycle: &mut Vec<LogEvent>,
    ) -> Arc<TriggerBinding> {
        let binding = Arc::new(TriggerBinding::new(spec, version));
        self.by_provider
            .entry(binding.provider.as_str().to_string())
            .or_default()
            .insert(binding.id.as_str().to_string());
        self.bindings
            .entry(binding.id.as_str().to_string())
            .or_default()
            .push(binding.clone());
        lifecycle.push(lifecycle_event(&binding, None, TriggerState::Registering));
        self.transition_binding_state(&binding, TriggerState::Active, lifecycle);
        binding
    }

    fn transition_binding_to_draining(
        &self,
        binding: &Arc<TriggerBinding>,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        if matches!(binding.state_snapshot(), TriggerState::Terminated) {
            return;
        }
        self.transition_binding_state(binding, TriggerState::Draining, lifecycle);
        self.maybe_finalize_draining(binding, lifecycle);
    }

    fn maybe_finalize_draining(
        &self,
        binding: &Arc<TriggerBinding>,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        if binding.state_snapshot() == TriggerState::Draining
            && binding.in_flight.load(Ordering::Relaxed) == 0
        {
            self.transition_binding_state(binding, TriggerState::Terminated, lifecycle);
        }
    }

    fn transition_binding_state(
        &self,
        binding: &Arc<TriggerBinding>,
        next: TriggerState,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        let mut state = binding.state.lock().expect("trigger state poisoned");
        let previous = *state;
        if previous == next {
            return;
        }
        *state = next;
        drop(state);
        // CH-04 (#1875): drop any pending aggregation buffers when the
        // binding terminates so a long-lived buffer doesn't fire after
        // the trigger has been removed. Leftover events are discarded —
        // matches the in-flight drain contract elsewhere in the
        // registry (the trigger is gone; we'd have nowhere to dispatch).
        if next == TriggerState::Terminated && binding.aggregation.is_some() {
            let _ = super::aggregation::drop_binding_aggregation(&binding.binding_key());
        }
        lifecycle.push(lifecycle_event(binding, Some(previous), next));
    }
}

fn lifecycle_event(
    binding: &TriggerBinding,
    from_state: Option<TriggerState>,
    to_state: TriggerState,
) -> LogEvent {
    LogEvent::new(
        "state_transition",
        serde_json::json!({
            "id": binding.id.as_str(),
            "binding_key": binding.binding_key(),
            "version": binding.version,
            "provider": binding.provider.as_str(),
            "kind": &binding.kind,
            "source": binding.source.as_str(),
            "handler_kind": binding.handler.kind(),
            "definition_fingerprint": &binding.definition_fingerprint,
            "from_state": from_state.map(TriggerState::as_str),
            "to_state": to_state.as_str(),
        }),
    )
}

async fn append_lifecycle_events(
    event_log: Option<Arc<AnyEventLog>>,
    events: Vec<LogEvent>,
) -> Result<(), TriggerRegistryError> {
    let Some(event_log) = event_log else {
        return Ok(());
    };
    if events.is_empty() {
        return Ok(());
    }

    let topic = Topic::new(TRIGGERS_LIFECYCLE_TOPIC)
        .expect("static triggers.lifecycle topic should always be valid");
    for event in events {
        event_log
            .append(&topic, event)
            .await
            .map_err(|error| TriggerRegistryError::EventLog(error.to_string()))?;
    }
    Ok(())
}

fn default_secret_provider() -> Option<Arc<dyn SecretProvider>> {
    configured_default_chain(default_secret_namespace())
        .ok()
        .map(|provider| Arc::new(provider) as Arc<dyn SecretProvider>)
}

fn default_secret_namespace() -> String {
    if let Ok(namespace) = std::env::var("HARN_SECRET_NAMESPACE") {
        if !namespace.trim().is_empty() {
            return namespace;
        }
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let leaf = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    format!("harn/{leaf}")
}

fn now_ms() -> i64 {
    clock::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{
        install_active_event_log, install_memory_for_current_thread, open_event_log,
        pin_test_occurred_at_ms, reset_active_event_log, EventLogBackendKind, EventLogConfig,
    };
    use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
    use std::path::Path;
    use std::rc::Rc;

    /// Build the `OffsetDateTime` that corresponds to a pinned event-log
    /// `occurred_at_ms`, so `as_of` cutoffs can be expressed in the same
    /// reference frame as `pin_test_occurred_at_ms` without touching the wall
    /// clock.
    fn offset_from_ms(ms: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000)
            .expect("epoch ms within OffsetDateTime range")
    }

    fn manifest_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "webhook".to_string(),
            provider: ProviderId::from("github"),
            autonomy_tier: crate::AutonomyTier::ActAuto,
            handler: TriggerHandlerSpec::Worker {
                queue: format!("{id}-queue"),
            },
            dispatch_priority: crate::WorkerQueuePriority::Normal,
            when: None,
            when_budget: None,
            retry: TriggerRetryConfig::default(),
            match_events: vec!["issues.opened".to_string()],
            dedupe_key: Some("event.dedupe_key".to_string()),
            dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
            filter: Some("event.kind".to_string()),
            daily_cost_usd: Some(5.0),
            hourly_cost_usd: None,
            max_autonomous_decisions_per_hour: None,
            max_autonomous_decisions_per_day: None,
            on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
            max_concurrent: Some(10),
            flow_control: crate::triggers::TriggerFlowControlConfig::default(),
            aggregation: None,
            manifest_path: None,
            package_name: Some("workspace".to_string()),
            definition_fingerprint: fingerprint.to_string(),
        }
    }

    fn install_test_memory_event_log() -> Arc<AnyEventLog> {
        install_memory_for_current_thread(512)
    }

    fn install_test_sqlite_event_log(base_dir: &Path) -> Arc<AnyEventLog> {
        let config = EventLogConfig {
            backend: EventLogBackendKind::Sqlite,
            file_dir: base_dir.join("events"),
            sqlite_path: base_dir.join("events.sqlite"),
            queue_depth: 512,
        };
        let log = open_event_log(&config).expect("open isolated sqlite event log");
        install_active_event_log(log)
    }

    fn dynamic_spec(id: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Dynamic,
            kind: "webhook".to_string(),
            provider: ProviderId::from("github"),
            autonomy_tier: crate::AutonomyTier::ActAuto,
            handler: TriggerHandlerSpec::Worker {
                queue: format!("{id}-queue"),
            },
            dispatch_priority: crate::WorkerQueuePriority::Normal,
            when: None,
            when_budget: None,
            retry: TriggerRetryConfig::default(),
            match_events: vec!["issues.opened".to_string()],
            dedupe_key: None,
            dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
            filter: None,
            daily_cost_usd: None,
            hourly_cost_usd: None,
            max_autonomous_decisions_per_hour: None,
            max_autonomous_decisions_per_day: None,
            on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
            max_concurrent: None,
            flow_control: crate::triggers::TriggerFlowControlConfig::default(),
            aggregation: None,
            manifest_path: None,
            package_name: None,
            definition_fingerprint: format!("dynamic:{id}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_loaded_trigger_registers_with_zeroed_metrics() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");

        let snapshots = snapshot_trigger_bindings();
        assert_eq!(snapshots.len(), 1);
        let binding = &snapshots[0];
        assert_eq!(binding.id, "github-new-issue");
        assert_eq!(binding.version, 1);
        assert_eq!(binding.state, TriggerState::Active);
        assert_eq!(binding.metrics, TriggerMetricsSnapshot::default());

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_register_assigns_unique_ids_and_rejects_duplicates() {
        clear_trigger_registry();

        let first = dynamic_register(dynamic_spec("dynamic-a"))
            .await
            .expect("first dynamic trigger");
        let second = dynamic_register(dynamic_spec("dynamic-b"))
            .await
            .expect("second dynamic trigger");
        assert_ne!(first, second);

        let error = dynamic_register(dynamic_spec("dynamic-a"))
            .await
            .expect_err("duplicate id should fail");
        assert!(matches!(error, TriggerRegistryError::DuplicateId(_)));

        clear_trigger_registry();
    }

    #[test]
    fn expected_predicate_cost_average_does_not_overflow() {
        let binding = TriggerBinding::new(manifest_spec("costed", "v1"), 1);
        record_predicate_cost_sample(&binding, u64::MAX);
        record_predicate_cost_sample(&binding, u64::MAX);

        assert_eq!(expected_predicate_cost_usd_micros(&binding), u64::MAX);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_waits_for_in_flight_events_before_terminating() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");

        drain("github-new-issue").await.expect("drain succeeds");
        let binding = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("binding snapshot");
        assert_eq!(binding.state, TriggerState::Draining);
        assert_eq!(binding.metrics.in_flight, 1);

        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish in-flight event");
        let binding = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("binding snapshot");
        assert_eq!(binding.state, TriggerState::Terminated);
        assert_eq!(binding.metrics.in_flight, 0);

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hot_reload_registers_new_version_while_old_binding_drains() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("initial manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("updated manifest trigger installs");

        let snapshots = snapshot_trigger_bindings();
        assert_eq!(snapshots.len(), 2);
        let old = snapshots
            .iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("old binding");
        let new = snapshots
            .iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 2)
            .expect("new binding");
        assert_eq!(old.state, TriggerState::Draining);
        assert_eq!(new.state, TriggerState::Active);

        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish old in-flight event");
        let old = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("old binding");
        assert_eq!(old.state, TriggerState::Terminated);

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_drops_terminated_versions_beyond_retention_limit() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("install v1");
        begin_in_flight("github-new-issue", 1).expect("pin v1");

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("install v2");
        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish v1");

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v3")])
            .await
            .expect("install v3");

        let snapshots = snapshot_trigger_bindings();
        let versions: Vec<u32> = snapshots
            .into_iter()
            .filter(|binding| binding.id == "github-new-issue")
            .map(|binding| binding.version)
            .collect();
        assert_eq!(versions, vec![2, 3]);

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_transitions_append_to_event_log() {
        clear_trigger_registry();
        reset_active_event_log();
        let log = install_test_memory_event_log();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");
        drain("github-new-issue").await.expect("drain succeeds");
        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish event");

        let topic = Topic::new("triggers.lifecycle").expect("valid lifecycle topic");
        let events = log
            .read_range(&topic, None, 32)
            .await
            .expect("read lifecycle events");
        let states: Vec<String> = events
            .into_iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("to_state")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            })
            .collect();
        assert_eq!(
            states,
            vec![
                "registering".to_string(),
                "active".to_string(),
                "draining".to_string(),
                "terminated".to_string(),
            ]
        );

        reset_active_event_log();
        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn version_history_reuses_historical_version_after_restart() {
        clear_trigger_registry();
        reset_active_event_log();
        let tempdir = tempfile::tempdir().expect("tempdir");
        install_test_sqlite_event_log(tempdir.path());

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("initial manifest trigger installs");
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("updated manifest trigger installs");

        clear_trigger_registry();
        reset_active_event_log();
        install_test_sqlite_event_log(tempdir.path());

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("manifest reload reuses historical version");

        let binding = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue")
            .expect("binding snapshot");
        assert_eq!(binding.version, 2);

        reset_active_event_log();
        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn binding_version_as_of_reports_historical_active_version() {
        clear_trigger_registry();
        reset_active_event_log();
        install_test_memory_event_log();

        // Pin the event-log timestamp instead of sleeping between captures:
        // v1's lifecycle events stamp at T1, v2's at T2 > T1, with zero
        // wall-clock dependence. `before_reload` sits at T1 (so v1 is active
        // but v2 is not yet), `after_reload` at T2 (so v2 is active).
        const T1_MS: i64 = 1_700_000_000_000;
        const T2_MS: i64 = T1_MS + 50;
        let before_reload = offset_from_ms(T1_MS);
        let after_reload = offset_from_ms(T2_MS);

        let clock = pin_test_occurred_at_ms(T1_MS);
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("initial manifest trigger installs");
        drop(clock);

        let clock = pin_test_occurred_at_ms(T2_MS);
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("updated manifest trigger installs");
        drop(clock);

        assert_eq!(
            binding_version_as_of("github-new-issue", before_reload)
                .expect("version before reload"),
            1
        );
        assert_eq!(
            binding_version_as_of("github-new-issue", after_reload).expect("version after reload"),
            2
        );

        reset_active_event_log();
        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_live_or_as_of_logs_structured_gc_fallback() {
        clear_trigger_registry();
        reset_active_event_log();
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());
        install_test_memory_event_log();

        // Pin the event-log timestamps: v1..v3 land at T1, v4 at T2 > T1, with
        // `received_at` at T1 so the gc fallback resolves to v3 (the latest
        // binding active at-or-before `received_at`) while v4 is strictly later.
        const T1_MS: i64 = 1_700_000_000_000;
        const T2_MS: i64 = T1_MS + 50;
        let received_at = offset_from_ms(T1_MS);

        let clock = pin_test_occurred_at_ms(T1_MS);
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("install v1");
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("install v2");
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v3")])
            .await
            .expect("install v3");
        drop(clock);

        let clock = pin_test_occurred_at_ms(T2_MS);
        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v4")])
            .await
            .expect("install v4");
        drop(clock);

        let binding = resolve_live_or_as_of(
            "github-new-issue",
            RecordedTriggerBinding {
                version: 1,
                received_at,
            },
        )
        .expect("resolve fallback binding");
        assert_eq!(binding.version, 3);

        let warning = sink
            .logs
            .borrow()
            .iter()
            .find(|log| log.category == "replay.binding_version_gc_fallback")
            .cloned()
            .expect("gc fallback warning");
        assert_eq!(warning.level, EventLevel::Warn);
        assert_eq!(
            warning.metadata.get("trigger_id"),
            Some(&serde_json::json!("github-new-issue"))
        );
        assert_eq!(
            warning.metadata.get("recorded_version"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            warning.metadata.get("received_at"),
            Some(&serde_json::json!(received_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| received_at.to_string())))
        );
        assert_eq!(
            warning.metadata.get("resolved_version"),
            Some(&serde_json::json!(3))
        );

        clear_event_sinks();
        crate::events::reset_event_sinks();
        reset_active_event_log();
        clear_trigger_registry();
    }
}
