use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::shared_state::ScopedKey;
use crate::stdlib::args::{ArgError, Args, ErrorKind};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{
    DeadlockError, VmAtomicHandle, VmChannelCloseState, VmChannelHandle, VmError, VmValue,
};
use crate::vm::Vm;
use crate::wait_for_graph::{channel_target, ChannelTarget, WaitGuard};

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    // sync
    &SYNC_RELEASE_BUILTIN_DEF,
    &CHANNEL_BUILTIN_DEF,
    &CLOSE_CHANNEL_BUILTIN_DEF,
    &CHANNEL_IS_CLOSED_BUILTIN_DEF,
    &TRY_RECEIVE_BUILTIN_DEF,
    &ATOMIC_BUILTIN_DEF,
    &ATOMIC_GET_BUILTIN_DEF,
    &ATOMIC_SET_BUILTIN_DEF,
    &ATOMIC_ADD_BUILTIN_DEF,
    &ATOMIC_CAS_BUILTIN_DEF,
    &TIMER_START_BUILTIN_DEF,
    &CIRCUIT_BREAKER_BUILTIN_DEF,
    &CIRCUIT_CHECK_BUILTIN_DEF,
    &CIRCUIT_RECORD_SUCCESS_BUILTIN_DEF,
    &CIRCUIT_RECORD_FAILURE_BUILTIN_DEF,
    &CIRCUIT_RESET_BUILTIN_DEF,
    &TIMER_END_BUILTIN_DEF,
    // async — sync primitives
    &SYNC_MUTEX_ACQUIRE_BUILTIN_DEF,
    &SYNC_SEMAPHORE_ACQUIRE_BUILTIN_DEF,
    &SYNC_GATE_ACQUIRE_BUILTIN_DEF,
    &SYNC_RWLOCK_ACQUIRE_BUILTIN_DEF,
    &SYNC_METRICS_BUILTIN_DEF,
    // async — shared state
    &SHARED_SCOPE_ID_BUILTIN_DEF,
    &SHARED_CELL_BUILTIN_DEF,
    &SHARED_GET_BUILTIN_DEF,
    &SHARED_SNAPSHOT_BUILTIN_DEF,
    &SHARED_SET_BUILTIN_DEF,
    &SHARED_CAS_BUILTIN_DEF,
    &SHARED_MAP_BUILTIN_DEF,
    &SHARED_MAP_GET_BUILTIN_DEF,
    &SHARED_MAP_SNAPSHOT_BUILTIN_DEF,
    &SHARED_MAP_ENTRIES_BUILTIN_DEF,
    &SHARED_MAP_SET_BUILTIN_DEF,
    &SHARED_MAP_DELETE_BUILTIN_DEF,
    &SHARED_MAP_CAS_BUILTIN_DEF,
    &SHARED_METRICS_BUILTIN_DEF,
    // async — mailbox
    &MAILBOX_OPEN_BUILTIN_DEF,
    &MAILBOX_LOOKUP_BUILTIN_DEF,
    &MAILBOX_SEND_BUILTIN_DEF,
    &MAILBOX_TRY_RECEIVE_BUILTIN_DEF,
    &MAILBOX_RECEIVE_BUILTIN_DEF,
    &MAILBOX_CLOSE_BUILTIN_DEF,
    &MAILBOX_METRICS_BUILTIN_DEF,
    // async — scheduler + channel
    &SLEEP_BUILTIN_DEF,
    &YIELD_NOW_BUILTIN_DEF,
    &SEND_BUILTIN_DEF,
    &RECEIVE_BUILTIN_DEF,
    &SELECT_BUILTIN_DEF,
    &SELECT_TIMEOUT_BUILTIN_DEF,
    &SELECT_TRY_BUILTIN_DEF,
    &SELECT_LIST_BUILTIN_DEF,
    &CHANNEL_SELECT_BUILTIN_DEF,
];

/// Build a select result dict with the given index, value, and channel name.
fn select_result(index: usize, value: VmValue, channel_name: &str) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(index as i64));
    result.insert("value".to_string(), value);
    result.put_str("channel", channel_name);
    VmValue::dict(result)
}

/// Build a select result dict indicating no channel was ready (index = -1).
fn select_none() -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(-1));
    result.insert("value".to_string(), VmValue::Nil);
    result.insert("channel".to_string(), VmValue::Nil);
    VmValue::dict(result)
}

fn require_channel_list(args: &[VmValue], builtin: &str) -> Result<Vec<VmValue>, VmError> {
    match args.first() {
        Some(VmValue::List(items)) => {
            for item in items.iter() {
                if !matches!(item, VmValue::Channel(_)) {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        format!("{builtin}: channel list must contain only channels"),
                    ))));
                }
            }
            Ok(items.as_ref().clone())
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{builtin}: first argument must be a list of channels"),
        )))),
    }
}

/// Try to receive from a list of channels (non-blocking).
fn try_poll_channels(channels: &[VmValue]) -> (Option<(usize, VmValue, String)>, bool) {
    let mut all_closed = true;
    for (i, ch_val) in channels.iter().enumerate() {
        if let VmValue::Channel(ch) = ch_val {
            if let Ok(mut rx) = ch.receiver.try_lock() {
                match rx.try_recv() {
                    Ok(val) => return (Some((i, val, ch.name.to_string())), false),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        if !ch.is_closed() {
                            all_closed = false;
                        }
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
                }
            } else {
                all_closed = false;
            }
        }
    }
    (None, all_closed)
}

pub(crate) fn cancelled_vm_error() -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
        "kind:cancelled:VM cancelled by host",
    )))
}

fn channel_closed_error(operation: &str, channel_name: &str) -> VmError {
    VmError::Thrown(VmValue::dict(BTreeMap::from([
        (
            "type".to_string(),
            VmValue::String(arcstr::ArcStr::from("ChannelClosed")),
        ),
        (
            "kind".to_string(),
            VmValue::String(arcstr::ArcStr::from("ChannelClosed")),
        ),
        (
            "category".to_string(),
            VmValue::String(arcstr::ArcStr::from("channel_closed")),
        ),
        (
            "message".to_string(),
            VmValue::String(arcstr::ArcStr::from(format!(
                "{operation}: channel '{channel_name}' is closed"
            ))),
        ),
    ])))
}

fn guard_sync_self_deadlock(
    vm: &Vm,
    kind: &str,
    key: &str,
    capacity: u32,
    permits: u32,
    timeout_ms: Option<u64>,
) -> Result<(), VmError> {
    if timeout_ms.is_some() {
        return Ok(());
    }
    let held = vm.held_permits_for(kind, key);
    if held > 0 && held.saturating_add(permits) > capacity {
        return Err(VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
            kind,
            key,
            format!(
                "re-entrant acquire of {permits} permit(s) while this task already holds {held}/{capacity}"
            ),
        ))));
    }
    Ok(())
}

