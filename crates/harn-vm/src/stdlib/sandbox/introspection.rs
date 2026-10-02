use harn_builtin_meta::CapabilityId;

use super::{
    active_backend_available, active_backend_name, ActiveBackend, SandboxBackend,
    SandboxMechanismAvailability, SandboxMechanismUnavailable,
};
use crate::orchestration::{current_execution_policy, CapabilityPolicy, SandboxProfile};
use crate::value::{VmDictExt, VmError, VmValue};
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
    vm.register_capability_method(
        CapabilityId::System,
        "sandbox_confinement",
        sandbox_confinement_impl,
    );
}

const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &SANDBOX_ACTIVE_BACKEND_IMPL_DEF,
    &SANDBOX_BACKEND_AVAILABLE_IMPL_DEF,
    &SANDBOX_ACTIVE_PROFILE_IMPL_DEF,
    &SANDBOX_CONFINEMENT_IMPL_DEF,
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

/// Schema of the value [`host_confinement`] returns.
pub const SANDBOX_CONFINEMENT_SCHEMA: &str = "harn.process.sandbox_confinement.v1";

/// Whether this host can confine the processes Harn spawns, as one typed fact.
///
/// `sandbox_backend_available` reports `true` on every Linux host, because the
/// backend itself is always present; only the filesystem mechanism under it
/// (Landlock) may be missing. A host embedding Harn needs that narrower fact
/// before the first spawn, to tell a person why every confined command will be
/// refused. `os_hardened_refusal` is the exact value a `catch` observes when an
/// `os_hardened` spawn is refused for the missing mechanism, so a notice read
/// here and a refusal caught later under the same policy cannot disagree.
/// The refusal uses the current policy's dimensions with the hardened profile;
/// each spawn still checks its own policy independently.
pub fn host_confinement() -> VmValue {
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        ..current_execution_policy().unwrap_or_default()
    };
    confinement_for::<ActiveBackend>(&policy)
}

fn confinement_for<B: SandboxBackend>(policy: &CapabilityPolicy) -> VmValue {
    let mechanism = B::filesystem_mechanism();
    let mechanism_name = mechanism.as_str();
    let confines = B::filesystem_available();
    let refusal = if confines {
        VmValue::Nil
    } else {
        // Spawn checks the enforcement row before preparing the backend.
        // Preserve its policy-dependent dimensions on unconfined platforms.
        super::enforcement::refusal_for_mechanism(mechanism_name, policy)
            .unwrap_or_else(|| {
                SandboxMechanismUnavailable::new(
                    mechanism,
                    SandboxMechanismAvailability::AbsentOnHost,
                    SandboxProfile::OsHardened,
                )
            })
            .thrown_value()
    };
    let mut dict = std::collections::BTreeMap::new();
    dict.put_str("schema", SANDBOX_CONFINEMENT_SCHEMA);
    dict.put_str("backend", B::name());
    dict.put_str("mechanism", mechanism_name);
    dict.insert("confines_processes".to_string(), VmValue::Bool(confines));
    dict.insert("os_hardened_refusal".to_string(), refusal);
    VmValue::dict(dict)
}

#[crate::stdlib::macros::harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "sandbox_confinement() -> {schema: string, backend: string, mechanism: string, confines_processes: bool, os_hardened_refusal: {category: string, message: string, source: string, sandbox_mechanism: {schema: string, mechanism: string, availability: string, profile: string, requirement: string, selector_honored: bool, unconfined: list<string>?}}?}",
    category = "sandbox"
)]
fn sandbox_confinement_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(host_confinement())
}

#[cfg(test)]
#[path = "introspection_tests.rs"]
mod tests;
