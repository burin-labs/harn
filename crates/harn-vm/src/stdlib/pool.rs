//! Named agent thread pools (PL-01/PL-03).
//!
//! Foundation for the agent pool epic (#1883). Provides a thread-local
//! registry of named pools that bound the number of concurrent Harn
//! closure executions and queue excess submissions. Queue strategy ships
//! here, with bounded backpressure policies layered on the single submit
//! path. Durability and channel composition arrive in later sub-tickets
//! (#1889..#1893).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};
use harn_parser::diagnostic_codes::Code;
use serde_json::json;

/// Default `max_concurrent` when a pool is created without one.
const DEFAULT_MAX_CONCURRENT: usize = 1;

/// Type tag stamped on every pool handle and task handle returned to Harn
/// code. `wait_agent` matches on `POOL_TASK_TYPE` to route pool task
/// handles to `__pool_wait` (see `agent/workers.harn`).
const POOL_TYPE: &str = "pool";
const POOL_TASK_TYPE: &str = "pool_task";
const POOL_AUDIT_TOPIC: &str = "lifecycle.pool.audit";
const POOL_EVENT_LOG_QUEUE_DEPTH: usize = 128;

#[derive(Clone)]
struct PendingTask {
    task_id: String,
    closure: Rc<VmClosure>,
    state: Rc<RefCell<TaskState>>,
    priority: i64,
    key: Option<String>,
    /// Tiebreaker so FIFO order is preserved among equal priorities.
    seq: u64,
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
    let id = next_pool_id();
    let entry = Rc::new(RefCell::new(PoolEntry {
        id: id.clone(),
        name: name.clone(),
        max_concurrent,
        created_at: uuid::Uuid::now_v7().to_string(),
        submit_counter: 0,
        queue: VecDeque::new(),
        queue_strategy,
        backpressure,
        round_robin_after: None,
        active: HashMap::new(),
        tasks: BTreeMap::new(),
        space_waiters: Vec::new(),
        config: ordered_pool_config(&opts),
    }));
    POOLS.with(|pools| pools.borrow_mut().insert(id.clone(), entry.clone()));
    POOL_NAMES.with(|names| names.borrow_mut().insert(name, id.clone()));
    let snapshot = pool_snapshot_value(&entry.borrow());
    Ok(snapshot)
}

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

    let entry = lookup_pool(&pool_id)?;
    let key = {
        let pool = entry.borrow();
        parse_submit_key(&opts, &pool.queue_strategy)?
    };

    loop {
        let attempt = {
            let mut pool = entry.borrow_mut();
            submit_or_wait(&mut pool, closure.clone(), key.clone(), priority)
        };
        match attempt {
            SubmitAttempt::Submitted { task, audits } => {
                for audit in audits {
                    emit_pool_drop(audit).await;
                }
                dispatch_ready(&entry);
                let handle = task_handle_value(&task.borrow());
                return Ok(handle);
            }
            SubmitAttempt::Wait(receiver) => {
                let _ = receiver.await;
            }
            SubmitAttempt::Fail(error) => return Err(error),
        }
    }
}

enum SubmitAttempt {
    Submitted {
        task: Rc<RefCell<TaskState>>,
        audits: Vec<PoolDropAudit>,
    },
    Wait(tokio::sync::oneshot::Receiver<()>),
    Fail(VmError),
}

fn submit_or_wait(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
) -> SubmitAttempt {
    if can_accept_now(pool) {
        let (state, pending) = create_pending_task(pool, closure, key, priority);
        enqueue_task(pool, pending);
        return SubmitAttempt::Submitted {
            task: state,
            audits: Vec::new(),
        };
    }

    match pool.backpressure.clone() {
        BackpressureStrategy::Unbounded => {
            let (state, pending) = create_pending_task(pool, closure, key, priority);
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

fn submit_with_oldest_drop(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    reason: &str,
    policy: &str,
    max_depth: Option<usize>,
) -> SubmitAttempt {
    let queue_depth = pool.queue.len();
    let (state, pending) = create_pending_task(pool, closure, key, priority);
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

fn submit_with_newest_drop(
    pool: &mut PoolEntry,
    closure: Rc<VmClosure>,
    key: Option<String>,
    priority: i64,
    reason: &str,
    policy: &str,
    max_depth: Option<usize>,
) -> SubmitAttempt {
    let queue_depth = pool.queue.len();
    let (state, _pending) = create_pending_task(pool, closure, key, priority);
    let task_id = state.borrow().id.clone();
    let waiters = reject_task_state(&state, reason, policy);
    wake_task_waiters(waiters);
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
) -> (Rc<RefCell<TaskState>>, PendingTask) {
    pool.submit_counter += 1;
    let seq = pool.submit_counter;
    let task_id = next_task_id(pool);
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
        waiters: Vec::new(),
    }));
    pool.tasks.insert(task_id.clone(), state.clone());
    let pending = PendingTask {
        task_id,
        closure,
        state: state.clone(),
        priority,
        key,
        seq,
    };
    (state, pending)
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
        ..
    } = pending;
    {
        let mut state_ref = state.borrow_mut();
        state_ref.status = TaskStatus::Running;
        state_ref.started_at = Some(uuid::Uuid::now_v7().to_string());
    }
    pool.borrow_mut().active.insert(task_id, state.clone());

    // Snapshot the active async-builtin VM now (synchronously, while the
    // submit builtin is still on the stack). The cloned VM moves into the
    // local task and runs the closure with a fresh execution context, so
    // each pool task is isolated from siblings.
    let Some(mut child_vm) = crate::vm::clone_async_builtin_child_vm() else {
        // Pool submissions are always called from an async builtin
        // (`__pool_submit`), so the slot should never be empty here.
        // Fail the task cleanly instead of leaving it stuck "running".
        finalize_task(
            &pool,
            &state,
            Err("pool: no VM execution context".to_string()),
        );
        return;
    };

    tokio::task::spawn_local(async move {
        let outcome = child_vm
            .call_closure(&closure, &[])
            .await
            .map_err(|error| error.to_string());
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
    pool.borrow_mut().active.remove(&task_id);
    wake_task_waiters(waiters);
    dispatch_ready(pool);
    wake_space_waiters(pool);
}

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

const POOL_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("__pool_create", pool_create_sync)
        .signature("__pool_create(options?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Create a named agent pool and register it in the local pool registry."),
    SyncBuiltin::new("__pool_get", pool_get_sync)
        .signature("__pool_get(name_or_id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Look up a pool by name or id; returns nil when missing."),
    SyncBuiltin::new("__pool_list", pool_list_sync)
        .signature("__pool_list()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("List every pool registered in the local pool registry."),
    SyncBuiltin::new("__pool_size", pool_size_sync)
        .signature("__pool_size(pool)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return active + queued task count for a pool."),
    SyncBuiltin::new("__pool_snapshot", pool_snapshot_sync)
        .signature("__pool_snapshot(pool)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return the full pool snapshot for inspection."),
];

const POOL_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!("__pool_submit", pool_submit_builtin)
        .signature("__pool_submit(pool, closure, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Submit a closure to a pool; spawns when a slot is free, otherwise queues."),
    async_builtin!("__pool_wait", pool_wait_builtin)
        .signature("__pool_wait(handle_or_handles)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Block until one or more pool task handles reach a terminal state."),
];

const POOL_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("pool")
    .sync(POOL_SYNC_PRIMITIVES)
    .async_(POOL_ASYNC_PRIMITIVES);

pub(crate) fn register_pool_builtins(vm: &mut Vm) {
    register_builtin_group(vm, POOL_PRIMITIVES);
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
