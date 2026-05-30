//! Named agent thread pools (PL-01/PL-03/PL-05).
//!
//! Foundation for the agent pool epic (#1883). Provides a thread-local
//! registry of named pools that bound the number of concurrent Harn
//! closure executions and queue excess submissions. Queue strategy ships
//! here, with bounded backpressure policies layered on the single submit
//! path. PL-05 (#1890) adds durability backends so pipeline-scope pools
//! survive process restart with stale-in-flight detection.
//!
//! Scope conventions follow the channel scope contract (CH-01 / CH-03):
//!
//! * `scope: "session"` (default) — in-memory only, lost on session close.
//! * `scope: "pipeline"` — file-backed JSONL store under `.harn/pools/`,
//!   keyed by pipeline id + pool name so reload across process restart
//!   reuses the same persistent state.
//! * `scope: "tenant"` / `scope: "org"` — host-routed (harn-cloud, see
//!   harn-cloud#306). Accepted at the API level so user code is portable;
//!   today they fail with a clear "host-routed (harn-cloud) — not
//!   wired" diagnostic until the host capability ships.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;
use harn_parser::diagnostic_codes::Code;
use serde::{Deserialize, Serialize};
use serde_json::json;

mod storage;
use storage::{PersistedPoolMeta, PersistedPoolState, PersistedTask, PoolDurableStore, PoolRecord};

/// Default `max_concurrent` when a pool is created without one.
const DEFAULT_MAX_CONCURRENT: usize = 1;

/// Type tag stamped on every pool handle and task handle returned to Harn
/// code. `wait_agent` matches on `POOL_TASK_TYPE` to route pool task
/// handles to `__pool_wait` (see `agent/workers.harn`).
const POOL_TYPE: &str = "pool";
const POOL_TASK_TYPE: &str = "pool_task";
const POOL_AUDIT_TOPIC: &str = "lifecycle.pool.audit";
const POOL_EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;

/// On-disk root for pipeline-scope pool state. Mirrors the channel scope
/// convention (`.harn/...`) so durable artifacts stay co-located with the
/// pipeline's other state.
const PIPELINE_POOLS_ROOT: &str = ".harn/pools";

/// Default stale-in-flight threshold. A task whose heartbeat is older than
/// this on reload is re-enqueued. Configurable via `opts.stale_after_ms`.
const DEFAULT_STALE_AFTER_MS: i64 = 30_000;

#[derive(Clone)]
struct PendingTask {
    task_id: String,
    closure: Rc<VmClosure>,
    state: Rc<RefCell<TaskState>>,
    priority: i64,
    key: Option<String>,
    /// Tiebreaker so FIFO order is preserved among equal priorities.
    seq: u64,
    /// Async-builtin execution context captured at submit time, while the
    /// `__pool_submit` builtin still holds the `task_local` context. Pool tasks
    /// are dispatched from slot-free callbacks (a sibling task finishing, a
    /// `pool_wait`, etc.) that run with NO ambient context — `task_local` is
    /// task-scoped and, unlike the old `thread_local!` stack, does not leak
    /// across `spawn_local`. So the runner VM must travel with the task rather
    /// than be re-cloned from ambient context at dispatch time. See harn#2667.
    /// Wrapped in `Rc<RefCell<_>>` because `PendingTask` is `Clone` and `Vm`
    /// is not; the handle is only ever cloned-into a single dispatch.
    context_vm: Option<Rc<RefCell<crate::vm::Vm>>>,
}

struct TaskState {
    id: String,
    pool_id: String,
    pool_name: String,
    key: Option<String>,
    priority: i64,
    status: TaskStatus,
    submitted_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    result: Option<VmValue>,
    error: Option<String>,
    rejection_reason: Option<String>,
    rejection_policy: Option<String>,
    /// Caller-supplied dedupe key. When set, two submissions with the
    /// same `idempotency_key` resolve to the same task (the second call
    /// returns the existing handle / terminal snapshot instead of being
    /// re-enqueued). Persisted to the durable store so resubmission after
    /// a process restart short-circuits to the previously recorded
    /// outcome.
    idempotency_key: Option<String>,
    /// Wall-clock ms of the latest progress signal (submit, dispatch,
    /// terminal transition). Drives stale-in-flight detection on
    /// pipeline-scope pool reload: any task whose `heartbeat_at_ms` is
    /// older than `stale_after_ms` at load time is re-enqueued.
    heartbeat_at_ms: i64,
    /// Wall-clock ms snapshot taken at submission, used to compute
    /// `queued_for_ms` on the `PoolDequeueReceipt` when the task is
    /// finally plucked from the queue (PL-06).
    submitted_at_ms: i64,
    /// Live span link to the `PoolSubmit` span. Populated when tracing is
    /// enabled at submit time so the deferred `PoolDequeue` span can link
    /// back across the async boundary (PL-06 / `set_span_link` from
    /// harn#1858).
    submit_span_link: Option<crate::tracing::SpanLink>,
    /// Caller identifier captured at submit time (`workflow_id`,
    /// `agent_session_id`, or `worker_id` when set; otherwise "user").
    /// Stamped on the `PoolSubmitReceipt`.
    submitted_by: String,
    /// Senders wake every `pool_wait` future the moment the task reaches
    /// a terminal state. The Sender side is dropped to fire the signal.
    waiters: Vec<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Rejected,
}

impl TaskStatus {
    fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Rejected => "rejected",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Rejected
        )
    }
}

