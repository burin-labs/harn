//! Shared option-bag and argument-extraction helpers for stdlib builtins.
//!
//! Builtins choose an [`ErrorKind`] per call: [`ErrorKind::Runtime`] for
//! failures that should bubble out as runtime errors, [`ErrorKind::Thrown`]
//! for failures that user code can `try` / `recover`.
//!
//! Conventions:
//! - Function-name strings are `&'static str` so error messages can be
//!   formatted without allocations on the success path.
//! - Optional fields treat both `Nil` and "missing" as None.
//! - String getters trim whitespace and treat trimmed-empty strings as None.
//! - [`OptionsParser`] tracks consumed keys; call
//!   [`OptionsParser::finish_strict`] on closed-schema option bags to reject
//!   unknown options.
//!
//! Several helpers below have no callers today but are kept available for
//! future stdlib additions — option-bag parsing is the most common
//! new-builtin shape. `#![allow(dead_code)]` keeps the API surface intact
//! without per-item attributes.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::time::Duration as StdDuration;

use crate::value::{VmError, VmValue};

/// Whether an option-parsing failure should bubble as a runtime error or be
/// surfaced as a catchable thrown value.
///
/// Most stdlib builtins use [`ErrorKind::Runtime`]. Session and a handful of
/// other catchable-by-default builtins use [`ErrorKind::Thrown`] so user code
/// can `try` / `recover`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ErrorKind {
    Runtime,
    Thrown,
}

impl ErrorKind {
    pub(crate) fn err(self, msg: impl Into<String>) -> VmError {
        match self {
            ErrorKind::Runtime => VmError::Runtime(msg.into()),
            ErrorKind::Thrown => VmError::Thrown(VmValue::String(std::sync::Arc::from(msg.into()))),
        }
    }
}

/// Build a `fn_name: msg` error of the requested kind.
pub(crate) fn fn_err(fn_name: &str, kind: ErrorKind, msg: impl Display) -> VmError {
    kind.err(format!("{fn_name}: {msg}"))
}

/// Require a positional dict argument. Returns the underlying `BTreeMap` by
/// reference to avoid copying.
pub(crate) fn dict_arg<'a>(
    args: &'a [VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
    kind: ErrorKind,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match args.get(idx) {
        Some(VmValue::Dict(dict)) => Ok(dict.as_ref()),
        Some(value) => Err(fn_err(
            fn_name,
            kind,
            format_args!("`{arg_name}` must be a dict (got {})", value.type_name()),
        )),
        None => Err(fn_err(
            fn_name,
            kind,
            format_args!("`{arg_name}` is required"),
        )),
    }
}

/// Optional positional dict argument. `None`/`Nil` → `None`.
pub(crate) fn optional_dict_arg<'a>(
    args: &'a [VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
    kind: ErrorKind,
) -> Result<Option<&'a BTreeMap<String, VmValue>>, VmError> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Dict(dict)) => Ok(Some(dict.as_ref())),
        Some(value) => Err(fn_err(
            fn_name,
            kind,
            format_args!(
                "`{arg_name}` must be a dict or nil (got {})",
                value.type_name()
            ),
        )),
    }
}

/// Require a positional string argument. The string is trimmed and an empty
/// result is rejected, matching the pre-existing helpers across the agent
/// modules.
pub(crate) fn required_string_arg(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
    kind: ErrorKind,
) -> Result<String, VmError> {
    match args.get(idx) {
        Some(VmValue::String(text)) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(fn_err(
            fn_name,
            kind,
            format_args!("`{arg_name}` must be a non-empty string"),
        )),
    }
}

/// Optional positional string argument. Trims whitespace; empty → `None`.
pub(crate) fn optional_string_arg(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
    kind: ErrorKind,
) -> Result<Option<String>, VmError> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(value) => Err(fn_err(
            fn_name,
            kind,
            format_args!(
                "`{arg_name}` must be a string or nil (got {})",
                value.type_name()
            ),
        )),
    }
}

