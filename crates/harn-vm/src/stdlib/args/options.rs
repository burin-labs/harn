//! Reader for dict option bags, phrased through the same vocabulary as
//! positional arguments.
//!
//! An option bag is the trailing `{ ... }` argument most stdlib builtins
//! accept. [`Options`] reads keys out of it, records which keys it consumed,
//! and — for closed schemas — rejects the ones nobody read, which is what
//! turns a typo like `{ timout: 5 }` into an error instead of a silently
//! ignored option.
//!
//! An absent bag is not a special case: [`Options::new`] takes
//! `Option<&DictMap>` and an absent bag simply has no keys, so a builtin
//! reads `options.opt_int("limit")?` the same way whether the caller passed a
//! bag or not.

use std::collections::BTreeSet;
use std::time::Duration as StdDuration;

use crate::value::{DictMap, VmError, VmValue};

use super::tag::Expected;
use super::{ArgError, ErrorKind};

/// Schema-driven reader for one option bag.
#[derive(Debug)]
pub(crate) struct Options<'a> {
    fn_name: &'a str,
    kind: ErrorKind,
    dict: Option<&'a DictMap>,
    seen: BTreeSet<&'static str>,
}

impl<'a> Options<'a> {
    pub(crate) fn new(fn_name: &'a str, kind: ErrorKind, dict: Option<&'a DictMap>) -> Self {
        Self {
            fn_name,
            kind,
            dict,
            seen: BTreeSet::new(),
        }
    }

    /// True when the caller passed no bag at all, or an empty one.
    pub(crate) fn is_empty(&self) -> bool {
        self.dict.is_none_or(DictMap::is_empty)
    }