fn optional_timeout_ms(value: Option<&VmValue>) -> Option<u64> {
    match value {
        Some(VmValue::Int(_)) | Some(VmValue::Duration(_)) | Some(VmValue::Float(_)) => {
            value.and_then(optional_timeout_scalar_ms)
        }
        Some(VmValue::Dict(dict)) => dict
            .get("timeout_ms")
            .or_else(|| dict.get("max_wait_ms"))
            .and_then(optional_timeout_scalar_ms),
        _ => None,
    }
}

fn optional_timeout_scalar_ms(value: &VmValue) -> Option<u64> {
    match Args::single("channel_select", ErrorKind::Runtime, Some(value)).millis(0, "timeout_ms") {
        Ok(ms) => Some(ms),
        Err(_) if is_negative_millis_value(value) => Some(0),
        Err(_) => None,
    }
}

fn is_negative_millis_value(value: &VmValue) -> bool {
    match value {
        VmValue::Duration(ms) | VmValue::Int(ms) => *ms < 0,
        VmValue::Float(ms) => ms.is_finite() && *ms < 0.0,
        _ => false,
    }
}

fn positive_u32_arg(
    args: &[VmValue],
    idx: usize,
    default: u32,
    builtin: &str,
    name: &str,
) -> Result<u32, VmError> {
    let bad = |what| ArgError::constraint(builtin, ErrorKind::Runtime, name, what);
    let raw = Args::runtime(builtin, args)
        .opt_int(idx, name)?
        .unwrap_or(default as i64);
    if raw <= 0 {
        return Err(bad("must be > 0".to_string()));
    }
    u32::try_from(raw).map_err(|_| bad(format!("must fit in a u32; got {raw}")))
}

fn dict_string(dict: &crate::value::DictMap, key: &str) -> Option<String> {
    dict.get(key).and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        VmValue::Nil => None,
        other => Some(other.display()),
    })
}

fn context_string(context: &VmValue, key: &str) -> Option<String> {
    context.as_dict().and_then(|dict| match dict.get(key) {
        Some(VmValue::String(text)) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    })
}

fn resolve_shared_scope(
    vm: &Vm,
    raw_scope: Option<&str>,
    options: Option<&crate::value::DictMap>,
    builtin: &str,
) -> Result<String, VmError> {
    let context = crate::runtime_context::runtime_context_value(vm);
    let scope = raw_scope.unwrap_or("task_group");
    let pick = |field: &str| context_string(&context, field);
    let resolved = match scope {
        "task" | "task_local" | "task-local" => {
            pick("task_id").unwrap_or_else(|| "task_root".to_string())
        }
        "root_task" | "root-task" => {
            pick("root_task_id").unwrap_or_else(|| "task_root".to_string())
        }
        "task_group" | "task-group" | "workflow" | "workflow_local" | "workflow-local" => {
            pick("task_group_id")
                .or_else(|| pick("run_id"))
                .or_else(|| pick("root_task_id"))
                .unwrap_or_else(|| "task_root".to_string())
        }
        "workflow_run" | "workflow-run" | "run" => pick("run_id")
            .or_else(|| pick("task_group_id"))
            .or_else(|| pick("root_task_id"))
            .unwrap_or_else(|| "task_root".to_string()),
        "agent_session" | "agent-session" | "session" => pick("agent_session_id")
            .or_else(|| pick("root_agent_session_id"))
            .or_else(|| pick("root_task_id"))
            .unwrap_or_else(|| "task_root".to_string()),
        "tenant" | "tenant_scoped" | "tenant-scoped" => options
            .and_then(|opts| dict_string(opts, "tenant_id"))
            .or_else(|| pick("tenant_id"))
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "{builtin}: tenant scope requires tenant_id in options or runtime context"
                ))
            })?,
        "process" | "global" => "process".to_string(),
        "durable" | "event_log" | "event-log" => {
            return Err(VmError::Runtime(format!(
                "{builtin}: durable shared state is explicit; use store_* or agent_state_* APIs"
            )));
        }
        "external" | "host" => {
            return Err(VmError::Runtime(format!(
                "{builtin}: external shared state must be provided by a host/connector builtin"
            )));
        }
        custom => custom.to_string(),
    };
    Ok(format!("{scope}:{resolved}"))
}

fn shared_options(args: &[VmValue]) -> Option<&crate::value::DictMap> {
    args.first().and_then(VmValue::as_dict)
}

fn scoped_from_open_args(
    vm: &Vm,
    args: &[VmValue],
    builtin: &str,
    key_field: &str,
) -> Result<(ScopedKey, Option<crate::value::DictMap>), VmError> {
    let options = shared_options(args);
    let key = if let Some(options) = options {
        dict_string(options, key_field)
            .or_else(|| dict_string(options, "key"))
            .or_else(|| dict_string(options, "name"))
    } else {
        args.first()
            .map(VmValue::display)
            .filter(|key| !key.is_empty())
    }
    .ok_or_else(|| VmError::Runtime(format!("{builtin}: key/name is required")))?;
    let raw_scope = options.and_then(|opts| dict_string(opts, "scope"));
    let scope = resolve_shared_scope(vm, raw_scope.as_deref(), options, builtin)?;
    Ok((ScopedKey { scope, key }, options.cloned()))
}

fn scoped_from_handle_or_name(
    vm: &Vm,
    value: &VmValue,
    expected_kind: &str,
    builtin: &str,
) -> Result<ScopedKey, VmError> {
    if let Some(dict) = value.as_dict() {
        let kind = dict_string(dict, "_type").unwrap_or_default();
        if kind != expected_kind {
            return Err(VmError::Runtime(format!(
                "{builtin}: expected {expected_kind} handle"
            )));
        }
        let scope = dict_string(dict, "scope").ok_or_else(|| {
            VmError::Runtime(format!("{builtin}: {expected_kind} handle missing scope"))
        })?;
        let key = dict_string(dict, "key").ok_or_else(|| {
            VmError::Runtime(format!("{builtin}: {expected_kind} handle missing key"))
        })?;
        return Ok(ScopedKey { scope, key });
    }
    let key = value.display();
    if key.is_empty() {
        return Err(VmError::Runtime(format!(
            "{builtin}: name must not be empty"
        )));
    }
    let scope = resolve_shared_scope(vm, None, None, builtin)?;
    Ok(ScopedKey { scope, key })
}

fn current_async_vm(ctx: &crate::vm::AsyncBuiltinCtx, _builtin: &str) -> Vm {
    ctx.child_vm()
}

fn channel_send_wait(vm: &Vm, target: ChannelTarget) -> Result<Option<WaitGuard>, VmError> {
    if !vm.deadlines.is_empty() {
        return Ok(None);
    }
    vm.wait_for_graph
        .wait_for_channel_send(&vm.runtime_context.task_id, target)
        .map(Some)
}

