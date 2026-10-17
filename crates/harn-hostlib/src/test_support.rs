//! Unit-test helpers shared by the `#[cfg(test)]` modules of this crate:
//! building request dicts, unpacking `VmValue` responses, and writing
//! source fixtures to temp files.

use std::io::Write;

use harn_vm::VmValue;
use tempfile::NamedTempFile;

pub(crate) fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

/// A dict value from `(key, value)` pairs.
pub(crate) fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    VmValue::dict(map)
}

/// `value[key]`, panicking when `value` is not a dict or lacks `key`.
pub(crate) fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d
            .get(key)
            .unwrap_or_else(|| panic!("missing field `{key}`")),
        _ => panic!("expected dict, got {value:?}"),
    }
}

pub(crate) fn s(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

pub(crate) fn n(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(i) => *i,
        other => panic!("expected int, got {other:?}"),
    }
}

pub(crate) fn b(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

/// A temp file with the given extension holding `source`.
pub(crate) fn write_temp(extension: &str, source: &str) -> NamedTempFile {
    let mut file = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile()
        .expect("temp file");
    file.write_all(source.as_bytes()).expect("write source");
    file
}
