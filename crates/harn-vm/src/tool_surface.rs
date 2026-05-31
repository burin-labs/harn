//! Validation for coherent tool surfaces before an agent spends model tokens.
//!
//! The checks here are deliberately structural and conservative. They do not
//! try to understand arbitrary prose; they validate declared registries,
//! policies, and prompt text with an explicit suppression convention.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::llm::tools::text_tool_call_tag_pairs;
use crate::orchestration::{CapabilityPolicy, ToolApprovalPolicy};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};
use crate::value::VmValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSurfaceSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolSurfaceDiagnostic {
    pub code: String,
    pub severity: ToolSurfaceSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl ToolSurfaceDiagnostic {
    fn warning(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity: ToolSurfaceSeverity::Warning,
            message: message.into(),
            tool: None,
            field: None,
        }
    }

    fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity: ToolSurfaceSeverity::Error,
            message: message.into(),
            tool: None,
            field: None,
        }
    }

    fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolSurfaceReport {
    pub valid: bool,
    pub diagnostics: Vec<ToolSurfaceDiagnostic>,
}

impl ToolSurfaceReport {
    fn new(diagnostics: Vec<ToolSurfaceDiagnostic>) -> Self {
        let valid = diagnostics
            .iter()
            .all(|d| d.severity != ToolSurfaceSeverity::Error);
        Self { valid, diagnostics }
    }
}

pub fn tool_names_from_spec(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(tool_names_from_spec)
                    .unwrap_or_default();
            }
            map.get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .map(|name| vec![name.to_string()])
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn max_side_effect_level(levels: impl Iterator<Item = String>) -> Option<String> {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    levels.max_by_key(|level| rank(level))
}

fn parse_tool_kind(value: Option<&serde_json::Value>) -> ToolKind {
    match value.and_then(|v| v.as_str()).unwrap_or("") {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

fn parse_tool_annotations(map: &serde_json::Map<String, serde_json::Value>) -> ToolAnnotations {
    let policy = map
        .get("policy")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();

    let capabilities = policy
        .get("capabilities")
        .and_then(|value| value.as_object())
        .map(|caps| {
            caps.iter()
                .map(|(capability, ops)| {
                    let values = ops
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    (capability.clone(), values)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let arg_schema = if let Some(schema) = policy.get("arg_schema") {
        serde_json::from_value::<ToolArgSchema>(schema.clone()).unwrap_or_default()
    } else {
        ToolArgSchema {
            path_params: policy
                .get("path_params")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            arg_aliases: policy
                .get("arg_aliases")
                .and_then(|value| value.as_object())
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default(),
            required: policy
                .get("required")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        }
    };

    let kind = parse_tool_kind(policy.get("kind"));
    let side_effect_level = policy
        .get("side_effect_level")
        .and_then(|value| value.as_str())
        .map(SideEffectLevel::parse)
        .unwrap_or_default();

    ToolAnnotations {
        kind,
        side_effect_level,
        arg_schema,
        capabilities,
        emits_artifacts: policy
            .get("emits_artifacts")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        result_readers: policy
            .get("result_readers")
            .or_else(|| policy.get("readable_result_routes"))
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        inline_result: policy
            .get("inline_result")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    }
}

pub fn tool_annotations_from_spec(value: &serde_json::Value) -> BTreeMap<String, ToolAnnotations> {
    match value {
        serde_json::Value::Null => BTreeMap::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| (name.to_string(), parse_tool_annotations(map))),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(tool_annotations_from_spec)
                    .unwrap_or_default();
            }
            map.get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .map(|name| {
                    let mut annotations = BTreeMap::new();
                    annotations.insert(name.to_string(), parse_tool_annotations(map));
                    annotations
                })
                .unwrap_or_default()
        }
        _ => BTreeMap::new(),
    }
}

pub fn tool_capability_policy_from_spec(value: &serde_json::Value) -> CapabilityPolicy {
    let tools = tool_names_from_spec(value);
    let tool_annotations = tool_annotations_from_spec(value);
    let mut capabilities: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for annotations in tool_annotations.values() {
        for (capability, ops) in &annotations.capabilities {
            let entry = capabilities.entry(capability.clone()).or_default();
            for op in ops {
                if !entry.contains(op) {
                    entry.push(op.clone());
                }
            }
            entry.sort();
        }
    }
    if !capabilities.is_empty() {
        let entry = capabilities.entry("llm".to_string()).or_default();
        let op = "call".to_string();
        if !entry.contains(&op) {
            entry.push(op);
            entry.sort();
        }
    }
    let side_effect_levels: Vec<String> = tool_annotations
        .values()
        .map(|annotations| annotations.side_effect_level.as_str().to_string())
        .filter(|level| level != "none")
        .collect();
    let side_effect_level = max_side_effect_level(side_effect_levels.into_iter());
    CapabilityPolicy {
        tools,
        capabilities,
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        side_effect_level,
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations,
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolSurfaceInput {
    pub tools: Option<VmValue>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub policy: Option<CapabilityPolicy>,
    pub approval_policy: Option<ToolApprovalPolicy>,
    pub prompt_texts: Vec<String>,
    pub tool_search_active: bool,
}

#[derive(Clone, Debug, Default)]
struct ToolEntry {
    name: String,
    parameter_keys: BTreeSet<String>,
    has_schema: bool,
    annotations: Option<ToolAnnotations>,
    has_executor: bool,
    defer_loading: bool,
    provider_native: bool,
}

pub fn validate_tool_surface(input: &ToolSurfaceInput) -> ToolSurfaceReport {
    ToolSurfaceReport::new(validate_tool_surface_diagnostics(input))
}

pub fn validate_tool_surface_diagnostics(input: &ToolSurfaceInput) -> Vec<ToolSurfaceDiagnostic> {
    let entries = collect_entries(input);
    let active_names = effective_active_names(&entries, input.policy.as_ref());
    let mut diagnostics = Vec::new();

    for entry in entries
        .iter()
        .filter(|entry| active_names.contains(entry.name.as_str()))
    {
        if !entry.has_schema {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_MISSING_SCHEMA",
                    format!("active tool '{}' has no parameter schema", entry.name),
                )
                .with_tool(entry.name.clone())
                .with_field("parameters"),
            );
        }
        if entry.annotations.is_none() {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_MISSING_ANNOTATIONS",
                    format!("active tool '{}' has no ToolAnnotations", entry.name),
                )
                .with_tool(entry.name.clone())
                .with_field("annotations"),
            );
        }
        if entry
            .annotations
            .as_ref()
            .is_some_and(|annotations| annotations.side_effect_level == SideEffectLevel::None)
        {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_MISSING_SIDE_EFFECT_LEVEL",
                    format!("active tool '{}' has no side-effect level", entry.name),
                )
                .with_tool(entry.name.clone())
                .with_field("side_effect_level"),
            );
        }
        if !entry.has_executor && !entry.provider_native {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_MISSING_EXECUTOR",
                    format!("active tool '{}' has no declared executor", entry.name),
                )
                .with_tool(entry.name.clone())
                .with_field("executor"),
            );
        }
        validate_execute_result_routes(entry, &entries, &active_names, &mut diagnostics);
    }

    validate_arg_constraints(
        input.policy.as_ref(),
        &entries,
        &active_names,
        &mut diagnostics,
    );
    validate_approval_patterns(
        input.approval_policy.as_ref(),
        &active_names,
        &mut diagnostics,
    );
    validate_prompt_references(input, &entries, &active_names, &mut diagnostics);
    validate_side_effect_ceiling(
        input.policy.as_ref(),
        &entries,
        &active_names,
        &mut diagnostics,
    );

    diagnostics
}