fn channel_receive_wait(vm: &Vm, channels: &[VmValue]) -> Result<Option<WaitGuard>, VmError> {
    if !vm.deadlines.is_empty() {
        return Ok(None);
    }
    let targets = channels
        .iter()
        .filter_map(|value| match value {
            VmValue::Channel(ch) => Some(channel_target(ch)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Ok(None);
    }
    vm.wait_for_graph
        .wait_for_channel_receive(&vm.runtime_context.task_id, targets)
        .map(Some)
}

pub(crate) fn register_concurrency_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sync_mutex_acquire(key?: string, timeout_ms?: int) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Acquire a named mutex permit."
)]
async fn sync_mutex_acquire_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "sync_mutex_acquire");
    let key = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "__default__".to_string());
    let timeout_ms = optional_timeout_ms(args.get(1));
    guard_sync_self_deadlock(&vm, "mutex", &key, 1, 1, timeout_ms)?;
    Ok(vm
        .sync_runtime
        .acquire("mutex", &key, 1, 1, timeout_ms, vm.cancel_token.clone())
        .await?
        .map(VmValue::sync_permit)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.runtime.sync_semaphore_acquire",
    effects = ["state.mutate@arg0"],
    sig = "sync_semaphore_acquire(key?: string, capacity?: int, permits?: int, timeout_ms?: int) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Acquire permits from a named semaphore."
)]
async fn sync_semaphore_acquire_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "sync_semaphore_acquire");
    let key = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "default".to_string());
    let capacity = positive_u32_arg(&args, 1, 1, "sync_semaphore_acquire", "capacity")?;
    let permits = positive_u32_arg(&args, 2, 1, "sync_semaphore_acquire", "permits")?;
    let timeout_ms = optional_timeout_ms(args.get(3));
    guard_sync_self_deadlock(&vm, "semaphore", &key, capacity, permits, timeout_ms)?;
    Ok(vm
        .sync_runtime
        .acquire(
            "semaphore",
            &key,
            capacity,
            permits,
            timeout_ms,
            vm.cancel_token.clone(),
        )
        .await?
        .map(VmValue::sync_permit)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.runtime.sync_gate_acquire",
    effects = ["state.mutate@arg0"],
    sig = "sync_gate_acquire(key?: string, limit?: int, timeout_ms?: int) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Acquire one permit from a named gate."
)]
async fn sync_gate_acquire_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "sync_gate_acquire");
    let key = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "default".to_string());
    let limit = positive_u32_arg(&args, 1, 1, "sync_gate_acquire", "limit")?;
    let timeout_ms = optional_timeout_ms(args.get(2));
    guard_sync_self_deadlock(&vm, "gate", &key, limit, 1, timeout_ms)?;
    Ok(vm
        .sync_runtime
        .acquire("gate", &key, limit, 1, timeout_ms, vm.cancel_token.clone())
        .await?
        .map(VmValue::sync_permit)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.runtime.sync_rwlock_acquire",
    effects = ["state.mutate@arg0"],
    sig = "sync_rwlock_acquire(key?: string, mode?: string, timeout_ms?: int) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Acquire a read or write permit from a named read-write lock."
)]
async fn sync_rwlock_acquire_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    const RWLOCK_CAPACITY: u32 = 1024;
    let vm = current_async_vm(&ctx, "sync_rwlock_acquire");
    let key = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "default".to_string());
    let mode = args
        .get(1)
        .map(|a| a.display())
        .unwrap_or_else(|| "read".to_string());
    let permits = match mode.as_str() {
        "read" => 1,
        "write" => RWLOCK_CAPACITY,
        _ => {
            return Err(VmError::Runtime(
                "sync_rwlock_acquire: mode must be read or write".to_string(),
            ));
        }
    };
    let timeout_ms = optional_timeout_ms(args.get(2));
    guard_sync_self_deadlock(&vm, "rwlock", &key, RWLOCK_CAPACITY, permits, timeout_ms)?;
    Ok(vm
        .sync_runtime
        .acquire(
            "rwlock",
            &key,
            RWLOCK_CAPACITY,
            permits,
            timeout_ms,
            vm.cancel_token.clone(),
        )
        .await?
        .map(VmValue::sync_permit)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.runtime.sync_release",
    effects = ["state.mutate@arg0"],
    sig = "sync_release(permit: any) -> bool",
    category = "concurrency",
    doc = "Release a synchronization permit."
)]
fn sync_release_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::SyncPermit(permit)) = args.first() else {
        return Err(VmError::Runtime(
            "sync_release: first argument must be a sync permit".to_string(),
        ));
    };
    Ok(VmValue::Bool(permit.release()))
}

