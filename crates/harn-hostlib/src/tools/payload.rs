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
use crate::process::EnvMode;
use crate::value_args;

/// Pull the single dict argument from a builtin call's argv. The dict
/// itself is the JSON request body; positional args are not used.
pub(crate) fn require_dict_arg(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<harn_vm::value::DictMap, HostlibError> {
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
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<String>, HostlibError> {
    value_args::optional_string(builtin, map, key)
}

/// Optional bool field on a request dict.
pub(crate) fn optional_bool(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<bool>, HostlibError> {
    value_args::optional_bool(builtin, map, key)
}

/// Optional non-negative integer field on a request dict.
pub(crate) fn optional_u64(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    value_args::optional_u64(builtin, map, key)
}

/// Convert an optional `timeout_ms` field into a `Duration`, treating zero
/// or absent values as "no timeout".
pub(crate) fn optional_timeout(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
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
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<Vec<String>>, HostlibError> {
    value_args::optional_string_list(builtin, map, key)
}

/// Optional `BTreeMap<String, String>` field on a request dict (e.g. `env`).
pub(crate) fn optional_string_map(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
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
                out.insert(k.to_string(), s.to_string());
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

/// Parse the shared process-tool `env_mode` option.
pub(crate) fn optional_env_mode(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
    env_supplied: bool,
) -> Result<EnvMode, HostlibError> {
    match optional_string(builtin, map, "env_mode")?.as_deref() {
        Some("inherit_clean") => Ok(EnvMode::InheritClean),
        Some("replace") => Ok(EnvMode::Replace),
        Some("patch") => Ok(EnvMode::Patch),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: "env_mode",
            message: format!(
                "unsupported env_mode {other:?}; expected inherit_clean, replace, or patch"
            ),
        }),
        None if env_supplied => Ok(EnvMode::Replace),
        None => Ok(EnvMode::InheritClean),
    }
}

/// Required string field on a request dict — fails if missing or wrong type.
pub(crate) fn require_string(
    builtin: &'static str,
    map: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<String, HostlibError> {
    value_args::require_string(builtin, map, key)
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
    value_args::describe(value)
}
