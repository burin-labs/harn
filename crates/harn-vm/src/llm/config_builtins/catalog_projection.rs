//! VM-value projections for catalog-owned capability metadata.

use crate::llm::capabilities::Capabilities;
use crate::value::VmValue;

pub(super) fn toml_value_to_vm_value(value: &toml::Value) -> VmValue {
    match value {
        toml::Value::String(s) => VmValue::String(arcstr::ArcStr::from(s.as_str())),
        toml::Value::Integer(i) => VmValue::Int(*i),
        toml::Value::Float(f) => VmValue::Float(*f),
        toml::Value::Boolean(b) => VmValue::Bool(*b),
        toml::Value::Datetime(dt) => VmValue::String(arcstr::ArcStr::from(dt.to_string())),
        toml::Value::Array(items) => {
            let list: Vec<VmValue> = items.iter().map(toml_value_to_vm_value).collect();
            VmValue::List(std::sync::Arc::new(list))
        }
        toml::Value::Table(table) => {
            let mut dict = crate::value::DictMap::new();
            for (key, item) in table {
                dict.insert(crate::value::intern_key(key), toml_value_to_vm_value(item));
            }
            VmValue::dict(dict)
        }
    }
}

pub(super) fn tool_mode_parity_value(caps: &Capabilities) -> VmValue {
    caps.tool_mode_parity
        .as_deref()
        .map(|status| VmValue::String(arcstr::ArcStr::from(status)))
        .unwrap_or(VmValue::Nil)
}

pub(super) fn tool_mode_parity_notes_value(caps: &Capabilities) -> VmValue {
    caps.tool_mode_parity_notes
        .as_deref()
        .map(|notes| VmValue::String(arcstr::ArcStr::from(notes)))
        .unwrap_or(VmValue::Nil)
}

pub(super) fn reasoning_history_wire_field_value(caps: &Capabilities) -> VmValue {
    caps.reasoning_history_wire_field
        .map(|field| VmValue::String(arcstr::ArcStr::from(field.as_str())))
        .unwrap_or(VmValue::Nil)
}

/// Mirrors the VM's tool-capability gate: either native or text-format tool
/// calling makes a route tool-capable.
pub(super) fn tools_value(caps: &Capabilities) -> VmValue {
    VmValue::Bool(caps.native_tools || caps.text_tool_wire_format_supported)
}