#[harn_builtin(
    exposure = "harness.runtime.sync_metrics",
    effects = ["state.read@arg0"],
    sig = "sync_metrics(kind?: string, key?: string) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Return synchronization runtime metrics."
)]
async fn sync_metrics_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "sync_metrics");
    let kind = args.first().map(|v| v.display());
    let key = args.get(1).map(|v| v.display());
    Ok(vm.sync_runtime.metrics(kind.as_deref(), key.as_deref()))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_scope_id",
    effects = ["state.read@const=execution-scope"],
    sig = "shared_scope_id(scope?: any, options?: dict) -> string",
    kind = "async",
    category = "concurrency",
    doc = "Resolve a shared-state scope identifier."
)]
async fn shared_scope_id_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_scope_id");
    let options = args.get(1).and_then(VmValue::as_dict);
    let raw_scope = args.first().map(VmValue::display);
    Ok(VmValue::String(arcstr::ArcStr::from(resolve_shared_scope(
        &vm,
        raw_scope.as_deref(),
        options,
        "shared_scope_id",
    )?)))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_cell",
    effects = ["state.mutate@const=shared-state"],
    sig = "shared_cell(options_or_key?: any, initial?: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Open or create a scoped shared cell."
)]
async fn shared_cell_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_cell");
    let shared_runtime = vm.shared_state_runtime.clone();
    let (scoped, options) = scoped_from_open_args(&vm, &args, "shared_cell", "key")?;
    let initial = options
        .as_ref()
        .and_then(|opts| opts.get("initial").cloned())
        .or_else(|| args.get(1).cloned())
        .unwrap_or(VmValue::Nil);
    Ok(shared_runtime.open_cell(scoped, initial))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_get",
    effects = ["state.read@arg0"],
    sig = "shared_get(handle: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Read a shared cell value."
)]
async fn shared_get_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_get");
    let shared_runtime = vm.shared_state_runtime.clone();
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime("shared_get: handle is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, handle, "shared_cell", "shared_get")?;
    Ok(shared_runtime.cell_get(&scoped))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_snapshot",
    effects = ["state.read@arg0"],
    sig = "shared_snapshot(handle: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Return a shared cell snapshot."
)]
async fn shared_snapshot_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_snapshot");
    let shared_runtime = vm.shared_state_runtime.clone();
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime("shared_snapshot: handle is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, handle, "shared_cell", "shared_snapshot")?;
    Ok(shared_runtime.cell_snapshot(&scoped))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_set",
    effects = ["state.write@arg0"],
    sig = "shared_set(handle: any, value: any) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Set a shared cell value."
)]
async fn shared_set_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_set");
    let shared_runtime = vm.shared_state_runtime.clone();
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime("shared_set: handle is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, handle, "shared_cell", "shared_set")?;
    let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    Ok(shared_runtime.cell_set(&scoped, value))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_cas",
    effects = ["state.mutate@arg0"],
    sig = "shared_cas(handle: any, expected: any, value: any) -> bool",
    kind = "async",
    category = "concurrency",
    doc = "Compare and swap a shared cell value."
)]
async fn shared_cas_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 3 {
        return Err(VmError::Runtime(
            "shared_cas: requires handle, expected, and new value".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_cas");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_cell", "shared_cas")?;
    Ok(VmValue::Bool(shared_runtime.cell_cas(
        &scoped,
        &args[1],
        args[2].clone(),
    )))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map",
    effects = ["state.mutate@const=shared-state"],
    sig = "shared_map(options_or_key?: any, initial?: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Open or create a scoped shared map."
)]
async fn shared_map_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_map");
    let shared_runtime = vm.shared_state_runtime.clone();
    let (scoped, options) = scoped_from_open_args(&vm, &args, "shared_map", "key")?;
    let initial = options
        .as_ref()
        .and_then(|opts| opts.get("initial"))
        .or_else(|| args.get(1))
        .and_then(VmValue::as_dict)
        .cloned();
    Ok(shared_runtime.open_map(scoped, initial))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_get",
    effects = ["state.read@arg0"],
    sig = "shared_map_get(handle: any, key: any, default?: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Read a shared map entry."
)]
async fn shared_map_get_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "shared_map_get: requires handle and key".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_map_get");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_map", "shared_map_get")?;
    let default = args.get(2).cloned().unwrap_or(VmValue::Nil);
    Ok(shared_runtime.map_get(&scoped, &args[1].display(), default))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_snapshot",
    effects = ["state.read@arg0"],
    sig = "shared_map_snapshot(handle: any, key: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Return a shared map entry snapshot."
)]
async fn shared_map_snapshot_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "shared_map_snapshot: requires handle and key".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_map_snapshot");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_map", "shared_map_snapshot")?;
    Ok(shared_runtime.map_snapshot(&scoped, &args[1].display()))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_entries",
    effects = ["state.read@arg0"],
    sig = "shared_map_entries(handle: any) -> list",
    kind = "async",
    category = "concurrency",
    doc = "Return all shared map entries."
)]
async fn shared_map_entries_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_map_entries");
    let shared_runtime = vm.shared_state_runtime.clone();
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime("shared_map_entries: handle is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, handle, "shared_map", "shared_map_entries")?;
    Ok(shared_runtime.map_entries(&scoped))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_set",
    effects = ["state.write@arg0"],
    sig = "shared_map_set(handle: any, key: any, value: any) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Set a shared map entry."
)]
async fn shared_map_set_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 3 {
        return Err(VmError::Runtime(
            "shared_map_set: requires handle, key, and value".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_map_set");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_map", "shared_map_set")?;
    Ok(shared_runtime.map_set(&scoped, args[1].display(), args[2].clone()))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_delete",
    effects = ["state.mutate@arg0"],
    sig = "shared_map_delete(handle: any, key: any) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Delete a shared map entry."
)]
async fn shared_map_delete_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "shared_map_delete: requires handle and key".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_map_delete");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_map", "shared_map_delete")?;
    Ok(shared_runtime.map_delete(&scoped, &args[1].display()))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_map_cas",
    effects = ["state.mutate@arg0"],
    sig = "shared_map_cas(handle: any, key: any, expected: any, value: any) -> bool",
    kind = "async",
    category = "concurrency",
    doc = "Compare and swap a shared map entry."
)]
async fn shared_map_cas_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 4 {
        return Err(VmError::Runtime(
            "shared_map_cas: requires handle, key, expected, and new value".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "shared_map_cas");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "shared_map", "shared_map_cas")?;
    Ok(VmValue::Bool(shared_runtime.map_cas(
        &scoped,
        args[1].display(),
        &args[2],
        args[3].clone(),
    )))
}

