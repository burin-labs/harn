//! Reading Harn call arguments and dict fields.
//!
//! Every trigger builtin takes its configuration as a `dict` or a positional
//! `VmValue`, so these accessors are the one place that decides what a missing
//! field, a nil, and a wrong type each mean — and phrases the resulting
//! [`VmError`] the same way across the whole surface.

use serde::Serialize;

use crate::value::{VmError, VmValue};

pub(super) fn require_dict_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(dict)) => Ok(dict),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: expected dict argument at position {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing dict argument at position {}",
            index + 1
        ))),
    }
}

pub(super) fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    optional_string_arg(args, index, builtin, name)?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing string argument `{name}`")))
}

pub(super) fn optional_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<Option<String>, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn required_string(
    map: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    optional_string(map, key)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing string field `{key}`")))
}

pub(super) fn optional_string(map: &crate::value::DictMap, key: &str) -> Option<String> {
    map.get(key).and_then(|value| match value {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    })
}

pub(super) fn optional_bool(
    map: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<bool>, VmError> {
    match map.get(key) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(_) => Err(VmError::Runtime(format!(
            "{builtin}: field `{key}` must be a bool"
        ))),
    }
}

pub(super) fn parse_string_list(value: &VmValue) -> Result<Vec<String>, VmError> {
    let VmValue::List(items) = value else {
        return Err(VmError::Runtime(
            "trigger_register: `events` must be a list of strings".to_string(),
        ));
    };
    items
        .iter()
        .map(|item| match item {
            VmValue::String(text) => Ok(text.to_string()),
            other => Err(VmError::Runtime(format!(
                "trigger_register: `events` entries must be strings, got {}",
                other.type_name()
            ))),
        })
        .collect()
}

pub(super) fn number_value(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(number) => Some(*number),
        VmValue::Int(number) => Some(*number as f64),
        _ => None,
    }
}

pub(super) fn trigger_registry_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("trigger stdlib: {error}"))
}

pub(super) fn value_from_serde<T: Serialize>(value: &T) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::to_value(value).unwrap_or_default())
}
