//! VM-value projections for catalog-owned capability metadata.

use crate::llm::capabilities::Capabilities;
use crate::value::VmValue;

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
