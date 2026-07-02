use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::recursion::guard_recursion;
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
        (VmValue::String(x), VmValue::String(y)) => arcstr::ArcStr::ptr_eq(x, y) || x == y,
        (VmValue::Bytes(x), VmValue::Bytes(y)) => Arc::ptr_eq(x, y) || x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::BuiltinRefId(x), VmValue::BuiltinRefId(y)) => x.name == y.name,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRefId(y))
        | (VmValue::BuiltinRefId(y), VmValue::BuiltinRef(x)) => x.as_str() == y.name.as_str(),
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
        VmValue::BuiltinRefId(r) => format!("builtin@{}", r.name),
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
        VmValue::Int(n) => write_int_hash_key(*n, out),
        VmValue::Float(n) => {
            // A float that is numerically an integer must hash identically to
            // that integer, because `values_equal` treats `1 == 1.0` (and
            // `0.0 == -0.0`) as equal — otherwise hash-keyed de-duplication
            // would disagree with `==`. Non-integral / non-finite floats keep
            // their bit pattern (so distinct NaNs may collide, which is fine:
            // dedup confirms every hash hit with `values_equal`).
            match float_as_equivalent_int(*n) {
                Some(i) => write_int_hash_key(i, out),
                None => {
                    out.push('f');
                    out.push_str(&n.to_bits().to_string());
                    out.push(';');
                }
            }
        }
        VmValue::String(s) => {
            // Length-prefixed: s<len>:<content> — no ambiguity from content
            out.push('s');
            write_len_prefixed(s, out);
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
        // Normalize the scale so equal decimals hash alike (`1.5` and `1.50`
        // are `values_equal`, so their keys must match). Decimal is its own
        // namespace ('M') — it never collides with Int/Float keys, mirroring
        // that `values_equal` treats Decimal as a distinct type from them.
        VmValue::Decimal(dec) => {
            out.push('M');
            out.push_str(&dec.normalize().to_string());
            out.push(';');
        }
        VmValue::List(items) => {
            out.push('L');
            guard_recursion(|| {
                for item in items.iter() {
                    write_structural_hash_key(item, out);
                    out.push(',');
                }
            });
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('D');
            guard_recursion(|| {
                for (k, v) in map.iter() {
                    write_len_prefixed(k, out);
                    out.push('=');
                    write_structural_hash_key(v, out);
                    out.push(',');
                }
            });
            out.push('}');
        }
        VmValue::Set(set) => {
            // Sets hash order-independently via their already-resident sorted
            // structural keys.
            out.push('S');
            for k in set.sorted_keys() {
                out.push_str(k);
                out.push(',');
            }
            out.push('}');
        }
        // Composite values that can contain numbers recurse through this
        // function (rather than the display fallback below) so numeric
        // normalization propagates — e.g. `Some(1)` and `Some(1.0)`, which
        // `values_equal` treats as equal, must hash alike.
        VmValue::Pair(pair) => {
            out.push('P');
            guard_recursion(|| {
                write_structural_hash_key(&pair.0, out);
                out.push(',');
                write_structural_hash_key(&pair.1, out);
            });
            out.push(';');
        }
        VmValue::EnumVariant(ev) => {
            out.push('E');
            write_len_prefixed(&ev.enum_name, out);
            write_len_prefixed(&ev.variant, out);
            guard_recursion(|| {
                for field in ev.fields.iter() {
                    write_structural_hash_key(field, out);
                    out.push(',');
                }
            });
            out.push(';');
        }
        VmValue::StructInstance(data) => {
            let (layout, fields) = (&data.layout, &data.fields);
            // Use the same name-keyed map `values_equal` compares, so field
            // order in the layout never affects the key.
            out.push('I');
            write_len_prefixed(layout.struct_name(), out);
            guard_recursion(|| {
                for (k, v) in super::struct_fields_to_map(layout, fields) {
                    write_len_prefixed(&k, out);
                    out.push('=');
                    write_structural_hash_key(&v, out);
                    out.push(',');
                }
            });
            out.push('}');
        }
        other => {
            // Identity-only values (handles, channels, …) that `values_equal`
            // never reports equal. Keyed by type + display purely so distinct
            // ones rarely collide; dedup still confirms with `values_equal`.
            out.push('o');
            write_len_prefixed(other.type_name(), out);
            write_len_prefixed(&other.display(), out);
        }
    }
}

