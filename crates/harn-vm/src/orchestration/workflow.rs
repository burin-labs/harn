//! Workflow graph types, normalization, validation, and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::{
    new_id, now_rfc3339, redact_transcript_visibility, ArtifactRecord, AutoCompactPolicy,
    BranchSemantics, CapabilityPolicy, ContextPolicy, EscalationPolicy, JoinPolicy, MapPolicy,
    ModelPolicy, ReducePolicy, RetryPolicy, StageContract,
};
use crate::llm::{extract_llm_options, vm_call_llm_full, vm_value_to_json};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};
use crate::value::{VmError, VmValue};

pub const WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY: &str = "workflow_verification_contracts";
pub const WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY: &str = "workflow_verification_scope";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowNode {
    pub id: Option<String>,
    pub kind: String,
    pub mode: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub task_label: Option<String>,
    pub done_sentinel: Option<String>,
    pub tools: serde_json::Value,
    pub model_policy: ModelPolicy,
    /// Per-stage auto-compaction settings for the agent loop's context
    /// window. Lifecycle operations (reset, fork, trim, compact) are NOT
    /// expressible here — call the `agent_session_*` builtins before the
    /// stage or in a prior stage.
    pub auto_compact: AutoCompactPolicy,
    /// Output visibility filter applied to the transcript after the
    /// stage's agent loop exits. `"public"` / `"public_only"` drops
    /// `tool_result` messages and non-public events. `None` or any
    /// unknown string is a no-op.
    #[serde(default)]
    pub output_visibility: Option<String>,
    pub context_policy: ContextPolicy,
    pub retry_policy: RetryPolicy,
    pub capability_policy: CapabilityPolicy,
    pub approval_policy: super::ToolApprovalPolicy,
    pub input_contract: StageContract,
    pub output_contract: StageContract,
    pub branch_semantics: BranchSemantics,
    pub map_policy: MapPolicy,
    pub join_policy: JoinPolicy,
    pub reduce_policy: ReducePolicy,
    pub escalation_policy: EscalationPolicy,
    pub verify: Option<serde_json::Value>,
    /// When true, the stage's agent loop gates the done sentinel on the most
    /// recent `run()` tool call exiting cleanly (`exit_code == 0`). Use for
    /// persistent execute stages that fold verification into the loop via a
    /// shell-exec tool the model invokes explicitly.
    #[serde(default)]
    pub exit_when_verified: bool,
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub raw_tools: Option<VmValue>,
    /// Raw auto_compact VmValue dict — preserved for extracting closure
    /// fields (compress_callback, mask_callback, custom_compactor) that
    /// can't go through serde.
    #[serde(skip)]
    pub raw_auto_compact: Option<VmValue>,
    /// Raw model_policy VmValue dict — preserved for extracting closure
    /// fields (post_turn_callback) that can't go through serde.
    #[serde(skip)]
    pub raw_model_policy: Option<VmValue>,
    /// Raw context_assembler VmValue dict — when set, the stage's
    /// artifact context is packed through `assemble_context` before
    /// rendering the system prompt. Closure fields (`ranker_callback`)
    /// are preserved here because they can't round-trip through serde.
    #[serde(skip)]
    pub raw_context_assembler: Option<VmValue>,
}

impl PartialEq for WorkflowNode {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationRequirement {
    pub kind: String,
    pub value: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationContract {
    pub source_node: Option<String>,
    pub summary: Option<String>,
    pub command: Option<String>,
    pub expect_status: Option<i64>,
    pub assert_text: Option<String>,
    pub expect_text: Option<String>,
    pub required_identifiers: Vec<String>,
    pub required_paths: Vec<String>,
    pub required_text: Vec<String>,
    pub notes: Vec<String>,
    pub checks: Vec<VerificationRequirement>,
}

impl VerificationContract {
    fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.command.is_none()
            && self.expect_status.is_none()
            && self.assert_text.is_none()
            && self.expect_text.is_none()
            && self.required_identifiers.is_empty()
            && self.required_paths.is_empty()
            && self.required_text.is_empty()
            && self.notes.is_empty()
            && self.checks.is_empty()
    }
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if !values.iter().any(|existing| existing == trimmed) {
        values.push(trimmed.to_string());
    }
}

fn push_unique_requirement(
    values: &mut Vec<VerificationRequirement>,
    kind: &str,
    value: &str,
    note: Option<&str>,
) {
    let trimmed_kind = kind.trim();
    let trimmed_value = value.trim();
    let trimmed_note = note
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string());
    if trimmed_kind.is_empty() || trimmed_value.is_empty() {
        return;
    }
    let candidate = VerificationRequirement {
        kind: trimmed_kind.to_string(),
        value: trimmed_value.to_string(),
        note: trimmed_note,
    };
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

fn json_string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(text)) => {
            let mut values = Vec::new();
            push_unique_string(&mut values, text);
            values
        }
        Some(serde_json::Value::Array(items)) => {
            let mut values = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    push_unique_string(&mut values, text);
                }
            }
            values
        }
        _ => Vec::new(),
    }
}

fn merge_verification_requirement_list(
    target: &mut Vec<VerificationRequirement>,
    value: Option<&serde_json::Value>,
) {
    let Some(items) = value.and_then(|raw| raw.as_array()) else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let kind = object
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let value = object
            .get("value")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let note = object
            .get("note")
            .or_else(|| object.get("description"))
            .or_else(|| object.get("reason"))
            .and_then(|value| value.as_str());
        push_unique_requirement(target, kind, value, note);
    }
}