struct PoolEntry {
    id: String,
    name: String,
    max_concurrent: usize,
    created_at: String,
    submit_counter: u64,
    queue: VecDeque<PendingTask>,
    queue_strategy: QueueStrategy,
    backpressure: BackpressureStrategy,
    round_robin_after: Option<String>,
    active: HashMap<String, Rc<RefCell<TaskState>>>,
    tasks: BTreeMap<String, Rc<RefCell<TaskState>>>,
    space_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
    /// Optional per-create user-supplied config (queue strategy, priority
    /// fn, backpressure). Queue strategy is evaluated by this module;
    /// later pool tickets wire the other config knobs.
    config: BTreeMap<String, VmValue>,
    /// Durability scope (PL-05 / #1890). `Session` is in-memory only.
    /// `Pipeline` writes a JSONL append-log under `.harn/pools/` so the
    /// pool's pending queue + in-flight task metadata survives process
    /// restart. `Tenant` / `Org` are reserved for harn-cloud (#306) and
    /// today fail with a clear host-routed diagnostic.
    scope: PoolScope,
    /// Scope identifier (e.g. pipeline run id). Empty for session-scoped
    /// pools because `Session` is registry-local.
    scope_id: String,
    /// Idempotency-key → existing task id. Populated when a submission
    /// carries `idempotency_key`, and used to short-circuit duplicate
    /// submissions to the same `task_handle_value`.
    idempotency_index: HashMap<String, String>,
    /// Stale-in-flight threshold (ms). Used by the file-backed reload
    /// path to decide which `Running` tasks must be re-enqueued.
    stale_after_ms: i64,
    /// Optional durable store the pool serializes state mutations into.
    /// `None` for session-scoped pools.
    store: Option<Rc<RefCell<PoolDurableStore>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QueueStrategy {
    Fifo,
    Priority,
    Lifo,
    FairRoundRobin { key_field: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BackpressureStrategy {
    Unbounded,
    Queue {
        max_depth: usize,
        on_full: QueueOnFullPolicy,
    },
    FailFast,
    RingBuffer {
        capacity: usize,
    },
}

impl BackpressureStrategy {
    fn name(&self) -> &'static str {
        match self {
            BackpressureStrategy::Unbounded => "unbounded",
            BackpressureStrategy::Queue { .. } => "queue",
            BackpressureStrategy::FailFast => "fail_fast",
            BackpressureStrategy::RingBuffer { .. } => "ring_buffer",
        }
    }

    fn max_depth(&self) -> Option<usize> {
        match self {
            BackpressureStrategy::Queue { max_depth, .. } => Some(*max_depth),
            BackpressureStrategy::RingBuffer { capacity } => Some(*capacity),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueOnFullPolicy {
    BlockSubmitter,
    DropOldest,
    DropNewest,
    FailSubmitter,
}

impl QueueOnFullPolicy {
    fn as_str(self) -> &'static str {
        match self {
            QueueOnFullPolicy::BlockSubmitter => "block_submitter",
            QueueOnFullPolicy::DropOldest => "drop_oldest",
            QueueOnFullPolicy::DropNewest => "drop_newest",
            QueueOnFullPolicy::FailSubmitter => "fail_submitter",
        }
    }
}

/// Durability scope for a registered pool. Follows the channel scope
/// contract (CH-01 / CH-03 in `channels.rs`) so user code stays portable
/// across primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PoolScope {
    /// In-memory; lost on session close. Default when `scope` is omitted.
    Session,
    /// File-backed JSONL store under `.harn/pools/`. Pending queue +
    /// in-flight task metadata survive process restart.
    Pipeline,
    /// Reserved for harn-cloud (#306). Today fails with a clear
    /// host-routed diagnostic until the host capability lands.
    Tenant,
    /// Reserved for harn-cloud (#306). Today fails with a clear
    /// host-routed diagnostic until the host capability lands.
    Org,
}

impl PoolScope {
    fn parse(value: &str) -> Result<Self, VmError> {
        match value.trim() {
            "" | "session" => Ok(Self::Session),
            "pipeline" => Ok(Self::Pipeline),
            "tenant" => Ok(Self::Tenant),
            "org" => Ok(Self::Org),
            other => Err(VmError::Runtime(format!(
                "pool_create: unknown scope '{other}' (expected one of session/pipeline/tenant/org)"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Pipeline => "pipeline",
            Self::Tenant => "tenant",
            Self::Org => "org",
        }
    }

    /// True for scopes the host must service (harn-cloud). At the
    /// language level we still accept the keyword so user code stays
    /// portable across runtimes, but the in-process pool registry has
    /// no business persisting tenant/org state on its own.
    fn is_host_routed(self) -> bool {
        matches!(self, Self::Tenant | Self::Org)
    }
}

#[derive(Clone)]
struct PoolDropAudit {
    pool_id: String,
    pool_name: String,
    task_id: String,
    replacement_task_id: Option<String>,
    reason: String,
    policy: String,
    queue_depth: usize,
    max_depth: Option<usize>,
    occurred_at: String,
}

/// `PoolSubmitReceipt` (PL-06 / #1891). One per accepted submission.
/// Pairs 1:1 with a `PoolSubmit` span on the audit timeline.
#[derive(Clone)]
struct PoolSubmitReceipt {
    pool_id: String,
    pool_name: String,
    task_id: String,
    submitted_at: String,
    priority: i64,
    key: Option<String>,
    idempotency_key: Option<String>,
    submitted_by: String,
}

/// `PoolDequeueReceipt` (PL-06 / #1891). One per task plucked out of the
/// queue by the dispatcher. Pairs 1:1 with a `PoolDequeue` span that
/// links back to the originating `PoolSubmit` span.
#[derive(Clone)]
struct PoolDequeueReceipt {
    pool_id: String,
    pool_name: String,
    task_id: String,
    dequeued_at: String,
    queued_for_ms: i64,
    /// Sequential slot index inside `pool.active` at the moment of
    /// dispatch. Useful when correlating dequeue receipts with
    /// `max_concurrent` capacity exhaustion.
    slot_index: usize,
}

/// RAII guard around a pool tracing span. Mirrors the
/// `LifecycleSpanGuard` pattern used by P-05 (#1858): we open both a
/// thread-local Harn span (for `trace_spans()` introspection) and an
/// OTel `tracing::Span` (for the exporter), wire OTel span links via
/// `crate::observability::otel::set_span_link`, and close them both on
/// `end()` / `Drop`. Disabled-tracing path is a no-op because
/// `crate::tracing::span_start_*` returns id 0 and short-circuits.
struct PoolSpanGuard {
    span_id: u64,
    otel_span: tracing::Span,
}

impl PoolSpanGuard {
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
            target: "harn.vm.pool",
            "harn.pool",
            harn.kind = kind.as_str(),
            harn.name = %name,
        );
        for link in links {
            let trace_id = crate::TraceId(link.trace_id);
            let mut attributes: std::collections::HashMap<String, String> =
                link.attributes.into_iter().collect();
            attributes
                .entry("harn.link.kind".to_string())
                .or_insert_with(|| "pool_submit".to_string());
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

impl Drop for PoolSpanGuard {
    fn drop(&mut self) {
        self.end();
    }
}

impl QueueStrategy {
    fn name(&self) -> &'static str {
        match self {
            QueueStrategy::Fifo => "fifo",
            QueueStrategy::Priority => "priority",
            QueueStrategy::Lifo => "lifo",
            QueueStrategy::FairRoundRobin { .. } => "fair_round_robin",
        }
    }

    fn key_field(&self) -> Option<&str> {
        match self {
            QueueStrategy::FairRoundRobin { key_field } => Some(key_field.as_str()),
            _ => None,
        }
    }
}

thread_local! {
    static POOLS: RefCell<HashMap<String, Rc<RefCell<PoolEntry>>>> =
        RefCell::new(HashMap::new());
    /// Name → pool_id lookup so `pool_get("...")` and `pool_create({name: ...})`
    /// duplicate detection stay O(1).
    static POOL_NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn next_pool_id() -> String {
    format!("pool_{}", uuid::Uuid::now_v7())
}

/// Deterministic pool id for pipeline-scope pools. Same `(scope_id,
/// name)` always maps to the same id so reloads after restart bind to
/// the existing JSONL file. The hash never includes raw user input on
/// the filesystem path (see [`pipeline_pool_file_path`]).
fn deterministic_pool_id(scope: PoolScope, scope_id: &str, name: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(scope.as_str().as_bytes());
    hasher.update(b"\x00");
    hasher.update(scope_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize().to_hex();
    // Take the first 32 hex chars — enough collision resistance for the
    // single-pipeline scope while keeping ids ergonomic in logs.
    format!("pool_{}_{}", scope.as_str(), &digest.as_str()[..32])
}

fn next_task_id(pool: &PoolEntry) -> String {
    format!("{}_task_{}", pool.id, uuid::Uuid::now_v7())
}

fn lookup_pool(pool_id: &str) -> Result<Rc<RefCell<PoolEntry>>, VmError> {
    POOLS.with(|pools| {
        pools
            .borrow()
            .get(pool_id)
            .cloned()
            .ok_or_else(|| VmError::Runtime(format!("pool not found: {pool_id}")))
    })
}

fn pool_id_from_value(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Dict(map) => map
            .get("id")
            .map(|value| value.display())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| VmError::Runtime(format!("{builtin}: pool handle missing id"))),
        _ => Err(VmError::Runtime(format!(
            "{builtin}: expected pool handle or pool id"
        ))),
    }
}

fn task_handle_from_value(value: &VmValue, builtin: &str) -> Result<(String, String), VmError> {
    let map = value.as_dict().ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: expected pool task handle (got {})",
            value.type_name()
        ))
    })?;
    let pool_id = map
        .get("pool_id")
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: task handle missing pool_id")))?;
    let task_id = map
        .get("id")
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: task handle missing id")))?;
    Ok((pool_id, task_id))
}

fn parse_options(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(map)) => Ok((**map).clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: options must be a dict (got {})",
            other.type_name()
        ))),
    }
}

fn parse_max_concurrent(opts: &BTreeMap<String, VmValue>) -> Result<usize, VmError> {
    match opts.get("max_concurrent") {
        None | Some(VmValue::Nil) => Ok(DEFAULT_MAX_CONCURRENT),
        Some(VmValue::Int(n)) => {
            if *n < 1 {
                return Err(VmError::Runtime(
                    "pool_create: max_concurrent must be >= 1".to_string(),
                ));
            }
            Ok(*n as usize)
        }
        Some(other) => Err(VmError::Runtime(format!(
            "pool_create: max_concurrent must be an int (got {})",
            other.type_name()
        ))),
    }
}

fn parse_name(opts: &BTreeMap<String, VmValue>) -> Option<String> {
    opts.get("name").and_then(|value| match value {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    })
}

fn parse_scope(opts: &BTreeMap<String, VmValue>) -> Result<PoolScope, VmError> {
    match opts.get("scope") {
        None | Some(VmValue::Nil) => Ok(PoolScope::Session),
        Some(VmValue::String(text)) => PoolScope::parse(text),
        Some(other) => Err(VmError::Runtime(format!(
            "pool_create: scope must be a string (got {})",
            other.type_name()
        ))),
    }
}

fn parse_scope_id_override(opts: &BTreeMap<String, VmValue>) -> Option<String> {
    for key in ["scope_id", "pipeline_id", "run_id"] {
        if let Some(VmValue::String(text)) = opts.get(key) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn parse_stale_after_ms(opts: &BTreeMap<String, VmValue>) -> Result<i64, VmError> {
    match opts.get("stale_after_ms") {
        None | Some(VmValue::Nil) => Ok(DEFAULT_STALE_AFTER_MS),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n),
        Some(VmValue::Duration(n)) if *n >= 0 => Ok(*n),
        Some(other) => Err(VmError::Runtime(format!(
            "pool_create: stale_after_ms must be a non-negative int or duration (got {})",
            other.type_name()
        ))),
    }
}

fn parse_idempotency_key(opts: &BTreeMap<String, VmValue>) -> Result<Option<String>, VmError> {
    match opts.get("idempotency_key").or_else(|| opts.get("id")) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) if !text.trim().is_empty() => Ok(Some(text.to_string())),
        Some(VmValue::String(_)) => Err(VmError::Runtime(
            "pool.submit: idempotency_key cannot be empty".to_string(),
        )),
        Some(other) => Err(VmError::Runtime(format!(
            "pool.submit: idempotency_key must be a string (got {})",
            other.type_name()
        ))),
    }
}

/// Resolve the pipeline id for a pipeline-scope pool. Tries explicit
/// option overrides first, then falls back to the active runtime
/// context (workflow_id / run_id). Returns a friendly error matching the
/// channel scope contract when no pipeline id is in scope.
fn resolve_pipeline_scope_id(opts: &BTreeMap<String, VmValue>) -> Result<String, VmError> {
    if let Some(explicit) = parse_scope_id_override(opts) {
        return Ok(explicit);
    }
    if let Some(vm) = crate::vm::clone_async_builtin_child_vm() {
        if let VmValue::Dict(values) = crate::runtime_context::runtime_context_value(&vm) {
            for key in ["workflow_id", "run_id"] {
                if let Some(VmValue::String(text)) = values.get(key) {
                    if !text.is_empty() {
                        return Ok(text.to_string());
                    }
                }
            }
        }
    }
    Err(VmError::Runtime(
        "pool_create: pipeline-scope pool requires a pipeline_id (or active workflow/run \
         context); pass options.pipeline_id explicitly when creating from outside a pipeline"
            .to_string(),
    ))
}

fn parse_priority(opts: &BTreeMap<String, VmValue>) -> Result<i64, VmError> {
    match opts.get("priority") {
        None | Some(VmValue::Nil) => Ok(0),
        Some(VmValue::Int(n)) => Ok(*n),
        Some(other) => Err(VmError::Runtime(format!(
            "pool.submit: priority must be an int (got {})",
            other.type_name()
        ))),
    }
}

fn parse_key(opts: &BTreeMap<String, VmValue>) -> Result<Option<String>, VmError> {
    match opts.get("key") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(VmError::Runtime(format!(
            "pool.submit: key must be a string (got {})",
            other.type_name()
        ))),
    }
}

fn parse_submit_key(
    opts: &BTreeMap<String, VmValue>,
    queue_strategy: &QueueStrategy,
) -> Result<Option<String>, VmError> {
    if let Some(field) = queue_strategy.key_field() {
        match opts.get(field) {
            Some(VmValue::String(text)) => return Ok(Some(text.to_string())),
            Some(VmValue::Nil) | None => {}
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "pool.submit: {field} must be a string (got {})",
                    other.type_name()
                )));
            }
        }
    }
    parse_key(opts)
}

fn parse_queue_strategy(opts: &BTreeMap<String, VmValue>) -> Result<QueueStrategy, VmError> {
    let Some(value) = opts.get("queue") else {
        return Ok(QueueStrategy::Priority);
    };
    match value {
        VmValue::Nil => Ok(QueueStrategy::Priority),
        VmValue::String(text) => parse_queue_strategy_name(text),
        VmValue::Dict(map) => {
            let kind = map
                .get("kind")
                .or_else(|| map.get("strategy"))
                .map(VmValue::display)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VmError::Runtime("pool_create: queue strategy missing kind".to_string())
                })?;
            match kind.as_str() {
                "fair_round_robin" => {
                    let key_field = map
                        .get("key")
                        .or_else(|| map.get("key_field"))
                        .map(VmValue::display)
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "key".to_string());
                    Ok(QueueStrategy::FairRoundRobin { key_field })
                }
                _ => parse_queue_strategy_name(&kind),
            }
        }
        other => Err(VmError::Runtime(format!(
            "pool_create: queue must be a strategy dict or string (got {})",
            other.type_name()
        ))),
    }
}

