//! Authority narrowing and typed capability enforcement for an invocation.

use super::{
    contract_effect_allowed_by_ceiling, current_execution_policy, effects, reject_policy,
    trusted_bridge_call_is_active, CapabilityPolicy,
};
use crate::value::{VmError, VmValue};

/// Narrow invocation observation without replacing the caller's authority.
pub(crate) async fn scope_read_only_invocation<T>(
    session_id: &str,
    future: impl std::future::Future<Output = Result<T, VmError>>,
) -> Result<T, VmError> {
    let overlay = CapabilityPolicy {
        side_effect_level: Some("read_only".into()),
        ..CapabilityPolicy::neutral()
    };
    let ceiling = current_execution_policy()
        .unwrap_or_default()
        .intersect(&overlay)
        .map_err(|message| VmError::CategorizedError {
            message,
            category: crate::value::ErrorCategory::ToolRejected,
        })?;
    crate::orchestration::scope_agent_session(
        session_id.to_string(),
        crate::orchestration::scope_execution_policy(ceiling, future),
    )
    .await
}

/// Enforce a typed Harness method at the authoritative source policy boundary.
pub fn enforce_current_policy_for_capability(
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
    args: &[VmValue],
) -> Result<(), VmError> {
    let entry = crate::stdlib::capability_method_manifest_entry(capability, method);
    crate::tool_registry::preparation_scope::enforce_contract(
        method,
        entry.map(|entry| &entry.contract),
    )?;
    // Manifest/lifecycle VM hooks install `allow_trusted_bridge_calls` for the
    // duration of the handler. That guard already exempts bridged builtins;
    // Harness methods must honor the same depth, or a PreToolUse handler that
    // migrated from ambient `store_get` / `agent_session_current_id` to
    // `harness.runtime.store_get` / `harness.agent.current_id` silently loses
    // state:read under the tool's effect ceiling (observed downstream).
    if trusted_bridge_call_is_active() {
        return Ok(());
    }
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    let Some(entry) = entry else {
        return reject_policy(format!(
            "undeclared Harness capability method `harness.{}.{method}`",
            capability.field_name()
        ));
    };
    let denied = effects::runtime_effects_from_contract(entry.contract.effects, args)
        .into_iter()
        .find(|effect| !contract_effect_allowed_by_ceiling(effect, entry.contract, &policy));
    if let Some(effect) = denied {
        return reject_policy(format!(
            "harness.{}.{method} exceeds the active effect ceiling: {}",
            capability.field_name(),
            effects::effect_record_summary(&effect)
        ));
    }
    Ok(())
}