fn merge_verification_contract_fields(
    target: &mut VerificationContract,
    object: &serde_json::Map<String, serde_json::Value>,
) {
    if target.summary.is_none() {
        target.summary = object
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.command.is_none() {
        target.command = object
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_status.is_none() {
        target.expect_status = object.get("expect_status").and_then(|value| value.as_i64());
    }
    if target.assert_text.is_none() {
        target.assert_text = object
            .get("assert_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_text.is_none() {
        target.expect_text = object
            .get("expect_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }

    for value in json_string_list(
        object
            .get("required_identifiers")
            .or_else(|| object.get("identifiers")),
    ) {
        push_unique_string(&mut target.required_identifiers, &value);
    }
    for value in json_string_list(object.get("required_paths").or_else(|| object.get("paths"))) {
        push_unique_string(&mut target.required_paths, &value);
    }
    for value in json_string_list(
        object
            .get("required_text")
            .or_else(|| object.get("exact_text"))
            .or_else(|| object.get("required_strings")),
    ) {
        push_unique_string(&mut target.required_text, &value);
    }
    for value in json_string_list(object.get("notes")) {
        push_unique_string(&mut target.notes, &value);
    }
    merge_verification_requirement_list(&mut target.checks, object.get("checks"));
}

fn load_verification_contract_file(path: &str) -> Result<serde_json::Value, VmError> {
    let resolved = crate::stdlib::process::resolve_source_asset_path(path);
    let contents = std::fs::read_to_string(&resolved).map_err(|error| {
        VmError::Runtime(format!(
            "workflow verification contract read failed for {}: {error}",
            resolved.display()
        ))
    })?;
    serde_json::from_str(&contents).map_err(|error| {
        VmError::Runtime(format!(
            "workflow verification contract parse failed for {}: {error}",
            resolved.display()
        ))
    })
}

fn resolve_verification_contract_path(
    verify: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<serde_json::Value>, VmError> {
    let Some(path) = verify
        .get("contract_path")
        .or_else(|| verify.get("verification_contract_path"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(load_verification_contract_file(path)?))
}

pub fn verification_contract_from_verify(
    node_id: &str,
    verify: Option<&serde_json::Value>,
) -> Result<Option<VerificationContract>, VmError> {
    let Some(verify_object) = verify.and_then(|value| value.as_object()) else {
        return Ok(None);
    };

    let mut contract = VerificationContract {
        source_node: Some(node_id.to_string()),
        ..Default::default()
    };

    if let Some(file_contract) = resolve_verification_contract_path(verify_object)? {
        let Some(object) = file_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verification contract file must parse to a JSON object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    if let Some(inline_contract) = verify_object.get("contract") {
        let Some(object) = inline_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verify.contract must be an object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    merge_verification_contract_fields(&mut contract, verify_object);

    if let Some(assert_text) = contract.assert_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "visible_text_contains",
            &assert_text,
            Some("verify stage requires visible output to contain this text"),
        );
    }
    if let Some(expect_text) = contract.expect_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "combined_output_contains",
            &expect_text,
            Some("verify command requires combined stdout/stderr to contain this text"),
        );
    }
    if let Some(expect_status) = contract.expect_status {
        push_unique_requirement(
            &mut contract.checks,
            "expect_status",
            &expect_status.to_string(),
            Some("verify command exit status must match exactly"),
        );
    }
    for identifier in contract.required_identifiers.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "identifier",
            &identifier,
            Some("use this exact identifier spelling"),
        );
    }
    for path in contract.required_paths.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "path",
            &path,
            Some("preserve this exact path"),
        );
    }
    for text in contract.required_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "text",
            &text,
            Some("required exact text or wiring snippet"),
        );
    }

    if contract.is_empty() {
        return Ok(None);
    }
    Ok(Some(contract))
}

fn push_unique_contract(values: &mut Vec<VerificationContract>, candidate: VerificationContract) {
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

pub fn workflow_verification_contracts(
    graph: &WorkflowGraph,
) -> Result<Vec<VerificationContract>, VmError> {
    let mut contracts = Vec::new();
    for (node_id, node) in &graph.nodes {
        if let Some(contract) = verification_contract_from_verify(node_id, node.verify.as_ref())? {
            push_unique_contract(&mut contracts, contract);
        }
    }
    Ok(contracts)
}

pub fn inject_workflow_verification_contracts(
    node: &mut WorkflowNode,
    contracts: &[VerificationContract],
) {
    if contracts.is_empty() {
        return;
    }
    node.metadata.insert(
        WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY.to_string(),
        serde_json::to_value(contracts).unwrap_or_default(),
    );
}

pub fn stage_verification_contracts(
    node_id: &str,
    node: &WorkflowNode,
) -> Result<Vec<VerificationContract>, VmError> {
    let local_contract = verification_contract_from_verify(node_id, node.verify.as_ref())?;
    let local_only = matches!(
        node.metadata
            .get(WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY)
            .and_then(|value| value.as_str()),
        Some("local_only")
    );
    if local_only {
        return Ok(local_contract.into_iter().collect());
    }

    let mut contracts = node
        .metadata
        .get(WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY)
        .cloned()
        .map(|value| {
            serde_json::from_value::<Vec<VerificationContract>>(value).map_err(|error| {
                VmError::Runtime(format!(
                    "workflow stage {node_id} verification contract metadata parse failed: {error}"
                ))
            })
        })
        .transpose()?
        .unwrap_or_default();

    if let Some(local_contract) = local_contract {
        push_unique_contract(&mut contracts, local_contract);
    }
    Ok(contracts)
}

pub fn workflow_tool_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| name.to_string()),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(workflow_tool_names)
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
                                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    (capability.clone(), values)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    // Accept both the structured `policy.arg_schema` object and the legacy
    // flat fields on `policy` so pipelines can migrate gradually.
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
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
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
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
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
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        inline_result: policy
            .get("inline_result")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    }
}

