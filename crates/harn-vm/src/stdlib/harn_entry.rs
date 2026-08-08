//! Typed Rust-to-Harn calls for stdlib implementations.
//!
//! Harn exports are ordinary imported functions, never ambient runtime
//! builtins. Rust runtime code that owns an orchestration seam may call a
//! specific export through this module without installing a second source
//! namespace.

use serde::de::DeserializeOwned;

use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

pub(crate) async fn call_harn_export_by_name(
    ctx: &AsyncBuiltinCtx,
    import_path: &str,
    export_name: &str,
    label: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let mut vm = ctx.child_vm();
    let result = call_harn_export_on_vm(&mut vm, import_path, export_name, label, args).await;
    let output = vm.take_output();
    ctx.forward_output(&output);
    result
}

pub(crate) async fn call_harn_export_on_vm(
    vm: &mut crate::vm::Vm,
    import_path: &str,
    export_name: &str,
    label: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let saved_env = std::mem::take(&mut vm.env);
    let saved_imported_paths = std::mem::take(&mut vm.imported_paths);
    let saved_source_dir = vm.source_dir.clone();
    let exports = vm.load_module_exports_from_import(import_path).await;
    vm.env = saved_env;
    vm.imported_paths = saved_imported_paths;
    vm.source_dir = saved_source_dir;
    let exports = exports?;
    let closure = exports.get(export_name).cloned().ok_or_else(|| {
        VmError::Runtime(format!(
            "{label}: stdlib module {import_path} did not export `{export_name}`"
        ))
    })?;
    vm.call_closure_pub(&closure, args).await
}

pub(crate) async fn call_agent_loop(
    ctx: &AsyncBuiltinCtx,
    prompt: String,
    system: Option<String>,
    options: crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let harness = ctx.child_vm().root_harness_value().ok_or_else(|| {
        VmError::Runtime(
            "workflow_stage_agent_loop: execution has no root Harness authority".to_string(),
        )
    })?;
    call_harn_export_by_name(
        ctx,
        "std/agent/loop",
        "agent_loop",
        "workflow_stage_agent_loop",
        &[
            harness,
            VmValue::String(arcstr::ArcStr::from(prompt)),
            system
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
            VmValue::dict(options),
        ],
    )
    .await
}

pub(crate) async fn call_harn_export_json(
    ctx: &AsyncBuiltinCtx,
    import_path: &str,
    export_name: &str,
    label: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let result = call_harn_export_by_name(
        ctx,
        import_path,
        export_name,
        label,
        &[crate::stdlib::json_to_vm_value(&payload)],
    )
    .await?;
    Ok(crate::llm::vm_value_to_json(&result))
}

pub(crate) async fn call_harn_export_typed<T>(
    ctx: &AsyncBuiltinCtx,
    import_path: &str,
    export_name: &str,
    label: &str,
    payload: serde_json::Value,
) -> Result<T, VmError>
where
    T: DeserializeOwned,
{
    let result = call_harn_export_json(ctx, import_path, export_name, label, payload).await?;
    serde_json::from_value(result)
        .map_err(|error| VmError::Runtime(format!("{label} returned invalid shape: {error}")))
}
