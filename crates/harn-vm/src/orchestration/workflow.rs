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
use crate::tool_surface::{tool_capability_policy_from_spec, tool_names_from_spec};
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

fn raw_model_policy_dict(node: &WorkflowNode) -> Option<&BTreeMap<String, VmValue>> {
    node.raw_model_policy
        .as_ref()
        .and_then(|value| value.as_dict())
}

fn insert_json_vm_option<T: Serialize>(
    options: &mut BTreeMap<String, VmValue>,
    key: &str,
    value: &T,
) -> Result<(), VmError> {
    let json = serde_json::to_value(value).map_err(|error| {
        VmError::Runtime(format!("workflow stage option encode error: {error}"))
    })?;
    options.insert(key.to_string(), crate::stdlib::json_to_vm_value(&json));
    Ok(())
}

fn merge_raw_model_policy_options(options: &mut BTreeMap<String, VmValue>, node: &WorkflowNode) {
    if let Some(raw) = raw_model_policy_dict(node) {
        for (key, value) in raw {
            if !matches!(value, VmValue::Nil) {
                options.insert(key.clone(), value.clone());
            }
        }
    }
}

fn preserve_nested_command_policy(options: &mut BTreeMap<String, VmValue>, node: &WorkflowNode) {
    if options.contains_key("command_policy") {
        return;
    }
    let Some(command_policy) = raw_model_policy_dict(node)
        .and_then(|dict| dict.get("policy"))
        .and_then(|value| value.as_dict())
        .and_then(|policy| policy.get("command_policy"))
    else {
        return;
    };
    options.insert("command_policy".to_string(), command_policy.clone());
}

fn stage_tools_value(node: &WorkflowNode) -> Option<VmValue> {
    node.raw_tools.clone().or_else(|| {
        if matches!(node.tools, serde_json::Value::Null) {
            None
        } else {
            Some(crate::stdlib::json_to_vm_value(&node.tools))
        }
    })
}

fn add_stage_tools_option(
    options: &mut BTreeMap<String, VmValue>,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
) {
    if !tool_names.is_empty() {
        if let Some(value) = tools_value.clone() {
            options.insert("tools".to_string(), value);
        }
    }
}

fn workflow_stage_llm_options(
    node: &WorkflowNode,
    stage_session_id: &str,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
    stage_agent_options: &super::WorkflowStageAgentOptions,
) -> BTreeMap<String, VmValue> {
    let mut options = stage_agent_options.llm_options_vm_dict();
    merge_raw_model_policy_options(&mut options, node);
    options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(stage_session_id.to_string())),
    );
    options.insert(
        "tool_format".to_string(),
        VmValue::String(Rc::from(stage_agent_options.tool_format.clone())),
    );
    add_stage_tools_option(&mut options, tools_value, tool_names);
    options
}

fn add_workflow_agent_compaction_options(
    options: &mut BTreeMap<String, VmValue>,
    node: &WorkflowNode,
) {
    if !node.auto_compact.enabled {
        options.insert("auto_compact".to_string(), VmValue::Bool(false));
        return;
    }
    options.insert("auto_compact".to_string(), VmValue::Bool(true));
    if let Some(value) = node.auto_compact.token_threshold {
        options.insert("compact_threshold".to_string(), VmValue::Int(value as i64));
    }
    if let Some(value) = node.auto_compact.tool_output_max_chars {
        options.insert(
            "tool_output_max_chars".to_string(),
            VmValue::Int(value as i64),
        );
    }
    if let Some(value) = node.auto_compact.hard_limit_tokens {
        options.insert("hard_limit_tokens".to_string(), VmValue::Int(value as i64));
    }
    if let Some(strategy) = node.auto_compact.compact_strategy.as_ref() {
        options.insert(
            "compact_strategy".to_string(),
            VmValue::String(Rc::from(strategy.clone())),
        );
    }
    if let Some(strategy) = node.auto_compact.hard_limit_strategy.as_ref() {
        options.insert(
            "hard_limit_strategy".to_string(),
            VmValue::String(Rc::from(strategy.clone())),
        );
    }
    if let Some(value) = raw_auto_compact_int(node, "compact_keep_last")
        .or_else(|| raw_auto_compact_int(node, "keep_last"))
    {
        options.insert("compact_keep_last".to_string(), VmValue::Int(value as i64));
    }
    if let Some(prompt) = raw_auto_compact_string(node, "summarize_prompt") {
        options.insert(
            "summarize_prompt".to_string(),
            VmValue::String(Rc::from(prompt)),
        );
    }
    if let Some(dict) = raw_auto_compact_dict(node) {
        for key in ["compress_callback", "mask_callback"] {
            if let Some(callback) = dict.get(key) {
                options.insert(key.to_string(), callback.clone());
            }
        }
        if let Some(callback) = dict.get("custom_compactor") {
            options.insert("compact_callback".to_string(), callback.clone());
        }
    }
}

