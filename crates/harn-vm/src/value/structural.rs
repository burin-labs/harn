use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::VmValue;

/// Reference / identity equality. For heap-allocated refcounted values
/// (List/Dict/Set/Closure) returns true only when both operands share the
/// same underlying shared allocation. For primitive scalars, falls back to
/// structural equality (since primitives have no distinct identity).
pub fn values_identical(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::List(x), VmValue::List(y)) => Arc::ptr_eq(x, y),
        (VmValue::Dict(x), VmValue::Dict(y)) => Arc::ptr_eq(x, y),
        (VmValue::Set(x), VmValue::Set(y)) => Arc::ptr_eq(x, y),
        (VmValue::Closure(x), VmValue::Closure(y)) => Arc::ptr_eq(x, y),
        (VmValue::String(x), VmValue::String(y)) => Arc::ptr_eq(x, y) || x == y,
        (VmValue::Bytes(x), VmValue::Bytes(y)) => Arc::ptr_eq(x, y) || x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::BuiltinRefId { name: x, .. }, VmValue::BuiltinRefId { name: y, .. }) => x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRefId { name: y, .. })
        | (VmValue::BuiltinRefId { name: y, .. }, VmValue::BuiltinRef(x)) => x == y,
        (VmValue::Pair(x), VmValue::Pair(y)) => Arc::ptr_eq(x, y),
        // Primitives: identity collapses to structural equality.
        _ => values_equal(a, b),
    }
}

/// Stable identity key for a value. Different allocations produce different
/// keys; two values with the same heap identity produce the same key. For
/// primitives the key is derived from the displayed value plus type name so
/// logically-equal primitives always compare equal.
pub fn value_identity_key(v: &VmValue) -> String {
    match v {
        VmValue::List(x) => format!("list@{:p}", Arc::as_ptr(x)),
        VmValue::Dict(x) => format!("dict@{:p}", Arc::as_ptr(x)),
        VmValue::Set(x) => format!("set@{:p}", Arc::as_ptr(x)),
        VmValue::Closure(x) => format!("closure@{:p}", Arc::as_ptr(x)),
        VmValue::String(x) => format!("string@{:p}", x.as_ptr()),
        VmValue::Bytes(x) => format!("bytes@{:p}", Arc::as_ptr(x)),
        VmValue::BuiltinRef(name) => format!("builtin@{name}"),
        VmValue::BuiltinRefId { name, .. } => format!("builtin@{name}"),
        other => format!("{}@{}", other.type_name(), other.display()),
    }
}

/// Canonical string form used as the keying material for `hash_value`.
/// Different types never collide (the type name is prepended) and collection
/// order is preserved so structurally-equal values always produce the same
/// key. Not intended for cross-process stability; depends on the in-process
/// iteration order for collections (Dict uses BTreeMap so keys are sorted).
pub fn value_structural_hash_key(v: &VmValue) -> String {
    let mut out = String::new();
    write_structural_hash_key(v, &mut out);
    out
}

