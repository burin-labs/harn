//! Lossy `VmValue` → JSON conversion for persistence layers (store /
//! checkpoint / metadata).
//!
//! This is deliberately narrower than [`crate::stdlib::json`]'s converters: it
//! covers the scalar / list / dict shapes those persistence paths actually
//! store. Ordinary non-data values become `null`, while nominal Harness values
//! fail closed—even when nested—because runtime authority must never become
//! domain state. Keep persistence callers on this shared conversion path so
//! handle/closure treatment stays consistent.

use crate::value::{VmError, VmValue};

/// Serialize `val` to JSON for persistence: scalars/list/dict are preserved,
/// `Decimal` becomes a precision-preserving string (read back via
/// `decimal(...)`), and any non-data value kind becomes `null`.
pub(crate) fn vm_to_storage_json(val: &VmValue) -> Result<serde_json::Value, VmError> {
    Ok(match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        // Decimal serializes as a string to preserve exact precision (JSON
        // numbers are binary floats); read back via `decimal(...)`.
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => serde_json::Value::Array(
            items
                .iter()
                .map(vm_to_storage_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        VmValue::Dict(map) => {
            let obj = map
                .iter()
                .map(|(k, v)| Ok((k.to_string(), vm_to_storage_json(v)?)))
                .collect::<Result<serde_json::Map<String, serde_json::Value>, VmError>>()?;
            serde_json::Value::Object(obj)
        }
        VmValue::Harness(handle) => {
            return Err(VmError::TypeError(format!(
                "{} is runtime authority and cannot be persisted as domain state",
                handle.type_name()
            )));
        }
        _ => serde_json::Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_harness_cannot_be_persisted_directly_or_nested() {
        let harness = crate::harness::Harness::null().into_vm_value();
        let direct = vm_to_storage_json(&harness).unwrap_err().to_string();
        assert!(
            direct.contains("Harness is runtime authority")
                && direct.contains("cannot be persisted as domain state"),
            "{direct}"
        );

        let nested = VmValue::List(std::sync::Arc::new(vec![harness]));
        let nested_error = vm_to_storage_json(&nested).unwrap_err().to_string();
        assert!(
            nested_error.contains("cannot be persisted as domain state"),
            "{nested_error}"
        );

        let VmValue::Harness(root) = crate::harness::Harness::null().into_vm_value() else {
            unreachable!("Harness lowers to VmValue::Harness")
        };
        let fs = VmValue::Harness(root.sub_handle("fs").expect("root exposes fs").into());
        let narrow_error = vm_to_storage_json(&fs).unwrap_err().to_string();
        assert!(
            narrow_error.contains("HarnessFs is runtime authority")
                && narrow_error.contains("cannot be persisted as domain state"),
            "{narrow_error}"
        );

        let tools = VmValue::Harness(root.sub_handle("tools").expect("root exposes tools").into());
        let bundle = VmValue::dict([("fs", fs), ("tools", tools)]);
        let bundle_error = vm_to_storage_json(&bundle).unwrap_err().to_string();
        assert!(
            bundle_error.contains("runtime authority")
                && bundle_error.contains("cannot be persisted as domain state"),
            "{bundle_error}"
        );
    }
}
