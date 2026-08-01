//! Tool-registry enforcement at the active execution-policy boundary.

use crate::agent_events::DenialGate;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

use super::{
    current_execution_policy, policy_allows_capability, policy_allows_side_effect,
    policy_allows_tool, reject_tool, PolicyDenial, SideEffectCeilingGrant,
    SideEffectCeilingViolation,
};

pub fn enforce_current_policy_for_tool(tool_name: &str) -> Result<(), PolicyDenial> {
    enforce_current_policy_for_tool_with_side_effect_grant(tool_name, None)
}

/// Enforce the active tool policy, optionally honoring one exact
/// dispatch-local side-effect grant. Tool and capability ceilings remain hard
/// requirements, and argument constraints are enforced by the caller.
pub(crate) fn enforce_current_policy_for_tool_with_side_effect_grant(
    tool_name: &str,
    side_effect_grant: Option<&SideEffectCeilingGrant>,
) -> Result<(), PolicyDenial> {
    enforce_current_policy_for_tool_with_annotations_and_side_effect_grant(
        tool_name,
        None,
        side_effect_grant,
    )
}

/// Prefer ambient policy annotations, falling back to the concrete dispatch
/// catalog for dynamic registries assembled after the policy was installed.
pub(crate) fn enforce_current_policy_for_tool_with_annotations_and_side_effect_grant(
    tool_name: &str,
    dispatch_annotations: Option<&ToolAnnotations>,
    side_effect_grant: Option<&SideEffectCeilingGrant>,
) -> Result<(), PolicyDenial> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    if !policy_allows_tool(&policy, tool_name) {
        return reject_tool(
            DenialGate::ToolCeiling,
            None,
            format!("tool '{tool_name}' is not in the active allowed-tool list"),
        );
    }
    if let Some(annotations) = policy
        .tool_annotations
        .get(tool_name)
        .or(dispatch_annotations)
    {
        for (capability, ops) in &annotations.capabilities {
            for op in ops {
                if !policy_allows_capability(&policy, capability, op) {
                    return reject_tool(
                        DenialGate::CapabilityCeiling,
                        Some(format!("{capability}.{op}")),
                        format!("tool '{tool_name}' requires {capability}.{op}"),
                    );
                }
            }
        }
        let requested_level = annotations.side_effect_level;
        if requested_level != SideEffectLevel::None
            && !policy_allows_side_effect(&policy, requested_level.as_str())
        {
            let ceiling = policy
                .side_effect_level
                .as_deref()
                .map(SideEffectLevel::parse)
                .expect("a side-effect refusal requires an active policy ceiling");
            let violation = SideEffectCeilingViolation {
                ceiling,
                required_level: requested_level,
            };
            if side_effect_grant.is_some_and(|grant| grant.matches(tool_name, violation)) {
                return Ok(());
            }
            return Err(PolicyDenial {
                gate: DenialGate::SideEffectCeiling,
                capability: None,
                reason: DenialGate::SideEffectCeiling.render_reason(format!(
                    "tool '{tool_name}' requires side-effect level '{}' but the active ceiling is '{}'",
                    requested_level.as_str(),
                    ceiling.as_str(),
                )),
                side_effect_ceiling: Some(violation),
            });
        }
    }
    Ok(())
}
