//! Parameter parsing helpers for tool builtins.
//!
//! Tools accept a single `Dict` argument from Harn (so callers can write
//! `hostlib_tools_search({pattern: "TODO", path: "src"})`). These helpers
//! pull strongly-typed values out of that dict and produce
//! [`HostlibError`] variants on shape mismatches so the script side gets a
//! structured exception.

use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::value_args;

/// Extract the first argument as a Dict. Tools always receive a single
/// dict from Harn-side callers; if the caller passed nothing we treat it
/// as an empty payload.
pub fn dict_arg(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<Arc<harn_vm::value::DictMap>, HostlibError> {
    match args.first() {
        Some(VmValue::Dict(dict)) => Ok(dict.clone()),
        Some(VmValue::Nil) | None => Ok(Arc::new(harn_vm::value::DictMap::new())),
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
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<String, HostlibError> {
    value_args::require_string(builtin, dict, key)
}

/// Optional string field. Missing/`Nil` returns `None`.
pub fn optional_string(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<String>, HostlibError> {
    value_args::optional_string(builtin, dict, key)
}

/// Optional `bool`. Defaults to `default` when missing or `Nil`.
pub fn optional_bool(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
    default: bool,
) -> Result<bool, HostlibError> {
    value_args::optional_bool(builtin, dict, key).map(|value| value.unwrap_or(default))
}

/// Optional integer. Defaults to `default` when missing or `Nil`.
/// Also accepts `Float` values that are whole numbers (Harn treats numeric
/// literals as ints by default, but JSON-decoded payloads can show up as
/// floats).
pub fn optional_int(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
    default: i64,
) -> Result<i64, HostlibError> {
    value_args::optional_i64(builtin, dict, key, default)
}

/// Optional list of strings. Missing/`Nil` returns `Vec::new()`.
pub fn optional_string_list(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Vec<String>, HostlibError> {
    value_args::optional_string_list(builtin, dict, key).map(|value| value.unwrap_or_default())
}

/// Optional list of integers. Missing/`Nil` returns `Vec::new()`.
///
/// Only the code-index trigram-query builtin consumes this, so it is gated
/// on the `ast` feature to keep the lean (tools-only) build warning-clean.
#[cfg(feature = "ast")]
pub fn optional_int_list(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Vec<i64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                out.push(value_args::coerce_i64(builtin, key, item)?);
            }
            Ok(out)
        }
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!(
                "expected list of integers, got {}",
                value_args::describe(other)
            ),
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
    let mut map: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
    for (k, v) in entries {
        map.insert(k.into(), v);
    }
    VmValue::dict(map)
}

/// Convenience constructor for `VmValue::String` from a `&str`.
pub fn str_value(s: impl AsRef<str>) -> VmValue {
    VmValue::string(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: [(&'static str, VmValue); 1]) -> harn_vm::value::DictMap {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    #[test]
    fn optional_int_rejects_non_finite_float() {
        let payload = dict([("limit", VmValue::Float(f64::INFINITY))]);
        assert!(matches!(
            optional_int("test", &payload, "limit", 0),
            Err(HostlibError::InvalidParameter { param: "limit", .. })
        ));
    }

    #[test]
    fn optional_int_rejects_out_of_range_float() {
        let payload = dict([("limit", VmValue::Float(1.0e100))]);
        assert!(matches!(
            optional_int("test", &payload, "limit", 0),
            Err(HostlibError::InvalidParameter { param: "limit", .. })
        ));
    }
}
