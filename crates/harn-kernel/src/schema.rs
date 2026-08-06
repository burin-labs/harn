use std::collections::BTreeMap;

use crate::value::VmValue;

/// Project the compiler's canonical Harn schema vocabulary to JSON Schema.
pub(crate) fn schema_to_json_schema_value(schema: &VmValue) -> Result<VmValue, String> {
    let VmValue::Dict(entries) = schema else {
        return Err("schema must be a record".to_string());
    };
    let mut output = BTreeMap::new();
    for (key, value) in entries.iter() {
        match (key.as_str(), value) {
            ("type", VmValue::String(kind)) => {
                if let Some(kind) = json_type_name(kind) {
                    output.insert("type".to_string(), VmValue::String(kind.into()));
                }
                if kind.as_str() == "set" {
                    output.insert("uniqueItems".to_string(), VmValue::Bool(true));
                }
            }
            ("properties", VmValue::Dict(fields)) => {
                let projected = fields
                    .iter()
                    .map(|(name, field)| Ok((name.clone(), schema_to_json_schema_value(field)?)))
                    .collect::<Result<BTreeMap<_, _>, String>>()?;
                output.insert("properties".to_string(), VmValue::dict(projected));
            }
            ("items" | "additional_properties", child) => {
                let json_key = if key == "additional_properties" {
                    "additionalProperties"
                } else {
                    "items"
                };
                output.insert(json_key.to_string(), schema_to_json_schema_value(child)?);
            }
            ("union", VmValue::List(items)) => {
                let projected = items
                    .iter()
                    .map(schema_to_json_schema_value)
                    .collect::<Result<Vec<_>, _>>()?;
                output.insert(
                    "anyOf".to_string(),
                    VmValue::List(std::sync::Arc::new(projected)),
                );
            }
            ("all_of", VmValue::List(items)) => {
                let projected = items
                    .iter()
                    .map(schema_to_json_schema_value)
                    .collect::<Result<Vec<_>, _>>()?;
                output.insert(
                    "allOf".to_string(),
                    VmValue::List(std::sync::Arc::new(projected)),
                );
            }
            ("required", value) => {
                output.insert(key.clone(), value.clone());
            }
            ("const" | "enum" | "default", value) => {
                output.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    Ok(VmValue::dict(output))
}

fn json_type_name(name: &str) -> Option<&'static str> {
    match name {
        "string" | "str" => Some("string"),
        "int" | "integer" => Some("integer"),
        "float" | "number" | "decimal" => Some("number"),
        "bool" | "boolean" => Some("boolean"),
        "nil" | "null" => Some("null"),
        "list" | "set" | "array" => Some("array"),
        "dict" | "map" | "object" => Some("object"),
        "any" | "unknown" => None,
        _ => None,
    }
}