pub fn validate_workflow_graph(
    graph: &crate::orchestration::WorkflowGraph,
) -> Vec<ToolSurfaceDiagnostic> {
    let mut diagnostics = Vec::new();
    diagnostics.extend(
        validate_tool_surface_diagnostics(&ToolSurfaceInput {
            tools: None,
            native_tools: Some(workflow_tools_as_native(
                &graph.capability_policy,
                &graph.nodes,
            )),
            policy: Some(graph.capability_policy.clone()),
            approval_policy: Some(graph.approval_policy.clone()),
            prompt_texts: Vec::new(),
            tool_search_active: false,
        })
        .into_iter()
        .map(|mut diagnostic| {
            diagnostic.message = format!("workflow: {}", diagnostic.message);
            diagnostic
        }),
    );
    for (node_id, node) in &graph.nodes {
        let prompt_texts = [node.system.clone(), node.prompt.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        diagnostics.extend(
            validate_tool_surface_diagnostics(&ToolSurfaceInput {
                tools: None,
                native_tools: Some(workflow_node_tools_as_native(node)),
                policy: Some(node.capability_policy.clone()),
                approval_policy: Some(node.approval_policy.clone()),
                prompt_texts,
                tool_search_active: false,
            })
            .into_iter()
            .map(|mut diagnostic| {
                diagnostic.message = format!("node {node_id}: {}", diagnostic.message);
                diagnostic
            }),
        );
    }
    diagnostics
}

pub fn surface_report_to_json(report: &ToolSurfaceReport) -> serde_json::Value {
    serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({"valid": false}))
}

