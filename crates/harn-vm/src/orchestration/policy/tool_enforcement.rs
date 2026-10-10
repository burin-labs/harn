//! Tool-registry enforcement at the active execution-policy boundary.

use crate::agent_events::DenialGate;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

use super::{
    current_execution_policy, policy_allows_capability, policy_allows_side_effect,
    policy_allows_tool, reject_tool, PolicyDenial, SideEffectCeilingGrant,
    SideEffectCeilingViolation,
};

/// Execution-policy authority for one dispatch, including its exact one-use
/// exception. Callers provide observed invocation facts, never policy state.
pub(crate) struct ToolDispatchPolicy<'a> {
    active: bool,
    annotations: Option<&'a ToolAnnotations>,
    grant: Option<GrantedInvocation>,
}

struct GrantedInvocation {
    tool: String,
    args: serde_json::Value,
    ceiling: SideEffectCeilingGrant,
}

impl<'a> ToolDispatchPolicy<'a> {
    pub(crate) fn new(active: bool, annotations: Option<&'a ToolAnnotations>) -> Self {
        Self {
            active,
            annotations,
            grant: None,
        }
    }

    pub(crate) fn enforce(
        &self,
        tool: &str,
        args: &serde_json::Value,
        grant: Option<&SideEffectCeilingGrant>,
    ) -> Result<(), PolicyDenial> {
        self.enforce_with_annotations(tool, args, self.annotations, grant)
    }

    pub(crate) fn retain_grant(
        &mut self,
        tool: &str,
        args: &serde_json::Value,
        ceiling: SideEffectCeilingGrant,
    ) {
        self.grant = Some(GrantedInvocation {
            tool: tool.into(),
            args: args.clone(),
            ceiling,
        });
    }

    pub(crate) fn recheck(
        &self,
        tool: &str,
        args: &serde_json::Value,
        annotations: Option<&ToolAnnotations>,
    ) -> Result<(), PolicyDenial> {
        // An exception never follows a rewrite. The canonical enforcer below
        // also requires the original ceiling and requested effect to match.
        let grant = self
            .grant
            .as_ref()
            .filter(|grant| grant.tool == tool && grant.args == *args)
            .map(|grant| &grant.ceiling);
        self.enforce_with_annotations(tool, args, annotations, grant)
    }

    fn enforce_with_annotations(
        &self,
        tool: &str,
        args: &serde_json::Value,
        annotations: Option<&ToolAnnotations>,
        grant: Option<&SideEffectCeilingGrant>,
    ) -> Result<(), PolicyDenial> {
        if !self.active {
            return Ok(());
        }
        enforce_current_policy_for_tool_with_annotations_and_side_effect_grant(
            tool,
            annotations,
            grant,
        )?;
        super::enforce_tool_arg_constraints(
            &current_execution_policy().unwrap_or_default(),
            tool,
            args,
        )
    }
}

impl PolicyDenial {
    /// Project the policy owner's retryability and ceiling facts once.
    pub(crate) fn into_tool_denial(self, tool: &str) -> crate::agent_events::ToolDenial {
        use crate::agent_events::{SideEffectCeilingDetails, SideEffectCeilingRemedy, ToolDenial};
        let details = self
            .side_effect_ceiling
            .map(|violation| SideEffectCeilingDetails {
                ceiling: violation.ceiling,
                required_level: violation.required_level,
                tool: tool.into(),
                remedy: SideEffectCeilingRemedy::RaiseSideEffectCeiling,
            });
        let denial = if self.gate == DenialGate::ArgConstraint {
            ToolDenial::retryable(self.gate, self.capability, self.reason)
        } else {
            ToolDenial::terminal(self.gate, self.capability, self.reason)
        };
        match details {
            Some(details) => denial.with_side_effect_ceiling(details),
            None => denial,
        }
    }
}

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
