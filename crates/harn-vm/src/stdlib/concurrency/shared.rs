use super::{
    current_async_vm, dict_string, resolve_shared_scope, scoped_from_handle_or_name,
    scoped_from_open_args,
};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Host, fn register_shared_builtins, rebinds_execution_state);

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
    Ok(VmValue::string(resolve_shared_scope(
        &vm,
        raw_scope.as_deref(),
        options,
        "shared_scope_id",
    )?))
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
    let shared_runtime = vm.shared_state().clone();
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
