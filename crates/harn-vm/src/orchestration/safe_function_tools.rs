//! Allowlisted Harn function-to-agent-tool adapter.
//!
//! The author of a workflow patch needs *some* deterministic, side-effect-free
//! way to inspect a bundle, simulate a patch, and ask the validator whether
//! the result is sane. We deliberately do **not** expose every Harn stdlib
//! function as a model-callable tool — that would require schema generation,
//! capability classification, and an audit trail for every entrypoint we ship.
//!
//! Instead, this module hosts a small, hand-curated registry of functions
//! that:
//!
//! - have no side effects (Read / Search / Think kinds only),
//! - have a stable JSON-shaped argument schema we can validate up front,
//! - declare an explicit ACP-compatible [`ToolAnnotations`] entry,
//! - dispatch into a deterministic Rust handler in this crate.
//!
//! The first version is scoped to the workflow-patch authoring loop: load
//! a bundle, validate it, preview its graph, and dry-run a patch. Adding
//! more entries is a deliberate, reviewed change — there is no auto
//! discovery.
//!
//! Hosts surface this registry to a model by enumerating the descriptors,
//! then dispatch tool calls back through [`SafeFunctionTool::dispatch`].
//! The execution policy is enforced by the host; this layer only describes
//! the contract.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::workflow_bundle::{
    load_workflow_bundle, preview_workflow_bundle, validate_workflow_bundle, WorkflowBundle,
};
use super::workflow_patch::{bundle_capability_ceiling, validate_workflow_patch, WorkflowPatch};
use super::CapabilityPolicy;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};

pub const SAFE_FUNCTION_TOOL_SCHEMA_VERSION: u32 = 1;

/// Public descriptor for one safe function tool. Hosts can serialize
/// these directly into a tool listing surface for an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafeFunctionToolDescriptor {
    pub name: String,
    pub description: String,
    pub annotations: ToolAnnotations,
    pub arg_schema: serde_json::Value,
}

/// Internal registry entry. Splits the descriptor (data the host serializes)
/// from the dispatch handler (function pointer, not serializable).
#[derive(Clone)]
pub struct SafeFunctionTool {
    pub descriptor: SafeFunctionToolDescriptor,
    pub dispatch: fn(&serde_json::Value) -> Result<serde_json::Value, SafeFunctionToolError>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafeFunctionToolError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SafeFunctionToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SafeFunctionToolError {}

impl SafeFunctionToolError {
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_arguments".to_string(),
            message: message.into(),
        }
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error".to_string(),
            message: message.into(),
        }
    }
}

/// All currently-allowlisted safe function tools. Hosts that need to
/// expose a subset can filter by `name`. Adding an entry here is a
/// deliberate review: the new function must be deterministic, side-
/// effect-free, and carry an ACP-aligned [`ToolAnnotations`] value.
pub fn safe_function_tools() -> Vec<SafeFunctionTool> {
    vec![
        SafeFunctionTool {
            descriptor: SafeFunctionToolDescriptor {
                name: "workflow_bundle_validate".to_string(),
                description:
                    "Validate a workflow bundle JSON file at PATH and return the validator report."
                        .to_string(),
                annotations: read_only_annotations("workspace", &["read_text"]),
                arg_schema: bundle_path_schema(),
            },
            dispatch: dispatch_workflow_bundle_validate,
        },
        SafeFunctionTool {
            descriptor: SafeFunctionToolDescriptor {
                name: "workflow_bundle_preview".to_string(),
                description:
                    "Preview a workflow bundle: return the normalized graph, validation report, mermaid text, triggers, connectors, and editable fields."
                        .to_string(),
                annotations: read_only_annotations("workspace", &["read_text"]),
                arg_schema: bundle_path_schema(),
            },
            dispatch: dispatch_workflow_bundle_preview,
        },
        SafeFunctionTool {
            descriptor: SafeFunctionToolDescriptor {
                name: "workflow_bundle_capability_ceiling".to_string(),
                description:
                    "Compute the capability ceiling that running this bundle would request of its parent runtime."
                        .to_string(),
                annotations: think_annotations(),
                arg_schema: bundle_path_schema(),
            },
            dispatch: dispatch_workflow_bundle_capability_ceiling,
        },
        SafeFunctionTool {
            descriptor: SafeFunctionToolDescriptor {
                name: "workflow_patch_validate".to_string(),
                description:
                    "Apply a workflow patch JSON to a bundle in memory, run bundle validation, compute the structural diff and capability delta, and return the report."
                        .to_string(),
                annotations: read_only_annotations("workspace", &["read_text"]),
                arg_schema: patch_validate_schema(),
            },
            dispatch: dispatch_workflow_patch_validate,
        },
    ]
}

