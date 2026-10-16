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
    // Keep the child interpreter out of every composing export-call future.
    let mut vm = Box::new(ctx.child_vm());
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
    vm.call_module_export(import_path, export_name, label, args)
        .await
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
            VmValue::string(prompt),
            system.map(VmValue::string).unwrap_or(VmValue::Nil),
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

#[cfg(test)]
#[path = "harn_entry_tests.rs"]
mod tests;
