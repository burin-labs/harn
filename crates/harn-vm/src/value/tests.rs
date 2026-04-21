use std::rc::Rc;

use super::*;

#[cfg(target_pointer_width = "64")]
#[test]
fn vm_value_layout_budget() {
    assert_eq!(std::mem::size_of::<VmValue>(), 48);
    assert_eq!(std::mem::size_of::<Option<VmValue>>(), 48);
    assert_eq!(std::mem::size_of::<VmChannelHandle>(), 40);
    assert_eq!(std::mem::size_of::<VmAtomicHandle>(), 8);
    assert_eq!(std::mem::size_of::<VmRange>(), 24);
    assert_eq!(std::mem::size_of::<VmGenerator>(), 16);
}

fn s(val: &str) -> VmValue {
    VmValue::String(Rc::from(val))
}

fn i(val: i64) -> VmValue {
    VmValue::Int(val)
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

fn dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::Dict(Rc::new(
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    ))
}

#[test]
fn hash_key_cross_type_distinct() {
    // Int(1) vs String("1") vs Bool(true) must all differ
    let k_int = value_structural_hash_key(&i(1));
    let k_str = value_structural_hash_key(&s("1"));
    let k_bool = value_structural_hash_key(&VmValue::Bool(true));
    assert_ne!(k_int, k_str);
    assert_ne!(k_int, k_bool);
    assert_ne!(k_str, k_bool);
}

#[test]
fn hash_key_string_with_separator_chars() {
    // ["a,string:b"] (1-element list) vs ["a", "b"] (2-element list)
    let one_elem = list(vec![s("a,string:b")]);
    let two_elem = list(vec![s("a"), s("b")]);
    assert_ne!(
        value_structural_hash_key(&one_elem),
        value_structural_hash_key(&two_elem),
        "length-prefixed strings must prevent separator collisions"
    );
}

#[test]
fn hash_key_dict_key_with_equals() {
    // Dict with key "a=b" vs dict with key "a" and value containing "b"
    let d1 = dict(vec![("a=b", i(1))]);
    let d2 = dict(vec![("a", i(1))]);
    assert_ne!(
        value_structural_hash_key(&d1),
        value_structural_hash_key(&d2)
    );
}

#[test]
fn hash_key_nested_list_vs_flat() {
    // [[1]] vs [1]
    let nested = list(vec![list(vec![i(1)])]);
    let flat = list(vec![i(1)]);
    assert_ne!(
        value_structural_hash_key(&nested),
        value_structural_hash_key(&flat)
    );
}

#[test]
fn hash_key_nil() {
    assert_eq!(
        value_structural_hash_key(&VmValue::Nil),
        value_structural_hash_key(&VmValue::Nil)
    );
}

#[test]
fn hash_key_float_zero_vs_neg_zero() {
    let pos = VmValue::Float(0.0);
    let neg = VmValue::Float(-0.0);
    // 0.0 and -0.0 have different bit representations
    assert_ne!(
        value_structural_hash_key(&pos),
        value_structural_hash_key(&neg)
    );
}

#[test]
fn hash_key_equal_values_match() {
    let a = list(vec![s("hello"), i(42), VmValue::Bool(false)]);
    let b = list(vec![s("hello"), i(42), VmValue::Bool(false)]);
    assert_eq!(value_structural_hash_key(&a), value_structural_hash_key(&b));
}

#[test]
fn hash_key_dict_with_comma_key() {
    let d1 = dict(vec![("a,b", i(1))]);
    let d2 = dict(vec![("a", i(1))]);
    assert_ne!(
        value_structural_hash_key(&d1),
        value_structural_hash_key(&d2)
    );
}

// --- VmRange arithmetic safety at i64 boundaries ---
//
// These guard the saturating/checked arithmetic in `VmRange::len` and
// `VmRange::get` / `VmRange::to_vec`. Before the saturating rewrite the
// inclusive `i64::MIN to 0` case panicked in debug builds on
// `(end - start) + 1`.

#[test]
fn vm_range_len_inclusive_saturates_at_i64_max() {
    let r = VmRange {
        start: i64::MIN,
        end: 0,
        inclusive: true,
    };
    // True width overflows i64; saturating at i64::MAX keeps this total.
    assert_eq!(r.len(), i64::MAX);
}

#[test]
fn vm_range_len_exclusive_full_range_saturates() {
    let r = VmRange {
        start: i64::MIN,
        end: i64::MAX,
        inclusive: false,
    };
    assert_eq!(r.len(), i64::MAX);
}

#[test]
fn vm_range_len_inclusive_full_range_saturates() {
    let r = VmRange {
        start: i64::MIN,
        end: i64::MAX,
        inclusive: true,
    };
    assert_eq!(r.len(), i64::MAX);
}

#[test]
fn vm_range_get_near_max_does_not_overflow() {
    let r = VmRange {
        start: i64::MAX - 2,
        end: i64::MAX,
        inclusive: true,
    };
    assert_eq!(r.len(), 3);
    assert_eq!(r.get(0), Some(i64::MAX - 2));
    assert_eq!(r.get(2), Some(i64::MAX));
    assert_eq!(r.get(3), None);
}

#[test]
fn vm_range_reversed_is_empty() {
    let r = VmRange {
        start: 5,
        end: 1,
        inclusive: true,
    };
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert_eq!(r.first(), None);
    assert_eq!(r.last(), None);
}

#[test]
fn vm_range_contains_near_bounds() {
    let r = VmRange {
        start: 1,
        end: 5,
        inclusive: true,
    };
    assert!(r.contains(1));
    assert!(r.contains(5));
    assert!(!r.contains(0));
    assert!(!r.contains(6));
    let r = VmRange {
        start: 1,
        end: 5,
        inclusive: false,
    };
    assert!(r.contains(1));
    assert!(r.contains(4));
    assert!(!r.contains(5));
}

#[test]
fn vm_range_to_vec_matches_direct_iteration() {
    let r = VmRange {
        start: -2,
        end: 2,
        inclusive: true,
    };
    let v = r.to_vec();
    assert_eq!(v.len(), 5);
    assert_eq!(
        v.iter()
            .map(|x| match x {
                VmValue::Int(n) => *n,
                _ => panic!("non-int in range"),
            })
            .collect::<Vec<_>>(),
        vec![-2, -1, 0, 1, 2]
    );
}