pub fn workflow_tool_annotations(value: &serde_json::Value) -> BTreeMap<String, ToolAnnotations> {
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
                    .map(workflow_tool_annotations)
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

pub fn workflow_tool_policy_from_tools(value: &serde_json::Value) -> CapabilityPolicy {
    let tools = workflow_tool_names(value);
    let tool_annotations = workflow_tool_annotations(value);
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
    let side_effect_level = max_side_effect_level(
        tool_annotations
            .values()
            .map(|annotations| annotations.side_effect_level.as_str().to_string())
            .filter(|level| level != "none"),
    );
    CapabilityPolicy {
        tools,
        capabilities,
        workspace_roots: Vec::new(),
        side_effect_level,
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    pub branch: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowGraph {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub version: usize,
    pub entry: String,
    pub nodes: BTreeMap<String, WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    pub capability_policy: CapabilityPolicy,
    pub approval_policy: super::ToolApprovalPolicy,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub audit_log: Vec<WorkflowAuditEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowAuditEntry {
    pub id: String,
    pub op: String,
    pub node_id: Option<String>,
    pub timestamp: String,
    pub reason: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reachable_nodes: Vec<String>,
}

pub fn parse_workflow_node_value(value: &VmValue, label: &str) -> Result<WorkflowNode, VmError> {
    let mut node: WorkflowNode = super::parse_json_payload(vm_value_to_json(value), label)?;
    let dict = value.as_dict();
    node.raw_tools = dict.and_then(|d| d.get("tools")).cloned();
    node.raw_auto_compact = dict.and_then(|d| d.get("auto_compact")).cloned();
    node.raw_model_policy = dict.and_then(|d| d.get("model_policy")).cloned();
    node.raw_context_assembler = dict.and_then(|d| d.get("context_assembler")).cloned();
    Ok(node)
}

pub fn parse_workflow_node_json(
    json: serde_json::Value,
    label: &str,
) -> Result<WorkflowNode, VmError> {
    super::parse_json_payload(json, label)
}

pub fn parse_workflow_edge_json(
    json: serde_json::Value,
    label: &str,
) -> Result<WorkflowEdge, VmError> {
    super::parse_json_payload(json, label)
}

pub fn normalize_workflow_value(value: &VmValue) -> Result<WorkflowGraph, VmError> {
    let mut graph: WorkflowGraph = super::parse_json_value(value)?;
    let as_dict = value.as_dict().cloned().unwrap_or_default();

    if graph.nodes.is_empty() {
        for key in ["act", "verify", "repair"] {
            if let Some(node_value) = as_dict.get(key) {
                let mut node = parse_workflow_node_value(node_value, "orchestration")?;
                let raw_node = node_value.as_dict().cloned().unwrap_or_default();
                node.id = Some(key.to_string());
                if node.kind.is_empty() {
                    node.kind = if key == "verify" {
                        "verify".to_string()
                    } else {
                        "stage".to_string()
                    };
                }
                if node.model_policy.provider.is_none() {
                    node.model_policy.provider = as_dict
                        .get("provider")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model.is_none() {
                    node.model_policy.model = as_dict
                        .get("model")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model_tier.is_none() {
                    node.model_policy.model_tier = as_dict
                        .get("model_tier")
                        .or_else(|| as_dict.get("tier"))
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.temperature.is_none() {
                    node.model_policy.temperature = as_dict.get("temperature").and_then(|value| {
                        if let VmValue::Float(number) = value {
                            Some(*number)
                        } else {
                            value.as_int().map(|number| number as f64)
                        }
                    });
                }
                if node.model_policy.max_tokens.is_none() {
                    node.model_policy.max_tokens =
                        as_dict.get("max_tokens").and_then(|value| value.as_int());
                }
                if node.mode.is_none() {
                    node.mode = as_dict
                        .get("mode")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.done_sentinel.is_none() {
                    node.done_sentinel = as_dict
                        .get("done_sentinel")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if key == "verify"
                    && node.verify.is_none()
                    && (raw_node.contains_key("assert_text")
                        || raw_node.contains_key("command")
                        || raw_node.contains_key("expect_status")
                        || raw_node.contains_key("expect_text"))
                {
                    node.verify = Some(serde_json::json!({
                        "assert_text": raw_node.get("assert_text").map(vm_value_to_json),
                        "command": raw_node.get("command").map(vm_value_to_json),
                        "expect_status": raw_node.get("expect_status").map(vm_value_to_json),
                        "expect_text": raw_node.get("expect_text").map(vm_value_to_json),
                    }));
                }
                graph.nodes.insert(key.to_string(), node);
            }
        }
        if graph.entry.is_empty() && graph.nodes.contains_key("act") {
            graph.entry = "act".to_string();
        }
        if graph.edges.is_empty() && graph.nodes.contains_key("act") {
            if graph.nodes.contains_key("verify") {
                graph.edges.push(WorkflowEdge {
                    from: "act".to_string(),
                    to: "verify".to_string(),
                    branch: None,
                    label: None,
                });
            }
            if graph.nodes.contains_key("repair") {
                graph.edges.push(WorkflowEdge {
                    from: "verify".to_string(),
                    to: "repair".to_string(),
                    branch: Some("failed".to_string()),
                    label: None,
                });
                graph.edges.push(WorkflowEdge {
                    from: "repair".to_string(),
                    to: "verify".to_string(),
                    branch: Some("retry".to_string()),
                    label: None,
                });
            }
        }
    }

    if graph.type_name.is_empty() {
        graph.type_name = "workflow_graph".to_string();
    }
    if graph.id.is_empty() {
        graph.id = new_id("workflow");
    }
    if graph.version == 0 {
        graph.version = 1;
    }
    if graph.entry.is_empty() {
        graph.entry = graph
            .nodes
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "act".to_string());
    }
    for (node_id, node) in &mut graph.nodes {
        if node.raw_tools.is_none() {
            node.raw_tools = as_dict
                .get("nodes")
                .and_then(|nodes| nodes.as_dict())
                .and_then(|nodes| nodes.get(node_id))
                .and_then(|node_value| node_value.as_dict())
                .and_then(|raw_node| raw_node.get("tools"))
                .cloned();
        }
        if node.id.is_none() {
            node.id = Some(node_id.clone());
        }
        if node.kind.is_empty() {
            node.kind = "stage".to_string();
        }
        if node.join_policy.strategy.is_empty() {
            node.join_policy.strategy = "all".to_string();
        }
        if node.reduce_policy.strategy.is_empty() {
            node.reduce_policy.strategy = "concat".to_string();
        }
        if node.output_contract.output_kinds.is_empty() {
            node.output_contract.output_kinds = vec![match node.kind.as_str() {
                "verify" => "verification_result".to_string(),
                "reduce" => node
                    .reduce_policy
                    .output_kind
                    .clone()
                    .unwrap_or_else(|| "summary".to_string()),
                "map" => node
                    .map_policy
                    .output_kind
                    .clone()
                    .unwrap_or_else(|| "artifact".to_string()),
                "escalation" => "plan".to_string(),
                _ => "artifact".to_string(),
            }];
        }
        if node.retry_policy.max_attempts == 0 {
            node.retry_policy.max_attempts = 1;
        }
    }
    Ok(graph)
}

pub fn validate_workflow(
    graph: &WorkflowGraph,
    ceiling: Option<&CapabilityPolicy>,
) -> WorkflowValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !graph.nodes.contains_key(&graph.entry) {
        errors.push(format!("entry node does not exist: {}", graph.entry));
    }

    let node_ids: BTreeSet<String> = graph.nodes.keys().cloned().collect();
    for edge in &graph.edges {
        if !node_ids.contains(&edge.from) {
            errors.push(format!("edge.from references unknown node: {}", edge.from));
        }
        if !node_ids.contains(&edge.to) {
            errors.push(format!("edge.to references unknown node: {}", edge.to));
        }
    }

    let reachable_nodes = reachable_nodes(graph);
    for node_id in &node_ids {
        if !reachable_nodes.contains(node_id) {
            warnings.push(format!("node is unreachable: {node_id}"));
        }
    }

    for (node_id, node) in &graph.nodes {
        let incoming = graph
            .edges
            .iter()
            .filter(|edge| edge.to == *node_id)
            .count();
        let outgoing: Vec<&WorkflowEdge> = graph
            .edges
            .iter()
            .filter(|edge| edge.from == *node_id)
            .collect();
        if let Some(min_inputs) = node.input_contract.min_inputs {
            if let Some(max_inputs) = node.input_contract.max_inputs {
                if min_inputs > max_inputs {
                    errors.push(format!(
                        "node {node_id}: input contract min_inputs exceeds max_inputs"
                    ));
                }
            }
        }
        match node.kind.as_str() {
            "condition" => {
                let has_true = outgoing
                    .iter()
                    .any(|edge| edge.branch.as_deref() == Some("true"));
                let has_false = outgoing
                    .iter()
                    .any(|edge| edge.branch.as_deref() == Some("false"));
                if !has_true || !has_false {
                    errors.push(format!(
                        "node {node_id}: condition nodes require both 'true' and 'false' branch edges"
                    ));
                }
            }
            "fork" if outgoing.len() < 2 => {
                errors.push(format!(
                    "node {node_id}: fork nodes require at least two outgoing edges"
                ));
            }
            "join" if incoming < 2 => {
                warnings.push(format!(
                    "node {node_id}: join node has fewer than two incoming edges"
                ));
            }
            "map"
                if node.map_policy.items.is_empty()
                    && node.map_policy.item_artifact_kind.is_none()
                    && node.input_contract.input_kinds.is_empty() =>
            {
                errors.push(format!(
                    "node {node_id}: map nodes require items, item_artifact_kind, or input_contract.input_kinds"
                ));
            }
            "reduce" if node.input_contract.input_kinds.is_empty() => {
                warnings.push(format!(
                    "node {node_id}: reduce node has no input_contract.input_kinds; it will consume all available artifacts"
                ));
            }
            _ => {}
        }
    }

    if let Some(ceiling) = ceiling {
        if let Err(error) = ceiling.intersect(&graph.capability_policy) {
            errors.push(error);
        }
        for (node_id, node) in &graph.nodes {
            if let Err(error) = ceiling.intersect(&node.capability_policy) {
                errors.push(format!("node {node_id}: {error}"));
            }
        }
    }

    for diagnostic in crate::tool_surface::validate_workflow_graph(graph) {
        let message = format!("{}: {}", diagnostic.code, diagnostic.message);
        match diagnostic.severity {
            crate::tool_surface::ToolSurfaceSeverity::Error => errors.push(message),
            crate::tool_surface::ToolSurfaceSeverity::Warning => warnings.push(message),
        }
    }

    WorkflowValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
        reachable_nodes: reachable_nodes.into_iter().collect(),
    }
}

fn reachable_nodes(graph: &WorkflowGraph) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![graph.entry.clone()];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.from == node_id) {
            stack.push(edge.to.clone());
        }
    }
    seen
}

/// Pick the session id a stage should run under. Prefers an explicit
/// `session_id` on the node's `model_policy` dict (so pipelines with
/// `agent_session_open` / `agent_session_fork` flowing through a graph
/// line up); falls back to a stable, node-derived id so multi-stage
/// graphs with no explicit session share a conversation across stages.
/// Per-stage skill registry. Per-node `model_policy.skills` takes
/// precedence over the workflow-level `run_options.skills` — authors
/// can scope a skill set to one stage without affecting siblings. When
/// neither is set, returns `None` so the agent loop runs without
/// skill matching (preserves pre-Gap-2 behavior for callers that
/// didn't opt in).
fn resolve_stage_skill_registry(node: &WorkflowNode) -> Option<VmValue> {
    let per_node = node
        .raw_model_policy
        .as_ref()
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("skills"))
        .cloned()
        .and_then(normalize_inline_registry);
    if per_node.is_some() {
        return per_node;
    }
    super::current_workflow_skill_context().and_then(|ctx| ctx.registry)
}

/// Mirror of `resolve_stage_skill_registry` for the match config:
/// per-node `model_policy.skill_match` wins, falling back to the
/// workflow-level setting.
fn resolve_stage_skill_match(node: &WorkflowNode) -> crate::llm::SkillMatchConfig {
    let per_node = node
        .raw_model_policy
        .as_ref()
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("skill_match"))
        .and_then(|v| v.as_dict().cloned());
    if let Some(dict) = per_node {
        return crate::llm::parse_skill_match_config_dict(&dict);
    }
    super::current_workflow_skill_context()
        .and_then(|ctx| ctx.match_config)
        .and_then(|v| v.as_dict().cloned())
        .map(|d| crate::llm::parse_skill_match_config_dict(&d))
        .unwrap_or_default()
}

/// Accept both a validated `skill_registry` dict and a bare list of
/// skill entries. The workflow-level parser in `register.rs` does the
/// same — we duplicate here so per-node `model_policy.skills` settings
/// (not routed through that parser) also benefit.
fn normalize_inline_registry(value: VmValue) -> Option<VmValue> {
    use std::collections::BTreeMap;
    use std::rc::Rc;
    match &value {
        VmValue::Dict(d)
            if d.get("_type")
                .map(|v| v.display() == "skill_registry")
                .unwrap_or(false) =>
        {
            Some(value)
        }
        VmValue::List(list) => {
            let mut dict = BTreeMap::new();
            dict.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("skill_registry")),
            );
            dict.insert("skills".to_string(), VmValue::List(list.clone()));
            Some(VmValue::Dict(Rc::new(dict)))
        }
        _ => None,
    }
}