/// Require a positional int argument.
pub(crate) fn required_int_arg(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
    kind: ErrorKind,
) -> Result<i64, VmError> {
    args.get(idx)
        .and_then(VmValue::as_int)
        .ok_or_else(|| fn_err(fn_name, kind, format_args!("`{arg_name}` must be an int")))
}

/// Coerce a Harn duration-like value to non-negative milliseconds.
///
/// Several stdlib modules accept either a `duration`, an integer millisecond
/// count, or a finite float millisecond count. Keep the conversion here so the
/// edge cases stay consistent across waitpoints, monitors, HITL, and storage
/// connectors.
pub(crate) fn non_negative_millis_from_value(
    value: &VmValue,
    fn_name: &'static str,
    value_name: &str,
    kind: ErrorKind,
) -> Result<u64, VmError> {
    match value {
        VmValue::Duration(ms) | VmValue::Int(ms) if *ms >= 0 => Ok(*ms as u64),
        VmValue::Duration(_) | VmValue::Int(_) => Err(fn_err(
            fn_name,
            kind,
            format_args!("`{value_name}` must be >= 0"),
        )),
        VmValue::Float(ms) if ms.is_finite() && *ms >= 0.0 && *ms <= u64::MAX as f64 => {
            Ok(*ms as u64)
        }
        VmValue::Float(_) => Err(fn_err(
            fn_name,
            kind,
            format_args!("`{value_name}` must be a finite non-negative millisecond count"),
        )),
        other => Err(fn_err(
            fn_name,
            kind,
            format_args!(
                "`{value_name}` must be a duration or millisecond count (got {})",
                other.type_name()
            ),
        )),
    }
}

pub(crate) fn duration_from_value(
    value: &VmValue,
    fn_name: &'static str,
    value_name: &str,
    kind: ErrorKind,
) -> Result<StdDuration, VmError> {
    non_negative_millis_from_value(value, fn_name, value_name, kind).map(StdDuration::from_millis)
}

/// Schema-driven parser for an option-bag dict.
///
/// Tracks which keys have been consumed; pair with [`OptionsParser::finish_strict`]
/// to reject unknown keys. For builtins that intentionally forward extras to
/// other layers (e.g. daemon spawn options piped into the agent runtime),
/// either skip the strict check or pass the forwarded keys to
/// [`OptionsParser::allow`] first.
pub(crate) struct OptionsParser<'a> {
    fn_name: &'static str,
    dict: &'a BTreeMap<String, VmValue>,
    seen: BTreeSet<&'static str>,
    kind: ErrorKind,
}

impl<'a> OptionsParser<'a> {
    pub(crate) fn new(
        fn_name: &'static str,
        dict: &'a BTreeMap<String, VmValue>,
        kind: ErrorKind,
    ) -> Self {
        Self {
            fn_name,
            dict,
            seen: BTreeSet::new(),
            kind,
        }
    }

    fn err(&self, msg: impl Display) -> VmError {
        fn_err(self.fn_name, self.kind, msg)
    }

    fn mark(&mut self, key: &'static str) {
        self.seen.insert(key);
    }

    /// Mark `key` as consumed without reading its value. Useful when the
    /// caller will reach into the raw dict separately but still wants
    /// `finish_strict` to consider the key known.
    pub(crate) fn allow(&mut self, key: &'static str) {
        self.mark(key);
    }

