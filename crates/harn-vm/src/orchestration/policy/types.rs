//! Native policy enforcement and workflow policies carrying interpreter values.

pub use harn_execution_policy::{
    default_read_deny_home_paths, CapabilityPolicy, LoopbackEnforcement, ProcessNetworkProxy,
    ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile, ToolArgConstraint,
    UnixSocketEnforcement,
};
pub use harn_orchestration::policies::*;

use super::super::glob_match;
use super::{reject_tool, PolicyDenial};
use crate::agent_events::DenialGate;

/// Check declared tool argument constraints without guessing argument semantics.
/// Explicit `arg_key` wins over the tool annotation's declared path parameters.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), PolicyDenial> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) || constraint.arg_patterns.is_empty() {
            continue;
        }
        let declared_keys: Vec<String> = if let Some(key) = constraint.arg_key.as_ref() {
            vec![key.clone()]
        } else {
            policy
                .tool_annotations
                .get(tool_name)
                .map(|a| a.arg_schema.path_params.clone())
                .unwrap_or_default()
        };
        let (arg_key, arg_value): (String, Option<String>) = if let Some(obj) = args.as_object() {
            if declared_keys.is_empty() {
                crate::events::log_warn(
                    "policy.constraint_unresolved",
                    &format!(
                        "tool_arg_constraint for tool '{tool_name}' has no arg_key and tool_annotations.arg_schema.path_params is empty; skipping (policy author should declare arg_key on the constraint or path_params in the tool's annotations)"
                    ),
                );
                continue;
            }
            let mut found: (String, Option<String>) = (declared_keys[0].clone(), None);
            for param in &declared_keys {
                if let Some(value) = obj.get(param).and_then(|v| v.as_str()) {
                    found = (param.clone(), Some(value.to_string()));
                    break;
                }
            }
            found
        } else {
            ("value".to_string(), args.as_str().map(|s| s.to_string()))
        };
        let Some(candidate) = arg_value else {
            continue;
        };
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, &candidate));
        if !matches {
            return reject_tool(
                DenialGate::ArgConstraint,
                None,
                format!(
                    "tool '{tool_name}' {arg_key} '{candidate}' is outside your allowed scope. \
                     Allowed {arg_key} pattern(s): {:?}. This is fixable: re-issue the call with a \
                     {arg_key} that matches one of those patterns. If '{candidate}' is a \
                     reference/example you only need to consult, read it with `look` instead — never \
                     write to a path outside your scope.",
                    constraint.arg_patterns
                ),
            );
        }
    }
    Ok(())
}