fn resolve_node_session_id(node: &WorkflowNode) -> String {
    if let Some(explicit) = node
        .raw_model_policy
        .as_ref()
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("session_id"))
        .and_then(|v| match v {
            VmValue::String(s) if !s.trim().is_empty() => Some(s.to_string()),
            _ => None,
        })
    {
        return explicit;
    }
    if let Some(persisted) = node
        .metadata
        .get("worker_session_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return persisted.to_string();
    }
    format!("workflow_stage_{}", uuid::Uuid::now_v7())
}

fn raw_auto_compact_dict(
    node: &WorkflowNode,
) -> Option<&std::collections::BTreeMap<String, VmValue>> {
    node.raw_auto_compact
        .as_ref()
        .and_then(|value| value.as_dict())
}

fn raw_auto_compact_int(node: &WorkflowNode, key: &str) -> Option<usize> {
    raw_auto_compact_dict(node)
        .and_then(|dict| dict.get(key))
        .and_then(|value| value.as_int())
        .filter(|value| *value >= 0)
        .map(|value| value as usize)
}

fn raw_auto_compact_string(node: &WorkflowNode, key: &str) -> Option<String> {
    raw_auto_compact_dict(node)
        .and_then(|dict| dict.get(key))
        .and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        })
}

pub(crate) async fn resolve_stage_auto_compact(
    node: &WorkflowNode,
    opts: &crate::llm::api::LlmCallOptions,
) -> Result<Option<crate::orchestration::AutoCompactConfig>, VmError> {
    if !node.auto_compact.enabled {
        return Ok(None);
    }

    let mut ac = crate::orchestration::AutoCompactConfig::default();
    if let Some(v) = node.auto_compact.token_threshold {
        ac.token_threshold = v;
    }
    if let Some(v) = node.auto_compact.tool_output_max_chars {
        ac.tool_output_max_chars = v;
    }
    if let Some(ref strategy) = node.auto_compact.compact_strategy {
        if let Ok(s) = crate::orchestration::parse_compact_strategy(strategy) {
            ac.compact_strategy = s;
        }
    }
    if let Some(v) = node.auto_compact.hard_limit_tokens {
        ac.hard_limit_tokens = Some(v);
    }
    if let Some(ref strategy) = node.auto_compact.hard_limit_strategy {
        if let Ok(s) = crate::orchestration::parse_compact_strategy(strategy) {
            ac.hard_limit_strategy = s;
        }
    }

    // Workflow nodes keep the richer agent-loop-only compaction knobs in the
    // raw dict because the typed policy shape intentionally models only the
    // common workflow fields.
    if let Some(v) = raw_auto_compact_int(node, "compact_keep_last")
        .or_else(|| raw_auto_compact_int(node, "keep_last"))
    {
        ac.keep_last = v;
    }
    if let Some(prompt) = raw_auto_compact_string(node, "summarize_prompt") {
        ac.summarize_prompt = Some(prompt);
    }

    // Closure fields can't round-trip through serde, so extract them directly
    // from the raw VmValue dict.
    if let Some(dict) = raw_auto_compact_dict(node) {
        if let Some(cb) = dict.get("compress_callback") {
            ac.compress_callback = Some(cb.clone());
        }
        if let Some(cb) = dict.get("mask_callback") {
            ac.mask_callback = Some(cb.clone());
        }
        if let Some(cb) = dict.get("custom_compactor") {
            ac.custom_compactor = Some(cb.clone());
        }
    }

    let user_specified_threshold = node.auto_compact.token_threshold.is_some();
    let user_specified_hard_limit = node.auto_compact.hard_limit_tokens.is_some();
    crate::llm::api::adapt_auto_compact_to_provider(
        &mut ac,
        user_specified_threshold,
        user_specified_hard_limit,
        &opts.provider,
        &opts.model,
        &opts.api_key,
    )
    .await;

    Ok(Some(ac))
}

