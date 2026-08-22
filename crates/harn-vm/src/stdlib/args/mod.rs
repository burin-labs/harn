//! The argument contract shared by stdlib builtins.
//!
//! A builtin receives `&[VmValue]` and has to turn it into typed Rust values,
//! rejecting anything that does not fit. That is the same job in every
//! builtin, and before this module each family did it by hand: the stdlib
//! carried well over three hundred one-off `*_arg` / `*_option` helpers, and
//! the same mistake produced four different sentences depending on which one
//! you happened to hit —
//!
//! ```text
//! crypto:     jwt_sign: alg must be a string, got int
//! git:        git_log: path must be a non-empty string, got int
//! bytes:      bytes_slice: expected string at argument 1, got int
//! connectors: connector_call: name is required
//! ```
//!
//! Some of those helpers were not just inconsistent but wrong: several
//! "string" arguments were read with `VmValue::display()`, which stringifies a
//! dict rather than rejecting it, so a mistyped call reached the network
//! instead of the type error.
//!
//! [`Args`] is the one owner. It reads positional arguments, [`Options`]
//! reads dict option bags, and both phrase failures through [`ArgError`] using
//! the [`Expected`] vocabulary, which is built from canonical
//! [`TypeTag`]s rather than free text. The messages a Harn author sees are
//! therefore one shape, and the type names in them are the names `type_of`
//! returns.
//!
//! ```ignore
//! let args = Args::new("jwt_sign", args);
//! let algorithm = args.string(0, "alg")?;      // &str, borrowed
//! let claims = args.dict(1, "claims")?;        // &DictMap
//! let key = args.string(2, "private_key")?;
//! ```
//!
//! Accessors borrow from the argument slice instead of allocating, so the
//! success path of a builtin costs no `String` per argument.

mod options;
mod tag;

#[cfg(test)]
mod drift_tests;
#[cfg(test)]
mod tests;

use std::time::Duration as StdDuration;

use crate::value::{DictMap, VmClosure, VmError, VmValue};

pub(crate) use options::Options;
#[cfg(test)]
pub(crate) use tag::tag_is_canonical;
pub(crate) use tag::{Expected, TypeTag};

/// Whether an argument failure bubbles as a runtime error or as a value the
/// script can `try` / `recover`.
///
/// Most builtins use [`ErrorKind::TypeError`] for a wrong type and let it
/// bubble. Builtins whose failures are part of their normal control flow —
/// sessions, connectors, HTTP — use [`ErrorKind::Thrown`] so scripts can
/// catch them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    Runtime,
    TypeError,
    Thrown,
}

impl ErrorKind {
    pub(crate) fn err(self, message: impl Into<String>) -> VmError {
        match self {
            Self::Runtime => VmError::Runtime(message.into()),
            Self::TypeError => VmError::TypeError(message.into()),
            Self::Thrown => VmError::Thrown(VmValue::string(message.into())),
        }
    }
}

/// Build a `{fn_name}: {message}` error of the requested kind.
pub(crate) fn fn_err(fn_name: &str, kind: ErrorKind, message: impl std::fmt::Display) -> VmError {
    kind.err(format!("{fn_name}: {message}"))
}

/// The failure sentences a builtin can produce about one argument.
///
/// Constructing errors through this type rather than `format!` at the call
/// site is what keeps the wording uniform; it is also what lets the
/// vocabulary drift test know where to look.
pub(crate) struct ArgError;

impl ArgError {
    /// `{fn}: `{name}` is required`
    pub(crate) fn required(fn_name: &str, kind: ErrorKind, name: &str) -> VmError {
        fn_err(fn_name, kind, format_args!("`{name}` is required"))
    }

    /// ``{fn}: `{name}` must be a string, got int``
    pub(crate) fn wrong_type(
        fn_name: &str,
        kind: ErrorKind,
        name: &str,
        expected: Expected,
        got: &VmValue,
    ) -> VmError {
        fn_err(
            fn_name,
            kind,
            format_args!(
                "`{name}` must be {expected}, got {}",
                crate::stdlib::args::describe(got)
            ),
        )
    }

