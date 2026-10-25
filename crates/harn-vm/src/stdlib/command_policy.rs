use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_command_policy_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "command_policy(config: dict) -> dict",
    category = "command_policy"
)]
fn command_policy_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_policy: config dict is required".to_string()))?;
    crate::orchestration::normalize_command_policy_value(config)
}

#[harn_builtin(
    exposure = "harness.runtime.command_policy_push",
    effects = ["state.mutate@const=command-policy"],
    sig = "command_policy_push(policy: dict) -> nil",
    category = "command_policy"
)]
fn command_policy_push_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let policy =
        crate::orchestration::parse_command_policy_value(args.first(), "command_policy_push")?
            .ok_or_else(|| {
                VmError::Runtime("command_policy_push: policy is required".to_string())
            })?;
    crate::orchestration::push_command_policy(policy);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.runtime.command_policy_pop",
    effects = ["state.mutate@const=command-policy"],
    sig = "command_policy_pop() -> nil", category = "command_policy"
)]
fn command_policy_pop_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::orchestration::pop_command_policy();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "pure",
    sig = "command_risk_scan(ctx: dict) -> dict",
    category = "command_policy"
)]
fn command_risk_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ctx = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_risk_scan: ctx is required".to_string()))?;
    crate::orchestration::command_risk_scan_value(ctx)
}

#[harn_builtin(
    exposure = "pure",
    sig = "command_result_scan(ctx: dict) -> dict",
    category = "command_policy"
)]
fn command_result_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ctx = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_result_scan: ctx is required".to_string()))?;
    crate::orchestration::command_result_scan_value(ctx)
}

#[harn_builtin(
    exposure = "pure",
    sig = "command_llm_risk_scan(ctx: dict, options?: dict) -> dict",
    category = "command_policy"
)]
fn command_llm_risk_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ctx = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_llm_risk_scan: ctx is required".to_string()))?;
    crate::orchestration::command_llm_risk_scan_value(ctx, args.get(1))
}

#[harn_builtin(
    exposure = "pure",
    sig = "command_workspace_effect(ctx: dict) -> dict",
    category = "command_policy"
)]
fn command_workspace_effect_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ctx = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_workspace_effect: ctx is required".to_string()))?;
    crate::orchestration::command_workspace_effect_value(ctx)
}