fn parse_queue_strategy_name(name: &str) -> Result<QueueStrategy, VmError> {
    match name {
        "fifo" => Ok(QueueStrategy::Fifo),
        "priority" => Ok(QueueStrategy::Priority),
        "lifo" => Ok(QueueStrategy::Lifo),
        "fair_round_robin" => Ok(QueueStrategy::FairRoundRobin {
            key_field: "key".to_string(),
        }),
        other => Err(VmError::Runtime(format!(
            "pool_create: unknown queue strategy '{other}'"
        ))),
    }
}

fn parse_backpressure(opts: &BTreeMap<String, VmValue>) -> Result<BackpressureStrategy, VmError> {
    let Some(value) = opts.get("backpressure") else {
        return Ok(BackpressureStrategy::Unbounded);
    };
    match value {
        VmValue::Nil => Ok(BackpressureStrategy::Unbounded),
        VmValue::String(text) => parse_backpressure_name(text),
        VmValue::Dict(map) => {
            let kind = map
                .get("kind")
                .or_else(|| map.get("strategy"))
                .map(VmValue::display)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VmError::Runtime("pool_create: backpressure missing kind".to_string())
                })?;
            match kind.as_str() {
                "queue" => {
                    let max_depth = parse_positive_usize(
                        map.get("max_depth").or_else(|| map.get("capacity")),
                        "pool_create: backpressure.max_depth",
                    )?;
                    let on_full = parse_on_full_policy(map.get("on_full"))?;
                    Ok(BackpressureStrategy::Queue { max_depth, on_full })
                }
                "ring_buffer" => {
                    let capacity = parse_positive_usize(
                        map.get("capacity").or_else(|| map.get("max_depth")),
                        "pool_create: backpressure.capacity",
                    )?;
                    Ok(BackpressureStrategy::RingBuffer { capacity })
                }
                _ => parse_backpressure_name(&kind),
            }
        }
        other => Err(VmError::Runtime(format!(
            "pool_create: backpressure must be a policy dict or string (got {})",
            other.type_name()
        ))),
    }
}

fn parse_backpressure_name(name: &str) -> Result<BackpressureStrategy, VmError> {
    match name {
        "unbounded" => Ok(BackpressureStrategy::Unbounded),
        "fail_fast" => Ok(BackpressureStrategy::FailFast),
        other => Err(VmError::Runtime(format!(
            "pool_create: unknown backpressure policy '{other}'"
        ))),
    }
}

fn parse_positive_usize(value: Option<&VmValue>, name: &str) -> Result<usize, VmError> {
    match value {
        Some(VmValue::Int(n)) if *n >= 1 => Ok(*n as usize),
        Some(VmValue::Int(_)) => Err(VmError::Runtime(format!("{name} must be >= 1"))),
        Some(other) => Err(VmError::Runtime(format!(
            "{name} must be an int (got {})",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{name} is required"))),
    }
}

fn parse_on_full_policy(value: Option<&VmValue>) -> Result<QueueOnFullPolicy, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(QueueOnFullPolicy::BlockSubmitter),
        Some(VmValue::String(text)) => match text.as_ref() {
            "block_submitter" => Ok(QueueOnFullPolicy::BlockSubmitter),
            "drop_oldest" => Ok(QueueOnFullPolicy::DropOldest),
            "drop_newest" => Ok(QueueOnFullPolicy::DropNewest),
            "fail_submitter" => Ok(QueueOnFullPolicy::FailSubmitter),
            other => Err(VmError::Runtime(format!(
                "pool_create: unknown backpressure on_full policy '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "pool_create: backpressure.on_full must be a string (got {})",
            other.type_name()
        ))),
    }
}

/// Public surface: snapshot dict mirroring the structure returned by
/// `pool.snapshot()`. Pulled into its own helper so `__pool_create`,
/// `__pool_snapshot`, `__pool_list`, and `__pool_get` stay byte-for-byte
/// consistent.
fn pool_snapshot_value(pool: &PoolEntry) -> VmValue {
    let mut tasks: Vec<VmValue> = pool
        .tasks
        .values()
        .map(|task| task_snapshot_value(&task.borrow()))
        .collect();
    tasks.sort_by_key(task_sort_key);
    let queued: i64 = pool.queue.len() as i64;
    let active: i64 = pool.active.len() as i64;
    let mut completed: i64 = 0;
    let mut failed: i64 = 0;
    let mut rejected: i64 = 0;
    for task in pool.tasks.values() {
        match task.borrow().status {
            TaskStatus::Completed => completed += 1,
            TaskStatus::Failed => failed += 1,
            TaskStatus::Rejected => rejected += 1,
            _ => {}
        }
    }
    let mut snapshot = BTreeMap::new();
    snapshot.insert("_type".to_string(), VmValue::String(Rc::from(POOL_TYPE)));
    snapshot.insert(
        "id".to_string(),
        VmValue::String(Rc::from(pool.id.as_str())),
    );
    snapshot.insert(
        "name".to_string(),
        VmValue::String(Rc::from(pool.name.as_str())),
    );
    snapshot.insert(
        "max_concurrent".to_string(),
        VmValue::Int(pool.max_concurrent as i64),
    );
    snapshot.insert(
        "created_at".to_string(),
        VmValue::String(Rc::from(pool.created_at.as_str())),
    );
    snapshot.insert("active".to_string(), VmValue::Int(active));
    snapshot.insert("queued".to_string(), VmValue::Int(queued));
    snapshot.insert("completed".to_string(), VmValue::Int(completed));
    snapshot.insert("failed".to_string(), VmValue::Int(failed));
    snapshot.insert("rejected".to_string(), VmValue::Int(rejected));
    snapshot.insert("total".to_string(), VmValue::Int(pool.tasks.len() as i64));
    snapshot.insert(
        "queue_strategy".to_string(),
        VmValue::String(Rc::from(pool.queue_strategy.name())),
    );
    snapshot.insert(
        "backpressure".to_string(),
        backpressure_snapshot_value(&pool.backpressure),
    );
    snapshot.insert(
        "blocked_submitters".to_string(),
        VmValue::Int(pool.space_waiters.len() as i64),
    );
    snapshot.insert("tasks".to_string(), VmValue::List(Rc::new(tasks)));
    snapshot.insert(
        "scope".to_string(),
        VmValue::String(Rc::from(pool.scope.as_str())),
    );
    if !pool.scope_id.is_empty() {
        snapshot.insert(
            "scope_id".to_string(),
            VmValue::String(Rc::from(pool.scope_id.as_str())),
        );
    }
    snapshot.insert("durable".to_string(), VmValue::Bool(pool.store.is_some()));
    snapshot.insert(
        "stale_after_ms".to_string(),
        VmValue::Int(pool.stale_after_ms),
    );
    if !pool.config.is_empty() {
        snapshot.insert(
            "config".to_string(),
            VmValue::Dict(Rc::new(pool.config.clone())),
        );
    }
    VmValue::Dict(Rc::new(snapshot))
}

fn backpressure_snapshot_value(backpressure: &BackpressureStrategy) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("backpressure")),
    );
    value.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(backpressure.name())),
    );
    if let Some(max_depth) = backpressure.max_depth() {
        value.insert("max_depth".to_string(), VmValue::Int(max_depth as i64));
    }
    if let BackpressureStrategy::Queue { on_full, .. } = backpressure {
        value.insert(
            "on_full".to_string(),
            VmValue::String(Rc::from(on_full.as_str())),
        );
    }
    VmValue::Dict(Rc::new(value))
}

fn task_sort_key(task: &VmValue) -> String {
    match task {
        VmValue::Dict(map) => map
            .get("submitted_at")
            .map(|value| value.display())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn task_snapshot_value(task: &TaskState) -> VmValue {
    let mut entry = BTreeMap::new();
    entry.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(POOL_TASK_TYPE)),
    );
    entry.insert(
        "id".to_string(),
        VmValue::String(Rc::from(task.id.as_str())),
    );
    entry.insert(
        "pool_id".to_string(),
        VmValue::String(Rc::from(task.pool_id.as_str())),
    );
    entry.insert(
        "pool".to_string(),
        VmValue::String(Rc::from(task.pool_name.as_str())),
    );
    entry.insert(
        "status".to_string(),
        VmValue::String(Rc::from(task.status.as_str())),
    );
    entry.insert("priority".to_string(), VmValue::Int(task.priority));
    entry.insert(
        "submitted_at".to_string(),
        VmValue::String(Rc::from(task.submitted_at.as_str())),
    );
    if let Some(key) = &task.key {
        entry.insert("key".to_string(), VmValue::String(Rc::from(key.as_str())));
    }
    if let Some(started_at) = &task.started_at {
        entry.insert(
            "started_at".to_string(),
            VmValue::String(Rc::from(started_at.as_str())),
        );
    }
    if let Some(finished_at) = &task.finished_at {
        entry.insert(
            "finished_at".to_string(),
            VmValue::String(Rc::from(finished_at.as_str())),
        );
    }
    if let Some(result) = &task.result {
        entry.insert("result".to_string(), result.clone());
    }
    if let Some(error) = &task.error {
        entry.insert(
            "error".to_string(),
            VmValue::String(Rc::from(error.as_str())),
        );
    }
    if let Some(reason) = &task.rejection_reason {
        entry.insert(
            "rejection_reason".to_string(),
            VmValue::String(Rc::from(reason.as_str())),
        );
    }
    if let Some(policy) = &task.rejection_policy {
        entry.insert(
            "rejection_policy".to_string(),
            VmValue::String(Rc::from(policy.as_str())),
        );
    }
    VmValue::Dict(Rc::new(entry))
}

