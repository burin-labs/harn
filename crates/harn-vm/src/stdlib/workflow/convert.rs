//! VM/JSON conversion helpers for workflow graphs.

use crate::orchestration::{WorkflowGraph, WorkflowNode};
use crate::value::{DictMap, VmError, VmValue};

pub(super) fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("workflow encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

pub(in crate::stdlib) fn workflow_graph_to_vm(graph: &WorkflowGraph) -> Result<VmValue, VmError> {
    let base = to_vm(graph)?;
    let VmValue::Dict(base_dict) = base else {
        return Err(VmError::Runtime(
            "workflow graph encoding did not produce a dict".to_string(),
        ));
    };
    let mut graph_dict = (*base_dict).clone();
    let nodes_value = graph_dict
        .get("nodes")
        .cloned()
        .ok_or_else(|| VmError::Runtime("workflow graph is missing nodes".to_string()))?;
    let VmValue::Dict(nodes_dict) = nodes_value else {
        return Err(VmError::Runtime(
            "workflow graph nodes encoding did not produce a dict".to_string(),
        ));
    };
    let mut nodes = (*nodes_dict).clone();
    for (node_id, node) in &graph.nodes {
        nodes.insert(
            crate::value::intern_key(node_id),
            node_to_vm_with_raw(node)?,
        );
    }
    graph_dict.insert(crate::value::intern_key("nodes"), VmValue::dict(nodes));
    Ok(VmValue::dict(graph_dict))
}

/// Preserve live policy values and closures at every node-to-VM crossing.
pub(super) fn node_to_vm_with_raw(node: &WorkflowNode) -> Result<VmValue, VmError> {
    let encoded = to_vm(node)?;
    let VmValue::Dict(dict) = encoded else {
        return Ok(encoded);
    };
    let mut dict = (*dict).clone();
    for (key, raw) in [
        ("tools", &node.raw_tools),
        ("model_policy", &node.raw_model_policy),
        ("context_assembler", &node.raw_context_assembler),
        ("auto_compact", &node.raw_auto_compact),
        ("verify", &node.raw_verify),
        ("executor", &node.raw_executor),
    ] {
        if let Some(value) = raw {
            let value = if key == "model_policy" {
                // Keep normalized workflow defaults beneath live policy entries.
                let mut policy = dict
                    .get(key)
                    .and_then(VmValue::as_dict)
                    .cloned()
                    .unwrap_or_default();
                if let Some(raw) = value.as_dict() {
                    for (key, value) in raw
                        .iter()
                        .filter(|(_, value)| !matches!(value, VmValue::Nil))
                    {
                        policy.insert(key.clone(), value.clone());
                    }
                }
                VmValue::dict(policy)
            } else {
                value.clone()
            };
            dict.insert(crate::value::intern_key(key), value);
        }
    }
    reattach_retry_prompt_builder(&mut dict, node);
    Ok(VmValue::dict(dict))
}

fn reattach_retry_prompt_builder(node_map: &mut DictMap, node: &WorkflowNode) {
    let Some(builder) = &node.retry_policy.repair_prompt_builder else {
        return;
    };
    let retry_policy_key = crate::value::intern_key("retry_policy");
    let mut retry_policy = match node_map.get("retry_policy") {
        Some(VmValue::Dict(existing)) => (**existing).clone(),
        _ => DictMap::new(),
    };
    retry_policy.insert(
        crate::value::intern_key("repair_prompt_builder"),
        builder.0.clone(),
    );
    node_map.insert(retry_policy_key, VmValue::dict(retry_policy));
}

pub(super) fn filter_workflow_tools(
    tools: &serde_json::Value,
    allowed: &[String],
) -> serde_json::Value {
    match tools {
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .filter(|item| match item {
                    serde_json::Value::Object(map) => map
                        .get("name")
                        .and_then(|value| value.as_str())
                        .map(|name| allowed.iter().any(|allowed_name| allowed_name == name))
                        .unwrap_or(false),
                    _ => false,
                })
                .cloned()
                .collect(),
        ),
        serde_json::Value::Object(map)
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") =>
        {
            let mut filtered = map.clone();
            let tool_items = map
                .get("tools")
                .map(|value| filter_workflow_tools(value, allowed))
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
            filtered.insert("tools".to_string(), tool_items);
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Object(map) => {
            let keep = map
                .get("name")
                .and_then(|value| value.as_str())
                .map(|name| allowed.iter().any(|allowed_name| allowed_name == name))
                .unwrap_or(false);
            if keep {
                tools.clone()
            } else {
                serde_json::Value::Null
            }
        }
        _ => serde_json::Value::Null,
    }
}

pub(super) fn filter_workflow_tools_vm(tools: &VmValue, allowed: &[String]) -> VmValue {
    match tools {
        VmValue::Nil => VmValue::Nil,
        VmValue::List(items) => VmValue::List(std::sync::Arc::new(
            items
                .iter()
                .filter(|item| {
                    item.as_dict()
                        .and_then(|map| map.get("name"))
                        .map(|name| name.display())
                        .map(|name| allowed.iter().any(|allowed_name| allowed_name == &name))
                        .unwrap_or(false)
                })
                .cloned()
                .collect(),
        )),
        VmValue::Dict(map)
            if map.get("_type").map(|value| value.display()).as_deref()
                == Some("tool_registry") =>
        {
            let mut filtered = (**map).clone();
            let tool_items = map
                .get("tools")
                .map(|value| filter_workflow_tools_vm(value, allowed))
                .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new())));
            filtered.insert(crate::value::intern_key("tools"), tool_items);
            VmValue::dict(filtered)
        }
        VmValue::Dict(map) => {
            let keep = map
                .get("name")
                .map(|value| value.display())
                .map(|name| allowed.iter().any(|allowed_name| allowed_name == &name))
                .unwrap_or(false);
            if keep {
                tools.clone()
            } else {
                VmValue::Nil
            }
        }
        _ => VmValue::Nil,
    }
}