    /// ``{fn}: `{name}` must be a string or nil, got int``
    pub(crate) fn wrong_type_optional(
        fn_name: &str,
        kind: ErrorKind,
        name: &str,
        expected: Expected,
        got: &VmValue,
    ) -> VmError {
        fn_err(
            fn_name,
            kind,
            format_args!(
                "`{name}` must be {expected} or nil, got {}",
                crate::stdlib::args::describe(got)
            ),
        )
    }

    /// ``{fn}: `{name}` must not be empty``
    pub(crate) fn empty(fn_name: &str, kind: ErrorKind, name: &str) -> VmError {
        fn_err(fn_name, kind, format_args!("`{name}` must not be empty"))
    }

    /// ``{fn}: `{name}` must be one of `a`, `b`; got `c` ``
    pub(crate) fn not_one_of(
        fn_name: &str,
        kind: ErrorKind,
        name: &str,
        allowed: &[&str],
        got: &str,
    ) -> VmError {
        let allowed = allowed
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ");
        fn_err(
            fn_name,
            kind,
            format_args!("`{name}` must be one of {allowed}; got `{got}`"),
        )
    }

    /// ``{fn}: `{name}` {constraint}`` — for range and shape rules a type
    /// alone cannot express, e.g. `must be >= 0`.
    pub(crate) fn constraint(
        fn_name: &str,
        kind: ErrorKind,
        name: &str,
        constraint: impl std::fmt::Display,
    ) -> VmError {
        fn_err(fn_name, kind, format_args!("`{name}` {constraint}"))
    }
}

/// The type name to show for a value that failed a check.
///
/// This is `VmValue::type_name` for every ordinary value; it exists as a seam
/// so the "got …" half of a message can never be spelled by hand.
fn describe(value: &VmValue) -> &'static str {
    value.type_name()
}

/// Positional-argument reader for one builtin call.
///
/// Cheap to construct (two words plus a slice reference), so build one at the
/// top of a builtin and read through it rather than indexing `args` directly.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Args<'a> {
    fn_name: &'a str,
    values: &'a [VmValue],
    kind: ErrorKind,
}

impl<'a> Args<'a> {
    /// A reader whose failures bubble as `VmError::TypeError`.
    pub(crate) fn new(fn_name: &'a str, values: &'a [VmValue]) -> Self {
        Self {
            fn_name,
            values,
            kind: ErrorKind::TypeError,
        }
    }

    /// A reader whose failures are catchable by `try` / `recover`.
    pub(crate) fn thrown(fn_name: &'a str, values: &'a [VmValue]) -> Self {
        Self {
            fn_name,
            values,
            kind: ErrorKind::Thrown,
        }
    }

    /// A reader whose failures bubble as `VmError::Runtime`.
    pub(crate) fn runtime(fn_name: &'a str, values: &'a [VmValue]) -> Self {
        Self {
            fn_name,
            values,
            kind: ErrorKind::Runtime,
        }
    }