fn task_handle_value(task: &TaskState) -> VmValue {
    let mut handle = BTreeMap::new();
    handle.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(POOL_TASK_TYPE)),
    );
    handle.insert(
        "id".to_string(),
        VmValue::String(Rc::from(task.id.as_str())),
    );
    handle.insert(
        "pool_id".to_string(),
        VmValue::String(Rc::from(task.pool_id.as_str())),
    );
    handle.insert(
        "pool".to_string(),
        VmValue::String(Rc::from(task.pool_name.as_str())),
    );
    handle.insert(
        "submitted_at".to_string(),
        VmValue::String(Rc::from(task.submitted_at.as_str())),
    );
    handle.insert(
        "status".to_string(),
        VmValue::String(Rc::from(task.status.as_str())),
    );
    if let Some(key) = &task.key {
        handle.insert("key".to_string(), VmValue::String(Rc::from(key.as_str())));
    }
    if let Some(error) = &task.error {
        handle.insert(
            "error".to_string(),
            VmValue::String(Rc::from(error.as_str())),
        );
    }
    if let Some(reason) = &task.rejection_reason {
        handle.insert(
            "rejection_reason".to_string(),
            VmValue::String(Rc::from(reason.as_str())),
        );
    }
    if let Some(policy) = &task.rejection_policy {
        handle.insert(
            "rejection_policy".to_string(),
            VmValue::String(Rc::from(policy.as_str())),
        );
    }
    VmValue::Dict(Rc::new(handle))
}

fn ordered_pool_config(opts: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
    let mut config = BTreeMap::new();
    for key in ["queue", "backpressure", "priority"] {
        if let Some(value) = opts.get(key) {
            config.insert(key.to_string(), value.clone());
        }
    }
    config
}

/// Create a named agent pool and register it in the local pool registry.
#[harn_builtin(
    sig = "__pool_create(options?: dict|nil) -> dict",
    category = "pool",
    runtime_only = true
)]
fn pool_create_sync(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let opts = parse_options(args.first(), "pool_create")?;
    let name = parse_name(&opts).unwrap_or_else(|| format!("pool_{}", uuid::Uuid::now_v7()));
    if let Some(existing) = POOL_NAMES.with(|names| names.borrow().get(&name).cloned()) {
        return Err(VmError::Runtime(format!(
            "pool_create: pool '{name}' already exists (id={existing}); use pool_get to reuse"
        )));
    }
    let max_concurrent = parse_max_concurrent(&opts)?;
    let queue_strategy = parse_queue_strategy(&opts)?;
    let backpressure = parse_backpressure(&opts)?;
    let scope = parse_scope(&opts)?;
    let stale_after_ms = parse_stale_after_ms(&opts)?;

    if scope.is_host_routed() {
        return Err(VmError::Runtime(format!(
            "pool_create: scope '{}' is host-routed (harn-cloud, see harn-cloud#306) and \
             not wired in the in-process runtime. Use scope: \"session\" or \
             scope: \"pipeline\" until the host capability ships",
            scope.as_str()
        )));
    }

    let (id, scope_id, store, persisted) = match scope {
        PoolScope::Session => (next_pool_id(), String::new(), None, None),
        PoolScope::Pipeline => {
            let pipeline_id = resolve_pipeline_scope_id(&opts)?;
            let id = deterministic_pool_id(scope, &pipeline_id, &name);
            let dir_override = parse_durable_dir(&opts)?;
            let path = pipeline_pool_file_path(dir_override.as_deref(), &pipeline_id, &name);
            let store = PoolDurableStore::new(path);
            let persisted = store.load()?;
            (
                id,
                pipeline_id,
                Some(Rc::new(RefCell::new(store))),
                persisted,
            )
        }
        PoolScope::Tenant | PoolScope::Org => unreachable!("host-routed scope returned above"),
    };

    // Compute the live submit counter from any persisted state so newly
    // submitted tasks always observe a strictly-increasing seq across
    // restarts.
    let submit_counter = persisted
        .as_ref()
        .map(|state| state.meta.submit_counter)
        .unwrap_or(0);

    let entry = Rc::new(RefCell::new(PoolEntry {
        id: id.clone(),
        name: name.clone(),
        max_concurrent,
        created_at: uuid::Uuid::now_v7().to_string(),
        submit_counter,
        queue: VecDeque::new(),
        queue_strategy,
        backpressure,
        round_robin_after: None,
        active: HashMap::new(),
        tasks: BTreeMap::new(),
        space_waiters: Vec::new(),
        config: ordered_pool_config(&opts),
        scope,
        scope_id,
        idempotency_index: HashMap::new(),
        stale_after_ms,
        store: store.clone(),
    }));

    // Hydrate persisted tasks BEFORE registering, so any reader that
    // races a `pool_get` on the same registry sees a populated pool.
    if let (Some(persisted), Some(store_ref)) = (persisted, store.clone()) {
        rehydrate_persisted_state(&entry, &store_ref, persisted, stale_after_ms)?;
    } else if let Some(store_ref) = store {
        // Fresh pipeline-scope pool: stamp the header so reloads find a
        // well-formed log even if no tasks have been submitted yet.
        let meta = persisted_meta_from_entry(&entry.borrow());
        store_ref.borrow().compact(&meta, &[])?;
    }

    POOLS.with(|pools| pools.borrow_mut().insert(id.clone(), entry.clone()));
    POOL_NAMES.with(|names| names.borrow_mut().insert(name, id.clone()));
    let snapshot = pool_snapshot_value(&entry.borrow());
    Ok(snapshot)
}

fn pipeline_pool_file_path(dir_override: Option<&str>, pipeline_id: &str, name: &str) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(pipeline_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize().to_hex();
    let safe_pipeline = crate::event_log::sanitize_topic_component(pipeline_id);
    let safe_name = crate::event_log::sanitize_topic_component(name);
    let root = match dir_override {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(PIPELINE_POOLS_ROOT),
    };
    root.join(format!(
        "{safe_pipeline}__{safe_name}__{}.jsonl",
        &digest.as_str()[..16]
    ))
}

fn parse_durable_dir(opts: &BTreeMap<String, VmValue>) -> Result<Option<String>, VmError> {
    match opts.get("dir") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) if !text.trim().is_empty() => Ok(Some(text.to_string())),
        Some(VmValue::String(_)) => Err(VmError::Runtime(
            "pool_create: dir cannot be empty".to_string(),
        )),
        Some(other) => Err(VmError::Runtime(format!(
            "pool_create: dir must be a string (got {})",
            other.type_name()
        ))),
    }
}

/// Look up a pool by name or id; returns nil when missing.
#[harn_builtin(
    sig = "__pool_get(name_or_id: string|dict) -> dict|nil",
    category = "pool",
    runtime_only = true
)]
fn pool_get_sync(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args
        .first()
        .map(VmValue::display)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime("pool_get: name is required".to_string()))?;
    let pool_id = POOL_NAMES.with(|names| names.borrow().get(&key).cloned());
    let id = match pool_id {
        Some(id) => id,
        None => {
            if POOLS.with(|pools| pools.borrow().contains_key(&key)) {
                key
            } else {
                return Ok(VmValue::Nil);
            }
        }
    };
    let entry = lookup_pool(&id)?;
    let snapshot = pool_snapshot_value(&entry.borrow());
    Ok(snapshot)
}

/// List every pool registered in the local pool registry.
#[harn_builtin(sig = "__pool_list() -> list", category = "pool", runtime_only = true)]
fn pool_list_sync(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut entries: Vec<Rc<RefCell<PoolEntry>>> =
        POOLS.with(|pools| pools.borrow().values().cloned().collect());
    entries.sort_by(|a, b| a.borrow().created_at.cmp(&b.borrow().created_at));
    Ok(VmValue::List(Rc::new(
        entries
            .iter()
            .map(|entry| pool_snapshot_value(&entry.borrow()))
            .collect(),
    )))
}

/// Return active + queued task count for a pool.
#[harn_builtin(
    sig = "__pool_size(pool: string|dict) -> int",
    category = "pool",
    runtime_only = true
)]
fn pool_size_sync(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let pool_id = pool_id_from_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("pool.size: pool handle is required".to_string()))?,
        "pool.size",
    )?;
    let entry = lookup_pool(&pool_id)?;
    let entry = entry.borrow();
    Ok(VmValue::Int(
        (entry.active.len() + entry.queue.len()) as i64,
    ))
}

/// Test-only entrypoint that drops the in-process pool registry so a
/// subsequent `pool_create({scope: "pipeline", ...})` reloads its state
/// from the on-disk JSONL artifact. Conformance fixtures use this to
/// simulate "kill process → restart" without actually forking a new
/// process. Returns `nil`.
/// Drop the in-process pool registry; pipeline-scope pools reload from disk on next pool_create.
#[harn_builtin(
    sig = "__pool_simulate_restart() -> nil",
    category = "pool",
    runtime_only = true
)]
fn pool_reload_sync(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    reset_pool_state();
    Ok(VmValue::Nil)
}

/// Return the full pool snapshot for inspection.
#[harn_builtin(
    sig = "__pool_snapshot(pool: string|dict) -> dict",
    category = "pool",
    runtime_only = true
)]
fn pool_snapshot_sync(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let pool_id = pool_id_from_value(
        args.first().ok_or_else(|| {
            VmError::Runtime("pool.snapshot: pool handle is required".to_string())
        })?,
        "pool.snapshot",
    )?;
    let entry = lookup_pool(&pool_id)?;
    let snapshot = pool_snapshot_value(&entry.borrow());
    Ok(snapshot)
}

/// Submit a closure to a pool; spawns when a slot is free, otherwise queues.
#[harn_builtin(
    sig = "__pool_submit(pool: string|dict, closure: closure, options?: dict|nil) -> dict",
    kind = "async",
    category = "pool",
    runtime_only = true
)]
async fn pool_submit_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool_id = pool_id_from_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("pool.submit: pool handle is required".to_string()))?,
        "pool.submit",
    )?;
    let closure = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "pool.submit: second argument must be a closure (got {})",
                other.type_name()
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "pool.submit: closure is required".to_string(),
            ));
        }
    };
    let opts = parse_options(args.get(2), "pool.submit")?;
    let priority = parse_priority(&opts)?;
    let idempotency_key = parse_idempotency_key(&opts)?;

    let entry = lookup_pool(&pool_id)?;
    let key = {
        let pool = entry.borrow();
        parse_submit_key(&opts, &pool.queue_strategy)?
    };

    let state = submit_to_pool_entry(&entry, closure, key, priority, idempotency_key).await?;
    let handle = task_handle_value(&state.borrow());
    Ok(handle)
}

