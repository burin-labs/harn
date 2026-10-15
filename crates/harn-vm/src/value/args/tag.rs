//! The type vocabulary builtin diagnostics are allowed to speak.
//!
//! Every "must be a …" message a builtin shows a Harn author names a runtime
//! type. Those names have to be the ones `type_of(x)` returns, or the author
//! reads `must be an integer` and writes `type_of(n) == "integer"`, which is
//! never true. Spelling them by hand let `integer`, `boolean`, `number`,
//! `record`, and `object` into user-facing errors alongside the real tags.
//!
//! [`TypeTag`] closes that: a diagnostic names a type by picking a variant,
//! and [`tag_is_canonical`] (asserted over every variant in this module's
//! tests) keeps the variants pinned to
//! [`harn_builtin_meta::runtime_type_tags::ALL`] — the same list
//! `VmValue::type_name` and the typechecker's `type_of` narrowing agree on.

use std::fmt;

/// A runtime type, named the way `type_of` names it.
///
/// Only the tags builtins actually demand of an argument are listed. Add a
/// variant when a builtin needs to require a type not here yet; the
/// canonical-tag test will reject a spelling the runtime does not produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeTag {
    String,
    Bytes,
    Int,
    Float,
    Bool,
    List,
    Dict,
    Closure,
    Duration,
    Set,
}

impl TypeTag {
    /// The tag `type_of` returns for this type.
    pub(crate) const fn tag(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bytes => "bytes",
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::List => "list",
            Self::Dict => "dict",
            Self::Closure => "closure",
            Self::Duration => "duration",
            Self::Set => "set",
        }
    }

    /// Every variant, so the canonical-tag test cannot miss one.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::String,
        Self::Bytes,
        Self::Int,
        Self::Float,
        Self::Bool,
        Self::List,
        Self::Dict,
        Self::Closure,
        Self::Duration,
        Self::Set,
    ];
}

impl fmt::Display for TypeTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.tag())
    }
}

/// Whether `tag` is a string `type_of` can actually return.
#[cfg(test)]
pub(crate) fn tag_is_canonical(tag: &str) -> bool {
    harn_builtin_meta::runtime_type_tags::ALL.contains(&tag)
}

/// What an argument is allowed to be.
///
/// Composed from [`TypeTag`]s rather than free text so a union or element
/// type cannot smuggle in a non-canonical spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expected {
    /// Exactly one type: `must be a dict`.
    One(TypeTag),
    /// Either of two: `must be bytes or a string`.
    Either(TypeTag, TypeTag),
    /// A homogeneous list: `must be a list<string>`.
    ListOf(TypeTag),
}

impl Expected {
    pub(crate) const STRING: Self = Self::One(TypeTag::String);
    pub(crate) const BYTES: Self = Self::One(TypeTag::Bytes);
    pub(crate) const INT: Self = Self::One(TypeTag::Int);
    pub(crate) const BOOL: Self = Self::One(TypeTag::Bool);
    pub(crate) const FLOAT: Self = Self::One(TypeTag::Float);
    pub(crate) const LIST: Self = Self::One(TypeTag::List);
    pub(crate) const DICT: Self = Self::One(TypeTag::Dict);
    pub(crate) const CLOSURE: Self = Self::One(TypeTag::Closure);
    pub(crate) const INT_OR_FLOAT: Self = Self::Either(TypeTag::Int, TypeTag::Float);
    pub(crate) const STRING_LIST: Self = Self::ListOf(TypeTag::String);
    pub(crate) const LIST_OR_SET: Self = Self::Either(TypeTag::List, TypeTag::Set);
    pub(crate) const BYTES_OR_STRING: Self = Self::Either(TypeTag::Bytes, TypeTag::String);
    pub(crate) const DURATION_OR_INT: Self = Self::Either(TypeTag::Duration, TypeTag::Int);
}

/// `a` or `an`, by the tag's first letter. Written out rather than pulled
/// from a crate because the vocabulary is closed and ASCII.
const fn article(tag: &str) -> &'static str {
    match tag.as_bytes().first() {
        Some(b'a' | b'e' | b'i' | b'o' | b'u') => "an",
        _ => "a",
    }
}

impl fmt::Display for Expected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::One(tag) => write!(formatter, "{} {tag}", article(tag.tag())),
            // `bytes` is a mass noun; the others take an article. Writing
            // "a bytes or a string" reads worse than dropping both, so the
            // union form drops the article on the left only when it is
            // `bytes`, which is the only mass-noun tag in the vocabulary.
            Self::Either(TypeTag::Bytes, right) => {
                write!(formatter, "bytes or {} {right}", article(right.tag()))
            }
            Self::Either(left, right) => write!(
                formatter,
                "{} {left} or {} {right}",
                article(left.tag()),
                article(right.tag())
            ),
            Self::ListOf(element) => write!(formatter, "a list<{element}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_names_a_tag_type_of_returns() {
        for tag in TypeTag::ALL {
            assert!(
                tag_is_canonical(tag.tag()),
                "TypeTag::{tag:?} spells `{}`, which `type_of` never returns; \
                 use a tag from harn_builtin_meta::runtime_type_tags::ALL",
                tag.tag()
            );
        }
    }

    #[test]
    fn expected_reads_as_english() {
        assert_eq!(Expected::STRING.to_string(), "a string");
        assert_eq!(Expected::INT.to_string(), "an int");
        assert_eq!(Expected::STRING_LIST.to_string(), "a list<string>");
        assert_eq!(Expected::BYTES_OR_STRING.to_string(), "bytes or a string");
        assert_eq!(
            Expected::DURATION_OR_INT.to_string(),
            "a duration or an int"
        );
    }
}