/// Length-prefixed `<len>:<content>` encoding, so variable-length content can
/// never be confused with surrounding structure (e.g. `"a,b"` vs `"a", "b"`).
fn write_len_prefixed(s: &str, out: &mut String) {
    out.push_str(&s.len().to_string());
    out.push(':');
    out.push_str(s);
}

/// Canonical integer hash-key encoding, shared by `Int` and by any `Float`
/// that is numerically an integer (see [`float_as_equivalent_int`]).
fn write_int_hash_key(n: i64, out: &mut String) {
    out.push('i');
    out.push_str(&n.to_string());
    out.push(';');
}

/// Returns `Some(i)` when `n` compares equal to the `i64` value `i` under the
/// same coercion `values_equal` uses for `Int`/`Float` (`(i as f64) == n`).
/// `None` for non-integral or non-finite floats (incl. NaN / ±inf). This is
/// the single source of truth that keeps the hash key consistent with `==`.
fn float_as_equivalent_int(n: f64) -> Option<i64> {
    let candidate = n as i64; // saturating, and NaN -> 0
    ((candidate as f64) == n).then_some(candidate)
}

pub fn values_equal(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        // Decimal is a distinct type for equality (a clean island): it compares
        // equal only to another Decimal, by value (rust_decimal's `==` ignores
        // scale, so `1.5 == 1.50`). It is deliberately NOT cross-type-equal to
        // Int/Float — that keeps equality transitive given the existing
        // `Int == Float` coercion, and avoids silently equating exact money to
        // lossy binary floats. Convert explicitly with `decimal(x)` to compare.
        (VmValue::Decimal(x), VmValue::Decimal(y)) => x == y,
        (VmValue::String(x), VmValue::String(y)) => x == y,
        (VmValue::Bytes(x), VmValue::Bytes(y)) => x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::BuiltinRefId(x), VmValue::BuiltinRefId(y)) => x.name == y.name,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRefId(y))
        | (VmValue::BuiltinRefId(y), VmValue::BuiltinRef(x)) => x.as_str() == y.name.as_str(),
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
            a.len() == b.len()
                && guard_recursion(|| a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)))
        }
        (VmValue::Dict(a), VmValue::Dict(b)) => {
            a.len() == b.len()
                && guard_recursion(|| {
                    a.iter()
                        .zip(b.iter())
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
                })
        }
        (VmValue::EnumVariant(a), VmValue::EnumVariant(b)) => {
            a.enum_name == b.enum_name
                && a.variant == b.variant
                && a.fields.len() == b.fields.len()
                && guard_recursion(|| {
                    a.fields
                        .iter()
                        .zip(b.fields.iter())
                        .all(|(x, y)| values_equal(x, y))
                })
        }
        (VmValue::StructInstance(a), VmValue::StructInstance(b)) => {
            let (a_layout, a_fields) = (&a.layout, &a.fields);
            let (b_layout, b_fields) = (&b.layout, &b.fields);
            if a_layout.struct_name() != b_layout.struct_name() {
                return false;
            }
            let a_map = super::struct_fields_to_map(a_layout, a_fields);
            let b_map = super::struct_fields_to_map(b_layout, b_fields);
            a_map.len() == b_map.len()
                && guard_recursion(|| {
                    a_map
                        .iter()
                        .zip(b_map.iter())
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
                })
        }
        (VmValue::Set(a), VmValue::Set(b)) => {
            // Order-independent: equal length plus every member of `a` present
            // in `b`, using `b`'s O(1) structural-key index.
            a.len() == b.len() && a.iter().all(|x| b.contains(x))
        }
        (VmValue::Generator(_), VmValue::Generator(_)) => false, // generators are never equal
        (VmValue::Stream(_), VmValue::Stream(_)) => false,       // streams are never equal
        (VmValue::Range(a), VmValue::Range(b)) => {
            a.start == b.start && a.end == b.end && a.inclusive == b.inclusive
        }
        (VmValue::Iter(a), VmValue::Iter(b)) => Arc::ptr_eq(a, b),
        (VmValue::Pair(a), VmValue::Pair(b)) => {
            guard_recursion(|| values_equal(&a.0, &b.0) && values_equal(&a.1, &b.1))
        }
        // Harness handles carry runtime capability state, not values. Two
        // handles that refer to the same backing capability are still
        // observed-distinct because the script never compares them. Returning
        // `false` matches `Channel` / `Generator` / `Stream` precedent.
        (VmValue::Harness(_), VmValue::Harness(_)) => false,
        _ => false,
    }
}

