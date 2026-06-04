use std::sync::Arc;

use super::*;

static_assertions::assert_impl_all!(VmValue: Send, Sync);
static_assertions::assert_impl_all!(crate::Chunk: Send, Sync);
static_assertions::assert_impl_all!(crate::vm::Vm: Send);
static_assertions::assert_impl_all!(VmBuiltinFn: Send, Sync);
static_assertions::assert_impl_all!(VmAsyncBuiltinFn: Send, Sync);

#[cfg(target_pointer_width = "64")]
#[test]
fn vm_value_layout_budget() {
    assert_eq!(std::mem::size_of::<VmValue>(), 32);
    assert_eq!(std::mem::size_of::<Option<VmValue>>(), 32);
    assert_eq!(std::mem::size_of::<Arc<VmEnumVariant>>(), 8);
    assert_eq!(std::mem::size_of::<VmChannelHandle>(), 40);
    assert_eq!(std::mem::size_of::<Arc<VmChannelHandle>>(), 8);
    assert_eq!(std::mem::size_of::<VmAtomicHandle>(), 8);
    assert_eq!(std::mem::size_of::<VmRange>(), 24);
    assert_eq!(std::mem::size_of::<VmGenerator>(), 16);
}

fn s(val: &str) -> VmValue {
    VmValue::String(std::sync::Arc::from(val))
}

fn i(val: i64) -> VmValue {
    VmValue::Int(val)
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

fn dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(
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
fn hash_key_signed_zero_and_int_zero_all_match() {
    // `values_equal` treats `0.0 == -0.0 == 0`, so all three must hash alike
    // even though `0.0` and `-0.0` have different bit patterns.
    let pos = value_structural_hash_key(&VmValue::Float(0.0));
    let neg = value_structural_hash_key(&VmValue::Float(-0.0));
    let int = value_structural_hash_key(&i(0));
    assert_eq!(pos, neg);
    assert_eq!(pos, int);
}

#[test]
fn hash_key_integral_float_matches_int() {
    // `1 == 1.0` per `values_equal`, so they must share a hash key; `1.5` must
    // not collide with any integer.
    assert_eq!(
        value_structural_hash_key(&i(1)),
        value_structural_hash_key(&VmValue::Float(1.0))
    );
    assert_ne!(
        value_structural_hash_key(&i(1)),
        value_structural_hash_key(&VmValue::Float(1.5))
    );
}

#[test]
fn hash_key_nan_is_not_an_int() {
    // NaN is non-integral, so it keeps a float-shaped key (it must never alias
    // an integer bucket).
    let nan = value_structural_hash_key(&VmValue::Float(f64::NAN));
    assert!(
        nan.starts_with('f'),
        "NaN key should be float-shaped: {nan}"
    );
    assert_ne!(nan, value_structural_hash_key(&i(0)));
}

#[test]
fn dedup_values_matches_equality_operator() {
    // `1 == 1.0` -> collapses; nested inside a pair too.
    let collapsed = dedup_values(&[i(1), VmValue::Float(1.0), i(1)]);
    assert_eq!(collapsed.len(), 1);

    let pair = |a, b| VmValue::Pair(std::sync::Arc::new((a, b)));
    let pairs = dedup_values(&[pair(i(1), s("x")), pair(VmValue::Float(1.0), s("x"))]);
    assert_eq!(pairs.len(), 1, "Pair(1, x) and Pair(1.0, x) are ==");

    // NaN != NaN, so two NaNs are both kept despite sharing a hash bucket.
    let nans = dedup_values(&[VmValue::Float(f64::NAN), VmValue::Float(f64::NAN)]);
    assert_eq!(nans.len(), 2);
}

#[test]
fn dedup_values_preserves_first_occurrence_order() {
    let out = dedup_values(&[i(3), i(1), i(3), i(2), i(1)]);
    let got: Vec<i64> = out
        .iter()
        .map(|v| match v {
            VmValue::Int(n) => *n,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    assert_eq!(got, vec![3, 1, 2]);
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

/// Helper: unwrap a `VmValue::String` to its backing `Arc<str>`.
fn arc_of(value: &VmValue) -> &Arc<str> {
    match value {
        VmValue::String(s) => s,
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn char_value_interns_ascii() {
    // Two independent single-char values for the same ASCII char must share the
    // interned allocation — this is the invariant that keeps materializing a
    // large source file into `chars(...)` allocation-free on the common path.
    let a = VmValue::char_value('{');
    let b = VmValue::char_value('{');
    assert!(Arc::ptr_eq(arc_of(&a), arc_of(&b)));
    assert_eq!(arc_of(&a).as_ref(), "{");

    // Distinct ASCII chars use distinct interned slots.
    let nl = VmValue::char_value('\n');
    assert!(!Arc::ptr_eq(arc_of(&a), arc_of(&nl)));
}

#[test]
fn char_value_handles_non_ascii() {
    let e = VmValue::char_value('é');
    assert_eq!(arc_of(&e).as_ref(), "é");
}

#[test]
fn chars_list_materializes_each_scalar() {
    let cs = match VmValue::chars_list("a{é}") {
        VmValue::List(items) => items,
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(cs.len(), 4);
    assert_eq!(arc_of(&cs[0]).as_ref(), "a");
    assert_eq!(arc_of(&cs[1]).as_ref(), "{");
    assert_eq!(arc_of(&cs[2]).as_ref(), "é");
    assert_eq!(arc_of(&cs[3]).as_ref(), "}");

    // ASCII entries reuse the interned table rather than allocating per char.
    let brace = VmValue::char_value('{');
    assert!(Arc::ptr_eq(arc_of(&cs[1]), arc_of(&brace)));

    assert!(matches!(VmValue::chars_list(""), VmValue::List(items) if items.is_empty()));
}

#[test]
fn try_compare_orders_finite_numbers() {
    let f = VmValue::Float;
    assert_eq!(try_compare_values(&i(1), &i(2)), Some(-1));
    assert_eq!(try_compare_values(&i(2), &i(2)), Some(0));
    assert_eq!(try_compare_values(&f(2.5), &f(1.5)), Some(1));
    // Mixed int/float still produces a total order.
    assert_eq!(try_compare_values(&i(2), &f(2.5)), Some(-1));
    assert_eq!(try_compare_values(&f(2.0), &i(2)), Some(0));
}

#[test]
fn try_compare_reports_nan_as_unordered() {
    let nan = VmValue::Float(f64::NAN);
    // Any comparison involving NaN is unordered (`None`), so relational
    // operators must treat it as false rather than "equal".
    assert_eq!(try_compare_values(&nan, &VmValue::Float(5.0)), None);
    assert_eq!(try_compare_values(&VmValue::Float(5.0), &nan), None);
    assert_eq!(try_compare_values(&nan, &i(5)), None);
    assert_eq!(try_compare_values(&nan, &nan), None);
    // A NaN nested inside a pair propagates the unordered result.
    let pair_a = VmValue::Pair(std::sync::Arc::new((i(1), nan.clone())));
    let pair_b = VmValue::Pair(std::sync::Arc::new((i(1), VmValue::Float(5.0))));
    assert_eq!(try_compare_values(&pair_a, &pair_b), None);

    // The total-order wrapper keeps a sort-stable fallback of 0.
    assert_eq!(compare_values(&nan, &VmValue::Float(5.0)), 0);
}
