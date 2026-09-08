use harn_builtin_meta::CapabilityId;

use super::{active_backend_available, active_backend_name};
use crate::orchestration::{current_execution_policy, SandboxProfile};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Register Harn-callable sandbox diagnostics and conformance builtins.
pub fn register_sandbox_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    vm.register_capability_method(
        CapabilityId::System,
        "sandbox_active_backend",
        sandbox_active_backend_impl,
    );
    vm.register_capability_method(
        CapabilityId::System,
        "sandbox_backend_available",
        sandbox_backend_available_impl,
    );
    vm.register_capability_method(
        CapabilityId::System,
        "sandbox_active_profile",
        sandbox_active_profile_impl,
    );
}

const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &SANDBOX_ACTIVE_BACKEND_IMPL_DEF,
    &SANDBOX_BACKEND_AVAILABLE_IMPL_DEF,
    &SANDBOX_ACTIVE_PROFILE_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sandbox_active_backend() -> string",
    category = "sandbox"
)]
fn sandbox_active_backend_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(active_backend_name())))
}

#[crate::stdlib::macros::harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sandbox_backend_available() -> bool",
    category = "sandbox"
)]
fn sandbox_backend_available_impl(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(active_backend_available()))
}

#[crate::stdlib::macros::harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sandbox_active_profile() -> string",
    category = "sandbox"
)]
fn sandbox_active_profile_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let profile = current_execution_policy()
        .map(|policy| policy.sandbox_profile)
        .unwrap_or(SandboxProfile::Unrestricted);
    Ok(VmValue::String(arcstr::ArcStr::from(profile.as_str())))
}
