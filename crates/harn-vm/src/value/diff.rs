//! Structural, path-addressed comparison of two values.
//!
//! This is the engine behind `assert_eq` / `assert_ne` / `value_diff`. Where
//! `values_equal` answers "are these the same?", this answers "*where* are they
//! different, and how?" — which is the question an author actually has when an
//! assertion fails at 2am.
//!
//! Two decisions shape everything here:
//!
//! * **Address, don't dump.** A mismatch three levels inside a dict is reported
//!   as `.user.roles[1]`, not by printing both whole values and leaving the
//!   reader to compare them by eye. Only the leaves that actually differ are
//!   rendered.
//! * **Use the types.** Values carry their type at runtime, so a diff can say
//!   `1 (int)` vs `"1" (string)` instead of showing two identical-looking
//!   glyphs. Text-based differs cannot do this; we can, so we do.
//!
//! Rendering is deterministic: dict keys iterate in sorted order and sets are
//! sorted by their rendered form, so failure output is stable across runs and
//! safe to assert on byte-for-byte.

use std::fmt::Write;

use super::core::{struct_fields_to_map, VmValue};
use super::recursion::guard_recursion;
use super::structural::values_equal;

/// The most differing leaves reported before the diff summarizes the rest.
/// Past this, a wall of text stops being a diff and starts being a haystack.
const MAX_DIFFERENCES: usize = 10;

/// The longest a single rendered value may be before it is abbreviated. Chosen
/// so a value still fits comfortably on one terminal line alongside its label.
const MAX_LEAF_CHARS: usize = 120;

/// How the two sides disagree at one path.
#[derive(Debug, Clone)]
pub enum DifferenceKind {
    /// Both sides have a value here, and the values are not equal.
    Unequal { actual: VmValue, expected: VmValue },
    /// Only the actual value has anything here: an extra dict key, list item,
    /// or set member.
    Unexpected { actual: VmValue },
    /// Only the expected value has anything here: a dict key, list item, or set
    /// member the actual value is missing.
    Missing { expected: VmValue },
}

/// One place where two values disagree.
#[derive(Debug, Clone)]
pub struct ValueDifference {
    /// Path from the root of the compared values, in Harn access syntax:
    /// `.user.name`, `.items[2]`, `.headers["content-type"]`. Empty at the root.
    pub path: String,
    pub kind: DifferenceKind,
}

/// Every place `actual` and `expected` disagree, deepest-addressable first.
///
/// Returns an empty vec exactly when `values_equal(actual, expected)` is true —
/// the two are kept in lockstep by [`tests::diff_is_empty_iff_values_equal`].
pub fn diff_values(actual: &VmValue, expected: &VmValue) -> Vec<ValueDifference> {
    let mut out = Vec::new();
    walk(String::new(), actual, expected, &mut out);
    out
}

fn walk(path: String, actual: &VmValue, expected: &VmValue, out: &mut Vec<ValueDifference>) {
    if values_equal(actual, expected) {
        return;
    }
    match (actual, expected) {
        (VmValue::Dict(a), VmValue::Dict(e)) => {
            guard_recursion(|| {
                // Both maps iterate sorted, so merging them by key is both
                // linear and deterministic.
                let mut keys: Vec<&str> = a.keys().map(|k| k.as_str()).collect();
                keys.extend(e.keys().map(|k| k.as_str()));
                keys.sort_unstable();
                keys.dedup();
                for key in keys {
                    let child = format!("{path}{}", render_key_step(key));
                    match (a.get(key), e.get(key)) {
                        (Some(av), Some(ev)) => walk(child, av, ev, out),
                        (Some(av), None) => out.push(ValueDifference {
                            path: child,
                            kind: DifferenceKind::Unexpected { actual: av.clone() },
                        }),
                        (None, Some(ev)) => out.push(ValueDifference {
                            path: child,
                            kind: DifferenceKind::Missing {
                                expected: ev.clone(),
                            },
                        }),
                        (None, None) => {}
                    }
                }
            });
        }
        (VmValue::List(a), VmValue::List(e)) => {
            guard_recursion(|| walk_sequence(&path, a, e, out));
        }
        (VmValue::StructInstance(a), VmValue::StructInstance(e))
            if a.layout.struct_name() == e.layout.struct_name() =>
        {
            guard_recursion(|| {
                let a_fields = struct_fields_to_map(&a.layout, &a.fields);
                let e_fields = struct_fields_to_map(&e.layout, &e.fields);
                let mut keys: Vec<&str> = a_fields.keys().map(|k| k.as_str()).collect();
                keys.extend(e_fields.keys().map(|k| k.as_str()));
                keys.sort_unstable();
                keys.dedup();
                for key in keys {
                    let child = format!("{path}{}", render_key_step(key));
                    match (a_fields.get(key), e_fields.get(key)) {
                        (Some(av), Some(ev)) => walk(child, av, ev, out),
                        (Some(av), None) => out.push(ValueDifference {
                            path: child,
                            kind: DifferenceKind::Unexpected { actual: av.clone() },
                        }),
                        (None, Some(ev)) => out.push(ValueDifference {
                            path: child,
                            kind: DifferenceKind::Missing {
                                expected: ev.clone(),
                            },
                        }),
                        (None, None) => {}
                    }
                }
            });
        }
        (VmValue::EnumVariant(a), VmValue::EnumVariant(e))
            if a.enum_name == e.enum_name
                && a.variant == e.variant
                && a.fields.len() == e.fields.len() =>
        {
            // Same enum and variant, differing payload. The payload of a
            // variant is reached as `value.fields[i]` in Harn, so that is what
            // the path says — a bare `[i]` here would read as a list index into
            // something that is not a list.
            guard_recursion(|| {
                walk_sequence(&format!("{path}.fields"), &a.fields, &e.fields, out);
            });
        }
        (VmValue::Set(a), VmValue::Set(e)) => {
            // Sets are unordered, so an index-wise walk would report noise.
            // Report membership only, sorted by rendering for determinism.
            let mut extra: Vec<&VmValue> = a.iter().filter(|v| !e.contains(v)).collect();
            let mut absent: Vec<&VmValue> = e.iter().filter(|v| !a.contains(v)).collect();
            extra.sort_by_cached_key(|v| repr(v));
            absent.sort_by_cached_key(|v| repr(v));
            for value in extra {
                out.push(ValueDifference {
                    path: format!("{path}{{{}}}", repr(value)),
                    kind: DifferenceKind::Unexpected {
                        actual: value.clone(),
                    },
                });
            }
            for value in absent {
                out.push(ValueDifference {
                    path: format!("{path}{{{}}}", repr(value)),
                    kind: DifferenceKind::Missing {
                        expected: value.clone(),
                    },
                });
            }
        }
        // Two values of unrelated shape (or scalars): the disagreement is here,
        // whole. Recursing into a dict against a list would invent a
        // correspondence that does not exist.
        _ => out.push(ValueDifference {
            path,
            kind: DifferenceKind::Unequal {
                actual: actual.clone(),
                expected: expected.clone(),
            },
        }),
    }
}