#[harn_builtin(
    exposure = "harness.runtime.shared_metrics",
    effects = ["state.read@const=shared-state"],
    sig = "shared_metrics(handle?: any) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Return shared-state runtime metrics."
)]
async fn shared_metrics_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "shared_metrics");
    let shared_runtime = vm.shared_state_runtime.clone();
    let Some(handle) = args.first() else {
        return Ok(shared_runtime.metrics(None, None));
    };
    let Some(dict) = handle.as_dict() else {
        return Ok(shared_runtime.metrics(None, None));
    };
    let kind = dict_string(dict, "_type")
        .ok_or_else(|| VmError::Runtime("shared_metrics: handle missing _type".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, handle, &kind, "shared_metrics")?;
    Ok(shared_runtime.metrics(Some(&kind), Some(&scoped)))
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_open",
    effects = ["state.mutate@arg0"],
    sig = "mailbox_open(options_or_name?: any, capacity?: int) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Open or create a scoped mailbox."
)]
async fn mailbox_open_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_open");
    let shared_runtime = vm.shared_state_runtime.clone();
    let (scoped, options) = scoped_from_open_args(&vm, &args, "mailbox_open", "name")?;
    let capacity_arg = options
        .as_ref()
        .and_then(|opts| opts.get("capacity"))
        .or_else(|| args.get(1));
    let capacity = optional_positive_usize_arg(capacity_arg, 256, "mailbox_open", "capacity")?;
    Ok(shared_runtime.open_mailbox(scoped, capacity))
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_lookup",
    effects = ["state.read@arg0"],
    sig = "mailbox_lookup(target: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Look up a scoped mailbox handle."
)]
async fn mailbox_lookup_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_lookup");
    let shared_runtime = vm.shared_state_runtime.clone();
    let target = args.first().ok_or_else(|| {
        VmError::Runtime("mailbox_lookup: name or handle is required".to_string())
    })?;
    let scoped = scoped_from_handle_or_name(&vm, target, "mailbox", "mailbox_lookup")?;
    Ok(shared_runtime.mailbox(&scoped).unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_send",
    effects = ["state.write@arg0"],
    sig = "mailbox_send(target: any, value: any) -> bool",
    kind = "async",
    category = "concurrency",
    doc = "Send a value to a mailbox."
)]
async fn mailbox_send_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "mailbox_send: requires target and value".to_string(),
        ));
    }
    let vm = current_async_vm(&ctx, "mailbox_send");
    let shared_runtime = vm.shared_state_runtime.clone();
    let scoped = scoped_from_handle_or_name(&vm, &args[0], "mailbox", "mailbox_send")?;
    let Some(channel) = shared_runtime.mailbox_channel(&scoped) else {
        return Ok(VmValue::Bool(false));
    };
    if channel.is_closed() {
        shared_runtime.note_mailbox_send(&scoped, false);
        return Ok(VmValue::Bool(false));
    }
    let ok = channel.sender.send(args[1].clone()).await.is_ok();
    shared_runtime.note_mailbox_send(&scoped, ok);
    Ok(VmValue::Bool(ok))
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_try_receive",
    effects = ["state.mutate@arg0"],
    sig = "mailbox_try_receive(target: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Try to receive one mailbox value without blocking."
)]
async fn mailbox_try_receive_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_try_receive");
    let shared_runtime = vm.shared_state_runtime.clone();
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("mailbox_try_receive: target is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, target, "mailbox", "mailbox_try_receive")?;
    let Some(channel) = shared_runtime.mailbox_channel(&scoped) else {
        return Ok(VmValue::Nil);
    };
    let Ok(mut rx) = channel.receiver.try_lock() else {
        return Ok(VmValue::Nil);
    };
    match rx.try_recv() {
        Ok(value) => {
            shared_runtime.note_mailbox_receive(&scoped);
            Ok(value)
        }
        Err(_) => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_receive",
    effects = ["state.observe@arg0", "state.mutate@arg0"],
    sig = "mailbox_receive(target: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Receive one mailbox value."
)]
async fn mailbox_receive_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_receive");
    let shared_runtime = vm.shared_state_runtime.clone();
    let cancel_token = vm.cancel_token.clone();
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("mailbox_receive: target is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, target, "mailbox", "mailbox_receive")?;
    let Some(channel) = shared_runtime.mailbox_channel(&scoped) else {
        return Ok(VmValue::Nil);
    };
    loop {
        if cancel_token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            return Err(cancelled_vm_error());
        }
        if channel.is_closed() {
            let mut rx = channel.receiver.lock().await;
            return match rx.try_recv() {
                Ok(value) => {
                    shared_runtime.note_mailbox_receive(&scoped);
                    Ok(value)
                }
                Err(_) => Ok(VmValue::Nil),
            };
        }
        let mut rx = channel.receiver.lock().await;
        let poll = tokio::time::sleep(tokio::time::Duration::from_millis(10));
        tokio::pin!(poll);
        tokio::select! {
            value = rx.recv() => {
                return match value {
                    Some(value) => {
                        shared_runtime.note_mailbox_receive(&scoped);
                        Ok(value)
                    }
                    None => Ok(VmValue::Nil),
                };
            }
            _ = &mut poll => {}
        }
    }
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_close",
    effects = ["state.mutate@arg0"],
    sig = "mailbox_close(target: any) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Close a scoped mailbox."
)]
async fn mailbox_close_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_close");
    let shared_runtime = vm.shared_state_runtime.clone();
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("mailbox_close: target is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, target, "mailbox", "mailbox_close")?;
    Ok(VmValue::Bool(shared_runtime.close_mailbox(&scoped)))
}

#[harn_builtin(
    exposure = "harness.runtime.mailbox_metrics",
    effects = ["state.read@arg0"],
    sig = "mailbox_metrics(target: any) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Return metrics for a scoped mailbox."
)]
async fn mailbox_metrics_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = current_async_vm(&ctx, "mailbox_metrics");
    let shared_runtime = vm.shared_state_runtime.clone();
    let target = args
        .first()
        .ok_or_else(|| VmError::Runtime("mailbox_metrics: target is required".to_string()))?;
    let scoped = scoped_from_handle_or_name(&vm, target, "mailbox", "mailbox_metrics")?;
    Ok(shared_runtime.metrics(Some("mailbox"), Some(&scoped)))
}

#[harn_builtin(
    exposure = "harness.runtime.channel",
    effects = ["state.mutate@const=channels"],
    sig = "channel(name?: string, capacity?: int) -> any",
    category = "concurrency",
    doc = "Create an in-memory channel."
)]
fn channel_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "default".to_string());
    let capacity = optional_positive_usize_arg(args.get(1), 256, "channel", "capacity")?;
    let (tx, rx) = tokio::sync::mpsc::channel(capacity);
    Ok(VmValue::channel(VmChannelHandle {
        name: std::sync::Arc::from(name),
        sender: Arc::new(tx),
        receiver: Arc::new(tokio::sync::Mutex::new(rx)),
        close: Arc::new(VmChannelCloseState::open()),
    }))
}

#[harn_builtin(
    exposure = "harness.runtime.close_channel",
    effects = ["state.mutate@arg0"],
    sig = "close_channel(channel: any) -> nil",
    category = "concurrency",
    doc = "Mark a channel closed."
)]
fn close_channel_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "close_channel: requires a channel",
        ))));
    }
    if let VmValue::Channel(ch) = &args[0] {
        ch.close();
        if let Ok(mut rx) = ch.receiver.try_lock() {
            rx.close();
        }
        Ok(VmValue::Nil)
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "close_channel: first argument must be a channel",
        ))))
    }
}

#[harn_builtin(
    exposure = "harness.runtime.channel_is_closed",
    effects = ["state.read@arg0"],
    sig = "channel_is_closed(channel: any) -> bool",
    category = "concurrency",
    doc = "Return true if the channel has been marked closed."
)]
fn channel_is_closed_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first() {
        Some(VmValue::Channel(ch)) => Ok(VmValue::Bool(ch.is_closed())),
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "channel_is_closed: first argument must be a channel",
        )))),
    }
}

#[harn_builtin(
    exposure = "harness.runtime.try_receive",
    effects = ["state.mutate@arg0"],
    sig = "try_receive(channel: any) -> any",
    category = "concurrency",
    doc = "Try to receive one channel value without blocking."
)]
fn try_receive_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "try_receive: requires a channel",
        ))));
    }
    if let VmValue::Channel(ch) = &args[0] {
        match ch.receiver.try_lock() {
            Ok(mut rx) => match rx.try_recv() {
                Ok(val) => Ok(val),
                Err(_) => Ok(VmValue::Nil),
            },
            Err(_) => Ok(VmValue::Nil),
        }
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "try_receive: first argument must be a channel",
        ))))
    }
}

