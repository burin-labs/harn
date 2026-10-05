//! The invocation paths shared by host capture and VM permission evaluation.

use std::path::Path;

use serde_json::Value as JsonValue;

use crate::tool_annotations::ToolAnnotations;
use crate::workspace_path::{classify_permission_path, WorkspacePathInfo};

pub(super) const CONVENTIONAL_PATH_PARAMETERS: &[&str] = &[
    "path",
    "file",
    "target",
    "source_path",
    "new_path",
    "target_path",
    "paths",
];

pub(super) fn parameters(annotations: Option<&ToolAnnotations>) -> Vec<String> {
    annotations
        .map(|annotations| annotations.arg_schema.path_params.clone())
        .unwrap_or_else(|| {
            CONVENTIONAL_PATH_PARAMETERS
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        })
}

/// A malformed supplied path must be refused rather than disappearing during
/// classification. Hosts and VM dispatch validate through this same boundary.
pub(super) fn validate(arguments: &JsonValue, parameters: &[String]) -> Result<(), String> {
    for parameter in parameters {
        if parameter.trim().is_empty() {
            return Err(
                "tool_annotations.arg_schema.path_params must contain nonempty field names".into(),
            );
        }
        match arguments.get(parameter) {
            None => {}
            Some(JsonValue::String(value)) if !value.trim().is_empty() => {}
            Some(JsonValue::Array(values))
                if values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| !value.trim().is_empty())) => {}
            _ => {
                return Err(format!(
                    "workspace path argument '{parameter}' must be a string or list of strings"
                ));
            }
        }
    }
    Ok(())
}

/// An explicit catalog entry replaces conventional field inference. Command
/// reader paths still come from the existing semantic command-policy owner.
pub(super) fn classify(
    arguments: &JsonValue,
    annotations: Option<&ToolAnnotations>,
    workspace: &Path,
) -> Vec<WorkspacePathInfo> {
    let parameters = parameters(annotations);
    let mut paths = Vec::new();
    for parameter in parameters {
        match arguments.get(&parameter) {
            Some(JsonValue::String(path)) if !path.is_empty() => paths.push(path.clone()),
            Some(JsonValue::Array(items)) => paths.extend(
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .filter(|path| !path.is_empty())
                    .map(String::from),
            ),
            _ => {}
        }
    }
    paths.extend(super::super::super::command_policy::credential_read_path_candidates(arguments));
    paths.sort();
    paths.dedup();
    paths
        .iter()
        .map(|path| classify_permission_path(path, Some(workspace)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{ToolApprovalPolicy, ToolApprovalRequest, ToolApprovalWorkspaceBoundary};
    use super::*;
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    use serde_json::json;

    #[test]
    fn vm_and_host_refuse_malformed_path_shapes_before_mode_allow() {
        let root = crate::orchestration::execution_root_path();
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"source": "mode", "allow": "run"}]
        }))
        .unwrap();
        for annotated in [false, true] {
            let field = if annotated { "location" } else { "path" };
            let annotations = annotated.then(|| {
                serde_json::from_value::<ToolAnnotations>(json!({
                    "kind": "read", "arg_schema": {"path_params": [field]}
                }))
                .unwrap()
            });
            let mut ambient = CapabilityPolicy::default();
            if let Some(annotations) = &annotations {
                ambient
                    .tool_annotations
                    .insert("run".into(), annotations.clone());
            }
            push_execution_policy(ambient);
            for value in [
                json!(42),
                json!(["safe", 42]),
                json!(null),
                json!(" "),
                json!([" "]),
            ] {
                let arguments = json!({(field): value});
                let host = ToolApprovalRequest {
                    tool_name: "run".into(),
                    arguments: arguments.clone(),
                    workspace_boundary: Some(ToolApprovalWorkspaceBoundary {
                        root: root.to_string_lossy().into_owned(),
                    }),
                    tool_annotations: annotations.clone(),
                    ..Default::default()
                };
                let result =
                    super::super::evaluate_tool_approval_policy(&policy, "run", &arguments, None);
                assert!(result.is_deny(), "{arguments}: {result:?}");
                assert_eq!(result.receipt["matched_rule"]["id"], "invalid_host_request");
                assert!(policy.evaluate_request(&host).is_deny());
            }
            let arguments = if annotated {
                json!({(field): "reference.txt", "path": 42})
            } else {
                json!({(field): "reference.txt"})
            };
            let positive =
                super::super::evaluate_tool_approval_policy(&policy, "run", &arguments, None);
            let path_free =
                super::super::evaluate_tool_approval_policy(&policy, "run", &json!({}), None);
            pop_execution_policy();
            assert!(positive.is_allow(), "{positive:?}");
            assert!(path_free.is_allow(), "{path_free:?}");
        }
    }
}