    /// The underlying dict, for the few builtins that forward the whole bag.
    pub(crate) fn dict(&self) -> Option<&'a DictMap> {
        self.dict
    }

    fn lookup(&mut self, key: &'static str) -> Option<&'a VmValue> {
        self.seen.insert(key);
        match self.dict?.get(key) {
            None | Some(VmValue::Nil) => None,
            Some(value) => Some(value),
        }
    }

    /// Mark `key` consumed without reading it, so [`Options::finish`] does
    /// not report it as unknown.
    pub(crate) fn allow(&mut self, key: &'static str) {
        self.seen.insert(key);
    }

    /// The raw value for `key`, marking it consumed.
    pub(crate) fn raw(&mut self, key: &'static str) -> Option<&'a VmValue> {
        self.lookup(key)
    }

    pub(crate) fn err(&self, message: impl std::fmt::Display) -> VmError {
        super::fn_err(self.fn_name, self.kind, message)
    }

    fn wrong(&self, key: &str, expected: Expected, got: &VmValue) -> VmError {
        ArgError::wrong_type_optional(self.fn_name, self.kind, key, expected, got)
    }

    // ---- strings ----------------------------------------------------------

    /// A required option. Absent or `nil` is an error.
    pub(crate) fn string(&mut self, key: &'static str) -> Result<&'a str, VmError> {
        match self.lookup(key) {
            Some(VmValue::String(text)) => Ok(text.as_str()),
            Some(other) => Err(self.wrong(key, Expected::STRING, other)),
            None => Err(ArgError::required(self.fn_name, self.kind, key)),
        }
    }

    /// A required option that must have non-whitespace content, trimmed.
    pub(crate) fn non_empty_string(&mut self, key: &'static str) -> Result<&'a str, VmError> {
        let text = self.string(key)?.trim();
        if text.is_empty() {
            return Err(ArgError::empty(self.fn_name, self.kind, key));
        }
        Ok(text)
    }

    pub(crate) fn opt_string(&mut self, key: &'static str) -> Result<Option<&'a str>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::String(text)) => Ok(Some(text.as_str())),
            Some(other) => Err(self.wrong(key, Expected::STRING, other)),
        }
    }

    /// An optional option where a whitespace-only value reads as absent.
    pub(crate) fn opt_non_empty_string(
        &mut self,
        key: &'static str,
    ) -> Result<Option<&'a str>, VmError> {
        Ok(self
            .opt_string(key)?
            .map(str::trim)
            .filter(|text| !text.is_empty()))
    }

    pub(crate) fn string_or(
        &mut self,
        key: &'static str,
        default: &'a str,
    ) -> Result<&'a str, VmError> {
        Ok(self.opt_string(key)?.unwrap_or(default))
    }

    /// An option restricted to a closed set of spellings.
    pub(crate) fn opt_enum_string(
        &mut self,
        key: &'static str,
        allowed: &[&str],
    ) -> Result<Option<&'a str>, VmError> {
        let Some(text) = self.opt_string(key)? else {
            return Ok(None);
        };
        if allowed.contains(&text) {
            return Ok(Some(text));
        }
        Err(ArgError::not_one_of(
            self.fn_name,
            self.kind,
            key,
            allowed,
            text,
        ))
    }

    // ---- numbers ----------------------------------------------------------

    pub(crate) fn opt_int(&mut self, key: &'static str) -> Result<Option<i64>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong(key, Expected::INT, other)),
        }
    }

    pub(crate) fn int_or(&mut self, key: &'static str, default: i64) -> Result<i64, VmError> {
        Ok(self.opt_int(key)?.unwrap_or(default))
    }

    pub(crate) fn opt_usize(&mut self, key: &'static str) -> Result<Option<usize>, VmError> {
        let Some(value) = self.opt_int(key)? else {
            return Ok(None);
        };
        usize::try_from(value)
            .map(Some)
            .map_err(|_| ArgError::constraint(self.fn_name, self.kind, key, "must be >= 0"))
    }

    pub(crate) fn opt_number(&mut self, key: &'static str) -> Result<Option<f64>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value as f64)),
            Some(VmValue::Float(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong(key, Expected::INT_OR_FLOAT, other)),
        }
    }

    // ---- bools ------------------------------------------------------------

    pub(crate) fn opt_bool(&mut self, key: &'static str) -> Result<Option<bool>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Bool(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong(key, Expected::BOOL, other)),
        }
    }

    pub(crate) fn bool_or(&mut self, key: &'static str, default: bool) -> Result<bool, VmError> {
        Ok(self.opt_bool(key)?.unwrap_or(default))
    }

    // ---- containers -------------------------------------------------------

    pub(crate) fn opt_list(&mut self, key: &'static str) -> Result<Option<&'a [VmValue]>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::List(list)) => Ok(Some(list.as_slice())),
            Some(other) => Err(self.wrong(key, Expected::LIST, other)),
        }
    }

    pub(crate) fn opt_dict(&mut self, key: &'static str) -> Result<Option<&'a DictMap>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Dict(dict)) => Ok(Some(dict.as_ref())),
            Some(other) => Err(self.wrong(key, Expected::DICT, other)),
        }
    }

    /// A nested option bag, read with the same reader type.
    pub(crate) fn opt_options(&mut self, key: &'static str) -> Result<Options<'a>, VmError> {
        Ok(Options::new(self.fn_name, self.kind, self.opt_dict(key)?))
    }

    pub(crate) fn opt_string_list(
        &mut self,
        key: &'static str,
    ) -> Result<Option<Vec<&'a str>>, VmError> {
        let Some(list) = self.opt_list(key)? else {
            return Ok(None);
        };
        list.iter()
            .map(|value| match value {
                VmValue::String(text) => Ok(text.as_str()),
                other => Err(ArgError::wrong_type(
                    self.fn_name,
                    self.kind,
                    key,
                    Expected::STRING_LIST,
                    other,
                )),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    pub(crate) fn opt_closure(
        &mut self,
        key: &'static str,
    ) -> Result<Option<&'a crate::value::VmClosure>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Closure(closure)) => Ok(Some(closure.as_ref())),
            Some(other) => Err(self.wrong(key, Expected::CLOSURE, other)),
        }
    }

    // ---- durations --------------------------------------------------------

    pub(crate) fn opt_millis(&mut self, key: &'static str) -> Result<Option<u64>, VmError> {
        let (fn_name, kind) = (self.fn_name, self.kind);
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Duration(millis) | VmValue::Int(millis)) if *millis >= 0 => {
                Ok(Some(*millis as u64))
            }
            Some(VmValue::Duration(_) | VmValue::Int(_)) => {
                Err(ArgError::constraint(fn_name, kind, key, "must be >= 0"))
            }
            Some(VmValue::Float(millis))
                if millis.is_finite() && *millis >= 0.0 && *millis <= u64::MAX as f64 =>
            {
                Ok(Some(*millis as u64))
            }
            Some(VmValue::Float(_)) => Err(ArgError::constraint(
                fn_name,
                kind,
                key,
                "must be a finite millisecond count >= 0",
            )),
            Some(other) => Err(ArgError::wrong_type_optional(
                fn_name,
                kind,
                key,
                Expected::DURATION_OR_INT,
                other,
            )),
        }
    }

    pub(crate) fn opt_duration(
        &mut self,
        key: &'static str,
    ) -> Result<Option<StdDuration>, VmError> {
        Ok(self.opt_millis(key)?.map(StdDuration::from_millis))
    }

    // ---- closing ----------------------------------------------------------

    /// Reject keys nobody read. Call this on a closed schema so a misspelled
    /// option fails loudly instead of being ignored.
    ///
    /// `forwarded` names keys this builtin deliberately hands to another
    /// layer without reading.
    pub(crate) fn finish(self, forwarded: &[&str]) -> Result<(), VmError> {
        let Some(dict) = self.dict else {
            return Ok(());
        };
        let mut unknown: Vec<&str> = dict
            .keys()
            .map(arcstr::ArcStr::as_str)
            .filter(|key| !self.seen.contains(key) && !forwarded.contains(key))
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort_unstable();
        Err(super::fn_err(
            self.fn_name,
            self.kind,
            format_args!("unknown option(s): {}", unknown.join(", ")),
        ))
    }
}
