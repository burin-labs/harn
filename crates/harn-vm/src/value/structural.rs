use std::rc::Rc;
use std::sync::atomic::Ordering;

use super::VmValue;

/// Reference / identity equality. For heap-allocated refcounted values
/// (List/Dict/Set/Closure) returns true only when both operands share the
/// same underlying `Rc` allocation. For primitive scalars, falls back to
/// structural equality (since primitives have no distinct identity).
pub fn values_identical(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::List(x), VmValue::List(y)) => Rc::ptr_eq(x, y),
        (VmValue::Dict(x), VmValue::Dict(y)) => Rc::ptr_eq(x, y),
        (VmValue::Set(x), VmValue::Set(y)) => Rc::ptr_eq(x, y),
        (VmValue::Closure(x), VmValue::Closure(y)) => Rc::ptr_eq(x, y),
        (VmValue::String(x), VmValue::String(y)) => Rc::ptr_eq(x, y) || x == y,
        (VmValue::Bytes(x), VmValue::Bytes(y)) => Rc::ptr_eq(x, y) || x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::BuiltinRefId { name: x, .. }, VmValue::BuiltinRefId { name: y, .. }) => x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRefId { name: y, .. })
        | (VmValue::BuiltinRefId { name: y, .. }, VmValue::BuiltinRef(x)) => x == y,
        (VmValue::Pair(x), VmValue::Pair(y)) => Rc::ptr_eq(x, y),
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
        VmValue::List(x) => format!("list@{:p}", Rc::as_ptr(x)),
        VmValue::Dict(x) => format!("dict@{:p}", Rc::as_ptr(x)),
        VmValue::Set(x) => format!("set@{:p}", Rc::as_ptr(x)),
        VmValue::Closure(x) => format!("closure@{:p}", Rc::as_ptr(x)),
        VmValue::String(x) => format!("string@{:p}", x.as_ptr()),
        VmValue::Bytes(x) => format!("bytes@{:p}", Rc::as_ptr(x)),
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
        (
            VmValue::EnumVariant {
                enum_name: a_e,
                variant: a_v,
                fields: a_f,
            },
            VmValue::EnumVariant {
                enum_name: b_e,
                variant: b_v,
                fields: b_f,
            },
        ) => {
            a_e == b_e
                && a_v == b_v
                && a_f.len() == b_f.len()
                && a_f.iter().zip(b_f.iter()).all(|(x, y)| values_equal(x, y))
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
        (VmValue::Iter(a), VmValue::Iter(b)) => Rc::ptr_eq(a, b),
        (VmValue::Pair(a), VmValue::Pair(b)) => {
            values_equal(&a.0, &b.0) && values_equal(&a.1, &b.1)
        }
        _ => false,
    }
}

pub fn compare_values(a: &VmValue, b: &VmValue) -> i32 {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x.cmp(y) as i32,
        (VmValue::Float(x), VmValue::Float(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        (VmValue::Int(x), VmValue::Float(y)) => {
            let x = *x as f64;
            if x < *y {
                -1
            } else if x > *y {
                1
            } else {
                0
            }
        }
        (VmValue::Float(x), VmValue::Int(y)) => {
            let y = *y as f64;
            if *x < y {
                -1
            } else if *x > y {
                1
            } else {
                0
            }
        }
        (VmValue::String(x), VmValue::String(y)) => x.cmp(y) as i32,
        (VmValue::Pair(x), VmValue::Pair(y)) => {
            let c = compare_values(&x.0, &y.0);
            if c != 0 {
                c
            } else {
                compare_values(&x.1, &y.1)
            }
        }
        _ => 0,
    }
}
