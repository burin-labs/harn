//! The invocation paths shared by host capture and VM permission evaluation.

use std::path::Path;

use serde_json::Value as JsonValue;

use crate::tool_annotations::ToolAnnotations;
use crate::workspace_path::{classify_permission_path, WorkspacePathInfo};

pub(super) use crate::tool_annotations::path_inputs::{
    parameters, validate, CONVENTIONAL_PATH_PARAMETERS,
};

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
    paths
        .extend(super::super::super::command_policy::permission_reader_paths(arguments, workspace));
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
    fn vm_and_host_bind_reader_paths_to_effective_cwd_without_moving_authority() {
        struct Restore(Option<crate::orchestration::RunExecutionRecord>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::orchestration::set_thread_execution_context(self.0.take());
                pop_execution_policy();
            }
        }
        let base = tempfile::tempdir().unwrap();
        let workspace = base.path().join("zone/workspace");
        std::fs::create_dir_all(workspace.join("src/inner")).unwrap();
        std::fs::write(workspace.join("Makefile"), "inside").unwrap();
        std::fs::write(base.path().join("Makefile"), "private outside canary").unwrap();
        let _restore = Restore(crate::orchestration::current_execution_context());
        crate::orchestration::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(workspace.to_string_lossy().into_owned()),
                ..Default::default()
            },
        ));
        let annotations: ToolAnnotations = serde_json::from_value(json!({
            "kind": "execute", "side_effect_level": "process_exec", "arg_schema": {"path_params": []}
        })).unwrap();
        let mut execution = CapabilityPolicy::default();
        execution
            .tool_annotations
            .insert("run".into(), annotations.clone());
        push_execution_policy(execution);
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"source": "mode", "allow": "run"}]
        }))
        .unwrap();
        let cases = [
            (json!({"command": "head Makefile", "shell": "sh"}), true),
            (
                json!({"command": "cd src/inner && head ../../Makefile", "shell": "sh"}),
                true,
            ),
            (
                json!({"command": "head ../../Makefile", "cwd": "src/inner"}),
                true,
            ),
            (
                json!({"argv": ["head", "../../Makefile"], "cwd": "src/inner"}),
                true,
            ),
            (
                json!({"command": "head ../../Makefile", "cwd": workspace.join("src/inner")}),
                true,
            ),
            (
                json!({"command": "head ../../Makefile", "cwd": "src/../src/inner"}),
                true,
            ),
            (
                json!({"command": "cd missing/inner && head ../../Makefile", "shell": "sh"}),
                false,
            ),
            (
                json!({"command": "cd src/inner; head ../../Makefile", "shell": "sh"}),
                false,
            ),
            (
                json!({"command": "cd missing/inner; head ../../Makefile", "shell": "sh"}),
                false,
            ),
            (
                json!({"command": "cd missing && head ../../Makefile", "shell": "sh"}),
                false,
            ),
            (
                json!({"command": "head Makefile", "cwd": base.path()}),
                false,
            ),
            (
                json!({"command": "cd src/inner && head ../../Makefile", "shell": "unknown-shell"}),
                false,
            ),
            (
                json!({"command": "cd $DIRECTORY && head ../../Makefile", "shell": "sh"}),
                false,
            ),
            (
                json!({"argv": ["bash", "-c", "cd src/inner && head ../../Makefile"]}),
                false,
            ),
            (json!({"argv": ["head", ".env"], "cwd": "src/inner"}), false),
        ];
        for (arguments, allowed) in cases {
            let host = ToolApprovalRequest {
                tool_name: "run".into(),
                arguments: arguments.clone(),
                workspace_boundary: Some(ToolApprovalWorkspaceBoundary {
                    root: workspace.to_string_lossy().into_owned(),
                }),
                tool_annotations: Some(annotations.clone()),
                ..Default::default()
            };
            let native = policy.evaluate_request(&host);
            let vm = super::super::evaluate_tool_approval_policy(&policy, "run", &arguments, None);
            assert_eq!(native.is_allow(), allowed, "host: {arguments}: {native:?}");
            assert_eq!(vm.is_allow(), allowed, "VM: {arguments}: {vm:?}");
            assert_eq!(native.denial_gate(), vm.denial_gate(), "{arguments}");
            if allowed {
                let actual = workspace.join("Makefile").canonicalize().unwrap();
                assert!(
                    classify(&arguments, Some(&annotations), &workspace)
                        .iter()
                        .any(|entry| {
                            entry.resolved_host_path().as_deref() == Some(actual.as_path())
                        }),
                    "approval must name the actual effective reader resource: {arguments}"
                );
            }
        }
    }

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