#[harn_builtin(
    exposure = "harness.runtime.atomic",
    effects = ["state.mutate@const=atomics"],
    sig = "atomic(initial?: any) -> any",
    category = "concurrency",
    doc = "Create an atomic integer handle."
)]
fn atomic_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let initial = match args.first() {
        Some(VmValue::Int(n)) => *n,
        Some(VmValue::Float(f)) => *f as i64,
        Some(VmValue::Bool(b)) => i64::from(*b),
        _ => 0,
    };
    Ok(VmValue::atomic(VmAtomicHandle {
        value: Arc::new(AtomicI64::new(initial)),
    }))
}

#[harn_builtin(
    exposure = "harness.runtime.atomic_get",
    effects = ["state.read@arg0"],
    sig = "atomic_get(handle: any) -> int",
    category = "concurrency",
    doc = "Read an atomic integer value."
)]
fn atomic_get_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if let Some(VmValue::Atomic(a)) = args.first() {
        Ok(VmValue::Int(a.value.load(Ordering::SeqCst)))
    } else {
        Ok(VmValue::Nil)
    }
}

#[harn_builtin(
    exposure = "harness.runtime.atomic_set",
    effects = ["state.write@arg0"],
    sig = "atomic_set(handle: any, value: int) -> int",
    category = "concurrency",
    doc = "Set an atomic integer and return the previous value."
)]
fn atomic_set_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        if let (VmValue::Atomic(a), Some(val)) = (&args[0], args[1].as_int()) {
            let old = a.value.swap(val, Ordering::SeqCst);
            return Ok(VmValue::Int(old));
        }
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.atomic_add",
    effects = ["state.mutate@arg0"],
    sig = "atomic_add(handle: any, delta: int) -> int",
    category = "concurrency",
    doc = "Add to an atomic integer and return the previous value."
)]
fn atomic_add_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        if let (VmValue::Atomic(a), Some(delta)) = (&args[0], args[1].as_int()) {
            let prev = a.value.fetch_add(delta, Ordering::SeqCst);
            return Ok(VmValue::Int(prev));
        }
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.atomic_cas",
    effects = ["state.mutate@arg0"],
    sig = "atomic_cas(handle: any, expected: int, value: int) -> bool",
    category = "concurrency",
    doc = "Compare and swap an atomic integer."
)]
fn atomic_cas_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 3 {
        if let (VmValue::Atomic(a), Some(expected), Some(new_val)) =
            (&args[0], args[1].as_int(), args[2].as_int())
        {
            let result =
                a.value
                    .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst);
            return Ok(VmValue::Bool(result.is_ok()));
        }
    }
    Ok(VmValue::Bool(false))
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sleep(ms?: any) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Suspend execution for a duration in milliseconds."
)]
async fn sleep_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let ms = match args.first() {
        Some(value) if is_negative_millis_value(value) => 0,
        Some(value) => Args::single("sleep", ErrorKind::Runtime, Some(value)).millis(0, "ms")?,
        _ => 0,
    };
    if ms == 0 {
        return Ok(VmValue::Nil);
    }
    if crate::stdlib::clock::is_mocked() {
        crate::stdlib::clock::advance(ms as i64);
        return Ok(VmValue::Nil);
    }
    let sleep = tokio::time::sleep(tokio::time::Duration::from_millis(ms));
    tokio::pin!(sleep);
    // The explicit ctx always carries a child VM, so we always poll for
    // cancellation while sleeping rather than blocking opaquely.
    let vm = ctx.child_vm();
    let mut poll = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
            _ = &mut sleep => break,
            _ = poll.tick() => {
                if vm.is_cancel_requested() {
                    return Err(cancelled_vm_error());
                }
            }
        }
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.yield_now",
    effects = ["clock.observe@const=scheduler"],
    sig = "yield_now() -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Yield cooperatively to other scheduled tasks."
)]
async fn yield_now_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    tokio::task::yield_now().await;
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.send",
    effects = ["state.write@arg0"],
    sig = "send(channel: any, value: any) -> bool",
    kind = "async",
    category = "concurrency",
    doc = "Send a value to a channel."
)]
async fn send_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "send: requires channel and value",
        ))));
    }
    if let VmValue::Channel(ch) = &args[0] {
        let vm = current_async_vm(&ctx, "send");
        let target = channel_target(ch);
        let mut closed_rx = ch.subscribe_closed();
        if ch.is_closed() || *closed_rx.borrow() {
            return Err(channel_closed_error("send", ch.name.as_ref()));
        }
        let val = args[1].clone();
        let val = match ch.sender.try_send(val) {
            Ok(()) => {
                vm.wait_for_graph.notify_channel_send(&target);
                return Ok(VmValue::Bool(true));
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(val)) => val,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                return Err(channel_closed_error("send", ch.name.as_ref()));
            }
        };
        let _wait = channel_send_wait(&vm, target.clone())?;
        tokio::select! {
            biased;
            _ = closed_rx.changed() => Err(channel_closed_error("send", ch.name.as_ref())),
            result = ch.sender.send(val) => {
                match result {
                    Ok(()) => {
                        vm.wait_for_graph.notify_channel_send(&target);
                        Ok(VmValue::Bool(true))
                    }
                    Err(_) => Err(channel_closed_error("send", ch.name.as_ref())),
                }
            }
        }
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "send: first argument must be a channel",
        ))))
    }
}

#[harn_builtin(
    exposure = "harness.runtime.receive",
    effects = ["state.observe@arg0", "state.mutate@arg0"],
    sig = "receive(channel: any) -> any",
    kind = "async",
    category = "concurrency",
    doc = "Receive one value from a channel."
)]
async fn receive_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "receive: requires a channel",
        ))));
    }
    if let VmValue::Channel(ch) = &args[0] {
        let vm = current_async_vm(&ctx, "receive");
        let target = channel_target(ch);
        let mut closed_rx = ch.subscribe_closed();
        if ch.is_closed() || *closed_rx.borrow() {
            let mut rx = ch.receiver.lock().await;
            return match rx.try_recv() {
                Ok(val) => {
                    vm.wait_for_graph.notify_channel_receive(&target);
                    Ok(val)
                }
                Err(_) => Err(channel_closed_error("receive", ch.name.as_ref())),
            };
        }
        let mut wait = None;
        let mut rx = match ch.receiver.try_lock() {
            Ok(rx) => rx,
            Err(_) => {
                wait = channel_receive_wait(&vm, std::slice::from_ref(&args[0]))?;
                ch.receiver.lock().await
            }
        };
        match rx.try_recv() {
            Ok(val) => {
                vm.wait_for_graph.notify_channel_receive(&target);
                return Ok(val);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(channel_closed_error("receive", ch.name.as_ref()));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        }
        if ch.is_closed() || *closed_rx.borrow() {
            return Err(channel_closed_error("receive", ch.name.as_ref()));
        }
        let _wait = match wait {
            Some(wait) => Some(wait),
            None => channel_receive_wait(&vm, std::slice::from_ref(&args[0]))?,
        };
        let mut cancel_poll = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                value = rx.recv() => {
                    return match value {
                        Some(val) => {
                            vm.wait_for_graph.notify_channel_receive(&target);
                            Ok(val)
                        }
                        None => Err(channel_closed_error("receive", ch.name.as_ref())),
                    };
                }
                _ = closed_rx.changed() => {
                    return match rx.try_recv() {
                        Ok(val) => {
                            vm.wait_for_graph.notify_channel_receive(&target);
                            Ok(val)
                        }
                        Err(_) => Err(channel_closed_error("receive", ch.name.as_ref())),
                    };
                }
                _ = cancel_poll.tick() => {
                    if vm.is_cancel_requested() {
                        return Err(cancelled_vm_error());
                    }
                }
            }
        }
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "receive: first argument must be a channel",
        ))))
    }
}