/// Find a safe function tool by its model-visible name.
pub fn find_safe_function_tool(name: &str) -> Option<SafeFunctionTool> {
    safe_function_tools()
        .into_iter()
        .find(|tool| tool.descriptor.name == name)
}

fn read_only_annotations(capability: &str, ops: &[&str]) -> ToolAnnotations {
    let mut capabilities = std::collections::BTreeMap::new();
    capabilities.insert(
        capability.to_string(),
        ops.iter().map(|s| (*s).to_string()).collect(),
    );
    ToolAnnotations {
        kind: ToolKind::Read,
        side_effect_level: SideEffectLevel::ReadOnly,
        arg_schema: ToolArgSchema {
            path_params: vec!["bundle".to_string()],
            ..ToolArgSchema::default()
        },
        capabilities,
        emits_artifacts: false,
        result_readers: Vec::new(),
        inline_result: true,
        ..ToolAnnotations::default()
    }
}

fn think_annotations() -> ToolAnnotations {
    ToolAnnotations {
        kind: ToolKind::Think,
        side_effect_level: SideEffectLevel::ReadOnly,
        arg_schema: ToolArgSchema {
            path_params: vec!["bundle".to_string()],
            ..ToolArgSchema::default()
        },
        capabilities: std::collections::BTreeMap::new(),
        emits_artifacts: false,
        result_readers: Vec::new(),
        inline_result: true,
        ..ToolAnnotations::default()
    }
}

fn bundle_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["bundle"],
        "additionalProperties": false,
        "properties": {
            "bundle": {
                "type": "string",
                "description": "Filesystem path to a portable workflow bundle JSON file.",
            }
        }
    })
}

fn patch_validate_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["bundle", "patch"],
        "additionalProperties": false,
        "properties": {
            "bundle": {
                "type": "string",
                "description": "Filesystem path to a portable workflow bundle JSON file.",
            },
            "patch": {
                "description": "Either an inline workflow patch object or a path string to a patch JSON file.",
                "oneOf": [
                    { "type": "string" },
                    { "type": "object" }
                ]
            },
            "parent_ceiling": {
                "description": "Optional CapabilityPolicy of the executing parent context. The patch is rejected when it widens this ceiling.",
                "type": "object"
            }
        }
    })
}

fn dispatch_workflow_bundle_validate(
    args: &serde_json::Value,
) -> Result<serde_json::Value, SafeFunctionToolError> {
    let bundle = load_bundle_arg(args)?;
    let report = validate_workflow_bundle(&bundle);
    serde_json::to_value(report).map_err(|err| SafeFunctionToolError::io(err.to_string()))
}

fn dispatch_workflow_bundle_preview(
    args: &serde_json::Value,
) -> Result<serde_json::Value, SafeFunctionToolError> {
    let bundle = load_bundle_arg(args)?;
    let preview = preview_workflow_bundle(&bundle);
    serde_json::to_value(preview).map_err(|err| SafeFunctionToolError::io(err.to_string()))
}

fn dispatch_workflow_bundle_capability_ceiling(
    args: &serde_json::Value,
) -> Result<serde_json::Value, SafeFunctionToolError> {
    let bundle = load_bundle_arg(args)?;
    let ceiling = bundle_capability_ceiling(&bundle);
    serde_json::to_value(ceiling).map_err(|err| SafeFunctionToolError::io(err.to_string()))
}

fn dispatch_workflow_patch_validate(
    args: &serde_json::Value,
) -> Result<serde_json::Value, SafeFunctionToolError> {
    let bundle = load_bundle_arg(args)?;
    let patch = load_patch_arg(args)?;
    let parent_ceiling = match args.get("parent_ceiling") {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value::<CapabilityPolicy>(value.clone())
                .map_err(|err| SafeFunctionToolError::invalid_args(err.to_string()))?,
        ),
        _ => None,
    };
    let report = validate_workflow_patch(&bundle, &patch, parent_ceiling.as_ref());
    serde_json::to_value(report).map_err(|err| SafeFunctionToolError::io(err.to_string()))
}

