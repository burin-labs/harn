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
        map.insert(harn_vm::value::intern_key(&k.into()), v);
    }
    VmValue::dict(map)
}

/// Convenience constructor for `VmValue::String` from a `&str`.
pub fn str_value(s: impl AsRef<str>) -> VmValue {
    VmValue::string(s)
}

/// Resolve a host filesystem argument relative to the current Harn execution
/// root before sandbox checks or disk I/O.
pub(crate) fn resolve_host_path(path: impl AsRef<str>) -> std::path::PathBuf {
    harn_vm::stdlib::process::resolve_source_relative_path(path.as_ref())
}

/// Normalize a filesystem path into the string form the agent/tool surface
/// must emit on **every** platform: forward-slash (`/`) separators.
///
/// `Path::display()` / `to_string_lossy()` render OS-native separators —
/// backslashes on Windows — so a raw path string leaks `crates\foo\bar.rs`
/// into tool output on Windows. That breaks the product invariant every
/// path-consuming layer downstream assumes: the LLM, the tool-call corpus,
/// pipeline glob/prefix matching, and cross-OS determinism all expect `/`.
///
/// This is the single chokepoint for that conversion. Any path string that
/// crosses into a `VmValue`, a tool-result payload, or anything the model or
/// a pipeline reads MUST go through here (or [`to_agent_path_str`] for a
/// `&str` you already hold). Do **not** hand-roll `.replace('\\', "/")` at
/// new call sites and do **not** feed the result back into a filesystem
/// syscall — keep OS-native paths for syscalls; only the agent-facing STRING
/// is normalized.
///
/// Idempotent: a value already using `/` is returned unchanged.
pub fn to_agent_path(path: impl AsRef<std::path::Path>) -> String {
    to_agent_path_str(path.as_ref().to_string_lossy())
}

/// [`to_agent_path`] for a string you already hold (e.g. a path label that was
/// captured before this normalization existed, or a `Cow<str>`). Emits
/// forward-slash separators regardless of the platform the string was built
/// on.
pub fn to_agent_path_str(path: impl AsRef<str>) -> String {
    path.as_ref().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: [(&'static str, VmValue); 1]) -> harn_vm::value::DictMap {
        entries
            .into_iter()
            .map(|(key, value)| (harn_vm::value::intern_key(key), value))
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

    #[test]
    fn to_agent_path_str_rewrites_backslashes() {
        // Simulates the string a Windows `Path::display()` would produce.
        assert_eq!(
            to_agent_path_str("crates\\burin-tui\\src\\lib.rs"),
            "crates/burin-tui/src/lib.rs"
        );
    }

    #[test]
    fn to_agent_path_str_leaves_forward_slashes_untouched() {
        // The Unix rendering (and any already-normalized value) must be
        // idempotent so the helper is safe to apply unconditionally.
        assert_eq!(
            to_agent_path_str("crates/burin-tui/src/lib.rs"),
            "crates/burin-tui/src/lib.rs"
        );
    }

    #[test]
    fn to_agent_path_never_emits_backslashes() {
        // Whatever the host separator, the emitted string carries only `/`.
        let joined: std::path::PathBuf = ["crates", "burin-tui", "src", "lib.rs"].iter().collect();
        let emitted = to_agent_path(&joined);
        assert!(
            !emitted.contains('\\'),
            "agent path must not contain backslashes, got {emitted:?}"
        );
        assert!(emitted.ends_with("crates/burin-tui/src/lib.rs"));
    }
}