    /// An [`Options`] reader over a bag already in hand, for the parsers
    /// that receive `Option<&DictMap>` rather than the raw argument slice.
    pub(crate) fn runtime_options(fn_name: &'a str, dict: Option<&'a DictMap>) -> Options<'a> {
        Options::new(fn_name, ErrorKind::Runtime, dict)
    }

    /// Read one value already in hand as if it were argument 0.
    ///
    /// Some builtins pull a value out of a dict or an event before checking
    /// it. They get the same vocabulary as a positional read rather than a
    /// parallel set of value-level helpers.
    pub(crate) fn single(fn_name: &'a str, kind: ErrorKind, value: &'a VmValue) -> Self {
        Self {
            fn_name,
            values: std::slice::from_ref(value),
            kind,
        }
    }

    pub(crate) fn fn_name(&self) -> &'a str {
        self.fn_name
    }

    pub(crate) fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    /// Build an error in this reader's kind, prefixed with the builtin name.
    pub(crate) fn err(&self, message: impl std::fmt::Display) -> VmError {
        fn_err(self.fn_name, self.kind, message)
    }

    /// The raw value at `index`, treating `Nil` as absent.
    pub(crate) fn get(&self, index: usize) -> Option<&'a VmValue> {
        match self.values.get(index) {
            None | Some(VmValue::Nil) => None,
            Some(value) => Some(value),
        }
    }

    /// The raw value at `index`, distinguishing an explicit `Nil` from a
    /// missing argument. Only needed by builtins where `nil` means something.
    pub(crate) fn raw(&self, index: usize) -> Option<&'a VmValue> {
        self.values.get(index)
    }

    /// Reject a call outside `min..=max` arguments before reading anything.
    pub(crate) fn arity(&self, min: usize, max: usize) -> Result<(), VmError> {
        let count = self.values.len();
        if count >= min && count <= max {
            return Ok(());
        }
        let expected = if min == max {
            format!("{min}")
        } else {
            format!("{min}-{max}")
        };
        Err(self.err(format_args!("expected {expected} argument(s), got {count}")))
    }

    fn required_at(&self, index: usize, name: &str) -> Result<&'a VmValue, VmError> {
        self.get(index)
            .ok_or_else(|| ArgError::required(self.fn_name, self.kind, name))
    }

    fn wrong(&self, name: &str, expected: Expected, got: &VmValue) -> VmError {
        ArgError::wrong_type(self.fn_name, self.kind, name, expected, got)
    }

    fn wrong_optional(&self, name: &str, expected: Expected, got: &VmValue) -> VmError {
        ArgError::wrong_type_optional(self.fn_name, self.kind, name, expected, got)
    }

    // ---- strings ----------------------------------------------------------

    /// A required string, borrowed. Empty strings are allowed; use
    /// [`Args::non_empty_string`] when they are not.
    pub(crate) fn string(&self, index: usize, name: &str) -> Result<&'a str, VmError> {
        match self.required_at(index, name)? {
            VmValue::String(text) => Ok(text.as_str()),
            other => Err(self.wrong(name, Expected::STRING, other)),
        }
    }

    /// A required string that must have non-whitespace content. The returned
    /// slice is trimmed.
    pub(crate) fn non_empty_string(&self, index: usize, name: &str) -> Result<&'a str, VmError> {
        let text = self.string(index, name)?.trim();
        if text.is_empty() {
            return Err(ArgError::empty(self.fn_name, self.kind, name));
        }
        Ok(text)
    }

    /// An optional string. Missing and `nil` both read as `None`.
    pub(crate) fn opt_string(&self, index: usize, name: &str) -> Result<Option<&'a str>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::String(text)) => Ok(Some(text.as_str())),
            Some(other) => Err(self.wrong_optional(name, Expected::STRING, other)),
        }
    }

    /// An optional string, trimmed, where a whitespace-only value means
    /// "not supplied" rather than "supplied as empty".
    pub(crate) fn opt_non_empty_string(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<&'a str>, VmError> {
        Ok(self
            .opt_string(index, name)?
            .map(str::trim)
            .filter(|text| !text.is_empty()))
    }

    /// A required string restricted to a closed set of spellings.
    pub(crate) fn enum_string(
        &self,
        index: usize,
        name: &str,
        allowed: &[&str],
    ) -> Result<&'a str, VmError> {
        let text = self.string(index, name)?;
        if allowed.contains(&text) {
            return Ok(text);
        }
        Err(ArgError::not_one_of(
            self.fn_name,
            self.kind,
            name,
            allowed,
            text,
        ))
    }

    // ---- numbers ----------------------------------------------------------

    /// A required int. Floats are rejected; use [`Args::number`] where a
    /// float is genuinely acceptable.
    pub(crate) fn int(&self, index: usize, name: &str) -> Result<i64, VmError> {
        match self.required_at(index, name)? {
            VmValue::Int(value) => Ok(*value),
            other => Err(self.wrong(name, Expected::INT, other)),
        }
    }

    pub(crate) fn opt_int(&self, index: usize, name: &str) -> Result<Option<i64>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong_optional(name, Expected::INT, other)),
        }
    }

    /// A required non-negative int, narrowed to `usize`.
    pub(crate) fn usize(&self, index: usize, name: &str) -> Result<usize, VmError> {
        let value = self.int(index, name)?;
        usize::try_from(value)
            .map_err(|_| ArgError::constraint(self.fn_name, self.kind, name, "must be >= 0"))
    }

    pub(crate) fn opt_usize(&self, index: usize, name: &str) -> Result<Option<usize>, VmError> {
        let Some(value) = self.opt_int(index, name)? else {
            return Ok(None);
        };
        usize::try_from(value)
            .map(Some)
            .map_err(|_| ArgError::constraint(self.fn_name, self.kind, name, "must be >= 0"))
    }

    /// A required number, accepting an int or a float and yielding `f64`.
    pub(crate) fn number(&self, index: usize, name: &str) -> Result<f64, VmError> {
        match self.required_at(index, name)? {
            VmValue::Int(value) => Ok(*value as f64),
            VmValue::Float(value) => Ok(*value),
            other => Err(self.wrong(name, Expected::INT_OR_FLOAT, other)),
        }
    }

    pub(crate) fn opt_number(&self, index: usize, name: &str) -> Result<Option<f64>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::Int(value)) => Ok(Some(*value as f64)),
            Some(VmValue::Float(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong_optional(name, Expected::INT_OR_FLOAT, other)),
        }
    }

    // ---- bools ------------------------------------------------------------

    pub(crate) fn bool(&self, index: usize, name: &str) -> Result<bool, VmError> {
        match self.required_at(index, name)? {
            VmValue::Bool(value) => Ok(*value),
            other => Err(self.wrong(name, Expected::BOOL, other)),
        }
    }

    pub(crate) fn opt_bool(&self, index: usize, name: &str) -> Result<Option<bool>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::Bool(value)) => Ok(Some(*value)),
            Some(other) => Err(self.wrong_optional(name, Expected::BOOL, other)),
        }
    }

    pub(crate) fn bool_or(&self, index: usize, name: &str, default: bool) -> Result<bool, VmError> {
        Ok(self.opt_bool(index, name)?.unwrap_or(default))
    }

    // ---- containers -------------------------------------------------------

    pub(crate) fn dict(&self, index: usize, name: &str) -> Result<&'a DictMap, VmError> {
        match self.required_at(index, name)? {
            VmValue::Dict(dict) => Ok(dict.as_ref()),
            other => Err(self.wrong(name, Expected::DICT, other)),
        }
    }

    pub(crate) fn opt_dict(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<&'a DictMap>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::Dict(dict)) => Ok(Some(dict.as_ref())),
            Some(other) => Err(self.wrong_optional(name, Expected::DICT, other)),
        }
    }

    pub(crate) fn list(&self, index: usize, name: &str) -> Result<&'a [VmValue], VmError> {
        match self.required_at(index, name)? {
            VmValue::List(list) => Ok(list.as_slice()),
            other => Err(self.wrong(name, Expected::LIST, other)),
        }
    }

    pub(crate) fn opt_list(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<&'a [VmValue]>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::List(list)) => Ok(Some(list.as_slice())),
            Some(other) => Err(self.wrong_optional(name, Expected::LIST, other)),
        }
    }

    /// A required list whose every element is a string.
    ///
    /// The element type is checked here rather than by the caller, so a
    /// `["a", 3]` argument fails at the boundary with the element's own type
    /// named instead of silently stringifying.
    pub(crate) fn string_list(&self, index: usize, name: &str) -> Result<Vec<&'a str>, VmError> {
        self.collect_string_list(self.list(index, name)?, name)
    }

    pub(crate) fn opt_string_list(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<Vec<&'a str>>, VmError> {
        let Some(list) = self.opt_list(index, name)? else {
            return Ok(None);
        };
        self.collect_string_list(list, name).map(Some)
    }

    fn collect_string_list(
        &self,
        list: &'a [VmValue],
        name: &str,
    ) -> Result<Vec<&'a str>, VmError> {
        list.iter()
            .map(|value| match value {
                VmValue::String(text) => Ok(text.as_str()),
                other => Err(self.wrong(name, Expected::STRING_LIST, other)),
            })
            .collect()
    }

    // ---- bytes ------------------------------------------------------------

    pub(crate) fn bytes(&self, index: usize, name: &str) -> Result<&'a [u8], VmError> {
        match self.required_at(index, name)? {
            VmValue::Bytes(bytes) => Ok(bytes.as_slice()),
            other => Err(self.wrong(name, Expected::BYTES, other)),
        }
    }

    /// Bytes, or a string taken as its UTF-8 bytes.
    pub(crate) fn bytes_or_string(&self, index: usize, name: &str) -> Result<&'a [u8], VmError> {
        match self.required_at(index, name)? {
            VmValue::Bytes(bytes) => Ok(bytes.as_slice()),
            VmValue::String(text) => Ok(text.as_bytes()),
            other => Err(self.wrong(name, Expected::BYTES_OR_STRING, other)),
        }
    }

    // ---- closures ---------------------------------------------------------

    pub(crate) fn closure(&self, index: usize, name: &str) -> Result<&'a VmClosure, VmError> {
        match self.required_at(index, name)? {
            VmValue::Closure(closure) => Ok(closure.as_ref()),
            other => Err(self.wrong(name, Expected::CLOSURE, other)),
        }
    }

    pub(crate) fn opt_closure(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<&'a VmClosure>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(VmValue::Closure(closure)) => Ok(Some(closure.as_ref())),
            Some(other) => Err(self.wrong_optional(name, Expected::CLOSURE, other)),
        }
    }

    // ---- durations --------------------------------------------------------

    /// A non-negative millisecond count, from a `duration`, an int, or a
    /// finite float.
    ///
    /// Waitpoints, monitors, HITL, and the storage connectors all accept this
    /// trio; keeping the edge cases (negative, infinite, out-of-range float)
    /// in one place is why they agree.
    pub(crate) fn millis(&self, index: usize, name: &str) -> Result<u64, VmError> {
        let value = self.required_at(index, name)?;
        self.millis_from(value, name)
    }

    pub(crate) fn opt_millis(&self, index: usize, name: &str) -> Result<Option<u64>, VmError> {
        match self.get(index) {
            None => Ok(None),
            Some(value) => self.millis_from(value, name).map(Some),
        }
    }

    pub(crate) fn duration(&self, index: usize, name: &str) -> Result<StdDuration, VmError> {
        self.millis(index, name).map(StdDuration::from_millis)
    }

    pub(crate) fn opt_duration(
        &self,
        index: usize,
        name: &str,
    ) -> Result<Option<StdDuration>, VmError> {
        Ok(self.opt_millis(index, name)?.map(StdDuration::from_millis))
    }

    fn millis_from(&self, value: &VmValue, name: &str) -> Result<u64, VmError> {
        match value {
            VmValue::Duration(millis) | VmValue::Int(millis) if *millis >= 0 => Ok(*millis as u64),
            VmValue::Duration(_) | VmValue::Int(_) => Err(ArgError::constraint(
                self.fn_name,
                self.kind,
                name,
                "must be >= 0",
            )),
            VmValue::Float(millis)
                if millis.is_finite() && *millis >= 0.0 && *millis <= u64::MAX as f64 =>
            {
                Ok(*millis as u64)
            }
            VmValue::Float(_) => Err(ArgError::constraint(
                self.fn_name,
                self.kind,
                name,
                "must be a finite millisecond count >= 0",
            )),
            other => Err(self.wrong(name, Expected::DURATION_OR_INT, other)),
        }
    }

    // ---- json -------------------------------------------------------------

    /// A dict argument, converted to a JSON object. Missing or `nil` yields
    /// an empty object, which is what the observability and timing builtins
    /// want from an absent attribute bag.
    pub(crate) fn json_object(
        &self,
        index: usize,
        name: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
        let Some(value) = self.get(index) else {
            return Ok(serde_json::Map::new());
        };
        match value {
            VmValue::Dict(_) => match crate::llm::helpers::vm_value_to_json(value) {
                serde_json::Value::Object(map) => Ok(map),
                other => unreachable!("a dict converts to a JSON object, got {other:?}"),
            },
            other => Err(self.wrong_optional(name, Expected::DICT, other)),
        }
    }

    // ---- option bags ------------------------------------------------------

    /// Read a trailing option-bag argument. A missing or `nil` argument
    /// yields an empty bag, so callers need no separate absent case.
    pub(crate) fn options(&self, index: usize, name: &str) -> Result<Options<'a>, VmError> {
        Ok(Options::new(
            self.fn_name,
            self.kind,
            self.opt_dict(index, name)?,
        ))
    }
}
