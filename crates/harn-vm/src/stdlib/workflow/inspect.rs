//! Workflow inspection and structural manipulation builtins.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, normalize_workflow_value, validate_workflow, WorkflowEdge,
};
use crate::value::{VmError, VmValue};

use super::convert::{to_vm, workflow_graph_to_vm};
use super::policy::{normalize_policy, set_node_policy};

pub(super) fn workflow_graph_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    workflow_graph_to_vm(&graph)
}

pub(super) fn workflow_validate_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    to_vm(&validate_workflow(
        &graph,
        ceiling.as_ref().or(Some(&builtin_ceiling())),
    ))
}

pub(super) fn workflow_inspect_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    let builtin = builtin_ceiling();
    let report = validate_workflow(&graph, ceiling.as_ref().or(Some(&builtin)));
    to_vm(&serde_json::json!({
        "graph": graph,
        "validation": report,
        "node_count": graph.nodes.len(),
        "edge_count": graph.edges.len(),
    }))
}

pub(super) fn workflow_policy_report_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    let builtin = builtin_ceiling();
    let effective_ceiling = ceiling.unwrap_or(builtin);
    let report = validate_workflow(&graph, Some(&effective_ceiling));
    to_vm(&serde_json::json!({
        "workflow_policy": graph.capability_policy,
        "ceiling": effective_ceiling,
        "validation": report,
            "nodes": graph.nodes.iter().map(|(node_id, node)| serde_json::json!({
            "node_id": node_id,
            "policy": node.capability_policy,
            "tools": node.tools,
        })).collect::<Vec<_>>(),
    }))
}

pub(super) fn workflow_clone_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let mut graph = normalize_workflow_value(&input)?;
    graph.id = format!("{}_clone", graph.id);
    graph.version += 1;
    append_audit_entry(&mut graph, "clone", None, None, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}

pub(super) fn workflow_insert_node_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut graph =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_insert_node: missing workflow".to_string())
        })?)?;
    let node_value = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("workflow_insert_node: missing node".to_string()))?;
    let mut node =
        crate::orchestration::parse_workflow_node_value(node_value, "workflow_insert_node")?;
    let node_id = node
        .id
        .clone()
        .or_else(|| {
            node_value
                .as_dict()
                .and_then(|d| d.get("id"))
                .map(|v| v.display())
        })
        .unwrap_or_else(|| format!("node_{}", graph.nodes.len() + 1));
    node.id = Some(node_id.clone());
    graph.nodes.insert(node_id.clone(), node);
    if let Some(VmValue::Dict(edge_dict)) = args.get(2) {
        let edge_json = crate::llm::vm_value_to_json(&VmValue::Dict(edge_dict.clone()));
        let edge =
            crate::orchestration::parse_workflow_edge_json(edge_json, "workflow_insert_node edge")?;
        graph.edges.push(edge);
    }
    append_audit_entry(
        &mut graph,
        "insert_node",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    workflow_graph_to_vm(&graph)
}

pub(super) fn workflow_replace_node_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut graph =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing workflow".to_string())
        })?)?;
    let node_id = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_replace_node: missing node id".to_string()))?;
    let mut node = crate::orchestration::parse_workflow_node_value(
        args.get(2)
            .ok_or_else(|| VmError::Runtime("workflow_replace_node: missing node".to_string()))?,
        "workflow_replace_node",
    )?;
    node.id = Some(node_id.clone());
    graph.nodes.insert(node_id.clone(), node);
    append_audit_entry(
        &mut graph,
        "replace_node",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    workflow_graph_to_vm(&graph)
}

pub(super) fn workflow_rewire_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("workflow_rewire: missing workflow".to_string()))?,
    )?;
    let from = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_rewire: missing from".to_string()))?;
    let to = args
        .get(2)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_rewire: missing to".to_string()))?;
    let branch = args.get(3).map(|v| v.display()).filter(|s| !s.is_empty());
    graph
        .edges
        .retain(|edge| !(edge.from == from && edge.branch == branch));
    graph.edges.push(WorkflowEdge {
        from: from.clone(),
        to,
        branch,
        label: None,
    });
    append_audit_entry(&mut graph, "rewire", Some(from), None, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}

pub(super) fn workflow_set_model_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.model_policy = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_model_policy: {e}")))?;
        Ok(())
    })
}

pub(super) fn workflow_set_context_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.context_policy = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_context_policy: {e}")))?;
        Ok(())
    })
}

pub(super) fn workflow_set_auto_compact_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.auto_compact = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_auto_compact: {e}")))?;
        Ok(())
    })
}

pub(super) fn workflow_set_output_visibility_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.output_visibility = match policy {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s),
            _ => {
                return Err(VmError::Runtime(
                    "workflow_set_output_visibility: value must be a string or nil".into(),
                ))
            }
        };
        Ok(())
    })
}

pub(super) fn workflow_diff_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let left =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing left workflow".to_string())
        })?)?;
    let right =
        normalize_workflow_value(args.get(1).ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing right workflow".to_string())
        })?)?;
    let left_json = serde_json::to_value(&left).unwrap_or_default();
    let right_json = serde_json::to_value(&right).unwrap_or_default();
    to_vm(&serde_json::json!({
        "changed": left_json != right_json,
        "left": left,
        "right": right,
    }))
}

pub(super) fn workflow_commit_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("workflow_commit: missing workflow".to_string()))?,
    )?;
    let reason = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
    let report = validate_workflow(&graph, Some(&builtin_ceiling()));
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "workflow_commit: invalid workflow: {}",
            report.errors.join("; ")
        )));
    }
    append_audit_entry(&mut graph, "commit", None, reason, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}