    /// Return the raw VmValue for `key` if present. Marks the key consumed.
    pub(crate) fn raw(&mut self, key: &'static str) -> Option<&'a VmValue> {
        self.mark(key);
        self.dict.get(key)
    }

    pub(crate) fn required_string(&mut self, key: &'static str) -> Result<String, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            Some(VmValue::String(s)) if !s.trim().is_empty() => Ok(s.to_string()),
            Some(VmValue::String(_)) => {
                Err(self.err(format_args!("`{key}` must be a non-empty string")))
            }
            None | Some(VmValue::Nil) => Err(self.err(format_args!("`{key}` is required"))),
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a string (got {})",
                value.type_name()
            ))),
        }
    }

    pub(crate) fn optional_string(&mut self, key: &'static str) -> Result<Option<String>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::String(s)) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a string or nil (got {})",
                value.type_name()
            ))),
        }
    }

    /// Optional string field that preserves the value exactly. Use this for
    /// fields where leading/trailing whitespace is part of the public contract.
    pub(crate) fn optional_string_raw(
        &mut self,
        key: &'static str,
    ) -> Result<Option<String>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::String(s)) => Ok(Some(s.to_string())),
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a string or nil (got {})",
                value.type_name()
            ))),
        }
    }

    pub(crate) fn optional_bool(&mut self, key: &'static str) -> Result<Option<bool>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::Bool(b)) => Ok(Some(*b)),
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a bool or nil (got {})",
                value.type_name()
            ))),
        }
    }

    pub(crate) fn bool_or(&mut self, key: &'static str, default: bool) -> Result<bool, VmError> {
        Ok(self.optional_bool(key)?.unwrap_or(default))
    }

    pub(crate) fn optional_int(&mut self, key: &'static str) -> Result<Option<i64>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(value) => match value.as_int() {
                Some(v) => Ok(Some(v)),
                None => Err(self.err(format_args!(
                    "`{key}` must be an int or nil (got {})",
                    value.type_name()
                ))),
            },
        }
    }

    pub(crate) fn optional_usize(&mut self, key: &'static str) -> Result<Option<usize>, VmError> {
        let Some(raw) = self.optional_int(key)? else {
            return Ok(None);
        };
        if raw < 0 {
            return Err(self.err(format_args!("`{key}` must be >= 0")));
        }
        Ok(Some(raw as usize))
    }

    pub(crate) fn optional_list(
        &mut self,
        key: &'static str,
    ) -> Result<Option<&'a [VmValue]>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::List(list)) => Ok(Some(list.as_slice())),
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a list or nil (got {})",
                value.type_name()
            ))),
        }
    }

    pub(crate) fn optional_dict(
        &mut self,
        key: &'static str,
    ) -> Result<Option<&'a BTreeMap<String, VmValue>>, VmError> {
        self.mark(key);
        match self.dict.get(key) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::Dict(d)) => Ok(Some(d.as_ref())),
            Some(value) => Err(self.err(format_args!(
                "`{key}` must be a dict or nil (got {})",
                value.type_name()
            ))),
        }
    }

    /// Reject any keys not yet marked consumed (and not in `extra_known`).
    /// Use this on closed-schema option bags. For bags that forward extras,
    /// either skip this call or pre-mark them with [`OptionsParser::allow`].
    pub(crate) fn finish_strict(self, extra_known: &[&'static str]) -> Result<(), VmError> {
        let mut unknown: Vec<&str> = self
            .dict
            .keys()
            .filter(|key| !self.seen.contains(key.as_str()))
            .filter(|key| !extra_known.contains(&key.as_str()))
            .map(|s| s.as_str())
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort();
        Err(fn_err(
            self.fn_name,
            self.kind,
            format!("unknown option(s): {}", unknown.join(", ")),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, VmValue)]) -> BTreeMap<String, VmValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn parser_required_string_present() {
        let d = dict(&[("task", VmValue::String(std::sync::Arc::from("do it")))]);
        let mut p = OptionsParser::new("daemon_spawn", &d, ErrorKind::Runtime);
        assert_eq!(p.required_string("task").unwrap(), "do it");
    }

    #[test]
    fn parser_required_string_missing() {
        let d = dict(&[]);
        let mut p = OptionsParser::new("daemon_spawn", &d, ErrorKind::Runtime);
        let err = p.required_string("task").unwrap_err();
        match err {
            VmError::Runtime(msg) => {
                assert!(msg.contains("daemon_spawn:"));
                assert!(msg.contains("`task`"));
            }
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }

    #[test]
    fn parser_required_string_empty() {
        let d = dict(&[("task", VmValue::String(std::sync::Arc::from("   ")))]);
        let mut p = OptionsParser::new("daemon_spawn", &d, ErrorKind::Runtime);
        assert!(p.required_string("task").is_err());
    }

    #[test]
    fn parser_optional_string_trims() {
        let d = dict(&[("name", VmValue::String(std::sync::Arc::from("  joe  ")))]);
        let mut p = OptionsParser::new("agent", &d, ErrorKind::Runtime);
        assert_eq!(p.optional_string("name").unwrap().as_deref(), Some("joe"));
    }

    #[test]
    fn parser_optional_string_raw_preserves_whitespace() {
        let d = dict(&[("prompt", VmValue::String(std::sync::Arc::from("  >  ")))]);
        let mut p = OptionsParser::new("std/io.read_line", &d, ErrorKind::Runtime);
        assert_eq!(
            p.optional_string_raw("prompt").unwrap().as_deref(),
            Some("  >  ")
        );
    }

    #[test]
    fn parser_optional_string_raw_accepts_empty_string() {
        let d = dict(&[("prompt", VmValue::String(std::sync::Arc::from("")))]);
        let mut p = OptionsParser::new("std/io.read_line", &d, ErrorKind::Runtime);
        assert_eq!(
            p.optional_string_raw("prompt").unwrap().as_deref(),
            Some("")
        );
    }

    #[test]
    fn parser_optional_bool_default() {
        let d = dict(&[]);
        let mut p = OptionsParser::new("agent", &d, ErrorKind::Runtime);
        assert!(!p.bool_or("wait", false).unwrap());
        let d2 = dict(&[("wait", VmValue::Bool(true))]);
        let mut p2 = OptionsParser::new("agent", &d2, ErrorKind::Runtime);
        assert!(p2.bool_or("wait", false).unwrap());
    }

    #[test]
    fn parser_thrown_kind_wraps_in_thrown_string() {
        let d = dict(&[]);
        let mut p = OptionsParser::new("agent_session_open", &d, ErrorKind::Thrown);
        let err = p.required_string("id").unwrap_err();
        match err {
            VmError::Thrown(VmValue::String(s)) => assert!(s.contains("agent_session_open:")),
            other => panic!("expected Thrown(String), got {other:?}"),
        }
    }

    #[test]
    fn parser_finish_strict_rejects_unknown() {
        let d = dict(&[
            ("name", VmValue::String(std::sync::Arc::from("a"))),
            ("typo_key", VmValue::Bool(true)),
        ]);
        let mut p = OptionsParser::new("agent", &d, ErrorKind::Runtime);
        p.optional_string("name").unwrap();
        let err = p.finish_strict(&[]).unwrap_err();
        match err {
            VmError::Runtime(msg) => assert!(msg.contains("typo_key"), "got: {msg}"),
            other => panic!("expected Runtime, got {other:?}"),
        }
    }

    #[test]
    fn parser_finish_strict_allows_extra_known() {
        let d = dict(&[("forwarded", VmValue::Bool(true))]);
        let p = OptionsParser::new("daemon_spawn", &d, ErrorKind::Runtime);
        p.finish_strict(&["forwarded"]).unwrap();
    }

    #[test]
    fn duration_coercion_rejects_infinite_float() {
        let err = non_negative_millis_from_value(
            &VmValue::Float(f64::INFINITY),
            "waitpoint_wait",
            "timeout",
            ErrorKind::Runtime,
        )
        .unwrap_err();
        match err {
            VmError::Runtime(msg) => assert!(msg.contains("finite non-negative"), "{msg}"),
            other => panic!("expected Runtime, got {other:?}"),
        }
    }

    #[test]
    fn duration_coercion_accepts_finite_float_millis() {
        let millis = non_negative_millis_from_value(
            &VmValue::Float(42.9),
            "waitpoint_wait",
            "timeout",
            ErrorKind::Runtime,
        )
        .unwrap();
        assert_eq!(millis, 42);
    }

    #[test]
    fn dict_arg_extracts() {
        let v = VmValue::Dict(std::sync::Arc::new(dict(&[("a", VmValue::Bool(true))])));
        let args = vec![v];
        let got = dict_arg(&args, 0, "fn", "config", ErrorKind::Runtime).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn dict_arg_rejects_non_dict() {
        let args = vec![VmValue::Int(3)];
        let err = dict_arg(&args, 0, "fn", "config", ErrorKind::Runtime).unwrap_err();
        assert!(matches!(err, VmError::Runtime(_)));
    }
}
