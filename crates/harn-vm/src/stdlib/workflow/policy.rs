//! Policy normalization and per-node policy derivation.

use std::collections::BTreeMap;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, normalize_workflow_value, workflow_tool_policy_from_tools,
    CapabilityPolicy, WorkflowGraph,
};
use crate::value::{VmError, VmValue};

use super::convert::{filter_workflow_tools, filter_workflow_tools_vm, workflow_graph_to_vm};

pub(super) fn normalize_policy(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|e| VmError::Runtime(format!("policy parse error: {e}")))
}

pub(super) fn set_node_policy(
    args: &[VmValue],
    updater: impl Fn(&mut crate::orchestration::WorkflowNode, serde_json::Value) -> Result<(), VmError>,
) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
        VmError::Runtime("workflow policy update: missing workflow".to_string())
    })?)?;
    let node_id = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow policy update: missing node id".to_string()))?;
    let policy =
        crate::llm::vm_value_to_json(args.get(2).ok_or_else(|| {
            VmError::Runtime("workflow policy update: missing policy".to_string())
        })?);
    let node = graph
        .nodes
        .get_mut(&node_id)
        .ok_or_else(|| VmError::Runtime(format!("unknown workflow node: {node_id}")))?;
    updater(node, policy)?;
    append_audit_entry(
        &mut graph,
        "set_policy",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    workflow_graph_to_vm(&graph)
}

pub(super) fn apply_runtime_node_overrides(
    mut node: crate::orchestration::WorkflowNode,
    options: &BTreeMap<String, VmValue>,
) -> crate::orchestration::WorkflowNode {
    if node.model_policy.provider.is_none() {
        node.model_policy.provider = options
            .get("provider")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.model.is_none() {
        node.model_policy.model = options
            .get("model")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.model_tier.is_none() {
        node.model_policy.model_tier = options
            .get("model_tier")
            .or_else(|| options.get("tier"))
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.temperature.is_none() {
        node.model_policy.temperature = options.get("temperature").and_then(|value| match value {
            VmValue::Float(number) => Some(*number),
            _ => value.as_int().map(|number| number as f64),
        });
    }
    if node.model_policy.max_tokens.is_none() {
        node.model_policy.max_tokens = options.get("max_tokens").and_then(|value| value.as_int());
    }
    if node.mode.is_none() {
        node.mode = options
            .get("mode")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if !node.capability_policy.tools.is_empty() {
        node.tools = filter_workflow_tools(&node.tools, &node.capability_policy.tools);
        node.raw_tools = node
            .raw_tools
            .as_ref()
            .map(|tools| filter_workflow_tools_vm(tools, &node.capability_policy.tools));
    }
    node
}

pub(super) fn effective_node_policy(
    graph: &WorkflowGraph,
    node: &crate::orchestration::WorkflowNode,
) -> Result<CapabilityPolicy, VmError> {
    let builtin = builtin_ceiling();
    let graph_policy = builtin
        .intersect(&graph.capability_policy)
        .map_err(VmError::Runtime)?;
    let node_policy = graph_policy
        .intersect(&node.capability_policy)
        .map_err(VmError::Runtime)?;
    node_policy
        .intersect(&workflow_tool_policy_from_tools(&node.tools))
        .map_err(VmError::Runtime)
}

pub(super) fn effective_node_approval_policy(
    graph: &WorkflowGraph,
    node: &crate::orchestration::WorkflowNode,
) -> crate::orchestration::ToolApprovalPolicy {
    let base = crate::orchestration::current_approval_policy()
        .unwrap_or_default()
        .intersect(&graph.approval_policy);
    base.intersect(&node.approval_policy)
}
