//! Policy normalization at the agent-tool dispatch boundary.

/// Execution-policy gate for one tool dispatch: the tool/capability/
/// side-effect ceilings plus the per-tool argument allow-lists.
///
/// `policy_machinery_active` is the dispatch fast-path gate (see
/// `host_agent_dispatch_tool_call`): when false, no execution-policy scope is
/// installed, so `enforce_current_policy_for_tool` would return `Ok(())`
/// unconditionally and `enforce_tool_arg_constraints` would iterate the empty
/// constraint list of `CapabilityPolicy::default()` — skipping both is
/// behavior-preserving and avoids building that default policy per call.
pub(super) struct DispatchPolicy<'a> {
    active: bool,
    annotations: Option<&'a crate::tool_annotations::ToolAnnotations>,
    grant: Option<GrantedInvocation>,
}

struct GrantedInvocation {
    tool: String,
    args: serde_json::Value,
    ceiling: crate::orchestration::SideEffectCeilingGrant,
}

impl<'a> DispatchPolicy<'a> {
    pub(super) fn new(
        active: bool,
        annotations: Option<&'a crate::tool_annotations::ToolAnnotations>,
    ) -> Self {
        Self {
            active,
            annotations,
            grant: None,
        }
    }

    pub(super) fn enforce(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        side_effect_grant: Option<&crate::orchestration::SideEffectCeilingGrant>,
    ) -> Result<(), crate::orchestration::PolicyDenial> {
        self.enforce_with_annotations(tool_name, tool_args, self.annotations, side_effect_grant)
    }

    pub(super) fn retain_grant(
        &mut self,
        tool: &str,
        args: &serde_json::Value,
        ceiling: crate::orchestration::SideEffectCeilingGrant,
    ) {
        self.grant = Some(GrantedInvocation {
            tool: tool.into(),
            args: args.clone(),
            ceiling,
        });
    }

    pub(super) fn recheck(
        &self,
        tool: &str,
        args: &serde_json::Value,
        annotations: Option<&crate::tool_annotations::ToolAnnotations>,
    ) -> Result<(), crate::orchestration::PolicyDenial> {
        // A once grant belongs to these exact policy facts. The policy owner
        // separately checks that its ceiling and required effect still match.
        let grant = self
            .grant
            .as_ref()
            .filter(|grant| grant.tool == tool && grant.args == *args)
            .map(|grant| &grant.ceiling);
        self.enforce_with_annotations(tool, args, annotations, grant)
    }

    fn enforce_with_annotations(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        annotations: Option<&crate::tool_annotations::ToolAnnotations>,
        side_effect_grant: Option<&crate::orchestration::SideEffectCeilingGrant>,
    ) -> Result<(), crate::orchestration::PolicyDenial> {
        if !self.active {
            return Ok(());
        }
        crate::orchestration::enforce_current_policy_for_tool_with_annotations_and_side_effect_grant(
            tool_name,
            annotations,
            side_effect_grant,
        )?;
        crate::orchestration::enforce_tool_arg_constraints(
            &crate::orchestration::current_execution_policy().unwrap_or_default(),
            tool_name,
            tool_args,
        )
    }
}

pub(super) fn tool_denial_from_policy(
    policy_denial: crate::orchestration::PolicyDenial,
    tool_name: &str,
) -> crate::agent_events::ToolDenial {
    let side_effect_ceiling = policy_denial.side_effect_ceiling.map(|violation| {
        crate::agent_events::SideEffectCeilingDetails {
            ceiling: violation.ceiling,
            required_level: violation.required_level,
            tool: tool_name.to_string(),
            remedy: crate::agent_events::SideEffectCeilingRemedy::RaiseSideEffectCeiling,
        }
    });
    let denial = if policy_denial.gate == crate::agent_events::DenialGate::ArgConstraint {
        crate::agent_events::ToolDenial::retryable(
            policy_denial.gate,
            policy_denial.capability,
            policy_denial.reason,
        )
    } else {
        crate::agent_events::ToolDenial::terminal(
            policy_denial.gate,
            policy_denial.capability,
            policy_denial.reason,
        )
    };
    match side_effect_ceiling {
        Some(details) => denial.with_side_effect_ceiling(details),
        None => denial,
    }
}
