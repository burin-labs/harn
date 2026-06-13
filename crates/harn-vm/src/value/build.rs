//! Ergonomic builders for the `BTreeMap<String, VmValue>` that backs
//! `VmValue::Dict`.
//!
//! Harn builtins assemble result dicts by hand, and the raw
//! `map.insert("key".to_string(), VmValue::String(std::sync::Arc::from(v)))`
//! spelling is both verbose and — once rustfmt wraps it — four lines per
//! field. [`VmDictExt`] collapses each field to a single call while keeping
//! the receiver, so the common shapes read as `dict.put_str("key", v)` /
//! `dict.put_opt_str("key", maybe.as_deref())`. There is no behavioural
//! change: every method is a thin wrapper over `BTreeMap::insert`.

use std::collections::BTreeMap;

use super::VmValue;

/// Field-insertion helpers for a `VmValue::Dict` backing map.
///
/// Implemented for the concrete `BTreeMap<String, VmValue>` only, so the
/// methods are unavailable on unrelated maps and cannot be applied by mistake.
pub trait VmDictExt {
    /// Inserts `value` under `key` (owning the key as a `String`).
    fn put(&mut self, key: &str, value: VmValue);
    /// Inserts a string value built via [`VmValue::string`].
    fn put_str(&mut self, key: &str, value: impl AsRef<str>);
    /// Inserts a string value only when `value` is `Some`.
    fn put_opt_str(&mut self, key: &str, value: Option<impl AsRef<str>>);
    /// Inserts a `VmValue` only when `value` is `Some`.
    fn put_opt(&mut self, key: &str, value: Option<VmValue>);
    /// Inserts a boolean value.
    fn put_bool(&mut self, key: &str, value: bool);
    /// Inserts an integer value.
    fn put_int(&mut self, key: &str, value: i64);
}

impl VmDictExt for BTreeMap<String, VmValue> {
    fn put(&mut self, key: &str, value: VmValue) {
        self.insert(key.to_string(), value);
    }

    fn put_str(&mut self, key: &str, value: impl AsRef<str>) {
        self.insert(key.to_string(), VmValue::string(value));
    }

    fn put_opt_str(&mut self, key: &str, value: Option<impl AsRef<str>>) {
        if let Some(value) = value {
            self.insert(key.to_string(), VmValue::string(value));
        }
    }

    fn put_opt(&mut self, key: &str, value: Option<VmValue>) {
        if let Some(value) = value {
            self.insert(key.to_string(), value);
        }
    }

    fn put_bool(&mut self, key: &str, value: bool) {
        self.insert(key.to_string(), VmValue::Bool(value));
    }

    fn put_int(&mut self, key: &str, value: i64) {
        self.insert(key.to_string(), VmValue::Int(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_str_matches_manual_construction() {
        let mut dict = BTreeMap::new();
        dict.put_str("k", "v");
        match dict.get("k") {
            Some(VmValue::String(s)) => assert_eq!(s.as_ref(), "v"),
            other => panic!("expected string value, got {other:?}"),
        }
    }

    #[test]
    fn put_opt_str_skips_none_and_inserts_some() {
        let mut dict = BTreeMap::new();
        dict.put_opt_str("present", Some("v"));
        dict.put_opt_str("absent", None::<&str>);
        assert!(dict.contains_key("present"));
        assert!(!dict.contains_key("absent"));
    }

    #[test]
    fn put_handles_scalars() {
        let mut dict = BTreeMap::new();
        dict.put_bool("b", true);
        dict.put_int("n", 7);
        dict.put("nil", VmValue::Nil);
        assert!(matches!(dict.get("b"), Some(VmValue::Bool(true))));
        assert!(matches!(dict.get("n"), Some(VmValue::Int(7))));
        assert!(matches!(dict.get("nil"), Some(VmValue::Nil)));
    }
}