/// Resolve a registered pool by name (or id) without producing a snapshot.
/// Returns `None` when no pool matches. Used by the trigger dispatcher's
/// SpawnToPool handler (#1889) so it can route inbound events into named
/// pools without going through the Harn-level builtin surface.
fn lookup_pool_by_name_or_id(name_or_id: &str) -> Option<Rc<RefCell<PoolEntry>>> {
    let id = POOL_NAMES
        .with(|names| names.borrow().get(name_or_id).cloned())
        .or_else(|| {
            POOLS.with(|pools| {
                pools
                    .borrow()
                    .contains_key(name_or_id)
                    .then(|| name_or_id.to_string())
            })
        })?;
    POOLS.with(|pools| pools.borrow().get(&id).cloned())
}

/// Public submission outcome returned by [`submit_closure_to_named_pool`].
/// The dispatcher inspects this to distinguish accepted-and-running tasks
/// from accepted-but-dropped (drop_newest) tasks so it can emit the right
/// audit and dispatch outcome.
pub struct PoolSubmitOutcome {
    /// Stable pool id (e.g. `pool_018f...`). Pair with `task_id` to build a
    /// `pool_wait` handle on the Harn side.
    pub pool_id: String,
    /// Human-friendly pool name (i.e. the registration argument).
    pub pool_name: String,
    /// Assigned task id; also stable across the lifetime of the pool.
    pub task_id: String,
    /// Status the task is in once `submit_closure_to_named_pool` returns:
    /// `"queued"`, `"running"`, or `"rejected"` (for the `drop_newest` case
    /// when the newly-submitted task is the one that gets evicted).
    pub status: &'static str,
    /// Set when `status == "rejected"` so the dispatcher can surface the
    /// pool's policy-driven drop reason in audit + action graph metadata.
    pub rejection_reason: Option<String>,
}

/// Submit a closure to a named pool from a non-Harn caller (e.g. the trigger
/// dispatcher). Honors the pool's queue strategy + backpressure policy in the
/// same way as `pool.submit` from Harn code. Returns an error when the pool
/// does not exist or the policy fails the submitter (fail_fast /
/// fail_submitter); awaits when the policy blocks the submitter.
pub async fn submit_closure_to_named_pool(
    pool_name: &str,
    closure: Rc<VmClosure>,
    priority: i64,
    key: Option<String>,
) -> Result<PoolSubmitOutcome, VmError> {
    let entry = lookup_pool_by_name_or_id(pool_name).ok_or_else(|| {
        VmError::Runtime(format!(
            "pool: pool '{pool_name}' not found; create it with pool_create first"
        ))
    })?;
    // Trigger-dispatcher pool submissions don't carry a caller-supplied
    // idempotency key today; the dispatcher's own dedupe runs upstream.
    let state = submit_to_pool_entry(&entry, closure, key, priority, None).await?;
    let task = state.borrow();
    Ok(PoolSubmitOutcome {
        pool_id: task.pool_id.clone(),
        pool_name: task.pool_name.clone(),
        task_id: task.id.clone(),
        status: task.status.as_str(),
        rejection_reason: task.rejection_reason.clone(),
    })
}

/// Shared inner submission loop used by both `pool.submit` (Harn builtin)
/// and `submit_closure_to_named_pool` (dispatcher). Honors the pool's
/// backpressure policy: blocks on `BlockSubmitter`, drops oldest/newest in
/// the corresponding policies, and fails on `FailFast`/`FailSubmitter`.
async fn submit_to_pool_entry(
    entry: &Rc<RefCell<PoolEntry>>,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    idempotency_key: Option<String>,
) -> Result<Rc<RefCell<TaskState>>, VmError> {
    // Idempotency short-circuit: if the caller previously submitted with
    // the same key, return the existing task (terminal snapshot or
    // pending handle). Mirrors the durable channel `id`-based dedupe
    // contract.
    if let Some(idem) = idempotency_key.as_ref() {
        if let Some(existing) = lookup_idempotency_match(entry, idem) {
            return Ok(existing);
        }
    }
    let submitted_by = current_submitter();
    let (pool_id_for_span, pool_name_for_span) = {
        let pool = entry.borrow();
        (pool.id.clone(), pool.name.clone())
    };
    loop {
        // Open the PoolSubmit span just-in-time for each attempt loop:
        // a single submitter that is blocked under `block_submitter`
        // backpressure may try several times before placing the task in
        // the queue. We span every attempt so the trace records the
        // submitter's queue-wait dwell explicitly.
        let mut submit_span = PoolSpanGuard::start(
            crate::tracing::SpanKind::PoolSubmit,
            format!("pool.submit {pool_name_for_span}"),
            Vec::new(),
        );
        submit_span.set_metadata("pool", serde_json::json!(pool_name_for_span));
        submit_span.set_metadata("pool_id", serde_json::json!(pool_id_for_span));
        submit_span.set_metadata("priority", serde_json::json!(priority));
        if let Some(key) = &key {
            submit_span.set_metadata("key", serde_json::json!(key));
        }
        if let Some(idem) = &idempotency_key {
            submit_span.set_metadata("idempotency_key", serde_json::json!(idem));
        }
        let submit_link = submit_span.link();

        let attempt = {
            let mut pool = entry.borrow_mut();
            submit_or_wait(
                &mut pool,
                closure.clone(),
                key.clone(),
                priority,
                idempotency_key.clone(),
                submit_link.clone(),
                submitted_by.clone(),
            )
        };
        match attempt {
            SubmitAttempt::Submitted { task, audits } => {
                {
                    let task_ref = task.borrow();
                    submit_span.set_metadata("task_id", serde_json::json!(task_ref.id));
                    submit_span.set_metadata("status", serde_json::json!(task_ref.status.as_str()));
                }
                let receipt = {
                    let task_ref = task.borrow();
                    pool_submit_receipt(&entry.borrow(), &task_ref)
                };
                emit_pool_submit_receipt(receipt).await;
                for audit in audits {
                    emit_pool_drop(audit).await;
                }
                submit_span.end();
                dispatch_ready(entry);
                return Ok(task);
            }
            SubmitAttempt::Wait(receiver) => {
                submit_span.set_metadata("blocked", serde_json::json!(true));
                submit_span.end();
                let _ = receiver.await;
            }
            SubmitAttempt::Fail(error) => {
                submit_span.set_metadata("error", serde_json::json!(error.to_string()));
                submit_span.end();
                return Err(error);
            }
        }
    }
}

/// Resolve the best-available identifier for who submitted a task. Mirrors
/// the runtime-context lookups in `runtime_context_value` so the
/// `PoolSubmitReceipt.submitted_by` field aligns with the rest of the
/// observability stack (workflow span, agent session, mutation session,
/// active worker). Falls back to `"user"` when no better identifier is
/// in scope (e.g. submission from a CLI smoke test).
fn current_submitter() -> String {
    if let Some(vm) = crate::vm::clone_async_builtin_child_vm() {
        if let VmValue::Dict(values) = crate::runtime_context::runtime_context_value(&vm) {
            for key in [
                "agent_session_id",
                "worker_id",
                "workflow_id",
                "run_id",
                "task_id",
            ] {
                if let Some(VmValue::String(text)) = values.get(key) {
                    if !text.is_empty() {
                        return text.to_string();
                    }
                }
            }
        }
    }
    "user".to_string()
}

fn lookup_idempotency_match(
    entry: &Rc<RefCell<PoolEntry>>,
    idempotency_key: &str,
) -> Option<Rc<RefCell<TaskState>>> {
    let pool = entry.borrow();
    let task_id = pool.idempotency_index.get(idempotency_key)?.clone();
    pool.tasks.get(&task_id).cloned()
}

enum SubmitAttempt {
    Submitted {
        task: Rc<RefCell<TaskState>>,
        audits: Vec<PoolDropAudit>,
    },
    Wait(tokio::sync::oneshot::Receiver<()>),
    Fail(VmError),
}

#[allow(clippy::too_many_arguments)]
fn submit_or_wait(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    idempotency_key: Option<String>,
    submit_span_link: Option<crate::tracing::SpanLink>,
    submitted_by: String,
) -> SubmitAttempt {
    if can_accept_now(pool) {
        let (state, pending) = create_pending_task(
            pool,
            closure,
            key,
            priority,
            idempotency_key,
            submit_span_link,
            submitted_by,
        );
        enqueue_task(pool, pending);
        return SubmitAttempt::Submitted {
            task: state,
            audits: Vec::new(),
        };
    }

    match pool.backpressure.clone() {
        BackpressureStrategy::Unbounded => {
            let (state, pending) = create_pending_task(
                pool,
                closure,
                key,
                priority,
                idempotency_key,
                submit_span_link,
                submitted_by,
            );
            enqueue_task(pool, pending);
            SubmitAttempt::Submitted {
                task: state,
                audits: Vec::new(),
            }
        }
        BackpressureStrategy::Queue {
            max_depth: _,
            on_full: QueueOnFullPolicy::BlockSubmitter,
        } => {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            pool.space_waiters.push(sender);
            SubmitAttempt::Wait(receiver)
        }
        BackpressureStrategy::Queue {
            max_depth,
            on_full: QueueOnFullPolicy::DropOldest,
        } => submit_with_oldest_drop(
            pool,
            closure,
            key,
            priority,
            idempotency_key,
            submit_span_link,
            submitted_by,
            "drop_oldest_queue_full",
            QueueOnFullPolicy::DropOldest.as_str(),
            Some(max_depth),
        ),
        BackpressureStrategy::Queue {
            max_depth,
            on_full: QueueOnFullPolicy::DropNewest,
        } => submit_with_newest_drop(
            pool,
            closure,
            key,
            priority,
            idempotency_key,
            submit_span_link,
            submitted_by,
            "drop_newest_queue_full",
            QueueOnFullPolicy::DropNewest.as_str(),
            Some(max_depth),
        ),
        BackpressureStrategy::Queue {
            on_full: QueueOnFullPolicy::FailSubmitter,
            ..
        } => SubmitAttempt::Fail(policy_error(
            Code::PoolBackpressureFull,
            format!(
                "pool.submit: pool '{}' queue is full under fail_submitter backpressure",
                pool.name
            ),
        )),
        BackpressureStrategy::FailFast => SubmitAttempt::Fail(policy_error(
            Code::PoolFailFastFull,
            format!(
                "pool.submit: pool '{}' has no immediate capacity under fail_fast backpressure",
                pool.name
            ),
        )),
        BackpressureStrategy::RingBuffer { capacity } => submit_with_oldest_drop(
            pool,
            closure,
            key,
            priority,
            idempotency_key,
            submit_span_link,
            submitted_by,
            "ring_buffer_drop_oldest",
            "ring_buffer",
            Some(capacity),
        ),
    }
}

