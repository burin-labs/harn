//! Internal VM/JSON conversion helpers for hostlib payload bridges.

use harn_vm::VmValue;

/// Convert a VM value into a JSON value for opaque hostlib payload fields.
pub(crate) fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::Value::Bool(*value),
        VmValue::Int(value) => serde_json::Value::from(*value),
        VmValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::String(value) => serde_json::Value::String(value.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(dict) => vm_dict_to_json(dict),
        _ => serde_json::Value::Null,
    }
}

/// Convert a VM dict into a JSON object while preserving nested VM data.
pub(crate) fn vm_dict_to_json(dict: &harn_vm::value::DictMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in dict.iter() {
        map.insert(key.as_str().to_string(), vm_value_to_json(value));
    }
    serde_json::Value::Object(map)
}
