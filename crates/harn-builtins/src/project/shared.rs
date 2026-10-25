use std::path::PathBuf;

use harn_vm_core::value::VmValue;

pub(super) fn optional_string_field(
    dict: &harn_vm_core::value::DictMap,
    key: &str,
) -> Option<String> {
    dict.get(key)
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
}

pub(super) fn value_as_string(value: &VmValue) -> Option<String> {
    match value {
        VmValue::Nil => None,
        VmValue::String(s) => Some(s.to_string()),
        other => Some(other.display()),
    }
}

pub(super) fn string_list_value(values: Vec<String>) -> VmValue {
    VmValue::strings(values)
}

pub(super) fn value_as_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

pub(super) fn read_text_if_exists(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