fn workflow_stage_agent_loop_options(
    node: &WorkflowNode,
    stage_session_id: &str,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
    stage_agent_options: &super::WorkflowStageAgentOptions,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    let mut options = stage_agent_options.agent_loop_options_vm_dict();
    merge_raw_model_policy_options(&mut options, node);
    if let Some(context) = crate::orchestration::current_workflow_skill_context() {
        if !options.contains_key("skills") {
            if let Some(registry) = context.registry {
                options.insert("skills".to_string(), registry);
            }
        }
        if !options.contains_key("skill_match") {
            if let Some(match_config) = context.match_config {
                options.insert("skill_match".to_string(), match_config);
            }
        }
    }
    preserve_nested_command_policy(&mut options, node);
    add_workflow_agent_compaction_options(&mut options, node);
    add_stage_tools_option(&mut options, tools_value, tool_names);
    let tool_policy = tool_capability_policy_from_spec(&node.tools);
    let effective_policy = tool_policy
        .intersect(&node.capability_policy)
        .map_err(VmError::Runtime)?;
    insert_json_vm_option(&mut options, "policy", &effective_policy)?;
    insert_json_vm_option(&mut options, "approval_policy", &node.approval_policy)?;
    options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(stage_session_id.to_string())),
    );
    options.insert(
        "tool_format".to_string(),
        VmValue::String(Rc::from(stage_agent_options.tool_format.clone())),
    );
    Ok(options)
}

pub async fn execute_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let selected_stage = super::select_workflow_stage_artifacts(
        artifacts,
        &node.context_policy,
        &node.input_contract,
    )
    .await?;
    let selected = selected_stage.artifacts;
    let context_policy = selected_stage.context_policy;
    let rendered_context_override = if let Some(assembler) = node.raw_context_assembler.as_ref() {
        let assembled =
            crate::stdlib::assemble::assemble_from_options(&selected, assembler).await?;
        Some(super::render_assembled_chunks(&assembled))
    } else {
        None
    };
    let verification_contracts = super::stage_verification_contracts(node_id, node)?;
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
    let prepared_prompt = super::prepare_workflow_stage_prompt(
        task,
        node.task_label.as_deref(),
        &selected,
        &context_policy,
        rendered_context_override.as_deref(),
        &verification_contracts,
    )
    .await?;
    let prompt = prepared_prompt.prompt;
    let rendered_context = prepared_prompt.rendered_context;
    let rendered_verification = prepared_prompt.rendered_verification;

    let tool_names = tool_names_from_spec(&node.tools);
    let stage_agent_options = super::prepare_workflow_stage_agent_options(
        node,
        &stage_session_id,
        !tool_names.is_empty(),
    )
    .await?;
    let tool_format = stage_agent_options.tool_format.clone();
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
        let tools_value = stage_tools_value(node);
        if stage_agent_options.run_agent_loop {
            let options = workflow_stage_agent_loop_options(
                node,
                &stage_session_id,
                &tools_value,
                &tool_names,
                &stage_agent_options,
            )?;
            let result = crate::stdlib::harn_entry::call_harn_export_by_name(
                "std/agent/loop",
                "agent_loop",
                "workflow_stage_agent_loop",
                &[
                    VmValue::String(Rc::from(prompt.clone())),
                    node.system
                        .clone()
                        .map(|s| VmValue::String(Rc::from(s)))
                        .unwrap_or(VmValue::Nil),
                    VmValue::Dict(Rc::new(options)),
                ],
            )
            .await?;
            crate::llm::vm_value_to_json(&result)
        } else {
            let options = workflow_stage_llm_options(
                node,
                &stage_session_id,
                &tools_value,
                &tool_names,
                &stage_agent_options,
            );
            let args = vec![
                VmValue::String(Rc::from(prompt.clone())),
                node.system
                    .clone()
                    .map(|s| VmValue::String(Rc::from(s)))
                    .unwrap_or(VmValue::Nil),
                VmValue::Dict(Rc::new(options)),
            ];
            let opts = extract_llm_options(&args)?;
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