/// Structural de-duplication that honors `values_equal`, preserving
/// first-occurrence order.
///
/// Candidates are bucketed by [`value_structural_hash_key`] (so the common
/// case stays near-O(n)), then every hash hit is confirmed with
/// [`values_equal`]. Because the hash key is consistent with `values_equal`,
/// numerically-equal values that hash alike (e.g. `1` and `1.0`) collapse to a
/// single entry, while hash collisions between unequal values (e.g. two `NaN`s
/// sharing a bit pattern) never merge — matching the `==` operator exactly.
pub fn dedup_values<'a, I>(items: I) -> Vec<VmValue>
where
    I: IntoIterator<Item = &'a VmValue>,
{
    use std::collections::HashMap;
    let mut buckets: HashMap<String, Vec<VmValue>> = HashMap::new();
    let mut result = Vec::new();
    for item in items {
        let bucket = buckets.entry(value_structural_hash_key(item)).or_default();
        if !bucket.iter().any(|kept| values_equal(kept, item)) {
            bucket.push(item.clone());
            result.push(item.clone());
        }
    }
    result
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
/// an int/float mix, or nested inside a pair or list). Callers implementing
/// `<`, `>`, `<=`, `>=` must treat `None` as "comparison is false".
pub fn try_compare_values(a: &VmValue, b: &VmValue) -> Option<i32> {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => Some(x.cmp(y) as i32),
        (VmValue::Float(x), VmValue::Float(y)) => float_ordering(*x, *y),
        (VmValue::Int(x), VmValue::Float(y)) => float_ordering(*x as f64, *y),
        (VmValue::Float(x), VmValue::Int(y)) => float_ordering(*x, *y as f64),
        // Decimal orders only against Decimal (the same island rule as
        // `values_equal`); a Decimal-vs-Int/Float comparison is *unordered*
        // (`None`) so relational operators yield false rather than coercing
        // through a lossy float or the `Some(0)` "equal" fallback below. `cmp`
        // is exact and scale-insensitive.
        (VmValue::Decimal(x), VmValue::Decimal(y)) => Some(x.cmp(y) as i32),
        (VmValue::Decimal(_), VmValue::Int(_) | VmValue::Float(_))
        | (VmValue::Int(_) | VmValue::Float(_), VmValue::Decimal(_)) => None,
        (VmValue::String(x), VmValue::String(y)) => Some(x.cmp(y) as i32),
        (VmValue::Pair(x), VmValue::Pair(y)) => guard_recursion(|| {
            let c = try_compare_values(&x.0, &y.0)?;
            if c != 0 {
                Some(c)
            } else {
                try_compare_values(&x.1, &y.1)
            }
        }),
        // Lists order lexicographically, element by element, with a length
        // tiebreak (a strict prefix sorts first). This is what makes
        // multi-key sorts like `xs.sort_by({ x -> [x.a, x.b] })` work.
        // Unordered elements (NaN) propagate `None` exactly like `Pair`.
        (VmValue::List(x), VmValue::List(y)) => guard_recursion(|| {
            for (item_a, item_b) in x.iter().zip(y.iter()) {
                let c = try_compare_values(item_a, item_b)?;
                if c != 0 {
                    return Some(c);
                }
            }
            Some(x.len().cmp(&y.len()) as i32)
        }),
        _ => Some(0),
    }
}

fn float_ordering(x: f64, y: f64) -> Option<i32> {
    x.partial_cmp(&y).map(|ord| ord as i32)
}