pub fn surface_input_from_vm(surface: &VmValue, options: Option<&VmValue>) -> ToolSurfaceInput {
    let dict = surface.as_dict();
    let options_dict = options.and_then(VmValue::as_dict);
    let tools = dict
        .and_then(|d| d.get("tools").cloned())
        .or_else(|| options_dict.and_then(|d| d.get("tools").cloned()))
        .or_else(|| Some(surface.clone()).filter(is_tool_registry_like));
    let native_tools = dict
        .and_then(|d| d.get("native_tools"))
        .or_else(|| options_dict.and_then(|d| d.get("native_tools")))
        .map(crate::llm::vm_value_to_json)
        .and_then(|value| value.as_array().cloned());
    let policy = dict
        .and_then(|d| d.get("policy"))
        .or_else(|| options_dict.and_then(|d| d.get("policy")))
        .map(crate::llm::vm_value_to_json)
        .and_then(|value| serde_json::from_value(value).ok());
    let approval_policy = dict
        .and_then(|d| d.get("approval_policy"))
        .or_else(|| options_dict.and_then(|d| d.get("approval_policy")))
        .map(crate::llm::vm_value_to_json)
        .and_then(|value| serde_json::from_value(value).ok());
    let mut prompt_texts = Vec::new();
    for source in [dict, options_dict].into_iter().flatten() {
        for key in ["system", "prompt"] {
            if let Some(text) = source.get(key).map(|value| value.display()) {
                if !text.is_empty() {
                    prompt_texts.push(text);
                }
            }
        }
        if let Some(VmValue::List(items)) = source.get("prompts") {
            for item in items.iter() {
                let text = item.display();
                if !text.is_empty() {
                    prompt_texts.push(text);
                }
            }
        }
    }
    let tool_search_active = dict
        .and_then(|d| d.get("tool_search"))
        .or_else(|| options_dict.and_then(|d| d.get("tool_search")))
        .is_some_and(|value| !matches!(value, VmValue::Bool(false) | VmValue::Nil));
    ToolSurfaceInput {
        tools,
        native_tools,
        policy,
        approval_policy,
        prompt_texts,
        tool_search_active,
    }
}

fn collect_entries(input: &ToolSurfaceInput) -> Vec<ToolEntry> {
    let mut entries = Vec::new();
    if let Some(tools) = input.tools.as_ref() {
        collect_vm_entries(tools, input.policy.as_ref(), &mut entries);
    }
    if let Some(native) = input.native_tools.as_ref() {
        let vm_names: BTreeSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        let mut native_entries = Vec::new();
        collect_native_entries(native, input.policy.as_ref(), &mut native_entries);
        entries.extend(
            native_entries
                .into_iter()
                .filter(|entry| !vm_names.contains(&entry.name)),
        );
    }
    entries
}

fn collect_vm_entries(
    tools: &VmValue,
    policy: Option<&CapabilityPolicy>,
    entries: &mut Vec<ToolEntry>,
) {
    let values: Vec<&VmValue> = match tools {
        VmValue::List(list) => list.iter().collect(),
        VmValue::Dict(dict) => match dict.get("tools") {
            Some(VmValue::List(list)) => list.iter().collect(),
            _ => vec![tools],
        },
        _ => Vec::new(),
    };
    for value in values {
        let Some(map) = value.as_dict() else { continue };
        let name = map
            .get("name")
            .map(|value| value.display())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let (has_schema, parameter_keys) = vm_parameter_keys(map.get("parameters"));
        let annotations = map
            .get("annotations")
            .map(crate::llm::vm_value_to_json)
            .and_then(|value| serde_json::from_value::<ToolAnnotations>(value).ok())
            .or_else(|| {
                policy
                    .and_then(|policy| policy.tool_annotations.get(&name))
                    .cloned()
            });
        let executor = map.get("executor").and_then(|value| match value {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        });
        entries.push(ToolEntry {
            name,
            parameter_keys,
            has_schema,
            annotations,
            has_executor: executor.is_some()
                || matches!(map.get("handler"), Some(VmValue::Closure(_)))
                || matches!(map.get("_mcp_server"), Some(VmValue::String(_))),
            defer_loading: matches!(map.get("defer_loading"), Some(VmValue::Bool(true))),
            provider_native: false,
        });
    }
}

