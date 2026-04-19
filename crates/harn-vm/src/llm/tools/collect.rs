use std::collections::BTreeSet;

use super::components::ComponentRegistry;
use super::json_schema::json_schema_to_type_expr;
use super::params::{extract_examples, extract_params_from_vm_dict, ToolParamSchema};
use crate::value::VmValue;

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolSchema {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) params: Vec<ToolParamSchema>,
    /// When true, render as a compact one-liner (name + params type + first
    /// sentence of description) instead of the full TypeScript declaration with
    /// JSDoc. Tools marked compact are still fully dispatchable — only the
    /// prompt rendering changes. The model can call `tool_schema({ name })`
    /// to get the full description on demand.
    pub(crate) compact: bool,
}

fn collect_vm_tool_schemas(
    tools_val: Option<&VmValue>,
    registry: &mut ComponentRegistry,
) -> Vec<ToolSchema> {
    // Mirror the root registry as JSON so `$ref` can resolve against
    // sibling `types` / `definitions` / `components.schemas`.
    let root_json = match tools_val {
        Some(value) => super::super::vm_value_to_json(value),
        None => serde_json::Value::Null,
    };

    let entries: Vec<&VmValue> = match tools_val {
        Some(VmValue::List(list)) => list.iter().collect(),
        Some(VmValue::Dict(dict)) => {
            if let Some(VmValue::List(tools)) = dict.get("tools") {
                tools.iter().collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    entries
        .into_iter()
        .filter_map(|value| match value {
            VmValue::Dict(td) => {
                let name = td.get("name")?.display();
                let description = td
                    .get("description")
                    .map(|value| value.display())
                    .unwrap_or_default();
                let params = extract_params_from_vm_dict(td, &root_json, registry);
                let compact = td
                    .get("compact")
                    .map(|value| matches!(value, VmValue::Bool(true)))
                    .unwrap_or(false);
                Some(ToolSchema {
                    name,
                    description,
                    params,
                    compact,
                })
            }
            _ => None,
        })
        .collect()
}

fn schema_description_from_json(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("description")
                .and_then(|inner| inner.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_default()
}

fn extract_params_from_provider_input_schema(
    provider_input_schema: &serde_json::Value,
    root: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> Vec<ToolParamSchema> {
    let required_set: BTreeSet<String> = provider_input_schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    provider_input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            let mut params = properties
                .iter()
                .map(|(name, value)| {
                    let examples = value.as_object().map(extract_examples).unwrap_or_default();
                    ToolParamSchema {
                        name: name.clone(),
                        ty: json_schema_to_type_expr(value, root, registry),
                        description: schema_description_from_json(value),
                        required: required_set.contains(name),
                        default: value.get("default").cloned(),
                        examples,
                    }
                })
                .collect::<Vec<_>>();
            // Required first; alphabetical within groups for determinism.
            params.sort_by(|a, b| {
                (!a.required)
                    .cmp(&!b.required)
                    .then_with(|| a.name.cmp(&b.name))
            });
            params
        })
        .unwrap_or_default()
}

fn collect_provider_declared_tool_schemas(
    provider_tools: Option<&[serde_json::Value]>,
    registry: &mut ComponentRegistry,
) -> Vec<ToolSchema> {
    provider_tools
        .unwrap_or(&[])
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function");
            let name = function
                .and_then(|value| value.get("name"))
                .or_else(|| tool.get("name"))
                .and_then(|value| value.as_str())?;
            let description = function
                .and_then(|value| value.get("description"))
                .or_else(|| tool.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let provider_input_schema = function
                .and_then(|value| value.get("parameters"))
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            // Resolve `$ref` against the tool wrapper itself (siblings
            // such as `components.schemas` hang off there).
            let root = tool.clone();
            Some(ToolSchema {
                name: name.to_string(),
                description,
                params: extract_params_from_provider_input_schema(
                    &provider_input_schema,
                    &root,
                    registry,
                ),
                compact: false,
            })
        })
        .collect()
}

/// Collect the full tool schema set AND the reusable type registry populated
/// by any `$ref` encounters during extraction. Callers that only need the
/// list of schemas can ignore the registry.
pub(crate) fn collect_tool_schemas_with_registry(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
) -> (Vec<ToolSchema>, ComponentRegistry) {
    let mut registry = ComponentRegistry::default();
    let mut merged = collect_vm_tool_schemas(tools_val, &mut registry);
    let mut seen = merged
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<BTreeSet<_>>();

    for schema in collect_provider_declared_tool_schemas(native_tools, &mut registry) {
        if seen.insert(schema.name.clone()) {
            merged.push(schema);
        }
    }

    merged.sort_by(|a, b| a.name.cmp(&b.name));
    (merged, registry)
}

pub(crate) fn collect_tool_schemas(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
) -> Vec<ToolSchema> {
    collect_tool_schemas_with_registry(tools_val, native_tools).0
}

/// Validate that all required parameters (those without defaults) are present
/// in the tool call arguments. Returns `Ok(())` when valid, or an error string
/// listing the missing parameters.
pub(crate) fn validate_tool_args(
    tool_name: &str,
    args: &serde_json::Value,
    schemas: &[ToolSchema],
) -> Result<(), String> {
    let Some(schema) = schemas.iter().find(|schema| schema.name == tool_name) else {
        return Ok(()); // Unknown tool — handled by the unknown-tool error path
    };
    let obj = args.as_object();
    let missing: Vec<&str> = schema
        .params
        .iter()
        .filter(|param| param.required && param.default.is_none())
        .filter(|param| {
            obj.is_none_or(|map| !map.contains_key(&param.name) || map[&param.name].is_null())
        })
        .map(|param| param.name.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Tool '{}' is missing required parameter(s): {}. \
             Provide all required parameters and try again.",
            tool_name,
            missing.join(", ")
        ))
    }
}
