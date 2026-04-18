use crate::value::{VmError, VmValue};

use super::convert::{annotations_to_json, vm_value_to_json};
use super::defs::McpToolDef;

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<Vec<McpToolDef>, VmError> {
    let dict = match registry {
        VmValue::Dict(d) => d,
        _ => {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry".into(),
            ));
        }
    };

    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => {}
        _ => {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry (created with tool_registry())".into(),
            ));
        }
    }

    let tools = match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => return Ok(Vec::new()),
    };

    let mut mcp_tools = Vec::new();
    for tool in tools.iter() {
        if let VmValue::Dict(entry) = tool {
            let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
            let title = entry.get("title").map(|v| v.display());
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();

            let handler = match entry.get("handler") {
                Some(VmValue::Closure(c)) => (**c).clone(),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "mcp_tools: tool '{name}' has no handler closure"
                    )));
                }
            };

            let input_schema = params_to_json_schema(entry.get("parameters"));
            let output_schema = entry.get("output_schema").and_then(|v| {
                if let VmValue::Dict(_) = v {
                    Some(vm_value_to_json(v))
                } else {
                    None
                }
            });
            let annotations = entry.get("annotations").and_then(annotations_to_json);

            mcp_tools.push(McpToolDef {
                name,
                title,
                description,
                input_schema,
                output_schema,
                annotations,
                handler,
            });
        }
    }

    Ok(mcp_tools)
}

/// Convert Harn tool_define parameter definitions to JSON Schema for MCP inputSchema.
pub(super) fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    let params_dict = match params {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return serde_json::json!({ "type": "object", "properties": {} });
        }
    };

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for (param_name, param_def) in params_dict.iter() {
        if let VmValue::Dict(def) = param_def {
            let mut prop = serde_json::Map::new();
            if let Some(VmValue::String(t)) = def.get("type") {
                prop.insert("type".into(), serde_json::Value::String(t.to_string()));
            }
            if let Some(VmValue::String(d)) = def.get("description") {
                prop.insert(
                    "description".into(),
                    serde_json::Value::String(d.to_string()),
                );
            }
            if matches!(def.get("required"), Some(VmValue::Bool(true))) {
                required.push(serde_json::Value::String(param_name.clone()));
            }
            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        } else if let VmValue::String(type_str) = param_def {
            let mut prop = serde_json::Map::new();
            prop.insert(
                "type".into(),
                serde_json::Value::String(type_str.to_string()),
            );
            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        }
    }

    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), serde_json::Value::String("object".into()));
    schema.insert("properties".into(), serde_json::Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), serde_json::Value::Array(required));
    }
    serde_json::Value::Object(schema)
}