fn collect_native_entries(
    native_tools: &[serde_json::Value],
    policy: Option<&CapabilityPolicy>,
    entries: &mut Vec<ToolEntry>,
) {
    for tool in native_tools {
        let name = tool
            .get("function")
            .and_then(|function| function.get("name"))
            .or_else(|| tool.get("name"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if name.is_empty() || name == "tool_search" || name.starts_with("tool_search_tool_") {
            continue;
        }
        let schema = tool
            .get("function")
            .and_then(|function| function.get("parameters"))
            .or_else(|| tool.get("input_schema"))
            .or_else(|| tool.get("parameters"));
        let (has_schema, parameter_keys) = json_parameter_keys(schema);
        let annotations = tool
            .get("annotations")
            .or_else(|| {
                tool.get("function")
                    .and_then(|function| function.get("annotations"))
            })
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolAnnotations>(value).ok())
            .or_else(|| {
                policy
                    .and_then(|policy| policy.tool_annotations.get(name))
                    .cloned()
            });
        entries.push(ToolEntry {
            name: name.to_string(),
            parameter_keys,
            has_schema,
            annotations,
            has_executor: true,
            defer_loading: tool
                .get("defer_loading")
                .and_then(|value| value.as_bool())
                .or_else(|| {
                    tool.get("function")
                        .and_then(|function| function.get("defer_loading"))
                        .and_then(|value| value.as_bool())
                })
                .unwrap_or(false),
            provider_native: true,
        });
    }
}

fn effective_active_names(
    entries: &[ToolEntry],
    policy: Option<&CapabilityPolicy>,
) -> BTreeSet<String> {
    let policy_tools = policy.map(|policy| policy.tools.as_slice()).unwrap_or(&[]);
    entries
        .iter()
        .filter(|entry| {
            policy_tools.is_empty()
                || policy_tools
                    .iter()
                    .any(|pattern| crate::orchestration::glob_match(pattern, &entry.name))
        })
        .map(|entry| entry.name.clone())
        .collect()
}

fn validate_execute_result_routes(
    entry: &ToolEntry,
    entries: &[ToolEntry],
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    let Some(annotations) = entry.annotations.as_ref() else {
        return;
    };
    if annotations.kind != ToolKind::Execute || !annotations.emits_artifacts {
        return;
    }
    if annotations.inline_result {
        return;
    }
    let active_reader_declared = annotations
        .result_readers
        .iter()
        .any(|reader| active_names.contains(reader));
    let command_output_reader = active_names.contains("read_command_output");
    let read_tool = entries.iter().any(|candidate| {
        active_names.contains(candidate.name.as_str())
            && candidate
                .annotations
                .as_ref()
                .is_some_and(|a| a.kind == ToolKind::Read || a.kind == ToolKind::Search)
    });
    if !active_reader_declared && !command_output_reader && !read_tool {
        diagnostics.push(
            ToolSurfaceDiagnostic::error(
                "TOOL_SURFACE_MISSING_RESULT_READER",
                format!(
                    "execute tool '{}' can emit output artifacts but has no active result reader",
                    entry.name
                ),
            )
            .with_tool(entry.name.clone())
            .with_field("result_readers"),
        );
    }
    for reader in &annotations.result_readers {
        if !active_names.contains(reader) {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_UNKNOWN_RESULT_READER",
                    format!(
                        "tool '{}' declares result reader '{}' that is not active",
                        entry.name, reader
                    ),
                )
                .with_tool(entry.name.clone())
                .with_field("result_readers"),
            );
        }
    }
}

