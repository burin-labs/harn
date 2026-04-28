//! Parameter parsing helpers for tool builtins.
//!
//! Tools accept a single `Dict` argument from Harn (so callers can write
//! `hostlib_tools_search({pattern: "TODO", path: "src"})`). These helpers
//! pull strongly-typed values out of that dict and produce
//! [`HostlibError`] variants on shape mismatches so the script side gets a
//! structured exception.

use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;

/// Extract the first argument as a Dict. Tools always receive a single
/// dict from Harn-side callers; if the caller passed nothing we treat it
/// as an empty payload.
pub fn dict_arg(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<Rc<BTreeMap<String, VmValue>>, HostlibError> {
    match args.first() {
        Some(VmValue::Dict(dict)) => Ok(dict.clone()),
        Some(VmValue::Nil) | None => Ok(Rc::new(BTreeMap::new())),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: "params",
            message: format!(
                "expected a dict argument, got {} ({:?})",
                other.type_name(),
                other
            ),
        }),
    }
}

/// Required string field.
pub fn require_string(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<String, HostlibError> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected string, got {}", other.type_name()),
        }),
        None => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

/// Optional string field. Missing/`Nil` returns `None`.
pub fn optional_string(
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
            message: format!("expected string, got {}", other.type_name()),
        }),
    }
}

/// Optional `bool`. Defaults to `default` when missing or `Nil`.
pub fn optional_bool(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
    default: bool,
) -> Result<bool, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(VmValue::Bool(b)) => Ok(*b),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected bool, got {}", other.type_name()),
        }),
    }
}

/// Optional integer. Defaults to `default` when missing or `Nil`.
/// Also accepts `Float` values that are whole numbers (Harn treats numeric
/// literals as ints by default, but JSON-decoded payloads can show up as
/// floats).
pub fn optional_int(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
    default: i64,
) -> Result<i64, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(VmValue::Int(n)) => Ok(*n),
        Some(VmValue::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", other.type_name()),
        }),
    }
}

/// Required integer field. Errors if the key is missing or not an int.
pub fn require_int(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<i64, HostlibError> {
    match dict.get(key) {
        Some(VmValue::Int(n)) => Ok(*n),
        Some(VmValue::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", other.type_name()),
        }),
        None => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

/// Optional list of strings. Missing/`Nil` returns `Vec::new()`.
pub fn optional_string_list(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Vec<String>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    VmValue::String(s) => out.push(s.to_string()),
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin,
                            param: key,
                            message: format!(
                                "expected list of strings, got element {}",
                                other.type_name()
                            ),
                        });
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of strings, got {}", other.type_name()),
        }),
    }
}

/// Optional list of integers. Missing/`Nil` returns `Vec::new()`.
pub fn optional_int_list(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Vec<i64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    VmValue::Int(n) => out.push(*n),
                    VmValue::Float(f) if f.fract() == 0.0 => out.push(*f as i64),
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin,
                            param: key,
                            message: format!(
                                "expected list of integers, got element {}",
                                other.type_name()
                            ),
                        });
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of integers, got {}", other.type_name()),
        }),
    }
}

/// Construct a [`VmValue::Dict`] from a `(key, value)` iterable. Used by
/// tool handlers when shaping their JSON-Schema-mirrored response.
pub fn build_dict<I, K>(entries: I) -> VmValue
where
    I: IntoIterator<Item = (K, VmValue)>,
    K: Into<String>,
{
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.into(), v);
    }
    VmValue::Dict(Rc::new(map))
}

/// Convenience constructor for `VmValue::String` from a `&str`.
pub fn str_value(s: impl AsRef<str>) -> VmValue {
    VmValue::String(Rc::from(s.as_ref()))
}