#[harn_builtin(
    exposure = "harness.runtime.select",
    effects = ["state.observe@dynamic", "state.mutate@dynamic"],
    sig = "select(...channels: any) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Wait until one of the provided channels yields a value."
)]
async fn select_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "select: requires at least one channel",
        ))));
    }
    for arg in &args {
        if !matches!(arg, VmValue::Channel(_)) {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "select: all arguments must be channels",
            ))));
        }
    }
    let vm = current_async_vm(&ctx, "select");
    let mut wait = None;
    loop {
        let (found, all_closed) = try_poll_channels(&args);
        if let Some((i, val, name)) = found {
            return Ok(select_result(i, val, &name));
        }
        if all_closed {
            return Ok(select_none());
        }
        if wait.is_none() {
            wait = channel_receive_wait(&vm, &args)?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
    }
}

#[harn_builtin(
    exposure = "harness.runtime.select_timeout",
    effects = ["clock.observe@const=monotonic", "state.observe@dynamic"],
    sig = "__select_timeout(channels: list, timeout: any) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Select from a channel list with a timeout."
)]
async fn select_timeout_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "__select_timeout: requires channel list and timeout",
        ))));
    }
    let channels = match &args[0] {
        VmValue::List(items) => (**items).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__select_timeout: first argument must be a list of channels",
            ))));
        }
    };
    let timeout_ms = optional_timeout_scalar_ms(&args[1]).unwrap_or(5000);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    loop {
        let (found, all_closed) = try_poll_channels(&channels);
        if let Some((i, val, name)) = found {
            return Ok(select_result(i, val, &name));
        }
        if all_closed || tokio::time::Instant::now() >= deadline {
            return Ok(select_none());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
    }
}

#[harn_builtin(
    exposure = "harness.runtime.select_try",
    effects = ["state.mutate@dynamic"],
    sig = "__select_try(channels: list) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Select from a channel list without blocking."
)]
async fn select_try_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "__select_try: requires channel list",
        ))));
    }
    let channels = match &args[0] {
        VmValue::List(items) => (**items).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__select_try: first argument must be a list of channels",
            ))));
        }
    };
    let (found, _) = try_poll_channels(&channels);
    if let Some((i, val, name)) = found {
        Ok(select_result(i, val, &name))
    } else {
        Ok(select_none())
    }
}

#[harn_builtin(
    exposure = "harness.runtime.select_list",
    effects = ["state.mutate@dynamic"],
    sig = "__select_list(channels: list) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Wait until one channel in a list yields a value."
)]
async fn select_list_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "__select_list: requires channel list",
        ))));
    }
    let channels = match &args[0] {
        VmValue::List(items) => (**items).clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "__select_list: first argument must be a list of channels",
            ))));
        }
    };
    let vm = current_async_vm(&ctx, "__select_list");
    let mut wait = None;
    loop {
        let (found, all_closed) = try_poll_channels(&channels);
        if let Some((i, val, name)) = found {
            return Ok(select_result(i, val, &name));
        }
        if all_closed {
            return Ok(select_none());
        }
        if wait.is_none() {
            wait = channel_receive_wait(&vm, &channels)?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
    }
}

#[harn_builtin(
    exposure = "harness.runtime.channel_select",
    effects = ["state.observe@dynamic", "state.mutate@dynamic"],
    sig = "channel_select(channels: list, timeout_ms?: int) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Select over a list of channels with an optional timeout."
)]
async fn channel_select_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let channels = require_channel_list(&args, "channel_select")?;
    let timeout_ms = optional_timeout_ms(args.get(1));
    let deadline =
        timeout_ms.map(|ms| tokio::time::Instant::now() + tokio::time::Duration::from_millis(ms));
    let vm = current_async_vm(&ctx, "channel_select");
    let mut wait = None;
    loop {
        let (found, all_closed) = try_poll_channels(&channels);
        if let Some((i, val, name)) = found {
            return Ok(select_result(i, val, &name));
        }
        if all_closed {
            return Ok(VmValue::Nil);
        }
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Ok(VmValue::Nil);
        }
        if deadline.is_none() && wait.is_none() {
            wait = channel_receive_wait(&vm, &channels)?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
    }
}

#[harn_builtin(
    exposure = "harness.runtime.timer_start",
    effects = ["clock.read@const=monotonic"],
    sig = "timer_start(name?: string) -> dict",
    category = "concurrency",
    doc = "Start a named timer and return its handle."
)]
fn timer_start_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "default".to_string());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let mut timer = BTreeMap::new();
    timer.put_str("name", name);
    timer.insert("start_ms".to_string(), VmValue::Int(now_ms));
    Ok(VmValue::dict(timer))
}

#[harn_builtin(
    exposure = "harness.runtime.circuit_breaker",
    effects = ["state.mutate@const=circuits"],
    sig = "circuit_breaker(name: string, threshold?: int, reset_ms?: int) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Create or reset a named circuit breaker."
)]
async fn circuit_breaker_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = circuit_name(&args, "circuit_breaker")?;
    let threshold = optional_positive_usize_arg(args.get(1), 5, "circuit_breaker", "threshold")?;
    let reset_ms =
        optional_non_negative_u64_arg(args.get(2), 30000, "circuit_breaker", "reset_ms")?;
    ctx.child_vm().shared_state_runtime.configure_circuit(
        name.clone(),
        threshold,
        Duration::from_millis(reset_ms),
    );
    Ok(VmValue::String(arcstr::ArcStr::from(name)))
}

fn optional_positive_usize_arg(
    value: Option<&VmValue>,
    default: usize,
    builtin: &str,
    key: &str,
) -> Result<usize, VmError> {
    let reader = Args::single(builtin, ErrorKind::Runtime, value);
    let Some(raw) = reader.opt_int(0, key)? else {
        return Ok(default);
    };
    usize::try_from(raw)
        .ok()
        .filter(|value| *value >= 1)
        .ok_or_else(|| ArgError::constraint(builtin, ErrorKind::Runtime, key, "must be >= 1"))
}

