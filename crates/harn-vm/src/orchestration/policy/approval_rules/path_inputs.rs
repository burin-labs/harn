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
