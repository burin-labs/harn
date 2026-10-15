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

use crate::value::{DictMap, VmError, VmValue};

use super::tag::Expected;
use super::{ArgError, ErrorKind};

/// Schema-driven reader for one option bag.
#[derive(Debug)]
pub(crate) struct Options<'name, 'a> {
    fn_name: &'name str,
    kind: ErrorKind,
    dict: Option<&'a DictMap>,
    seen: BTreeSet<&'static str>,
}

impl<'name, 'a> Options<'name, 'a> {
    pub(crate) fn new(fn_name: &'name str, kind: ErrorKind, dict: Option<&'a DictMap>) -> Self {
        Self {
            fn_name,
            kind,
            dict,
            seen: BTreeSet::new(),
        }
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

    /// The first present key among `keys`, with every one of them marked
    /// consumed.
    ///
    /// Several option bags accept more than one spelling of the same option
    /// (`max_filesize` / `max_file_size`, `include_hidden` / `hidden`). The
    /// alternatives are declared here rather than chained with `or_else` at
    /// the call site, so `finish` knows about all of them and a wrong type
    /// under any spelling is still an error.
    fn lookup_any(&mut self, keys: &[&'static str]) -> Option<(&'static str, &'a VmValue)> {
        let mut found = None;
        for key in keys {
            let value = self.lookup(key);
            if found.is_none() {
                if let Some(value) = value {
                    found = Some((*key, value));
                }
            }
        }
        found
    }

    pub(crate) fn opt_string_any(
        &mut self,
        keys: &[&'static str],
    ) -> Result<Option<&'a str>, VmError> {
        match self.lookup_any(keys) {
            None => Ok(None),
            Some((_, VmValue::String(text))) => Ok(Some(text.as_str())),
            Some((key, other)) => Err(self.wrong(key, Expected::STRING, other)),
        }
    }

    pub(crate) fn opt_bool_any(&mut self, keys: &[&'static str]) -> Result<Option<bool>, VmError> {
        match self.lookup_any(keys) {
            None => Ok(None),
            Some((_, VmValue::Bool(value))) => Ok(Some(*value)),
            Some((key, other)) => Err(self.wrong(key, Expected::BOOL, other)),
        }
    }

    pub(crate) fn opt_int_any(&mut self, keys: &[&'static str]) -> Result<Option<i64>, VmError> {
        match self.lookup_any(keys) {
            None => Ok(None),
            Some((_, VmValue::Int(value))) => Ok(Some(*value)),
            Some((key, other)) => Err(self.wrong(key, Expected::INT, other)),
        }
    }

    /// A `string | list<string>` option, flattened. One pattern or several.
    pub(crate) fn opt_string_or_list(
        &mut self,
        key: &'static str,
    ) -> Result<Vec<&'a str>, VmError> {
        match self.lookup(key) {
            None => Ok(Vec::new()),
            Some(VmValue::String(text)) => Ok(vec![text.as_str()]),
            Some(VmValue::List(items)) => items
                .iter()
                .map(|value| match value {
                    VmValue::String(text) => Ok(text.as_str()),
                    other => Err(self.wrong(key, Expected::STRING_LIST, other)),
                })
                .collect(),
            Some(other) => Err(self.wrong(key, Expected::STRING_LIST, other)),
        }
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

    // ---- numbers ----------------------------------------------------------

    pub(crate) fn opt_int(&mut self, key: &'static str) -> Result<Option<i64>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong(key, Expected::INT, other)),
        }
    }

    /// An int option, also accepting a whole-valued float. See
    /// [`Args::whole_int`](super::Args::whole_int) for why.
    pub(crate) fn opt_whole_int(&mut self, key: &'static str) -> Result<Option<i64>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value)),
            Some(VmValue::Float(value)) if value.fract() == 0.0 => Ok(Some(*value as i64)),
            Some(other) => Err(self.wrong(key, Expected::INT_OR_FLOAT, other)),
        }
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

    pub(crate) fn opt_usize(&mut self, key: &'static str) -> Result<Option<usize>, VmError> {
        let Some(value) = self.opt_int(key)? else {
            return Ok(None);
        };
        usize::try_from(value)
            .map(Some)
            .map_err(|_| ArgError::constraint(self.fn_name, self.kind, key, "must be >= 0"))
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
                other => Err(self.wrong(key, Expected::STRING_LIST, other)),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    pub(crate) fn opt_dict(&mut self, key: &'static str) -> Result<Option<&'a DictMap>, VmError> {
        match self.lookup(key) {
            None => Ok(None),
            Some(VmValue::Dict(dict)) => Ok(Some(dict.as_ref())),
            Some(other) => Err(self.wrong(key, Expected::DICT, other)),
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
    ) -> Result<Option<std::time::Duration>, VmError> {
        Ok(self.opt_millis(key)?.map(std::time::Duration::from_millis))
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