/// Writes the structural hash key for a value directly into `out`,
/// avoiding intermediate allocations. Uses length-prefixed encoding
/// for strings and dict keys to prevent separator collisions.
fn write_structural_hash_key(v: &VmValue, out: &mut String) {
    match v {
        VmValue::Nil => out.push('N'),
        VmValue::Bool(b) => {
            out.push(if *b { 'T' } else { 'F' });
        }
        VmValue::Int(n) => {
            out.push('i');
            out.push_str(&n.to_string());
            out.push(';');
        }
        VmValue::Float(n) => {
            out.push('f');
            out.push_str(&n.to_bits().to_string());
            out.push(';');
        }
        VmValue::String(s) => {
            // Length-prefixed: s<len>:<content> — no ambiguity from content
            out.push('s');
            out.push_str(&s.len().to_string());
            out.push(':');
            out.push_str(s);
        }
        VmValue::Bytes(bytes) => {
            out.push('b');
            for byte in bytes.iter() {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push(';');
        }
        VmValue::Duration(ms) => {
            out.push('d');
            out.push_str(&ms.to_string());
            out.push(';');
        }
        VmValue::List(items) => {
            out.push('L');
            for item in items.iter() {
                write_structural_hash_key(item, out);
                out.push(',');
            }
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('D');
            for (k, v) in map.iter() {
                // Length-prefixed key
                out.push_str(&k.len().to_string());
                out.push(':');
                out.push_str(k);
                out.push('=');
                write_structural_hash_key(v, out);
                out.push(',');
            }
            out.push('}');
        }
        VmValue::Set(items) => {
            // Sets need sorted keys for order-independence
            let mut keys: Vec<String> = items.iter().map(value_structural_hash_key).collect();
            keys.sort();
            out.push('S');
            for k in &keys {
                out.push_str(k);
                out.push(',');
            }
            out.push('}');
        }
        other => {
            let tn = other.type_name();
            out.push('o');
            out.push_str(&tn.len().to_string());
            out.push(':');
            out.push_str(tn);
            let d = other.display();
            out.push_str(&d.len().to_string());
            out.push(':');
            out.push_str(&d);
        }
    }
}

pub fn values_equal(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        (VmValue::String(x), VmValue::String(y)) => x == y,
        (VmValue::Bytes(x), VmValue::Bytes(y)) => x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::BuiltinRefId { name: x, .. }, VmValue::BuiltinRefId { name: y, .. }) => x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRefId { name: y, .. })
        | (VmValue::BuiltinRefId { name: y, .. }, VmValue::BuiltinRef(x)) => x == y,
        (VmValue::Bool(x), VmValue::Bool(y)) => x == y,
        (VmValue::Nil, VmValue::Nil) => true,
        (VmValue::Int(x), VmValue::Float(y)) => (*x as f64) == *y,
        (VmValue::Float(x), VmValue::Int(y)) => *x == (*y as f64),
        (VmValue::TaskHandle(a), VmValue::TaskHandle(b)) => a == b,
        (VmValue::Channel(_), VmValue::Channel(_)) => false, // channels are never equal
        (VmValue::Rng(_), VmValue::Rng(_)) => false,
        (VmValue::SyncPermit(_), VmValue::SyncPermit(_)) => false,
        (VmValue::Atomic(a), VmValue::Atomic(b)) => {
            a.value.load(Ordering::SeqCst) == b.value.load(Ordering::SeqCst)
        }
        (VmValue::List(a), VmValue::List(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (VmValue::Dict(a), VmValue::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
        }
        (VmValue::EnumVariant(a), VmValue::EnumVariant(b)) => {
            a.enum_name == b.enum_name
                && a.variant == b.variant
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|(x, y)| values_equal(x, y))
        }
        (
            VmValue::StructInstance {
                layout: a_layout,
                fields: a_fields,
            },
            VmValue::StructInstance {
                layout: b_layout,
                fields: b_fields,
            },
        ) => {
            if a_layout.struct_name() != b_layout.struct_name() {
                return false;
            }
            let a_map = super::struct_fields_to_map(a_layout, a_fields);
            let b_map = super::struct_fields_to_map(b_layout, b_fields);
            a_map.len() == b_map.len()
                && a_map
                    .iter()
                    .zip(b_map.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
        }
        (VmValue::Set(a), VmValue::Set(b)) => {
            a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| values_equal(x, y)))
        }
        (VmValue::Generator(_), VmValue::Generator(_)) => false, // generators are never equal
        (VmValue::Stream(_), VmValue::Stream(_)) => false,       // streams are never equal
        (VmValue::Range(a), VmValue::Range(b)) => {
            a.start == b.start && a.end == b.end && a.inclusive == b.inclusive
        }
        (VmValue::Iter(a), VmValue::Iter(b)) => Arc::ptr_eq(a, b),
        (VmValue::Pair(a), VmValue::Pair(b)) => {
            values_equal(&a.0, &b.0) && values_equal(&a.1, &b.1)
        }
        // Harness handles carry runtime capability state, not values. Two
        // handles that refer to the same backing capability are still
        // observed-distinct because the script never compares them. Returning
        // `false` matches `Channel` / `Generator` / `Stream` precedent.
        (VmValue::Harness(_), VmValue::Harness(_)) => false,
        _ => false,
    }
}

/// Total-order comparison used for sorting, `min`/`max`, and similar reductions.
///
/// IEEE-754 NaN is *unordered*, so [`try_compare_values`] returns `None` for it;
/// here we fall back to `0` (treat as equal) so a stray NaN does not destabilize
/// a sort. Relational operators (`<`, `>`, `<=`, `>=`) must NOT use this fallback —
/// they go through [`try_compare_values`] so that any comparison with NaN yields
/// `false`, as the language spec and IEEE-754 require.
pub fn compare_values(a: &VmValue, b: &VmValue) -> i32 {
    try_compare_values(a, b).unwrap_or(0)
}

/// Ordered comparison for relational operators. Returns `None` when the two
/// values are *unordered* — i.e. a floating-point NaN is involved (directly, via
/// an int/float mix, or nested inside a pair). Callers implementing `<`, `>`,
/// `<=`, `>=` must treat `None` as "comparison is false".
pub fn try_compare_values(a: &VmValue, b: &VmValue) -> Option<i32> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Some(x.cmp(y) as i32),
        (VmValue::Float(x), VmValue::Float(y)) => float_ordering(*x, *y),
        (VmValue::Int(x), VmValue::Float(y)) => float_ordering(*x as f64, *y),
        (VmValue::Float(x), VmValue::Int(y)) => float_ordering(*x, *y as f64),
        (VmValue::String(x), VmValue::String(y)) => Some(x.cmp(y) as i32),
        (VmValue::Pair(x), VmValue::Pair(y)) => {
            let c = try_compare_values(&x.0, &y.0)?;
            if c != 0 {
                Some(c)
            } else {
                try_compare_values(&x.1, &y.1)
            }
        }
        _ => Some(0),
    }
}

fn float_ordering(x: f64, y: f64) -> Option<i32> {
    x.partial_cmp(&y).map(|ord| ord as i32)
}
