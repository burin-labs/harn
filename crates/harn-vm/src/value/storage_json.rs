//! Lossy `VmValue` → JSON conversion for persistence layers (store /
//! checkpoint / metadata).
//!
//! This is deliberately narrower than [`crate::stdlib::json`]'s converters: it
//! covers the scalar / list / dict shapes those persistence paths actually
//! store and maps every other value kind (closures, handles, …) to `null`
//! rather than to a display string, because a persisted record should not carry
//! a stringified handle. The three persistence modules previously each kept an
//! identical private copy of this function; they now share this one.

use crate::value::VmValue;

/// Serialize `val` to JSON for persistence: scalars/list/dict are preserved,
/// `Decimal` becomes a precision-preserving string (read back via
/// `decimal(...)`), and any non-data value kind becomes `null`.
pub(crate) fn vm_to_storage_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        // Decimal serializes as a string to preserve exact precision (JSON
        // numbers are binary floats); read back via `decimal(...)`.
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_to_storage_json).collect())
        }
        VmValue::Dict(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), vm_to_storage_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}
