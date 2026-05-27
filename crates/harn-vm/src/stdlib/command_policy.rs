use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_command_policy_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
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

#[harn_builtin(sig = "command_policy_pop() -> nil", category = "command_policy")]
fn command_policy_pop_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::orchestration::pop_command_policy();
    Ok(VmValue::Nil)
}

#[harn_builtin(
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
    sig = "command_llm_risk_scan(ctx: dict, options?: dict) -> dict",
    category = "command_policy"
)]
fn command_llm_risk_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ctx = args
        .first()
        .ok_or_else(|| VmError::Runtime("command_llm_risk_scan: ctx is required".to_string()))?;
    crate::orchestration::command_llm_risk_scan_value(ctx, args.get(1))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &COMMAND_POLICY_IMPL_DEF,
    &COMMAND_POLICY_PUSH_IMPL_DEF,
    &COMMAND_POLICY_POP_IMPL_DEF,
    &COMMAND_RISK_SCAN_IMPL_DEF,
    &COMMAND_RESULT_SCAN_IMPL_DEF,
    &COMMAND_LLM_RISK_SCAN_IMPL_DEF,
];
