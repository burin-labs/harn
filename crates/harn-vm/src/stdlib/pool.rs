//! Named agent thread pools (PL-01).
//!
//! Foundation for the agent pool epic (#1883). Provides a thread-local
//! registry of named pools that bound the number of concurrent Harn
//! closure executions and queue excess submissions. Queue strategy,
//! backpressure, durability, and channel composition arrive in later
//! sub-tickets (#1887..#1893); this module focuses on the minimum that
//! `pool_create(...) + pool.submit(closure) + pool.size/snapshot` need.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

/// Default `max_concurrent` when a pool is created without one.
const DEFAULT_MAX_CONCURRENT: usize = 1;

/// Type tag stamped on every pool handle and task handle returned to Harn
/// code. `wait_agent` matches on `POOL_TASK_TYPE` to route pool task
/// handles to `__pool_wait` (see `agent/workers.harn`).
const POOL_TYPE: &str = "pool";
const POOL_TASK_TYPE: &str = "pool_task";

#[derive(Clone)]
struct PendingTask {
    task_id: String,
    closure: Rc<VmClosure>,
    state: Rc<RefCell<TaskState>>,
    priority: i64,
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
}

impl TaskStatus {
    fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Failed)
    }
}

struct PoolEntry {
    id: String,
    name: String,
    max_concurrent: usize,
    created_at: String,
    submit_counter: u64,
    queue: VecDeque<PendingTask>,
    active: HashMap<String, Rc<RefCell<TaskState>>>,
    tasks: BTreeMap<String, Rc<RefCell<TaskState>>>,
    /// Optional per-create user-supplied config (queue strategy, priority
    /// fn, backpressure). PL-01 stores it for inspection / snapshot but
    /// doesn't yet evaluate it — PL-02..PL-04 wire each strategy.
    config: BTreeMap<String, VmValue>,
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
    for task in pool.tasks.values() {
        match task.borrow().status {
            TaskStatus::Completed => completed += 1,
            TaskStatus::Failed => failed += 1,
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
    snapshot.insert("total".to_string(), VmValue::Int(pool.tasks.len() as i64));
    snapshot.insert("tasks".to_string(), VmValue::List(Rc::new(tasks)));
    if !pool.config.is_empty() {
        snapshot.insert(
            "config".to_string(),
            VmValue::Dict(Rc::new(pool.config.clone())),
        );
    }
    VmValue::Dict(Rc::new(snapshot))
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
    if let Some(key) = &task.key {
        handle.insert("key".to_string(), VmValue::String(Rc::from(key.as_str())));
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
    let id = next_pool_id();
    let entry = Rc::new(RefCell::new(PoolEntry {
        id: id.clone(),
        name: name.clone(),
        max_concurrent,
        created_at: uuid::Uuid::now_v7().to_string(),
        submit_counter: 0,
        queue: VecDeque::new(),
        active: HashMap::new(),
        tasks: BTreeMap::new(),
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
    let key = parse_key(&opts)?;
    let priority = parse_priority(&opts)?;

    let entry = lookup_pool(&pool_id)?;
    let task = {
        let mut pool = entry.borrow_mut();
        pool.submit_counter += 1;
        let seq = pool.submit_counter;
        let task_id = next_task_id(&pool);
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
            waiters: Vec::new(),
        }));
        pool.tasks.insert(task_id.clone(), state.clone());
        enqueue_task(
            &mut pool,
            PendingTask {
                task_id: task_id.clone(),
                closure: closure.clone(),
                state: state.clone(),
                priority,
                seq,
            },
        );
        state
    };
    dispatch_ready(&entry);
    let handle = task_handle_value(&task.borrow());
    Ok(handle)
}

fn enqueue_task(pool: &mut PoolEntry, pending: PendingTask) {
    let position = priority_insert_position(
        pool.queue.iter().map(|p| (p.priority, p.seq)),
        pending.priority,
        pending.seq,
    );
    pool.queue.insert(position, pending);
}

/// Insertion position for FIFO-with-priority: higher priority dequeues
/// first; ties dequeue by submission order (lower seq first). Pulled into
/// its own helper so the queue ordering rule has a unit test independent
/// of the rest of the pool plumbing.
fn priority_insert_position<I>(existing: I, priority: i64, seq: u64) -> usize
where
    I: Iterator<Item = (i64, u64)>,
{
    let mut len = 0usize;
    for (i, (p, s)) in existing.enumerate() {
        len = i + 1;
        if p < priority || (p == priority && s > seq) {
            return i;
        }
    }
    len
}

fn dispatch_ready(pool: &Rc<RefCell<PoolEntry>>) {
    loop {
        let next = {
            let mut pool_ref = pool.borrow_mut();
            if pool_ref.active.len() >= pool_ref.max_concurrent {
                break;
            }
            pool_ref.queue.pop_front()
        };
        let Some(pending) = next else { break };
        spawn_task(pool.clone(), pending);
    }
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
    for waiter in waiters {
        let _ = waiter.send(());
    }
    dispatch_ready(pool);
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
    use super::priority_insert_position;

    /// Simulates `enqueue_task` over a vector of (priority, seq) tuples
    /// so the ordering rule can be exercised without standing up a real
    /// `VmClosure` or `PoolEntry`.
    fn insert_all(items: &[(i64, u64)]) -> Vec<u64> {
        let mut queue: Vec<(i64, u64)> = Vec::new();
        for &(p, s) in items {
            let position = priority_insert_position(queue.iter().copied(), p, s);
            queue.insert(position, (p, s));
        }
        queue.into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn higher_priority_dequeues_first_ties_break_by_seq() {
        // Submit order: 1@0, 2@5, 3@5, 4@10
        // Expected dispatch order: 4 (highest), then 2 then 3 (older tie),
        // then 1 (lowest).
        assert_eq!(
            insert_all(&[(0, 1), (5, 2), (5, 3), (10, 4)]),
            vec![4, 2, 3, 1]
        );
    }

    #[test]
    fn equal_priority_is_pure_fifo() {
        assert_eq!(insert_all(&[(0, 1), (0, 2), (0, 3)]), vec![1, 2, 3]);
    }

    #[test]
    fn empty_queue_inserts_at_head() {
        assert_eq!(priority_insert_position(std::iter::empty(), 7, 1), 0);
    }
}