fn can_accept_now(pool: &PoolEntry) -> bool {
    if pool.active.len() < pool.max_concurrent && pool.queue.is_empty() {
        return true;
    }
    match &pool.backpressure {
        BackpressureStrategy::Unbounded => true,
        BackpressureStrategy::Queue { max_depth, .. } => pool.queue.len() < *max_depth,
        BackpressureStrategy::FailFast => false,
        BackpressureStrategy::RingBuffer { capacity } => pool.queue.len() < *capacity,
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_with_oldest_drop(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    idempotency_key: Option<String>,
    submit_span_link: Option<crate::tracing::SpanLink>,
    submitted_by: String,
    reason: &str,
    policy: &str,
    max_depth: Option<usize>,
) -> SubmitAttempt {
    let queue_depth = pool.queue.len();
    let (state, pending) = create_pending_task(
        pool,
        closure,
        key,
        priority,
        idempotency_key,
        submit_span_link,
        submitted_by,
    );
    let replacement_task_id = state.borrow().id.clone();
    let mut audits = Vec::new();
    if let Some(dropped) = pool.queue.pop_front() {
        audits.push(reject_pending_task(
            pool,
            dropped,
            Some(replacement_task_id.as_str()),
            reason,
            policy,
            queue_depth,
            max_depth,
        ));
    }
    enqueue_task(pool, pending);
    SubmitAttempt::Submitted {
        task: state,
        audits,
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_with_newest_drop(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    idempotency_key: Option<String>,
    submit_span_link: Option<crate::tracing::SpanLink>,
    submitted_by: String,
    reason: &str,
    policy: &str,
    max_depth: Option<usize>,
) -> SubmitAttempt {
    let queue_depth = pool.queue.len();
    let (state, _pending) = create_pending_task(
        pool,
        closure,
        key,
        priority,
        idempotency_key,
        submit_span_link,
        submitted_by,
    );
    let task_id = state.borrow().id.clone();
    let waiters = reject_task_state(&state, reason, policy);
    wake_task_waiters(waiters);
    persist_task_if_durable(pool, &state.borrow());
    let audit = pool_drop_audit(pool, &task_id, None, reason, policy, queue_depth, max_depth);
    SubmitAttempt::Submitted {
        task: state,
        audits: vec![audit],
    }
}

fn create_pending_task(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    idempotency_key: Option<String>,
    submit_span_link: Option<crate::tracing::SpanLink>,
    submitted_by: String,
) -> (Rc<RefCell<TaskState>>, PendingTask) {
    pool.submit_counter += 1;
    let seq = pool.submit_counter;
    let task_id = next_task_id(pool);
    let now_ms = now_ms_for_pool();
    let state = Rc::new(RefCell::new(TaskState {
        id: task_id.clone(),
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        key: key.clone(),
        priority,
        status: TaskStatus::Queued,
        submitted_at: uuid::Uuid::now_v7().to_string(),
        started_at: None,
        finished_at: None,
        result: None,
        error: None,
        rejection_reason: None,
        rejection_policy: None,
        idempotency_key: idempotency_key.clone(),
        heartbeat_at_ms: now_ms,
        submitted_at_ms: now_ms,
        submit_span_link,
        submitted_by,
        waiters: Vec::new(),
    }));
    if let Some(idem) = &idempotency_key {
        pool.idempotency_index.insert(idem.clone(), task_id.clone());
    }
    pool.tasks.insert(task_id.clone(), state.clone());
    persist_task_if_durable(pool, &state.borrow());
    let pending = PendingTask {
        task_id,
        closure,
        state: state.clone(),
        priority,
        key,
        seq,
        // Capture the execution context now, synchronously, while the submit
        // builtin still holds the task_local async-builtin context. Dispatch
        // happens later from a context-free callback. See harn#2667.
        context_vm: crate::vm::clone_async_builtin_child_vm().map(|vm| Rc::new(RefCell::new(vm))),
    };
    (state, pending)
}

/// Return the wall-clock millisecond timestamp used for task heartbeats.
/// Routes through `clock_mock` so test fixtures can deterministically
/// drive stale-in-flight detection on reload.
fn now_ms_for_pool() -> i64 {
    crate::clock_mock::now_ms()
}

fn persist_task_if_durable(pool: &PoolEntry, state: &TaskState) {
    let Some(store) = pool.store.as_ref() else {
        return;
    };
    let record = PoolRecord::Task {
        task: persisted_task_from_state(state),
    };
    if let Err(err) = store.borrow().append(&record) {
        // Best-effort: log + swallow rather than poisoning the submit
        // path. A genuine fsync failure surfaces on the next compact.
        let _ = err;
    }
}

fn persisted_meta_from_entry(pool: &PoolEntry) -> PersistedPoolMeta {
    PersistedPoolMeta {
        id: pool.id.clone(),
        name: pool.name.clone(),
        scope: pool.scope.as_str().to_string(),
        scope_id: pool.scope_id.clone(),
        max_concurrent: pool.max_concurrent,
        created_at: pool.created_at.clone(),
        submit_counter: pool.submit_counter,
    }
}

fn persisted_task_from_state(state: &TaskState) -> PersistedTask {
    PersistedTask {
        id: state.id.clone(),
        pool_id: state.pool_id.clone(),
        pool_name: state.pool_name.clone(),
        key: state.key.clone(),
        priority: state.priority,
        status: state.status.as_str().to_string(),
        submitted_at: state.submitted_at.clone(),
        started_at: state.started_at.clone(),
        finished_at: state.finished_at.clone(),
        error: state.error.clone(),
        rejection_reason: state.rejection_reason.clone(),
        rejection_policy: state.rejection_policy.clone(),
        idempotency_key: state.idempotency_key.clone(),
        result_display: state.result.as_ref().map(VmValue::display),
        heartbeat_at_ms: state.heartbeat_at_ms,
        seq: 0,
    }
}

/// Rehydrate a fresh `PoolEntry` from a durable JSONL log. Terminal
/// tasks are restored as-is so reads through `pool.snapshot()` / `pool_get`
/// keep the same history. Tasks that were `Queued` or `Running` at the
/// last checkpoint become "orphaned" markers: their `status` flips to
/// `failed` with a stale-restart message, and their `idempotency_key`
/// (when present) stays available so a fresh submit can re-execute them
/// without violating idempotency. The pool file is compacted in place
/// so the next process restart sees a tidy log.
fn rehydrate_persisted_state(
    entry: &Rc<RefCell<PoolEntry>>,
    store: &Rc<RefCell<PoolDurableStore>>,
    persisted: PersistedPoolState,
    stale_after_ms: i64,
) -> Result<(), VmError> {
    let now = now_ms_for_pool();
    let mut idempotency_index: HashMap<String, String> = HashMap::new();
    let mut tasks: BTreeMap<String, Rc<RefCell<TaskState>>> = BTreeMap::new();
    let mut rehydrated_persisted: Vec<PersistedTask> = Vec::new();

    {
        let mut pool = entry.borrow_mut();
        for (_, task) in persisted.tasks {
            let live = task_state_from_persisted(&pool, &task, now, stale_after_ms);
            let (task_id, idem) = {
                let borrowed = live.borrow();
                rehydrated_persisted.push(persisted_task_from_state(&borrowed));
                (borrowed.id.clone(), borrowed.idempotency_key.clone())
            };
            if let Some(idem) = idem {
                idempotency_index.insert(idem, task_id.clone());
            }
            tasks.insert(task_id, live);
        }
        pool.tasks = tasks;
        pool.idempotency_index = idempotency_index;
    }

    // Rewrite the file with the compacted snapshot. Atomic so a crash
    // mid-rewrite leaves the previous log intact.
    let meta = persisted_meta_from_entry(&entry.borrow());
    store.borrow().compact(&meta, &rehydrated_persisted)?;
    Ok(())
}

fn task_state_from_persisted(
    pool: &PoolEntry,
    persisted: &PersistedTask,
    now: i64,
    stale_after_ms: i64,
) -> Rc<RefCell<TaskState>> {
    let status = match persisted.status.as_str() {
        "queued" | "running" => {
            // Both `queued` and `running` survive a crash as "stale"
            // when the heartbeat is sufficiently old. They convert to
            // `Failed` with a stale-restart marker so re-submission by
            // idempotency_key gets a fresh task and existing handles
            // observe a terminal state.
            if now.saturating_sub(persisted.heartbeat_at_ms) >= stale_after_ms {
                TaskStatus::Failed
            } else {
                // Within the freshness window: treat as failed too,
                // because the process owning the in-flight execution is
                // gone. The stale_after_ms knob is reserved for callers
                // that want a deferred sweep — for now we always fail
                // on reload so the durable store never re-enqueues a
                // closure we cannot resurrect.
                TaskStatus::Failed
            }
        }
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "rejected" => TaskStatus::Rejected,
        _ => TaskStatus::Failed,
    };

    let finished_at = persisted
        .finished_at
        .clone()
        .or_else(|| Some(uuid::Uuid::now_v7().to_string()));
    let (error, rejection_reason, rejection_policy) = match status {
        TaskStatus::Failed if persisted.status != "failed" => (
            Some(format!(
                "pool: task {} reloaded as stale after process restart",
                persisted.id
            )),
            None,
            None,
        ),
        _ => (
            persisted.error.clone(),
            persisted.rejection_reason.clone(),
            persisted.rejection_policy.clone(),
        ),
    };
    let result = persisted
        .result_display
        .as_ref()
        .map(|text| VmValue::String(Rc::from(text.as_str())));

    Rc::new(RefCell::new(TaskState {
        id: persisted.id.clone(),
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        key: persisted.key.clone(),
        priority: persisted.priority,
        status,
        submitted_at: persisted.submitted_at.clone(),
        started_at: persisted.started_at.clone(),
        finished_at,
        result,
        error,
        rejection_reason,
        rejection_policy,
        idempotency_key: persisted.idempotency_key.clone(),
        heartbeat_at_ms: persisted.heartbeat_at_ms,
        // Rehydrated tasks predate the live VM's tracing context: the
        // submit span belongs to the prior process. Treat as no span;
        // the dequeue receipt's `queued_for_ms` is computed from the
        // persisted heartbeat instead.
        submitted_at_ms: persisted.heartbeat_at_ms,
        submit_span_link: None,
        // No live submitter context after a reload; receipts emitted on
        // re-execution take the live caller's identity at that point.
        submitted_by: "reloaded".to_string(),
        waiters: Vec::new(),
    }))
}

fn enqueue_task(pool: &mut PoolEntry, pending: PendingTask) {
    pool.queue.push_back(pending);
}

fn dispatch_ready(pool: &Rc<RefCell<PoolEntry>>) {
    let mut freed_queue_space = false;
    loop {
        let next = {
            let mut pool_ref = pool.borrow_mut();
            if pool_ref.active.len() >= pool_ref.max_concurrent {
                break;
            }
            let next = pop_next_task(&mut pool_ref);
            if next.is_some() {
                freed_queue_space = true;
            }
            next
        };
        let Some(pending) = next else { break };
        spawn_task(pool.clone(), pending);
    }
    if freed_queue_space {
        wake_space_waiters(pool);
    }
}

fn reject_pending_task(
    pool: &PoolEntry,
    pending: PendingTask,
    replacement_task_id: Option<&str>,
    reason: &str,
    policy: &str,
    queue_depth: usize,
    max_depth: Option<usize>,
) -> PoolDropAudit {
    let task_id = pending.task_id.clone();
    let waiters = reject_task_state(&pending.state, reason, policy);
    wake_task_waiters(waiters);
    persist_task_if_durable(pool, &pending.state.borrow());
    pool_drop_audit(
        pool,
        &task_id,
        replacement_task_id,
        reason,
        policy,
        queue_depth,
        max_depth,
    )
}

fn reject_task_state(
    state: &Rc<RefCell<TaskState>>,
    reason: &str,
    policy: &str,
) -> Vec<tokio::sync::oneshot::Sender<()>> {
    let mut state_ref = state.borrow_mut();
    state_ref.status = TaskStatus::Rejected;
    state_ref.finished_at = Some(uuid::Uuid::now_v7().to_string());
    state_ref.heartbeat_at_ms = now_ms_for_pool();
    state_ref.error = Some(reason.to_string());
    state_ref.rejection_reason = Some(reason.to_string());
    state_ref.rejection_policy = Some(policy.to_string());
    std::mem::take(&mut state_ref.waiters)
}

fn wake_task_waiters(waiters: Vec<tokio::sync::oneshot::Sender<()>>) {
    for waiter in waiters {
        let _ = waiter.send(());
    }
}

fn wake_space_waiters(pool: &Rc<RefCell<PoolEntry>>) {
    let waiters = {
        let mut pool_ref = pool.borrow_mut();
        std::mem::take(&mut pool_ref.space_waiters)
    };
    for waiter in waiters {
        let _ = waiter.send(());
    }
}

fn pool_drop_audit(
    pool: &PoolEntry,
    task_id: &str,
    replacement_task_id: Option<&str>,
    reason: &str,
    policy: &str,
    queue_depth: usize,
    max_depth: Option<usize>,
) -> PoolDropAudit {
    PoolDropAudit {
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        task_id: task_id.to_string(),
        replacement_task_id: replacement_task_id.map(str::to_string),
        reason: reason.to_string(),
        policy: policy.to_string(),
        queue_depth,
        max_depth,
        occurred_at: uuid::Uuid::now_v7().to_string(),
    }
}

async fn emit_pool_drop(audit: PoolDropAudit) {
    let topic = Topic::new(POOL_AUDIT_TOPIC).expect("static pool audit topic is valid");
    let mut headers = BTreeMap::new();
    headers.insert("schema".to_string(), "harn.pool_drop.v1".to_string());
    headers.insert("policy".to_string(), audit.policy.clone());
    let payload = json!({
        "pool_id": audit.pool_id,
        "pool": audit.pool_name,
        "task_id": audit.task_id,
        "replacement_task_id": audit.replacement_task_id,
        "reason": audit.reason,
        "policy": audit.policy,
        "queue_depth": audit.queue_depth,
        "max_depth": audit.max_depth,
        "occurred_at": audit.occurred_at,
    });
    let _ = ensure_pool_event_log()
        .append(
            &topic,
            LogEvent::new("pool_drop", payload).with_headers(headers),
        )
        .await;
}

fn pool_submit_receipt(pool: &PoolEntry, task: &TaskState) -> PoolSubmitReceipt {
    PoolSubmitReceipt {
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        task_id: task.id.clone(),
        submitted_at: task.submitted_at.clone(),
        priority: task.priority,
        key: task.key.clone(),
        idempotency_key: task.idempotency_key.clone(),
        submitted_by: task.submitted_by.clone(),
    }
}

async fn emit_pool_submit_receipt(receipt: PoolSubmitReceipt) {
    let topic = Topic::new(POOL_AUDIT_TOPIC).expect("static pool audit topic is valid");
    let mut headers = BTreeMap::new();
    headers.insert("schema".to_string(), "harn.pool_submit.v1".to_string());
    let payload = json!({
        "pool_id": receipt.pool_id,
        "pool": receipt.pool_name,
        "task_id": receipt.task_id,
        "submitted_at": receipt.submitted_at,
        "priority": receipt.priority,
        "key": receipt.key,
        "idempotency_key": receipt.idempotency_key,
        "submitted_by": receipt.submitted_by,
    });
    let _ = ensure_pool_event_log()
        .append(
            &topic,
            LogEvent::new("pool_submit", payload).with_headers(headers),
        )
        .await;
}

fn pool_dequeue_receipt(
    pool: &PoolEntry,
    task: &TaskState,
    slot_index: usize,
) -> PoolDequeueReceipt {
    let now_ms = now_ms_for_pool();
    let queued_for_ms = now_ms.saturating_sub(task.submitted_at_ms);
    PoolDequeueReceipt {
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        task_id: task.id.clone(),
        dequeued_at: task
            .started_at
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        queued_for_ms,
        slot_index,
    }
}

async fn emit_pool_dequeue_receipt(receipt: PoolDequeueReceipt) {
    let topic = Topic::new(POOL_AUDIT_TOPIC).expect("static pool audit topic is valid");
    let mut headers = BTreeMap::new();
    headers.insert("schema".to_string(), "harn.pool_dequeue.v1".to_string());
    let payload = json!({
        "pool_id": receipt.pool_id,
        "pool": receipt.pool_name,
        "task_id": receipt.task_id,
        "dequeued_at": receipt.dequeued_at,
        "queued_for_ms": receipt.queued_for_ms,
        "slot_index": receipt.slot_index,
    });
    let _ = ensure_pool_event_log()
        .append(
            &topic,
            LogEvent::new("pool_dequeue", payload).with_headers(headers),
        )
        .await;
}

fn ensure_pool_event_log() -> Arc<crate::event_log::AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(POOL_EVENT_LOG_QUEUE_DEPTH))
}

fn policy_error(code: Code, message: String) -> VmError {
    VmError::Runtime(format!("{}: {message}", code.as_str()))
}

fn pop_next_task(pool: &mut PoolEntry) -> Option<PendingTask> {
    match &pool.queue_strategy {
        QueueStrategy::Fifo => pool.queue.pop_front(),
        QueueStrategy::Lifo => pool.queue.pop_back(),
        QueueStrategy::Priority => {
            let index = priority_queue_index(pool.queue.iter().map(|p| (p.priority, p.seq)))?;
            pool.queue.remove(index)
        }
        QueueStrategy::FairRoundRobin { .. } => pop_fair_round_robin(pool),
    }
}

fn priority_queue_index<I>(existing: I) -> Option<usize>
where
    I: Iterator<Item = (i64, u64)>,
{
    existing
        .enumerate()
        .max_by(
            |(_, (left_priority, left_seq)), (_, (right_priority, right_seq))| {
                left_priority
                    .cmp(right_priority)
                    .then_with(|| right_seq.cmp(left_seq))
            },
        )
        .map(|(index, _)| index)
}

fn pop_fair_round_robin(pool: &mut PoolEntry) -> Option<PendingTask> {
    if pool.queue.is_empty() {
        return None;
    }
    let mut keys = Vec::<String>::new();
    for pending in &pool.queue {
        let key = fair_key(pending);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let selected_key = match pool.round_robin_after.as_deref() {
        Some(after) => keys
            .iter()
            .position(|key| key == after)
            .map(|index| keys[(index + 1) % keys.len()].clone())
            .unwrap_or_else(|| keys[0].clone()),
        None => keys[0].clone(),
    };
    let index = pool
        .queue
        .iter()
        .position(|pending| fair_key(pending) == selected_key)?;
    pool.round_robin_after = Some(selected_key);
    pool.queue.remove(index)
}

fn fair_key(pending: &PendingTask) -> String {
    pending.key.clone().unwrap_or_default()
}

fn spawn_task(pool: Rc<RefCell<PoolEntry>>, pending: PendingTask) {
    let PendingTask {
        task_id,
        closure,
        state,
        context_vm,
        ..
    } = pending;

    // PoolDequeue span (PL-06). Opened detached so it stands on its own
    // in the trace tree — the dispatcher fires it from whatever caller
    // happens to free a slot (submit, wait, or another task finishing),
    // and the natural parent of that work is the submitter's pipeline,
    // not the unrelated current span. Linking back to the submit span
    // via `set_span_link` is what stitches the trace tree together.
    let submit_link = state.borrow().submit_span_link.clone();
    let span_links: Vec<crate::tracing::SpanLink> = submit_link
        .into_iter()
        .map(|link| {
            link.with_attributes(BTreeMap::from([(
                "harn.link.kind".to_string(),
                "pool_submit".to_string(),
            )]))
        })
        .collect();
    let (pool_id_for_span, pool_name_for_span) = {
        let pool_ref = pool.borrow();
        (pool_ref.id.clone(), pool_ref.name.clone())
    };
    let mut dequeue_span = PoolSpanGuard::start_detached(
        crate::tracing::SpanKind::PoolDequeue,
        format!("pool.dequeue {pool_name_for_span}"),
        span_links,
    );
    dequeue_span.set_metadata("pool", serde_json::json!(pool_name_for_span));
    dequeue_span.set_metadata("pool_id", serde_json::json!(pool_id_for_span));
    dequeue_span.set_metadata("task_id", serde_json::json!(task_id));

    {
        let mut state_ref = state.borrow_mut();
        state_ref.status = TaskStatus::Running;
        state_ref.started_at = Some(uuid::Uuid::now_v7().to_string());
        state_ref.heartbeat_at_ms = now_ms_for_pool();
    }
    let dequeue_receipt = {
        let mut pool_ref = pool.borrow_mut();
        pool_ref.active.insert(task_id, state.clone());
        let slot_index = pool_ref.active.len().saturating_sub(1);
        let receipt = pool_dequeue_receipt(&pool_ref, &state.borrow(), slot_index);
        persist_task_if_durable(&pool_ref, &state.borrow());
        receipt
    };

    dequeue_span.set_metadata(
        "queued_for_ms",
        serde_json::json!(dequeue_receipt.queued_for_ms),
    );
    dequeue_span.set_metadata("slot_index", serde_json::json!(dequeue_receipt.slot_index));

    // Hand the dequeue receipt off to a tokio task. Append is async; we
    // already hold a sync borrow on the pool registry here so awaiting in
    // place would deadlock the next caller of `dispatch_ready`.
    tokio::task::spawn_local(emit_pool_dequeue_receipt(dequeue_receipt));

    // Use the execution context captured at submit time (see `PendingTask`).
    // The VM moves into the local task and runs the closure with a fresh
    // execution context, so each pool task is isolated from siblings. Dispatch
    // runs from a context-free callback, so we must NOT re-clone the ambient
    // task_local context here (it would be empty). See harn#2667.
    let Some(child_vm_cell) = context_vm else {
        // Pool submissions are always called from an async builtin
        // (`__pool_submit`), so the captured context should never be empty.
        // Fail the task cleanly instead of leaving it stuck "running".
        dequeue_span.end();
        finalize_task(
            &pool,
            &state,
            Err("pool: no VM execution context".to_string()),
        );
        return;
    };

    // Close the dequeue span before handing off to the spawned future:
    // the span tracks dispatcher work (slot reservation + child-VM clone),
    // not the runtime of the user closure itself. The closure executes
    // under whatever spans it opens for its own work.
    dequeue_span.end();

    tokio::task::spawn_local(async move {
        // The async-builtin context is task-scoped (`tokio::task_local`): unlike
        // the old `thread_local!` stack, it does NOT leak into a `spawn_local`
        // child running on the same thread. So this spawned pool task binds its
        // own context from the VM captured at submit time (`child_vm_cell`), so
        // the closure — and the async builtins it invokes — resolve a
        // `clone_async_builtin_child_vm` root. See harn#2667.
        let context_root = child_vm_cell.borrow().child_vm();
        let outcome = crate::vm::scope_async_builtin(context_root, async move {
            let Some(mut runner) = crate::vm::clone_async_builtin_child_vm() else {
                return Err("pool: lost VM execution context".to_string());
            };
            runner
                .call_closure_args(&closure, crate::vm::CallArgs::Empty)
                .await
                .map_err(|error| error.to_string())
        })
        .await;
        finalize_task(&pool, &state, outcome);
    });
}

fn finalize_task(
    pool: &Rc<RefCell<PoolEntry>>,
    state: &Rc<RefCell<TaskState>>,
    outcome: Result<VmValue, String>,
) {
    let waiters: Vec<tokio::sync::oneshot::Sender<()>>;
    let task_id;
    {
        let mut state_ref = state.borrow_mut();
        state_ref.finished_at = Some(uuid::Uuid::now_v7().to_string());
        state_ref.heartbeat_at_ms = now_ms_for_pool();
        match outcome {
            Ok(value) => {
                state_ref.status = TaskStatus::Completed;
                state_ref.result = Some(value);
            }
            Err(error) => {
                state_ref.status = TaskStatus::Failed;
                state_ref.error = Some(error);
            }
        }
        task_id = state_ref.id.clone();
        waiters = std::mem::take(&mut state_ref.waiters);
    }
    {
        let mut pool_ref = pool.borrow_mut();
        pool_ref.active.remove(&task_id);
        persist_task_if_durable(&pool_ref, &state.borrow());
    }
    wake_task_waiters(waiters);
    dispatch_ready(pool);
    wake_space_waiters(pool);
}

/// Block until one or more pool task handles reach a terminal state.
#[harn_builtin(
    sig = "__pool_wait(handle_or_handles: string|dict|list) -> dict",
    kind = "async",
    category = "pool",
    runtime_only = true
)]
async fn pool_wait_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("pool_wait: task handle is required".to_string()))?;
    match target {
        VmValue::List(items) => {
            let mut results = Vec::with_capacity(items.len());
            for item in items.iter() {
                results.push(wait_single_task(item).await?);
            }
            Ok(VmValue::List(Rc::new(results)))
        }
        _ => wait_single_task(target).await,
    }
}

