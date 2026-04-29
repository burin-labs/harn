//! VmValue → typed-payload helpers.
//!
//! Every tool builtin accepts a single dict argument shaped exactly like its
//! JSON request schema in `schemas/tools/`. Helpers here pull typed fields
//! out of that dict and surface schema mismatches as
//! [`HostlibError::InvalidParameter`].

use std::collections::BTreeMap;
use std::time::Duration;

use harn_vm::VmValue;

use crate::error::HostlibError;

/// Pull the single dict argument from a builtin call's argv. The dict
/// itself is the JSON request body; positional args are not used.
pub(crate) fn require_dict_arg(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<BTreeMap<String, VmValue>, HostlibError> {
    let first = args.first().ok_or(HostlibError::MissingParameter {
        builtin,
        param: "request",
    })?;
    match first {
        VmValue::Dict(map) => Ok((**map).clone()),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: "request",
            message: format!(
                "expected a dict (JSON request body), got {}",
                describe(other)
            ),
        }),
    }
}

/// Optional string field on a request dict.
pub(crate) fn optional_string(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<String>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(s) => Ok(Some(s.to_string())),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected string, got {}", describe(other)),
        }),
    }
}

/// Optional bool field on a request dict.
pub(crate) fn optional_bool(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<bool>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Bool(b) => Ok(Some(*b)),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected bool, got {}", describe(other)),
        }),
    }
}

/// Optional non-negative integer field on a request dict.
pub(crate) fn optional_u64(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Int(i) if *i >= 0 => Ok(Some(*i as u64)),
        VmValue::Int(i) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected non-negative integer, got {i}"),
        }),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", describe(other)),
        }),
    }
}

/// Convert an optional `timeout_ms` field into a `Duration`, treating zero
/// or absent values as "no timeout".
pub(crate) fn optional_timeout(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<Duration>, HostlibError> {
    Ok(optional_u64(builtin, map, key)?.and_then(|ms| {
        if ms == 0 {
            None
        } else {
            Some(Duration::from_millis(ms))
        }
    }))
}

/// Optional `Vec<String>` field on a request dict (e.g. `argv`, `packages`).
pub(crate) fn optional_string_list(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<Vec<String>>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, item) in list.iter().enumerate() {
                let VmValue::String(s) = item else {
                    return Err(HostlibError::InvalidParameter {
                        builtin,
                        param: key,
                        message: format!("expected string at index {i}, got {}", describe(item)),
                    });
                };
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of strings, got {}", describe(other)),
        }),
    }
}

/// Optional `BTreeMap<String, String>` field on a request dict (e.g. `env`).
pub(crate) fn optional_string_map(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<BTreeMap<String, String>>, HostlibError> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Dict(dict) => {
            let mut out = BTreeMap::new();
            for (k, v) in dict.iter() {
                let VmValue::String(s) = v else {
                    return Err(HostlibError::InvalidParameter {
                        builtin,
                        param: key,
                        message: format!("value for {k:?} must be string, got {}", describe(v)),
                    });
                };
                out.insert(k.clone(), s.to_string());
            }
            Ok(Some(out))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected dict<string,string>, got {}", describe(other)),
        }),
    }
}

/// Required string field on a request dict — fails if missing or wrong type.
pub(crate) fn require_string(
    builtin: &'static str,
    map: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<String, HostlibError> {
    optional_string(builtin, map, key)?.ok_or(HostlibError::MissingParameter {
        builtin,
        param: key,
    })
}

/// Split an argv list into `(program, remaining_args)`. Errors if the list
/// is empty or the program element is empty.
pub(crate) fn parse_argv_program(
    builtin: &'static str,
    mut argv: Vec<String>,
) -> Result<(String, Vec<String>), HostlibError> {
    if argv.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "argv",
            message: "argv must contain at least one element".to_string(),
        });
    }
    let program = argv.remove(0);
    if program.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "argv",
            message: "first argv element (program) must be non-empty".to_string(),
        });
    }
    Ok((program, argv))
}

fn describe(value: &VmValue) -> &'static str {
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
