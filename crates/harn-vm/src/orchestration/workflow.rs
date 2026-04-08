//! Workflow graph types, normalization, validation, and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::{
    apply_input_transcript_policy, apply_output_transcript_policy, new_id, now_rfc3339,
    ArtifactRecord, BranchSemantics, CapabilityPolicy, ContextPolicy, EscalationPolicy, JoinPolicy,
    MapPolicy, ModelPolicy, ReducePolicy, RetryPolicy, StageContract, ToolRuntimePolicyMetadata,
    TranscriptPolicy,
};
use crate::llm::{extract_llm_options, vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};

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
    pub transcript_policy: TranscriptPolicy,
    pub context_policy: ContextPolicy,
    pub retry_policy: RetryPolicy,
    pub capability_policy: CapabilityPolicy,
    pub input_contract: StageContract,
    pub output_contract: StageContract,
    pub branch_semantics: BranchSemantics,
    pub map_policy: MapPolicy,
    pub join_policy: JoinPolicy,
    pub reduce_policy: ReducePolicy,
    pub escalation_policy: EscalationPolicy,
    pub verify: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub raw_tools: Option<VmValue>,
}

impl PartialEq for WorkflowNode {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
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

fn parse_tool_runtime_policy(
    map: &serde_json::Map<String, serde_json::Value>,
) -> ToolRuntimePolicyMetadata {
    let Some(policy) = map.get("policy").and_then(|value| value.as_object()) else {
        return ToolRuntimePolicyMetadata::default();
    };

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

    let path_params = policy
        .get("path_params")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    ToolRuntimePolicyMetadata {
        capabilities,
        side_effect_level: policy
            .get("side_effect_level")
            .and_then(|value| value.as_str())
            .map(|s| s.to_string()),
        path_params,
        mutation_classification: policy
            .get("mutation_classification")
            .and_then(|value| value.as_str())
            .map(|s| s.to_string()),
    }
}

pub fn workflow_tool_metadata(
    value: &serde_json::Value,
) -> BTreeMap<String, ToolRuntimePolicyMetadata> {
    match value {
        serde_json::Value::Null => BTreeMap::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| (name.to_string(), parse_tool_runtime_policy(map))),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(workflow_tool_metadata)
                    .unwrap_or_default();
            }
            map.get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .map(|name| {
                    let mut metadata = BTreeMap::new();
                    metadata.insert(name.to_string(), parse_tool_runtime_policy(map));
                    metadata
                })
                .unwrap_or_default()
        }
        _ => BTreeMap::new(),
    }
}

pub fn workflow_tool_policy_from_tools(value: &serde_json::Value) -> CapabilityPolicy {
    let tools = workflow_tool_names(value);
    let tool_metadata = workflow_tool_metadata(value);
    let mut capabilities: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for metadata in tool_metadata.values() {
        for (capability, ops) in &metadata.capabilities {
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
        tool_metadata
            .values()
            .filter_map(|metadata| metadata.side_effect_level.clone()),
    );
    CapabilityPolicy {
        tools,
        capabilities,
        workspace_roots: Vec::new(),
        side_effect_level,
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_metadata,
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
    node.raw_tools = value.as_dict().and_then(|dict| dict.get("tools")).cloned();
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
            "fork" => {
                if outgoing.len() < 2 {
                    errors.push(format!(
                        "node {node_id}: fork nodes require at least two outgoing edges"
                    ));
                }
            }
            "join" => {
                if incoming < 2 {
                    warnings.push(format!(
                        "node {node_id}: join node has fewer than two incoming edges"
                    ));
                }
            }
            "map" => {
                if node.map_policy.items.is_empty()
                    && node.map_policy.item_artifact_kind.is_none()
                    && node.input_contract.input_kinds.is_empty()
                {
                    errors.push(format!(
                        "node {node_id}: map nodes require items, item_artifact_kind, or input_contract.input_kinds"
                    ));
                }
            }
            "reduce" => {
                if node.input_contract.input_kinds.is_empty() {
                    warnings.push(format!(
                        "node {node_id}: reduce node has no input_contract.input_kinds; it will consume all available artifacts"
                    ));
                }
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

pub async fn execute_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let mut selection_policy = node.context_policy.clone();
    if selection_policy.include_kinds.is_empty() && !node.input_contract.input_kinds.is_empty() {
        selection_policy.include_kinds = node.input_contract.input_kinds.clone();
    }
    let selected = super::select_artifacts_adaptive(artifacts.to_vec(), &selection_policy);
    let rendered_context = super::render_artifacts_context(&selected, &node.context_policy);
    let transcript = apply_input_transcript_policy(transcript, &node.transcript_policy);
    if node.input_contract.require_transcript && transcript.is_none() {
        return Err(VmError::Runtime(format!(
            "workflow stage {node_id} requires transcript input"
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
    let prompt = if rendered_context.is_empty() {
        task.to_string()
    } else {
        format!(
            "{rendered_context}\n\n{}:\n{task}",
            node.task_label
                .clone()
                .unwrap_or_else(|| "Task".to_string())
        )
    };

    let tool_format = std::env::var("HARN_AGENT_TOOL_FORMAT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "text".to_string());
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
            let mut process = if cfg!(target_os = "windows") {
                let mut cmd = tokio::process::Command::new("cmd");
                cmd.arg("/C").arg(command);
                cmd
            } else {
                let mut cmd = tokio::process::Command::new("/bin/sh");
                cmd.arg("-lc").arg(command);
                cmd
            };
            process.stdin(std::process::Stdio::null());
            if let Some(context) = crate::stdlib::process::current_execution_context() {
                if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
                    process.current_dir(cwd);
                }
                if !context.env.is_empty() {
                    process.envs(context.env);
                }
            }
            let output = process
                .output()
                .await
                .map_err(|e| VmError::Runtime(format!("workflow verify exec failed: {e}")))?;
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
        if let Some(transcript) = transcript.clone() {
            options.insert("transcript".to_string(), transcript);
        }

        let args = vec![
            VmValue::String(Rc::from(prompt.clone())),
            node.system
                .clone()
                .map(|s| VmValue::String(Rc::from(s)))
                .unwrap_or(VmValue::Nil),
            VmValue::Dict(Rc::new(options)),
        ];
        let mut opts = extract_llm_options(&args)?;

        if node.mode.as_deref() == Some("agent") || !tool_names.is_empty() {
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
                    tool_format: tool_format.clone(),
                    auto_compact: None,
                    context_callback: None,
                    policy: None,
                    daemon: false,
                    llm_retries: 2,
                    llm_backoff_ms: 2000,
                    exit_when_verified: false,
                    loop_detect_warn: 2,
                    loop_detect_block: 3,
                    loop_detect_skip: 4,
                    tool_examples: node.model_policy.tool_examples.clone(),
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
        payload.insert(
            "tool_calling_mode".to_string(),
            serde_json::json!(tool_format.clone()),
        );
    }

    let visible_text = llm_result["text"].as_str().unwrap_or_default().to_string();
    let transcript = llm_result
        .get("transcript")
        .cloned()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    let transcript = apply_output_transcript_policy(transcript, &node.transcript_policy);
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
