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
use crate::workspace_path::{classify_permission_path, normalize_lexical};

/// Check declared tool argument constraints without guessing argument semantics.
/// Explicit `arg_key` wins over the tool annotation's declared path parameters.
///
/// A key the tool's annotations declare in `arg_schema.path_params` is a path:
/// its value is resolved to its workspace-relative spelling and matched
/// slash-aware ([`harn_glob::match_path`]), so `src/*` admits `src/a.rs` but
/// not `src/nested/a.rs` and `src/**` never admits `src/../outside`. Every
/// other key keeps flat-name matching, where `*` crosses `/` (`cargo *`). An
/// array value is admitted only when every string in it is.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), PolicyDenial> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) || constraint.arg_patterns.is_empty() {
            continue;
        }
        let path_params: &[String] = policy
            .tool_annotations
            .get(tool_name)
            .map(|annotations| annotations.arg_schema.path_params.as_slice())
            .unwrap_or_default();
        let declared_keys: Vec<&str> = match constraint.arg_key.as_deref() {
            Some(key) => vec![key],
            None => path_params.iter().map(String::as_str).collect(),
        };
        let (arg_key, candidates): (&str, Vec<&str>) = if let Some(obj) = args.as_object() {
            if declared_keys.is_empty() {
                crate::events::log_warn(
                    "policy.constraint_unresolved",
                    &format!(
                        "tool_arg_constraint for tool '{tool_name}' has no arg_key and tool_annotations.arg_schema.path_params is empty; skipping (policy author should declare arg_key on the constraint or path_params in the tool's annotations)"
                    ),
                );
                continue;
            }
            let found = declared_keys.iter().find_map(|key| {
                let candidates = constraint_candidates(obj.get(*key)?);
                (!candidates.is_empty()).then_some((*key, candidates))
            });
            match found {
                Some(found) => found,
                None => continue,
            }
        } else {
            match args.as_str() {
                Some(value) => ("value", vec![value]),
                None => continue,
            }
        };
        let path_keyed = path_params.iter().any(|param| param == arg_key);
        for candidate in candidates {
            let admitted = if path_keyed {
                path_candidate_admitted(&constraint.arg_patterns, candidate)
            } else {
                constraint
                    .arg_patterns
                    .iter()
                    .any(|pattern| glob_match(pattern, candidate))
            };
            if !admitted {
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
    }
    Ok(())
}

/// The string values an argument carries: itself, or each string of an array.
fn constraint_candidates(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(value) => vec![value.as_str()],
        serde_json::Value::Array(items) => items.iter().filter_map(|item| item.as_str()).collect(),
        _ => Vec::new(),
    }
}

/// Whether a path argument resolves inside the workspace to a spelling one of
/// the workspace-relative patterns admits. A path that escapes the workspace,
/// directly or through a symlink, is admitted by no pattern.
fn path_candidate_admitted(patterns: &[String], candidate: &str) -> bool {
    let root = crate::stdlib::process::execution_root_path();
    let Some(workspace_path) = classify_permission_path(candidate, Some(&root)).workspace_path
    else {
        return false;
    };
    let workspace_path = if workspace_path.is_empty() {
        ".".to_string()
    } else {
        workspace_path
    };
    patterns
        .iter()
        .any(|pattern| harn_glob::match_path(&normalize_lexical(pattern), &workspace_path))
}
