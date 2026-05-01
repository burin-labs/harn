use crate::value::VmValue;
use std::collections::BTreeMap;

mod api;
mod canonicalize;
mod result;
mod transform;
mod type_check;
mod validate;

pub(crate) use api::{
    schema_assert_param, schema_expect_value, schema_extend_value, schema_from_json_schema_value,
    schema_from_openapi_schema_value, schema_is_value, schema_omit_value, schema_partial_value,
    schema_pick_value, schema_result_value, schema_to_json_schema_value,
    schema_to_openapi_schema_value,
};
pub use canonicalize::json_to_vm_value;

/// Canonicalize a JSON-Schema-shaped value for use as an MCP elicitation
/// `requestedSchema`. Reuses the same canonicalizer the rest of the
/// language uses for `schema_from_json_schema(...)` so behavior is
/// identical between user-facing schema builtins and elicitation.
pub fn elicitation_validate_schema(schema: &VmValue) -> Result<VmValue, crate::value::VmError> {
    schema_from_json_schema_value(schema)
}

/// Validate `data` against a canonicalized schema. Mirrors the
/// `schema_expect` semantics — returns the (possibly defaulted) value
/// on success and a thrown error string on failure.
pub fn elicitation_validate(
    data: &VmValue,
    schema: &VmValue,
) -> Result<VmValue, crate::value::VmError> {
    schema_expect_value(data, schema, false)
}

pub(crate) const BYTES_B64_TAG: &str = "$bytes_b64";

pub(crate) fn tagged_bytes_json(bytes: &[u8]) -> serde_json::Value {
    use base64::Engine;

    serde_json::json!({
        BYTES_B64_TAG: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn vm_value_to_serde_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::Value::Bool(*value),
        VmValue::Int(value) => serde_json::json!(value),
        VmValue::Float(value) => serde_json::json!(value),
        VmValue::String(value) => serde_json::Value::String(value.to_string()),
        VmValue::Bytes(bytes) => tagged_bytes_json(bytes),
        VmValue::List(items) | VmValue::Set(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_serde_json).collect())
        }
        VmValue::Dict(items) => serde_json::Value::Object(
            items
                .iter()
                .map(|(key, value)| (key.clone(), vm_value_to_serde_json(value)))
                .collect(),
        ),
        _ => serde_json::Value::String(value.display()),
    }
}

fn schema_bool(schema: &BTreeMap<String, VmValue>, key: &str) -> bool {
    matches!(schema.get(key), Some(VmValue::Bool(true)))
}

fn schema_i64(schema: &BTreeMap<String, VmValue>, key: &str) -> Option<i64> {
    match schema.get(key) {
        Some(VmValue::Int(value)) => Some(*value),
        _ => None,
    }
}

fn schema_number(schema: &BTreeMap<String, VmValue>, key: &str) -> Option<f64> {
    match schema.get(key) {
        Some(VmValue::Int(value)) => Some(*value as f64),
        Some(VmValue::Float(value)) => Some(*value),
        _ => None,
    }
}

fn location_label(path: &str) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.to_string()
    }
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

fn index_path(path: &str, index: usize) -> String {
    if path.is_empty() {
        format!("[{}]", index)
    } else {
        format!("{}[{}]", path, index)
    }
}

#[cfg(test)]
mod tests;
