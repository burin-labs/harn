//! User-facing result bodies for terminal policy denials.

use crate::agent_events::SideEffectCeilingDetails;

pub(super) fn tool_permission_hint_clause(allowed: &[String]) -> String {
    if allowed.is_empty() {
        return String::new();
    }
    format!(
        " Allowed tool names (permission hints): {}. These names do not disclose schemas. \
         Name permission does not approve these arguments or bypass other checks. \
         Use the advertised tool-search capability before calling a name whose schema has not \
         been supplied; without tool search, choose a tool with a supplied schema.",
        allowed.join(", ")
    )
}

pub(super) fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    let reason = reason.into();
    // Argument and resource checks can reject a name the policy permits.
    // Describe this invocation without projecting a new tool-availability policy.
    let policy = crate::orchestration::current_execution_policy();
    let allowed = policy
        .as_ref()
        .map_or_else(Vec::new, |policy| policy.tool_name_hints());
    let name_allowed = policy.is_some_and(|policy| policy.tool_pattern_allows(tool_name));
    let permission_hints = if name_allowed {
        String::new()
    } else {
        tool_permission_hint_clause(&allowed)
    };
    let next_step = format!(
        "This `{tool_name}` invocation was denied. Do not retry this invocation unchanged. \
         Address the specific reason before retrying, or choose a permitted approach. \
         Ask for permission only if resolving that reason requires new permission.\
         {permission_hints}"
    );
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}

/// Build actionable feedback for a hard side-effect ceiling. Unlike a generic
/// permission denial, this names the typed policy facts so the model can
/// choose a non-mutating path or ask an operator to change the owned policy.
pub(super) fn side_effect_ceiling_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
    details: &SideEffectCeilingDetails,
) -> serde_json::Value {
    let reason = reason.into();
    let next_step = format!(
        "`{tool_name}` requires side-effect level `{}`, but this session permits only through `{}`. \
         Do not retry the same call. Choose a non-mutating approach, or ask the operator to raise \
         the session side-effect ceiling to `{}` before retrying.",
        details.required_level.as_str(),
        details.ceiling.as_str(),
        details.required_level.as_str(),
    );
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}

#[cfg(test)]
mod tests {
    use super::denied_tool_result;
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    #[test]
    fn an_argument_denial_keeps_recovery_scoped_to_the_invocation() {
        for pattern in ["denied", "den*"] {
            push_execution_policy(CapabilityPolicy {
                tools: vec![pattern.into(), "usable".into()],
                ..Default::default()
            });
            let result = denied_tool_result("denied", "path is outside the workspace");
            pop_execution_policy();

            let recovery = result["next_step"].as_str().unwrap();
            assert!(!recovery.contains("Allowed tool names"));
            assert!(recovery.contains("This `denied` invocation was denied"));
            assert!(recovery.contains("Address the specific reason before retrying"));
            assert!(recovery.contains("only if resolving that reason requires new permission"));
            assert_eq!(result["reason"], "path is outside the workspace");
            assert!(!recovery.contains("tool is not permitted"));
        }
    }

    #[test]
    fn a_name_denial_does_not_offer_the_disallowed_name() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["usable".into()],
            ..Default::default()
        });
        let result = denied_tool_result("denied", "name is not allowed by the tool policy");
        pop_execution_policy();
        let hints = result["next_step"]
            .as_str()
            .unwrap()
            .split_once("Allowed tool names (permission hints):")
            .unwrap()
            .1;
        assert!(!hints.contains("denied"));
        assert!(hints.contains("usable"));
        assert!(hints.contains("These names do not disclose schemas"));
        assert!(hints.contains("advertised tool-search capability"));
    }

    #[test]
    fn an_unavailable_name_does_not_promote_permission_patterns_to_schemas() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["usable*".into()],
            ..Default::default()
        });
        let result = crate::llm::agent_tools::unavailable_tool_result("missing", "unknown name");
        pop_execution_policy();
        let recovery = result["next_step"].as_str().unwrap();
        assert_eq!(result["error"], "unknown_tool");
        assert!(recovery.contains("supplied schema"));
        assert!(recovery.contains("permission hints): usable*"));
        assert!(recovery.contains("These names do not disclose schemas"));
        assert!(!recovery.contains("Available tools:"));
        assert!(!recovery.contains("call it directly"));
    }
}