fn walk_sequence(path: &str, a: &[VmValue], e: &[VmValue], out: &mut Vec<ValueDifference>) {
    for index in 0..a.len().max(e.len()) {
        let child = format!("{path}[{index}]");
        match (a.get(index), e.get(index)) {
            (Some(av), Some(ev)) => walk(child, av, ev, out),
            (Some(av), None) => out.push(ValueDifference {
                path: child,
                kind: DifferenceKind::Unexpected { actual: av.clone() },
            }),
            (None, Some(ev)) => out.push(ValueDifference {
                path: child,
                kind: DifferenceKind::Missing {
                    expected: ev.clone(),
                },
            }),
            (None, None) => {}
        }
    }
}

/// A dict/struct key as a path step: `.name` when it is a plain identifier,
/// `["odd key"]` otherwise — so the path is always something the reader can
/// paste back into their program.
fn render_key_step(key: &str) -> String {
    let plain = !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        format!(".{key}")
    } else {
        format!("[{}]", quote_string(key))
    }
}

// -------------------------------------------------------------------------------------------------
// Rendering
// -------------------------------------------------------------------------------------------------

/// An unambiguous rendering of `value`, in Harn literal syntax where one
/// exists.
///
/// This is deliberately not `display()`: `display()` prints a string without
/// quotes, which makes `1` and `"1"` render identically — the single worst
/// property a value can have in assertion output.
pub fn repr(value: &VmValue) -> String {
    let mut out = String::new();
    write_repr(value, &mut out);
    out
}

fn write_repr(value: &VmValue, out: &mut String) {
    match value {
        VmValue::String(s) => out.push_str(&quote_string(s)),
        VmValue::List(items) => {
            out.push('[');
            guard_recursion(|| {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_repr(item, out);
                }
            });
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('{');
            guard_recursion(|| {
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&quote_string(k));
                    out.push_str(": ");
                    write_repr(v, out);
                }
            });
            out.push('}');
        }
        VmValue::Set(members) => {
            let mut rendered: Vec<String> = members.iter().map(repr).collect();
            rendered.sort();
            let _ = write!(out, "set([{}])", rendered.join(", "));
        }
        VmValue::StructInstance(data) => {
            let _ = write!(out, "{} {{", data.layout.struct_name());
            guard_recursion(|| {
                for (i, (k, v)) in struct_fields_to_map(&data.layout, &data.fields)
                    .iter()
                    .enumerate()
                {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{k}: ");
                    write_repr(v, out);
                }
            });
            out.push('}');
        }
        VmValue::EnumVariant(variant) => {
            let _ = write!(out, "{}::{}", variant.enum_name, variant.variant);
            if !variant.fields.is_empty() {
                out.push('(');
                guard_recursion(|| {
                    for (i, field) in variant.fields.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        write_repr(field, out);
                    }
                });
                out.push(')');
            }
        }
        // Everything else already renders unambiguously.
        other => other.write_display(out),
    }
}

fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `repr`, abbreviated in the middle if it is too long to read. Keeping both
/// ends matters: a long string usually differs at one end, and a trailing
/// ellipsis alone would hide it.
fn repr_abbreviated(value: &VmValue) -> String {
    let full = repr(value);
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= MAX_LEAF_CHARS {
        return full;
    }
    let keep = MAX_LEAF_CHARS / 2 - 6;
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("{head} … {tail}   ({} characters in all)", chars.len())
}

/// Render one side of a difference, tagging its type when the two sides'
/// types disagree — that is precisely when the values may look identical but
/// are not (`1` vs `"1"`, `1` vs `1.0`).
fn render_side(value: &VmValue, counterpart: Option<&VmValue>) -> String {
    let rendered = repr_abbreviated(value);
    match counterpart {
        Some(other) if other.type_name() != value.type_name() => {
            format!("{rendered} ({})", value.type_name())
        }
        _ => rendered,
    }
}

/// A one-line nudge for the mistakes a bare value comparison cannot explain by
/// itself. Absent when there is nothing useful to add — an unconditional hint
/// is noise, and noise is what makes people stop reading failure output.
fn hint_for(actual: &VmValue, expected: &VmValue) -> Option<String> {
    match (actual, expected) {
        (VmValue::Float(a), VmValue::Float(e)) => {
            let gap = (a - e).abs();
            if gap == 0.0 || !gap.is_finite() {
                return None;
            }
            Some(format!(
                "These differ by {gap:e}. Floating-point arithmetic is inexact, so exact \
                 equality on computed floats is usually a bug in the test, not the code — \
                 compare with a tolerance using assert_approx."
            ))
        }
        (VmValue::String(a), VmValue::String(e)) => {
            let index = a
                .chars()
                .zip(e.chars())
                .position(|(x, y)| x != y)
                .unwrap_or_else(|| a.chars().count().min(e.chars().count()));
            if a.chars().count() != e.chars().count()
                && index == a.chars().count().min(e.chars().count())
            {
                Some(format!(
                    "The first {index} characters match; the strings differ in length \
                     ({} vs {}).",
                    a.chars().count(),
                    e.chars().count()
                ))
            } else {
                Some(format!("The strings first differ at character {index}."))
            }
        }
        (VmValue::Int(_), VmValue::String(_)) | (VmValue::String(_), VmValue::Int(_)) => Some(
            "One side is a number and the other is text. If this came from parsed input, \
             the conversion may be missing."
                .to_string(),
        ),
        _ => None,
    }
}

/// The addressed, per-leaf diff naming what differs between the two values and
/// where. `None` when they are equal.
///
/// `headline` is the caller's framing (e.g. `Some("assert_eq failed")`), or
/// `None` to render the diff on its own for a caller that supplies its own
/// context.
pub fn render_diff(headline: Option<&str>, actual: &VmValue, expected: &VmValue) -> Option<String> {
    let differences = diff_values(actual, expected);
    if differences.is_empty() {
        return None;
    }
    let mut out = String::new();

    // The common case — one whole-value mismatch — deserves the plainest
    // possible rendering. Path headers and difference counts would be
    // ceremony around two lines of substance.
    let root_only = differences.len() == 1 && differences[0].path.is_empty();
    match (headline, root_only) {
        (Some(headline), true) => {
            let _ = writeln!(out, "{headline}.");
        }
        (Some(headline), false) => {
            let _ = writeln!(
                out,
                "{headline}: the two values differ in {}.\n",
                plural(differences.len(), "place", "places")
            );
        }
        (None, true) => {}
        (None, false) => {
            let _ = writeln!(
                out,
                "The two values differ in {}.\n",
                plural(differences.len(), "place", "places")
            );
        }
    }

    for difference in differences.iter().take(MAX_DIFFERENCES) {
        if !difference.path.is_empty() {
            let _ = writeln!(out, "  at {}", difference.path);
        }
        match &difference.kind {
            DifferenceKind::Unequal { actual, expected } => {
                let _ = writeln!(out, "    expected  {}", render_side(expected, Some(actual)));
                let _ = writeln!(out, "    actual    {}", render_side(actual, Some(expected)));
                if let Some(hint) = hint_for(actual, expected) {
                    let _ = writeln!(out, "    {hint}");
                }
            }
            DifferenceKind::Unexpected { actual } => {
                let _ = writeln!(out, "    expected  nothing here");
                let _ = writeln!(out, "    actual    {}", repr_abbreviated(actual));
            }
            DifferenceKind::Missing { expected } => {
                let _ = writeln!(out, "    expected  {}", repr_abbreviated(expected));
                let _ = writeln!(out, "    actual    nothing here");
            }
        }
        if !root_only {
            out.push('\n');
        }
    }

    if differences.len() > MAX_DIFFERENCES {
        let suppressed = differences.len() - MAX_DIFFERENCES;
        let _ = writeln!(
            out,
            "  … and {suppressed} more {}.",
            if suppressed == 1 {
                "difference"
            } else {
                "differences"
            }
        );
    }

    Some(out.trim_end().to_string())
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

#[cfg(test)]
mod tests;