async fn wait_single_task(value: &VmValue) -> Result<VmValue, VmError> {
    let (pool_id, task_id) = task_handle_from_value(value, "pool_wait")?;
    let entry = lookup_pool(&pool_id)?;
    let state = {
        let pool = entry.borrow();
        pool.tasks
            .get(&task_id)
            .cloned()
            .ok_or_else(|| VmError::Runtime(format!("pool_wait: task not found: {task_id}")))?
    };
    let receiver = {
        let mut state_ref = state.borrow_mut();
        if state_ref.status.is_terminal() {
            return Ok(task_snapshot_value(&state_ref));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        state_ref.waiters.push(tx);
        rx
    };
    // The sender is dropped on completion; both send() and drop wake the
    // receiver. Either is fine: we only care that the task reached a
    // terminal state, which `finalize_task` guarantees before signaling.
    let _ = receiver.await;
    let snapshot = task_snapshot_value(&state.borrow());
    Ok(snapshot)
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &POOL_CREATE_SYNC_DEF,
    &POOL_GET_SYNC_DEF,
    &POOL_LIST_SYNC_DEF,
    &POOL_SIZE_SYNC_DEF,
    &POOL_SNAPSHOT_SYNC_DEF,
    &POOL_RELOAD_SYNC_DEF,
    &POOL_SUBMIT_BUILTIN_DEF,
    &POOL_WAIT_BUILTIN_DEF,
];

pub(crate) fn register_pool_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

/// Drop all in-process pool registry state. Called from
/// `reset_thread_local_state` between top-level VM runs and from
/// conformance test harness reload sequences so a fresh `pool_create`
/// starts from a clean registry. The on-disk JSONL artifacts under
/// `.harn/pools/` are intentionally NOT removed — that is the whole
/// point of pipeline-scope durability.
pub fn reset_pool_state() {
    POOLS.with(|pools| pools.borrow_mut().clear());
    POOL_NAMES.with(|names| names.borrow_mut().clear());
}

/// Snapshot every pool task that has not yet reached a terminal state.
///
/// Powers the `pool_pending_tasks` bucket on
/// `UnsettledStateSnapshot` so pipeline `on_finish` callbacks (drain,
/// abandon, handoff presets) can observe pool work alongside suspended
/// sub-agents, queued triggers, partial handoffs, and in-flight LLM
/// calls. Walks the thread-local `POOLS` registry once, emitting one
/// JSON entry per task whose status is `queued` or `running`. Order is
/// pool-id ascending, then queued tasks in queue order followed by
/// running tasks in `tasks` btree order, so successive snapshots within
/// a single thread are deterministic.
pub(crate) fn snapshot_pending_tasks() -> Vec<serde_json::Value> {
    let now_ms = crate::stdlib::clock::now_wall_ms();
    POOLS.with(|pools| {
        let registry = pools.borrow();
        let mut ordered: Vec<(&String, &Rc<RefCell<PoolEntry>>)> = registry.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = Vec::new();
        for (_pool_id, entry) in ordered {
            let pool = entry.borrow();
            for pending in &pool.queue {
                let task = pending.state.borrow();
                if task.status.is_terminal() {
                    continue;
                }
                out.push(pending_task_snapshot_json(&pool, &task, now_ms));
            }
            for state in pool.tasks.values() {
                let task = state.borrow();
                if task.status != TaskStatus::Running {
                    continue;
                }
                out.push(pending_task_snapshot_json(&pool, &task, now_ms));
            }
        }
        out
    })
}

fn pending_task_snapshot_json(
    pool: &PoolEntry,
    task: &TaskState,
    now_ms: i64,
) -> serde_json::Value {
    let queued_at_ms = task.submitted_at_ms;
    let age_ms = now_ms.saturating_sub(queued_at_ms).max(0);
    serde_json::json!({
        "id": task.id.clone(),
        "task_id": task.id.clone(),
        "pool_id": pool.id.clone(),
        "pool_name": pool.name.clone(),
        "status": task.status.as_str(),
        "priority": task.priority,
        "key": task.key.clone(),
        "idempotency_key": task.idempotency_key.clone(),
        "submitted_at": task.submitted_at.clone(),
        "submitted_at_ms": queued_at_ms,
        "submitted_by": task.submitted_by.clone(),
        "started_at": task.started_at.clone(),
        "age_ms": age_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::priority_queue_index;

    /// Simulates priority dispatch over a vector of (priority, seq) tuples
    /// so the ordering rule can be exercised without standing up a real
    /// `VmClosure` or `PoolEntry`.
    fn dispatch_all(items: &[(i64, u64)]) -> Vec<u64> {
        let mut queue: Vec<(i64, u64)> = items.to_vec();
        let mut out = Vec::new();
        while let Some(index) = priority_queue_index(queue.iter().copied()) {
            let (_, seq) = queue.remove(index);
            out.push(seq);
        }
        out
    }

    #[test]
    fn higher_priority_dequeues_first_ties_break_by_seq() {
        // Submit order: 1@0, 2@5, 3@5, 4@10
        // Expected dispatch order: 4 (highest), then 2 then 3 (older tie),
        // then 1 (lowest).
        assert_eq!(
            dispatch_all(&[(0, 1), (5, 2), (5, 3), (10, 4)]),
            vec![4, 2, 3, 1]
        );
    }

    #[test]
    fn equal_priority_is_pure_fifo() {
        assert_eq!(dispatch_all(&[(0, 1), (0, 2), (0, 3)]), vec![1, 2, 3]);
    }

    #[test]
    fn empty_priority_queue_has_no_next_task() {
        assert_eq!(priority_queue_index(std::iter::empty()), None);
    }
}