pub async fn execute_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let mut selection_policy = node.context_policy.clone();
    if selection_policy.include_kinds.is_empty() && !node.input_contract.input_kinds.is_empty() {
        selection_policy.include_kinds = node.input_contract.input_kinds.clone();
    }
    let selected = super::select_artifacts_adaptive(artifacts.to_vec(), &selection_policy);
    let rendered_context = if let Some(assembler) = node.raw_context_assembler.as_ref() {
        let assembled =
            crate::stdlib::assemble::assemble_from_options(&selected, assembler).await?;
        super::render_assembled_chunks(&assembled)
    } else {
        super::render_artifacts_context(&selected, &node.context_policy)
    };
    let verification_contracts = super::stage_verification_contracts(node_id, node)?;
    let rendered_verification = super::render_verification_context(&verification_contracts);
    let stage_session_id = resolve_node_session_id(node);
    if node.input_contract.require_transcript && !crate::agent_sessions::exists(&stage_session_id) {
        return Err(VmError::Runtime(format!(
            "workflow stage {node_id} requires an existing session \
             (call agent_session_open and feed session_id through model_policy \
             before entering this stage)"
        )));
    }
    if let Some(min_inputs) = node.input_contract.min_inputs {
        if selected.len() < min_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} requires at least {min_inputs} input artifacts"
            )));
        }
    }
    if let Some(max_inputs) = node.input_contract.max_inputs {
        if selected.len() > max_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} accepts at most {max_inputs} input artifacts"
            )));
        }
    }
    let prompt = super::render_workflow_prompt(
        task,
        node.task_label.as_deref(),
        &rendered_verification,
        &rendered_context,
    );

    // Precedence for the tool-calling contract format:
    //   1. explicit `model_policy.tool_format` on the node
    //   2. `HARN_AGENT_TOOL_FORMAT` env override
    //   3. provider/model default
    // Mirrors the top-level agent_loop / llm_call resolution so workflow
    // authors can pin `tool_format: "native"` per-stage and have it
    // reach the inner agent loop.
    let tool_format = node
        .model_policy
        .tool_format
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("HARN_AGENT_TOOL_FORMAT")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| {
            let model = std::env::var("HARN_LLM_MODEL").unwrap_or_default();
            let provider = std::env::var("HARN_LLM_PROVIDER").unwrap_or_default();
            crate::llm_config::default_tool_format(&model, &provider)
        });
    let mut llm_result = if node.kind == "verify" {
        if let Some(command) = node
            .verify
            .as_ref()
            .and_then(|verify| verify.as_object())
            .and_then(|verify| verify.get("command"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let (program, args) = if cfg!(target_os = "windows") {
                ("cmd", vec!["/C".to_string(), command.to_string()])
            } else {
                ("/bin/sh", vec!["-lc".to_string(), command.to_string()])
            };
            let mut process_config = crate::stdlib::sandbox::ProcessCommandConfig {
                stdin_null: true,
                ..Default::default()
            };
            if let Some(context) = crate::stdlib::process::current_execution_context() {
                if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
                    crate::stdlib::sandbox::enforce_process_cwd(std::path::Path::new(&cwd))?;
                    process_config.cwd = Some(std::path::PathBuf::from(cwd));
                }
                if !context.env.is_empty() {
                    process_config.env.extend(context.env);
                }
            }
            let output = crate::stdlib::sandbox::command_output(program, &args, &process_config)?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = if stderr.is_empty() {
                stdout.clone()
            } else if stdout.is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            serde_json::json!({
                "status": "completed",
                "text": combined,
                "visible_text": combined,
                "command": command,
                "stdout": stdout,
                "stderr": stderr,
                "exit_status": output.status.code().unwrap_or(-1),
                "success": output.status.success(),
            })
        } else {
            serde_json::json!({
                "status": "completed",
                "text": "",
                "visible_text": "",
            })
        }
    } else {
        let mut options = BTreeMap::new();
        if let Some(provider) = &node.model_policy.provider {
            options.insert(
                "provider".to_string(),
                VmValue::String(Rc::from(provider.clone())),
            );
        }
        if let Some(model) = &node.model_policy.model {
            options.insert(
                "model".to_string(),
                VmValue::String(Rc::from(model.clone())),
            );
        }
        if let Some(model_tier) = &node.model_policy.model_tier {
            options.insert(
                "model_tier".to_string(),
                VmValue::String(Rc::from(model_tier.clone())),
            );
        }
        if let Some(temperature) = node.model_policy.temperature {
            options.insert("temperature".to_string(), VmValue::Float(temperature));
        }
        if let Some(max_tokens) = node.model_policy.max_tokens {
            options.insert("max_tokens".to_string(), VmValue::Int(max_tokens));
        }
        let tool_names = workflow_tool_names(&node.tools);
        let tools_value = node.raw_tools.clone().or_else(|| {
            if matches!(node.tools, serde_json::Value::Null) {
                None
            } else {
                Some(crate::stdlib::json_to_vm_value(&node.tools))
            }
        });
        if tools_value.is_some() && !tool_names.is_empty() {
            options.insert("tools".to_string(), tools_value.unwrap_or(VmValue::Nil));
        }
        options.insert(
            "session_id".to_string(),
            VmValue::String(Rc::from(stage_session_id.clone())),
        );

        let args = vec![
            VmValue::String(Rc::from(prompt.clone())),
            node.system
                .clone()
                .map(|s| VmValue::String(Rc::from(s)))
                .unwrap_or(VmValue::Nil),
            VmValue::Dict(Rc::new(options)),
        ];
        let mut opts = extract_llm_options(&args)?;
        let auto_compact = resolve_stage_auto_compact(node, &opts).await?;

        if node.mode.as_deref() == Some("agent") || !tool_names.is_empty() {
            let tool_policy = workflow_tool_policy_from_tools(&node.tools);
            let effective_policy = tool_policy
                .intersect(&node.capability_policy)
                .map_err(VmError::Runtime)?;
            let permissions = crate::llm::permissions::parse_dynamic_permission_policy(
                node.raw_model_policy
                    .as_ref()
                    .and_then(|value| value.as_dict())
                    .and_then(|dict| dict.get("permissions")),
                "workflow model_policy",
            )?;
            crate::llm::run_agent_loop_internal(
                &mut opts,
                crate::llm::AgentLoopConfig {
                    persistent: true,
                    max_iterations: node.model_policy.max_iterations.unwrap_or(16),
                    max_nudges: node.model_policy.max_nudges.unwrap_or(3),
                    nudge: node.model_policy.nudge.clone(),
                    done_sentinel: node.done_sentinel.clone(),
                    break_unless_phase: None,
                    tool_retries: 0,
                    tool_backoff_ms: 1000,
                    schema_retries: 0,
                    schema_retry_nudge: crate::llm::parse_schema_nudge(
                        &node
                            .raw_model_policy
                            .as_ref()
                            .and_then(|value| value.as_dict())
                            .cloned(),
                    ),
                    tool_format: tool_format.clone(),
                    native_tool_fallback: node.model_policy.native_tool_fallback,
                    auto_compact,
                    policy: Some(effective_policy),
                    command_policy: crate::orchestration::parse_command_policy_value(
                        node.raw_model_policy
                            .as_ref()
                            .and_then(|value| value.as_dict())
                            .and_then(|dict| dict.get("command_policy"))
                            .or_else(|| {
                                node.raw_model_policy
                                    .as_ref()
                                    .and_then(|value| value.as_dict())
                                    .and_then(|dict| dict.get("policy"))
                                    .and_then(|value| value.as_dict())
                                    .and_then(|policy| policy.get("command_policy"))
                            }),
                        "workflow model_policy",
                    )?,
                    permissions,
                    approval_policy: Some(node.approval_policy.clone()),
                    daemon: false,
                    daemon_config: Default::default(),
                    llm_retries: 2,
                    llm_backoff_ms: 2000,
                    token_budget: None,
                    exit_when_verified: node.exit_when_verified,
                    loop_detect_warn: 2,
                    loop_detect_block: 3,
                    loop_detect_skip: 4,
                    tool_examples: node.model_policy.tool_examples.clone(),
                    turn_policy: node.model_policy.turn_policy.clone(),
                    stop_after_successful_tools: node
                        .model_policy
                        .stop_after_successful_tools
                        .clone(),
                    require_successful_tools: node.model_policy.require_successful_tools.clone(),
                    // Use the same session id resolved for the stage so
                    // agent_subscribe handlers keyed on it, and session
                    // storage lookups in the agent loop, stay consistent.
                    session_id: stage_session_id.clone(),
                    event_sink: None,
                    // Seed from the stage's explicit deliverables/ledger so the
                    // graph carries a task-wide plan through map branches and
                    // nested stages. Empty ledger means no gate.
                    task_ledger: node
                        .raw_model_policy
                        .as_ref()
                        .and_then(|v| v.as_dict())
                        .and_then(|d| d.get("task_ledger"))
                        .map(crate::llm::helpers::vm_value_to_json)
                        .and_then(|json| serde_json::from_value(json).ok())
                        .unwrap_or_default(),
                    post_turn_callback: node
                        .raw_model_policy
                        .as_ref()
                        .and_then(|v| v.as_dict())
                        .and_then(|d| d.get("post_turn_callback"))
                        .filter(|v| matches!(v, crate::value::VmValue::Closure(_)))
                        .cloned(),
                    // Inherit the workflow-level skill wiring installed
                    // by `workflow_execute`. Per-node `model_policy.skills`
                    // (optional) overrides, letting authors scope a skill
                    // set to one stage without affecting siblings. Empty
                    // thread-local = no skills configured (direct
                    // `execute_stage_node` callers outside a workflow).
                    skill_registry: resolve_stage_skill_registry(node),
                    skill_match: resolve_stage_skill_match(node),
                    working_files: Vec::new(),
                },
            )
            .await?
        } else {
            let result = vm_call_llm_full(&opts).await?;
            crate::llm::agent_loop_result_from_llm(&result, opts)
        }
    };
    if let Some(payload) = llm_result.as_object_mut() {
        payload.insert("prompt".to_string(), serde_json::json!(prompt));
        payload.insert(
            "system_prompt".to_string(),
            serde_json::json!(node.system.clone().unwrap_or_default()),
        );
        payload.insert(
            "rendered_context".to_string(),
            serde_json::json!(rendered_context),
        );
        if !verification_contracts.is_empty() {
            payload.insert(
                "verification_contracts".to_string(),
                serde_json::to_value(&verification_contracts).unwrap_or_default(),
            );
            payload.insert(
                "rendered_verification_context".to_string(),
                serde_json::json!(rendered_verification),
            );
        }
        payload.insert(
            "selected_artifact_ids".to_string(),
            serde_json::json!(selected
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "selected_artifact_titles".to_string(),
            serde_json::json!(selected
                .iter()
                .map(|artifact| artifact.title.clone())
                .collect::<Vec<_>>()),
        );
        match payload
            .entry("tools".to_string())
            .or_insert_with(|| serde_json::json!({}))
        {
            serde_json::Value::Object(tools) => {
                tools.insert("mode".to_string(), serde_json::json!(tool_format.clone()));
            }
            slot => {
                *slot = serde_json::json!({ "mode": tool_format.clone() });
            }
        }
    }

    let visible_text = llm_result["text"].as_str().unwrap_or_default().to_string();
    // Non-LLM stages (verify command, condition, fork, join, ...) don't produce
    // a "transcript" field; fall back to the input so cross-stage conversation
    // state survives transitions.
    let result_transcript = llm_result
        .get("transcript")
        .cloned()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    let session_transcript = crate::agent_sessions::snapshot(&stage_session_id);
    let transcript = result_transcript
        .or(session_transcript)
        .and_then(|value| redact_transcript_visibility(&value, node.output_visibility.as_deref()));
    let output_kind = node
        .output_contract
        .output_kinds
        .first()
        .cloned()
        .unwrap_or_else(|| {
            if node.kind == "verify" {
                "verification_result".to_string()
            } else {
                "artifact".to_string()
            }
        });
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "input_artifact_ids".to_string(),
        serde_json::json!(selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>()),
    );
    metadata.insert("node_kind".to_string(), serde_json::json!(node.kind));
    if !node.approval_policy.write_path_allowlist.is_empty() {
        metadata.insert(
            "changed_paths".to_string(),
            serde_json::json!(node.approval_policy.write_path_allowlist),
        );
    }
    let artifact = ArtifactRecord {
        type_name: "artifact".to_string(),
        id: new_id("artifact"),
        kind: output_kind,
        title: Some(format!("stage {node_id} output")),
        text: Some(visible_text),
        data: Some(llm_result.clone()),
        source: Some(node_id.to_string()),
        created_at: now_rfc3339(),
        freshness: Some("fresh".to_string()),
        priority: None,
        lineage: selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata,
    }
    .normalize();

    Ok((llm_result, vec![artifact], transcript))
}

pub fn next_nodes_for(
    graph: &WorkflowGraph,
    current: &str,
    branch: Option<&str>,
) -> Vec<WorkflowEdge> {
    let mut matching: Vec<WorkflowEdge> = graph
        .edges
        .iter()
        .filter(|edge| edge.from == current && edge.branch.as_deref() == branch)
        .cloned()
        .collect();
    if matching.is_empty() {
        matching = graph
            .edges
            .iter()
            .filter(|edge| edge.from == current && edge.branch.is_none())
            .cloned()
            .collect();
    }
    matching
}

pub fn next_node_for(graph: &WorkflowGraph, current: &str, branch: &str) -> Option<String> {
    next_nodes_for(graph, current, Some(branch))
        .into_iter()
        .next()
        .map(|edge| edge.to)
}

pub fn append_audit_entry(
    graph: &mut WorkflowGraph,
    op: &str,
    node_id: Option<String>,
    reason: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    graph.audit_log.push(WorkflowAuditEntry {
        id: new_id("audit"),
        op: op.to_string(),
        node_id,
        timestamp: now_rfc3339(),
        reason,
        metadata,
    });
}