fn load_bundle_arg(args: &serde_json::Value) -> Result<WorkflowBundle, SafeFunctionToolError> {
    let path = args
        .get("bundle")
        .and_then(|value| value.as_str())
        .ok_or_else(|| SafeFunctionToolError::invalid_args("bundle must be a string path"))?;
    load_workflow_bundle(Path::new(path))
        .map_err(|err| SafeFunctionToolError::io(format!("failed to load bundle {path}: {err}")))
}

fn load_patch_arg(args: &serde_json::Value) -> Result<WorkflowPatch, SafeFunctionToolError> {
    let value = args
        .get("patch")
        .ok_or_else(|| SafeFunctionToolError::invalid_args("patch is required"))?;
    let raw = match value {
        serde_json::Value::String(path) => std::fs::read_to_string(path)
            .map(|text| serde_json::from_str(&text))
            .map_err(|err| {
                SafeFunctionToolError::io(format!("failed to read patch {path}: {err}"))
            })?
            .map_err(|err| SafeFunctionToolError::invalid_args(err.to_string()))?,
        other => serde_json::from_value(other.clone())
            .map_err(|err| SafeFunctionToolError::invalid_args(err.to_string()))?,
    };
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entries_are_readonly_or_think_only() {
        for tool in safe_function_tools() {
            let kind = tool.descriptor.annotations.kind;
            assert!(
                matches!(
                    kind,
                    ToolKind::Read | ToolKind::Search | ToolKind::Think | ToolKind::Fetch
                ),
                "tool {} has non-read kind {kind:?}",
                tool.descriptor.name
            );
            let level = tool.descriptor.annotations.side_effect_level;
            assert!(
                matches!(level, SideEffectLevel::None | SideEffectLevel::ReadOnly),
                "tool {} has non-readonly side-effect level {level:?}",
                tool.descriptor.name
            );
        }
    }

    #[test]
    fn registry_entries_have_object_argument_schemas() {
        for tool in safe_function_tools() {
            let schema = &tool.descriptor.arg_schema;
            assert_eq!(
                schema.get("type").and_then(|t| t.as_str()),
                Some("object"),
                "tool {} arg_schema must declare type=object",
                tool.descriptor.name,
            );
            assert!(
                schema.get("properties").is_some(),
                "tool {} arg_schema must declare properties",
                tool.descriptor.name,
            );
        }
    }

    #[test]
    fn find_safe_function_tool_returns_none_for_unknown_name() {
        assert!(find_safe_function_tool("does_not_exist").is_none());
        assert!(find_safe_function_tool("workflow_bundle_validate").is_some());
    }

    fn write_pr_monitor_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("pr-monitor.bundle.json");
        std::fs::write(
            &path,
            super::super::workflow_test_fixtures::PR_MONITOR_BUNDLE_JSON,
        )
        .unwrap();
        path
    }

    #[test]
    fn dispatch_validate_returns_validator_report() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_path = write_pr_monitor_fixture(temp.path());
        let tool = find_safe_function_tool("workflow_bundle_validate").unwrap();
        let result = (tool.dispatch)(&serde_json::json!({"bundle": bundle_path})).unwrap();
        assert_eq!(result["valid"], serde_json::Value::Bool(true));
        assert_eq!(result["bundle_id"], "github-pr-monitor");
    }

    #[test]
    fn dispatch_patch_validate_handles_inline_patch() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_path = write_pr_monitor_fixture(temp.path());
        let tool = find_safe_function_tool("workflow_patch_validate").unwrap();
        let inline_patch = serde_json::json!({
            "schema_version": 1,
            "id": "test-patch",
            "operations": [
                { "op": "insert_node", "node_id": "verifier", "node": {"kind": "action", "task_label": "verify"} },
                { "op": "add_edge", "from": "query_logs", "to": "verifier" }
            ]
        });
        let result = (tool.dispatch)(&serde_json::json!({
            "bundle": bundle_path,
            "patch": inline_patch
        }))
        .unwrap();
        assert_eq!(result["valid"], serde_json::Value::Bool(true));
        assert_eq!(result["patch_id"], "test-patch");
    }

    #[test]
    fn dispatch_invalid_arguments_returns_error_code() {
        let tool = find_safe_function_tool("workflow_bundle_validate").unwrap();
        let err = (tool.dispatch)(&serde_json::json!({"bundle": 42})).unwrap_err();
        assert_eq!(err.code, "invalid_arguments");
    }
}
