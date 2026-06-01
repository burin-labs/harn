use std::collections::BTreeMap;

use harn_vm::VmValue;

use crate::error::HostlibError;

pub(crate) fn describe(value: &VmValue) -> &'static str {
    match value {
        VmValue::Int(_) => "int",
        VmValue::Float(_) => "float",
        VmValue::String(_) => "string",
        VmValue::Bytes(_) => "bytes",
        VmValue::Bool(_) => "bool",
        VmValue::Nil => "nil",
        VmValue::List(_) => "list",
        VmValue::Dict(_) => "dict",
        VmValue::Set(_) => "set",
        _ => "other",
    }
}

pub(crate) fn require_string(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<String, HostlibError> {
    optional_string(builtin, dict, key)?.ok_or(HostlibError::MissingParameter {
        builtin,
        param: key,
    })
}

pub(crate) fn optional_string(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<String>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected string, got {}", describe(other)),
        }),
    }
}

pub(crate) fn optional_bool(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<bool>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected bool, got {}", describe(other)),
        }),
    }
}

pub(crate) fn optional_i64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
    default: i64,
) -> Result<i64, HostlibError> {
    match optional_i64_no_default(builtin, dict, key)? {
        Some(value) => Ok(value),
        None => Ok(default),
    }
}

pub(crate) fn optional_i64_no_default(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<i64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => coerce_i64(builtin, key, value).map(Some),
    }
}

pub(crate) fn optional_u64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    match optional_i64_no_default(builtin, dict, key)? {
        None => Ok(None),
        Some(value) if value >= 0 => Ok(Some(value as u64)),
        Some(value) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected non-negative integer, got {value}"),
        }),
    }
}

pub(crate) fn optional_string_list(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<Vec<String>>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::List(items)) => items
            .iter()
            .enumerate()
            .map(|(idx, item)| match item {
                VmValue::String(value) => Ok(value.to_string()),
                other => Err(HostlibError::InvalidParameter {
                    builtin,
                    param: key,
                    message: format!("expected string at index {idx}, got {}", describe(other)),
                }),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of strings, got {}", describe(other)),
        }),
    }
}

pub(crate) fn coerce_i64(
    builtin: &'static str,
    key: &'static str,
    value: &VmValue,
) -> Result<i64, HostlibError> {
    match value {
        VmValue::Int(value) => Ok(*value),
        VmValue::Float(value) if value.is_finite() && value.fract() == 0.0 => {
            const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
            if *value < i64::MIN as f64 || *value >= I64_MAX_EXCLUSIVE {
                return Err(HostlibError::InvalidParameter {
                    builtin,
                    param: key,
                    message: format!("integer is outside i64 range: {value}"),
                });
            }
            Ok(*value as i64)
        }
        VmValue::Float(value) if !value.is_finite() => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected finite integer, got {value}"),
        }),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", describe(other)),
        }),
    }
}