fn validate_arg_constraints(
    policy: Option<&CapabilityPolicy>,
    entries: &[ToolEntry],
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    let Some(policy) = policy else { return };
    for constraint in &policy.tool_arg_constraints {
        let matched = entries
            .iter()
            .filter(|entry| active_names.contains(entry.name.as_str()))
            .filter(|entry| crate::orchestration::glob_match(&constraint.tool, &entry.name))
            .collect::<Vec<_>>();
        if matched.is_empty() && !constraint.tool.contains('*') {
            diagnostics.push(
                ToolSurfaceDiagnostic::warning(
                    "TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_TOOL",
                    format!(
                        "ToolArgConstraint references tool '{}' which is not active",
                        constraint.tool
                    ),
                )
                .with_tool(constraint.tool.clone())
                .with_field("tool_arg_constraints.tool"),
            );
        }
        if let Some(arg_key) = constraint.arg_key.as_ref() {
            for entry in matched {
                let annotation_keys = entry
                    .annotations
                    .as_ref()
                    .map(|a| {
                        a.arg_schema
                            .path_params
                            .iter()
                            .chain(a.arg_schema.required.iter())
                            .chain(a.arg_schema.arg_aliases.keys())
                            .chain(a.arg_schema.arg_aliases.values())
                            .cloned()
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default();
                if !entry.parameter_keys.contains(arg_key) && !annotation_keys.contains(arg_key) {
                    diagnostics.push(
                        ToolSurfaceDiagnostic::warning(
                            "TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_KEY",
                            format!(
                                "ToolArgConstraint for '{}' targets unknown argument '{}'",
                                entry.name, arg_key
                            ),
                        )
                        .with_tool(entry.name.clone())
                        .with_field(format!("tool_arg_constraints.{arg_key}")),
                    );
                }
            }
        }
    }
}

fn validate_approval_patterns(
    approval: Option<&ToolApprovalPolicy>,
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    let Some(approval) = approval else { return };
    for (field, patterns) in [
        ("approval_policy.auto_approve", &approval.auto_approve),
        ("approval_policy.auto_deny", &approval.auto_deny),
        (
            "approval_policy.require_approval",
            &approval.require_approval,
        ),
    ] {
        for pattern in patterns {
            validate_approval_tool_pattern(pattern, field, active_names, diagnostics);
        }
    }
    for (index, rule) in approval.rules.iter().enumerate() {
        for pattern in &rule.matches.tool {
            validate_approval_tool_pattern(
                pattern,
                &format!("approval_policy.rules[{index}].tool"),
                active_names,
                diagnostics,
            );
        }
    }
}

fn validate_approval_tool_pattern(
    pattern: &str,
    field: &str,
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    if pattern.contains('*') {
        return;
    }
    if !active_names
        .iter()
        .any(|name| crate::orchestration::glob_match(pattern, name))
    {
        diagnostics.push(
            ToolSurfaceDiagnostic::warning(
                "TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH",
                format!("{field} pattern '{pattern}' matches no active tool"),
            )
            .with_field(field),
        );
    }
}

fn validate_prompt_references(
    input: &ToolSurfaceInput,
    entries: &[ToolEntry],
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    let deferred = entries
        .iter()
        .filter(|entry| entry.defer_loading)
        .map(|entry| entry.name.clone())
        .collect::<BTreeSet<_>>();
    let known_names = entries
        .iter()
        .map(|entry| entry.name.clone())
        .chain(active_names.iter().cloned())
        .collect::<BTreeSet<_>>();
    for text in &input.prompt_texts {
        let binding_text = prompt_binding_text(text);
        let calls = prompt_tool_calls(&binding_text);
        for call in &calls {
            let name = call.name;
            if !known_names.contains(name) && looks_like_tool_name(name) {
                diagnostics.push(
                    ToolSurfaceDiagnostic::warning(
                        "TOOL_SURFACE_UNKNOWN_PROMPT_TOOL",
                        format!("prompt references tool '{name}' which is not active"),
                    )
                    .with_tool(name.to_string())
                    .with_field("prompt"),
                );
                continue;
            }
            if known_names.contains(name) && !active_names.contains(name) {
                diagnostics.push(
                    ToolSurfaceDiagnostic::warning(
                        "TOOL_SURFACE_PROMPT_TOOL_NOT_IN_POLICY",
                        format!("prompt references tool '{name}' outside the active policy"),
                    )
                    .with_tool(name.to_string())
                    .with_field("prompt"),
                );
            }
            if deferred.contains(name) && !input.tool_search_active {
                diagnostics.push(
                    ToolSurfaceDiagnostic::warning(
                        "TOOL_SURFACE_DEFERRED_TOOL_PROMPT_REFERENCE",
                        format!(
                            "prompt references deferred tool '{name}' but tool_search is not active"
                        ),
                    )
                    .with_tool(name.to_string())
                    .with_field("prompt"),
                );
            }
        }
        for entry in entries {
            let Some(annotations) = entry.annotations.as_ref() else {
                continue;
            };
            for (alias, canonical) in &annotations.arg_schema.arg_aliases {
                if calls
                    .iter()
                    .any(|call| call.name == entry.name && contains_token(call.text, alias))
                {
                    diagnostics.push(
                        ToolSurfaceDiagnostic::warning(
                            "TOOL_SURFACE_DEPRECATED_ARG_ALIAS",
                            format!(
                                "prompt mentions alias '{}' for tool '{}'; use canonical argument '{}'",
                                alias, entry.name, canonical
                            ),
                        )
                        .with_tool(entry.name.clone())
                        .with_field(format!("arg_schema.arg_aliases.{alias}")),
                    );
                }
            }
        }
    }
}

struct PromptToolCall<'a> {
    name: &'a str,
    text: &'a str,
}

fn prompt_tool_calls(text: &str) -> Vec<PromptToolCall<'_>> {
    let mut calls = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some((open_tag, close_tag)) = text_tool_call_tag_pairs()
            .into_iter()
            .find(|(open_tag, _)| bytes[i..].starts_with(open_tag.as_bytes()))
        {
            let call_start = i;
            i += open_tag.len();
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let name_start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            if i > name_start {
                let call_end = text[i..]
                    .find(close_tag)
                    .map(|offset| i + offset + close_tag.len())
                    .unwrap_or(i);
                calls.push(PromptToolCall {
                    name: &text[name_start..i],
                    text: &text[call_start..call_end],
                });
                i = call_end;
            }
            continue;
        }

        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        while i < bytes.len() && is_ident_byte(bytes[i]) {
            i += 1;
        }

        let name = &text[start..i];
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' && !prompt_ref_stopword(name) {
            let end = prompt_call_end(bytes, j);
            calls.push(PromptToolCall {
                name,
                text: &text[start..end],
            });
            i = end;
            continue;
        }
    }
    calls
}

