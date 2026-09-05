//! Numeric argument coercion for the code-index builtins.
//!
//! Every code-index builtin takes its arguments as one VM dict, so the
//! question "is this key a positive integer?" is asked dozens of times and
//! has exactly one right answer per shape. These helpers own that answer and
//! the [`HostlibError::InvalidParameter`] wording that goes with it, so a
//! caller states the shape it needs and never re-derives the diagnostic.

use harn_vm::value::VmValue;

use super::file_table::FileId;

use crate::error::HostlibError;
use crate::value_args;

pub(super) fn parse_hash(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<u64, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(0),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(VmValue::Int(n)) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 0, got {n}"),
        }),
        Some(VmValue::String(s)) => s
            .parse::<u64>()
            .map_err(|_| HostlibError::InvalidParameter {
                builtin,
                param: key,
                message: format!("expected u64-parseable string, got {s:?}"),
            }),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!(
                "expected integer or numeric string, got {}",
                other.type_name()
            ),
        }),
    }
}

pub(super) fn require_positive_u64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<u64, HostlibError> {
    let raw = require_non_negative_u64(builtin, dict, key)?;
    if raw == 0 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must be >= 1".to_string(),
        });
    }
    Ok(raw)
}

pub(super) fn require_positive_file_id(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<FileId, HostlibError> {
    let raw = require_positive_u64(builtin, dict, key)?;
    FileId::try_from(raw).map_err(|_| HostlibError::InvalidParameter {
        builtin,
        param: key,
        message: "does not fit in file id".to_string(),
    })
}

pub(super) fn require_non_negative_u64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<u64, HostlibError> {
    match value_args::optional_i64_no_default(builtin, dict, key)? {
        Some(value) if value >= 0 => Ok(value as u64),
        Some(value) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 0, got {value}"),
        }),
        None => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

pub(super) fn optional_positive_u64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(_) => require_positive_u64(builtin, dict, key).map(Some),
    }
}

pub(super) fn optional_non_negative_u64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
    default: u64,
) -> Result<u64, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(_) => require_non_negative_u64(builtin, dict, key),
    }
}

pub(super) fn optional_positive_i64(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<i64>, HostlibError> {
    match value_args::optional_i64_no_default(builtin, dict, key)? {
        None => Ok(None),
        Some(value) if value >= 1 => Ok(Some(value)),
        Some(value) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 1, got {value}"),
        }),
    }
}

pub(super) fn optional_positive_usize(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<usize>, HostlibError> {
    match optional_positive_u64(builtin, dict, key)? {
        Some(value) => {
            usize::try_from(value)
                .map(Some)
                .map_err(|_| HostlibError::InvalidParameter {
                    builtin,
                    param: key,
                    message: "does not fit in usize".to_string(),
                })
        }
        None => Ok(None),
    }
}