fn optional_non_negative_u64_arg(
    value: Option<&VmValue>,
    default: u64,
    builtin: &str,
    key: &str,
) -> Result<u64, VmError> {
    let reader = Args::single(builtin, ErrorKind::Runtime, value);
    let Some(raw) = reader.opt_int(0, key)? else {
        return Ok(default);
    };
    u64::try_from(raw)
        .map_err(|_| ArgError::constraint(builtin, ErrorKind::Runtime, key, "must be >= 0"))
}

fn circuit_name(args: &[VmValue], builtin: &str) -> Result<String, VmError> {
    Ok(Args::runtime(builtin, args).string(0, "name")?.to_owned())
}

#[harn_builtin(
    exposure = "harness.runtime.circuit_check",
    effects = ["state.read@arg0", "clock.read@const=monotonic"],
    sig = "circuit_check(name: string) -> string",
    kind = "async",
    category = "concurrency",
    doc = "Return the state of a named circuit breaker."
)]
async fn circuit_check_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = circuit_name(&args, "circuit_check")?;
    let state = ctx
        .child_vm()
        .shared_state_runtime
        .circuit_status(&name, std::time::Instant::now());
    Ok(VmValue::String(arcstr::ArcStr::from(state.as_str())))
}

#[harn_builtin(
    exposure = "harness.runtime.circuit_record_success",
    effects = ["state.mutate@arg0"],
    sig = "circuit_record_success(name: string) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Record a successful call for a named circuit breaker."
)]
async fn circuit_record_success_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = circuit_name(&args, "circuit_record_success")?;
    ctx.child_vm()
        .shared_state_runtime
        .record_circuit_success(&name);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.circuit_record_failure",
    effects = ["state.mutate@arg0", "clock.read@const=monotonic"],
    sig = "circuit_record_failure(name: string) -> bool",
    kind = "async",
    category = "concurrency",
    doc = "Record a failed call and return whether the circuit opened."
)]
async fn circuit_record_failure_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = circuit_name(&args, "circuit_record_failure")?;
    let is_open = ctx
        .child_vm()
        .shared_state_runtime
        .record_circuit_failure(&name, std::time::Instant::now());
    Ok(VmValue::Bool(is_open))
}

#[harn_builtin(
    exposure = "harness.runtime.circuit_reset",
    effects = ["state.mutate@arg0"],
    sig = "circuit_reset(name: string) -> nil",
    kind = "async",
    category = "concurrency",
    doc = "Reset a named circuit breaker to closed."
)]
async fn circuit_reset_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = circuit_name(&args, "circuit_reset")?;
    ctx.child_vm().shared_state_runtime.reset_circuit(&name);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.timer_end",
    effects = ["clock.read@const=monotonic", "state.mutate@arg0"],
    sig = "timer_end(timer: any) -> int",
    category = "concurrency",
    doc = "End a timer, print elapsed milliseconds, and return the elapsed time."
)]
fn timer_end_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let timer = match args.first() {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "timer_end: argument must be a timer dict from timer_start",
            ))));
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let start_ms = timer
        .get("start_ms")
        .and_then(|v| v.as_int())
        .unwrap_or(now_ms);
    let elapsed = now_ms - start_ms;
    let name = timer.get("name").map(|v| v.display()).unwrap_or_default();
    out.push_str(&format!("[timer] {name}: {elapsed}ms\n"));
    Ok(VmValue::Int(elapsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_concurrency_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(v))
    }

    #[test]
    fn channel_rejects_invalid_capacity() {
        let mut vm = vm();
        let err = call(&mut vm, "channel", vec![s("bad_channel"), VmValue::Int(0)])
            .expect_err("zero capacity must fail");
        assert!(err.to_string().contains("capacity"));
    }

    #[test]
    fn closed_empty_channel_counts_as_closed_for_select() {
        let mut vm = vm();
        let channel = call(&mut vm, "channel", vec![s("done")]).unwrap();
        call(&mut vm, "close_channel", vec![channel.clone()]).unwrap();
        let (ready, all_closed) = try_poll_channels(&[channel]);
        assert!(ready.is_none());
        assert!(all_closed);
    }

    #[test]
    fn atomic_default_zero() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![]).unwrap();
        let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(val.display(), "0");
    }

    #[test]
    fn atomic_initial_value() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Int(42)]).unwrap();
        let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(val.display(), "42");
    }

    #[test]
    fn atomic_set_returns_old() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Int(10)]).unwrap();
        let old = call(&mut vm, "atomic_set", vec![atom.clone(), VmValue::Int(20)]).unwrap();
        assert_eq!(old.display(), "10");
        let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(cur.display(), "20");
    }

    #[test]
    fn atomic_add() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Int(5)]).unwrap();
        let prev = call(&mut vm, "atomic_add", vec![atom.clone(), VmValue::Int(3)]).unwrap();
        assert_eq!(prev.display(), "5");
        let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(cur.display(), "8");
    }

    #[test]
    fn atomic_cas_success() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Int(10)]).unwrap();
        let ok = call(
            &mut vm,
            "atomic_cas",
            vec![atom.clone(), VmValue::Int(10), VmValue::Int(20)],
        )
        .unwrap();
        assert_eq!(ok.display(), "true");
        let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(cur.display(), "20");
    }

    #[test]
    fn atomic_cas_failure() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Int(10)]).unwrap();
        let ok = call(
            &mut vm,
            "atomic_cas",
            vec![atom.clone(), VmValue::Int(99), VmValue::Int(20)],
        )
        .unwrap();
        assert_eq!(ok.display(), "false");
        let cur = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(cur.display(), "10");
    }

    #[test]
    fn atomic_bool_init() {
        let mut vm = vm();
        let atom = call(&mut vm, "atomic", vec![VmValue::Bool(true)]).unwrap();
        let val = call(&mut vm, "atomic_get", vec![atom]).unwrap();
        assert_eq!(val.display(), "1");
    }

    #[test]
    fn timer_start_returns_dict() {
        let mut vm = vm();
        let timer = call(&mut vm, "timer_start", vec![s("my_timer")]).unwrap();
        let dict = timer.as_dict().unwrap();
        assert_eq!(dict.get("name").unwrap().display(), "my_timer");
        assert!(dict.get("start_ms").unwrap().as_int().unwrap() > 0);
    }

    #[test]
    fn timer_end_returns_elapsed() {
        let mut vm = vm();
        let timer = call(&mut vm, "timer_start", vec![s("t")]).unwrap();
        let elapsed = call(&mut vm, "timer_end", vec![timer]).unwrap();
        assert!(elapsed.as_int().unwrap() >= 0);
        assert!(elapsed.as_int().unwrap() < 1000);
    }

    #[test]
    fn timer_end_non_dict_errors() {
        let mut vm = vm();
        let result = call(&mut vm, "timer_end", vec![VmValue::Int(42)]);
        assert!(result.is_err());
    }
}