fn prompt_call_end(bytes: &[u8], open_index: usize) -> usize {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut i = open_index;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(quote_byte) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote_byte {
                quote = None;
            }
            i += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' | b'`' => quote = Some(byte),
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

fn validate_side_effect_ceiling(
    policy: Option<&CapabilityPolicy>,
    entries: &[ToolEntry],
    active_names: &BTreeSet<String>,
    diagnostics: &mut Vec<ToolSurfaceDiagnostic>,
) {
    let Some(policy) = policy else { return };
    let Some(ceiling) = policy
        .side_effect_level
        .as_deref()
        .map(SideEffectLevel::parse)
    else {
        return;
    };
    for entry in entries
        .iter()
        .filter(|entry| active_names.contains(entry.name.as_str()))
    {
        let Some(level) = entry.annotations.as_ref().map(|a| a.side_effect_level) else {
            continue;
        };
        if level.rank() > ceiling.rank() {
            diagnostics.push(
                ToolSurfaceDiagnostic::error(
                    "TOOL_SURFACE_SIDE_EFFECT_CEILING_EXCEEDED",
                    format!(
                        "tool '{}' requires side-effect level '{}' but policy ceiling is '{}'",
                        entry.name,
                        level.as_str(),
                        ceiling.as_str()
                    ),
                )
                .with_tool(entry.name.clone())
                .with_field("side_effect_level"),
            );
        }
    }
}

pub fn prompt_tool_references(text: &str) -> BTreeSet<String> {
    let text = prompt_binding_text(text);
    prompt_tool_calls(&text)
        .into_iter()
        .map(|call| call.name.to_string())
        .collect()
}

fn prompt_binding_text(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    let mut ignore_block = false;
    let mut ignore_next = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if trimmed.contains("harn-tool-surface: ignore-start") {
            ignore_block = true;
            continue;
        }
        if trimmed.contains("harn-tool-surface: ignore-end") {
            ignore_block = false;
            continue;
        }
        if trimmed.contains("harn-tool-surface: ignore-next-line") {
            ignore_next = true;
            continue;
        }
        if in_fence
            || ignore_block
            || trimmed.contains("harn-tool-surface: ignore-line")
            || trimmed.contains("tool-surface-ignore")
        {
            continue;
        }
        if ignore_next {
            ignore_next = false;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn prompt_ref_stopword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "return"
            | "function"
            | "fn"
            | "JSON"
            | "print"
            | "println"
            | "contains"
            | "len"
            | "render"
            | "render_prompt"
    )
}

fn looks_like_tool_name(name: &str) -> bool {
    name.contains('_') || name.starts_with("tool") || name.starts_with("run")
}

fn contains_token(text: &str, needle: &str) -> bool {
    let bytes = text.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() || bytes.len() < needle_bytes.len() {
        return false;
    }
    for i in 0..=bytes.len() - needle_bytes.len() {
        if &bytes[i..i + needle_bytes.len()] != needle_bytes {
            continue;
        }
        let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let after = i + needle_bytes.len();
        let after_ok = after == bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_tool_registry_like(value: &VmValue) -> bool {
    value.as_dict().is_some_and(|dict| {
        dict.get("_type")
            .is_some_and(|value| value.display() == "tool_registry")
            || dict.contains_key("tools")
    })
}

fn vm_parameter_keys(value: Option<&VmValue>) -> (bool, BTreeSet<String>) {
    let Some(value) = value else {
        return (false, BTreeSet::new());
    };
    let json = crate::llm::vm_value_to_json(value);
    json_parameter_keys(Some(&json))
}

fn json_parameter_keys(value: Option<&serde_json::Value>) -> (bool, BTreeSet<String>) {
    let Some(value) = value else {
        return (false, BTreeSet::new());
    };
    let mut keys = BTreeSet::new();
    if let Some(properties) = value.get("properties").and_then(|value| value.as_object()) {
        keys.extend(properties.keys().cloned());
    } else if let Some(map) = value.as_object() {
        for key in map.keys() {
            if key != "type" && key != "required" && key != "description" {
                keys.insert(key.clone());
            }
        }
    }
    (true, keys)
}

fn workflow_node_tools_as_native(
    node: &crate::orchestration::WorkflowNode,
) -> Vec<serde_json::Value> {
    match &node.tools {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(_) => vec![node.tools.clone()],
        _ => Vec::new(),
    }
}

fn workflow_tools_as_native(
    policy: &CapabilityPolicy,
    nodes: &BTreeMap<String, crate::orchestration::WorkflowNode>,
) -> Vec<serde_json::Value> {
    let mut tools = Vec::new();
    let mut seen = BTreeSet::new();
    for node in nodes.values() {
        for tool in workflow_node_tools_as_native(node) {
            let name = tool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() && seen.insert(name) {
                tools.push(tool);
            }
        }
    }
    for (name, annotations) in &policy.tool_annotations {
        if seen.insert(name.clone()) {
            tools.push(serde_json::json!({
                "name": name,
                "parameters": {"type": "object"},
                "annotations": annotations,
                "executor": "host_bridge",
            }));
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ToolArgConstraint;
    use crate::tool_annotations::ToolArgSchema;

    fn execute_annotations() -> ToolAnnotations {
        ToolAnnotations {
            kind: ToolKind::Execute,
            side_effect_level: SideEffectLevel::ProcessExec,
            emits_artifacts: true,
            ..ToolAnnotations::default()
        }
    }

    #[test]
    fn tool_policy_preserves_agent_loop_transport_ceiling() {
        let mut annotations = ToolAnnotations {
            kind: ToolKind::Search,
            side_effect_level: SideEffectLevel::ReadOnly,
            ..ToolAnnotations::default()
        };
        annotations
            .capabilities
            .insert("workspace".into(), vec!["read_text".into()]);
        let policy = tool_capability_policy_from_spec(&serde_json::json!({
            "_type": "tool_registry",
            "tools": [
                {
                    "name": "look",
                    "parameters": {"type": "object"},
                    "policy": annotations
                }
            ]
        }));

        assert_eq!(policy.tools, vec!["look".to_string()]);
        assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
        assert!(policy
            .capabilities
            .get("llm")
            .is_some_and(|ops| ops.contains(&"call".to_string())));
        assert!(policy
            .capabilities
            .get("workspace")
            .is_some_and(|ops| ops.contains(&"read_text".to_string())));
    }

    #[test]
    fn tool_policy_without_capabilities_keeps_capability_ceiling_unspecified() {
        let policy = tool_capability_policy_from_spec(&serde_json::json!({
            "_type": "tool_registry",
            "tools": [
                {
                    "name": "look",
                    "parameters": {"type": "object"}
                }
            ]
        }));

        assert_eq!(policy.tools, vec!["look".to_string()]);
        assert!(policy.capabilities.is_empty());
        assert!(policy.side_effect_level.is_none());
    }

    #[test]
    fn execute_artifact_tool_requires_reader() {
        let mut policy = CapabilityPolicy::default();
        policy
            .tool_annotations
            .insert("run".into(), execute_annotations());
        let tools = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "_type".into(),
                VmValue::String(std::sync::Arc::from("tool_registry")),
            ),
            (
                "tools".into(),
                VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
                    std::sync::Arc::new(BTreeMap::from([
                        ("name".into(), VmValue::String(std::sync::Arc::from("run"))),
                        (
                            "parameters".into(),
                            VmValue::Dict(std::sync::Arc::new(BTreeMap::new())),
                        ),
                        (
                            "executor".into(),
                            VmValue::String(std::sync::Arc::from("host_bridge")),
                        ),
                    ])),
                )])),
            ),
        ])));
        let report = validate_tool_surface(&ToolSurfaceInput {
            tools: Some(tools),
            policy: Some(policy),
            ..ToolSurfaceInput::default()
        });
        assert!(report.diagnostics.iter().any(|d| {
            d.code == "TOOL_SURFACE_MISSING_RESULT_READER"
                && d.severity == ToolSurfaceSeverity::Error
        }));
        assert!(!report.valid);
    }

    #[test]
    fn execute_artifact_tool_accepts_inline_escape_hatch() {
        let mut annotations = execute_annotations();
        annotations.inline_result = true;
        let mut policy = CapabilityPolicy::default();
        policy.tool_annotations.insert("run".into(), annotations);
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "run",
                "parameters": {"type": "object"},
            })]),
            policy: Some(policy),
            ..ToolSurfaceInput::default()
        });
        assert!(!report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_MISSING_RESULT_READER"));
    }

    #[test]
    fn native_tool_annotations_are_read_from_tool_json() {
        let mut annotations = execute_annotations();
        annotations.inline_result = true;
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "run",
                "parameters": {"type": "object"},
                "annotations": annotations,
            })]),
            ..ToolSurfaceInput::default()
        });
        assert!(!report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_MISSING_ANNOTATIONS"));
        assert!(!report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_MISSING_RESULT_READER"));
    }

    #[test]
    fn prompt_reference_outside_policy_is_reported() {
        let policy = CapabilityPolicy {
            tools: vec!["read_file".into()],
            ..CapabilityPolicy::default()
        };
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![
                serde_json::json!({"name": "read_file", "parameters": {"type": "object"}}),
                serde_json::json!({"name": "run_command", "parameters": {"type": "object"}}),
            ]),
            policy: Some(policy),
            prompt_texts: vec!["Use run_command({command: \"cargo test\"})".into()],
            ..ToolSurfaceInput::default()
        });
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_PROMPT_TOOL_NOT_IN_POLICY"));
    }

    #[test]
    fn approval_rule_tool_references_are_reported() {
        let approval_policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "rules": [
                {"ask": {"tool": "missing_tool"}, "reason": "unknown"},
                {"allow": {"tool": "read_*"}}
            ]
        }))
        .unwrap();
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "read_file",
                "parameters": {"type": "object"},
            })]),
            approval_policy: Some(approval_policy),
            ..ToolSurfaceInput::default()
        });

        assert!(report.diagnostics.iter().any(|d| {
            d.code == "TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH"
                && d.field.as_deref() == Some("approval_policy.rules[0].tool")
        }));
        assert!(!report.diagnostics.iter().any(|d| {
            d.code == "TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH"
                && d.field.as_deref() == Some("approval_policy.rules[1].tool")
        }));
    }

    #[test]
    fn prompt_suppression_ignores_examples() {
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "read_file",
                "parameters": {"type": "object"},
            })]),
            prompt_texts: vec![
                "```text\nrun_command({command: \"old\"})\n```\n<!-- harn-tool-surface: ignore-next-line -->\nrun_command({command: \"old\"})".into(),
            ],
            ..ToolSurfaceInput::default()
        });
        assert!(!report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_UNKNOWN_PROMPT_TOOL"));
    }

    #[test]
    fn deprecated_alias_warnings_are_scoped_to_matching_tool_calls() {
        let mut edit_annotations = ToolAnnotations::default();
        edit_annotations
            .arg_schema
            .arg_aliases
            .insert("file".into(), "path".into());
        let mut look_annotations = ToolAnnotations::default();
        look_annotations
            .arg_schema
            .arg_aliases
            .insert("path".into(), "file".into());

        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![
                serde_json::json!({
                    "name": "edit",
                    "parameters": {"type": "object"},
                    "annotations": edit_annotations,
                }),
                serde_json::json!({
                    "name": "look",
                    "parameters": {"type": "object"},
                    "annotations": look_annotations,
                }),
            ]),
            prompt_texts: vec![
                "Use edit({ path: \"src/main.rs\", action: \"replace\" }) before look({ file: \"src/main.rs\" }).".into(),
            ],
            ..ToolSurfaceInput::default()
        });

        assert!(!report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
    }

    #[test]
    fn deprecated_alias_warnings_still_report_matching_multiline_calls() {
        let mut annotations = ToolAnnotations::default();
        annotations
            .arg_schema
            .arg_aliases
            .insert("file".into(), "path".into());

        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "edit",
                "parameters": {"type": "object"},
                "annotations": annotations,
            })]),
            prompt_texts: vec!["Use edit({\n  file: \"src/main.rs\"\n}) once.".into()],
            ..ToolSurfaceInput::default()
        });

        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
    }

    #[test]
    fn deprecated_alias_warnings_report_tagged_text_mode_calls() {
        let mut annotations = ToolAnnotations::default();
        annotations
            .arg_schema
            .arg_aliases
            .insert("file".into(), "path".into());

        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "edit",
                "parameters": {"type": "object"},
                "annotations": annotations,
            })]),
            prompt_texts: vec!["<tool_call>\nedit({ file: \"src/main.rs\" })\n</tool_call>".into()],
            ..ToolSurfaceInput::default()
        });

        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
    }

    #[test]
    fn prompt_reference_scanner_tolerates_non_ascii_text() {
        let references = prompt_tool_references("Résumé: use run_command({command: \"test\"})");
        assert!(references.contains("run_command"));
    }

    #[test]
    fn prompt_reference_scanner_reads_tagged_text_mode_calls() {
        let references =
            prompt_tool_references("<tool_call>\nrun({ command: \"cargo test\" })\n</tool_call>");
        assert!(references.contains("run"));
    }

    #[test]
    fn arg_constraint_key_must_exist() {
        let mut annotations = ToolAnnotations {
            kind: ToolKind::Read,
            side_effect_level: SideEffectLevel::ReadOnly,
            arg_schema: ToolArgSchema {
                path_params: vec!["path".into()],
                ..ToolArgSchema::default()
            },
            ..ToolAnnotations::default()
        };
        annotations.arg_schema.required.push("path".into());
        let mut policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "read_file".into(),
                arg_key: Some("missing".into()),
                arg_patterns: vec!["src/**".into()],
            }],
            ..CapabilityPolicy::default()
        };
        policy
            .tool_annotations
            .insert("read_file".into(), annotations);
        let report = validate_tool_surface(&ToolSurfaceInput {
            native_tools: Some(vec![serde_json::json!({
                "name": "read_file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
            })]),
            policy: Some(policy),
            ..ToolSurfaceInput::default()
        });
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_KEY"));
    }
}
