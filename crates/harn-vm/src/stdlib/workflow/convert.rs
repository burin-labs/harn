//! VM/JSON conversion helpers for workflow graphs.

use std::rc::Rc;

use crate::orchestration::WorkflowGraph;
use crate::value::{VmError, VmValue};

pub(super) fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("workflow encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

pub(super) fn workflow_graph_to_vm(graph: &WorkflowGraph) -> Result<VmValue, VmError> {
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
        let Some(raw_tools) = node.raw_tools.clone() else {
            continue;
        };
        let Some(node_value) = nodes.get(node_id).cloned() else {
            continue;
        };
        let VmValue::Dict(node_dict) = node_value else {
            continue;
        };
        let mut node_map = (*node_dict).clone();
        node_map.insert("tools".to_string(), raw_tools);
        nodes.insert(node_id.clone(), VmValue::Dict(Rc::new(node_map)));
    }
    graph_dict.insert("nodes".to_string(), VmValue::Dict(Rc::new(nodes)));
    Ok(VmValue::Dict(Rc::new(graph_dict)))
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
        VmValue::List(items) => VmValue::List(Rc::new(
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
                .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new())));
            filtered.insert("tools".to_string(), tool_items);
            VmValue::Dict(Rc::new(filtered))
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
