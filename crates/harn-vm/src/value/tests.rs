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
    // Every variant is held to a single machine word so the whole enum is 16
    // bytes (down from 24, and 32 before that): the oversized payloads
    // (`Range`, `BuiltinRefId`, `Decimal`, `StructInstance`) are boxed behind a
    // `Shared` pointer, and the string-shaped variants use the thin-pointer
    // `HarnStr` (`arcstr::ArcStr`, one word) instead of a 16-byte `Arc<str>`
    // fat pointer. Guard each of those so a regression that re-inlines one of
    // them — re-widening the value the interpreter moves on every push, pop,
    // clone, and local-slot write — fails loudly here.
    assert_eq!(std::mem::size_of::<VmValue>(), 16);
    assert_eq!(std::mem::size_of::<Option<VmValue>>(), 16);
    assert_eq!(std::mem::size_of::<HarnStr>(), 8);
    assert_eq!(std::mem::size_of::<Arc<VmEnumVariant>>(), 8);
    assert_eq!(std::mem::size_of::<Arc<VmRange>>(), 8);
    assert_eq!(std::mem::size_of::<Arc<VmBuiltinRefId>>(), 8);
    assert_eq!(std::mem::size_of::<Arc<rust_decimal::Decimal>>(), 8);
    assert_eq!(std::mem::size_of::<Arc<StructInstanceData>>(), 8);
    assert_eq!(std::mem::size_of::<VmChannelHandle>(), 40);
    assert_eq!(std::mem::size_of::<Arc<VmChannelHandle>>(), 8);
    assert_eq!(std::mem::size_of::<VmAtomicHandle>(), 8);
    assert_eq!(std::mem::size_of::<VmRange>(), 24);
    assert_eq!(std::mem::size_of::<VmGenerator>(), 16);
}

fn s(val: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(val))
}

fn i(val: i64) -> VmValue {
    VmValue::Int(val)
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

fn dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::dict(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<crate::value::DictMap>(),
    )
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

/// Helper: unwrap a `VmValue::String` to its backing `HarnStr`.
fn arc_of(value: &VmValue) -> &arcstr::ArcStr {
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
    assert!(arcstr::ArcStr::ptr_eq(arc_of(&a), arc_of(&b)));
    assert_eq!(arc_of(&a).as_str(), "{");

    // Distinct ASCII chars use distinct interned slots.
    let nl = VmValue::char_value('\n');
    assert!(!arcstr::ArcStr::ptr_eq(arc_of(&a), arc_of(&nl)));
}

#[test]
fn char_value_handles_non_ascii() {
    let e = VmValue::char_value('é');
    assert_eq!(arc_of(&e).as_str(), "é");
}

#[test]
fn chars_list_materializes_each_scalar() {
    let cs = match VmValue::chars_list("a{é}") {
        VmValue::List(items) => items,
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(cs.len(), 4);
    assert_eq!(arc_of(&cs[0]).as_str(), "a");
    assert_eq!(arc_of(&cs[1]).as_str(), "{");
    assert_eq!(arc_of(&cs[2]).as_str(), "é");
    assert_eq!(arc_of(&cs[3]).as_str(), "}");

    // ASCII entries reuse the interned table rather than allocating per char.
    let brace = VmValue::char_value('{');
    assert!(arcstr::ArcStr::ptr_eq(arc_of(&cs[1]), arc_of(&brace)));

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

#[test]
fn try_compare_orders_lists_lexicographically() {
    let s = |v: &str| VmValue::string(v);
    // First differing element decides.
    assert_eq!(
        try_compare_values(&list(vec![i(1), i(2)]), &list(vec![i(1), i(3)])),
        Some(-1)
    );
    assert_eq!(
        try_compare_values(&list(vec![i(2)]), &list(vec![i(1), i(9)])),
        Some(1)
    );
    // Equal contents compare equal; mixed int/float coerces per element.
    assert_eq!(
        try_compare_values(
            &list(vec![i(1), VmValue::Float(2.0)]),
            &list(vec![VmValue::Float(1.0), i(2)])
        ),
        Some(0)
    );
    // A strict prefix sorts before the longer list.
    assert_eq!(
        try_compare_values(&list(vec![i(1)]), &list(vec![i(1), i(0)])),
        Some(-1)
    );
    assert_eq!(
        try_compare_values(&list(vec![]), &list(vec![i(0)])),
        Some(-1)
    );
    // Strings order per element too.
    assert_eq!(
        try_compare_values(&list(vec![s("a"), s("b")]), &list(vec![s("a"), s("c")])),
        Some(-1)
    );
    // Nested lists recurse.
    assert_eq!(
        try_compare_values(
            &list(vec![i(1), list(vec![i(2), i(3)])]),
            &list(vec![i(1), list(vec![i(2), i(4)])])
        ),
        Some(-1)
    );
    // NaN inside a list propagates "unordered", matching Pair semantics …
    let nan = VmValue::Float(f64::NAN);
    assert_eq!(
        try_compare_values(&list(vec![nan.clone()]), &list(vec![i(1)])),
        None
    );
    // … and the total-order wrapper falls back to 0 for sort stability.
    assert_eq!(compare_values(&list(vec![nan]), &list(vec![i(1)])), 0);
}

// --- Deeply nested value recursion guards -------------------------------
//
// A Harn script can build a value nested far deeper than the native call
// stack tolerates (e.g. `x = [x]` in a loop, which adds no VM call frames and
// so never trips `max_vm_frames`). Before the recursion guards, walking or
// dropping such a value overflowed the OS thread stack and aborted the whole
// process. These tests build a value far deeper than any stack frame budget
// and assert the core operations complete and the value tears down safely.
//
// NOTE: each test must `dismantle` the deep value before it leaves scope —
// a bare `VmValue` dropped in plain Rust (outside the VM's guarded
// slot/scope teardown) would otherwise recurse on drop.

const DEEP: usize = 200_000;

fn deep_list(depth: usize) -> VmValue {
    let mut v = i(0);
    for _ in 0..depth {
        v = list(vec![v]);
    }
    v
}

#[test]
fn deeply_nested_equality_does_not_overflow() {
    let a = deep_list(DEEP);
    let b = deep_list(DEEP);
    assert!(values_equal(&a, &a));
    assert!(values_equal(&a, &b));
    // A divergence deep in the structure is still detected correctly.
    let c = {
        let mut v = i(1);
        for _ in 0..DEEP {
            v = list(vec![v]);
        }
        v
    };
    assert!(!values_equal(&a, &c));
    super::recursion::dismantle(a);
    super::recursion::dismantle(b);
    super::recursion::dismantle(c);
}

#[test]
fn deeply_nested_display_and_hash_do_not_overflow() {
    let a = deep_list(DEEP);
    // Display produces a string and structural hashing produces a key; both
    // walk the whole structure. We only assert they return without aborting.
    assert!(a.display().starts_with('['));
    assert!(!value_structural_hash_key(&a).is_empty());
    // Hash keys remain consistent with equality for deep values.
    let b = deep_list(DEEP);
    assert_eq!(value_structural_hash_key(&a), value_structural_hash_key(&b));
    super::recursion::dismantle(a);
    super::recursion::dismantle(b);
}

#[test]
fn deeply_nested_compare_does_not_overflow() {
    // Ordering only recurses through nested pairs; build a deep pair chain.
    let mut a = i(0);
    let mut b = i(0);
    for _ in 0..DEEP {
        a = VmValue::Pair(std::sync::Arc::new((i(1), a)));
        b = VmValue::Pair(std::sync::Arc::new((i(1), b)));
    }
    assert_eq!(try_compare_values(&a, &b), Some(0));
    super::recursion::dismantle(a);
    super::recursion::dismantle(b);
}

#[test]
fn dismantle_tears_down_deep_value_without_recursing() {
    // The whole point: this value's recursive drop would overflow the stack,
    // but `dismantle` reclaims it iteratively.
    let deep = deep_list(DEEP);
    super::recursion::dismantle(deep);
}

#[test]
fn dismantle_preserves_shared_children() {
    // A child still referenced elsewhere must survive dismantling its parent.
    let shared = list(vec![i(1), i(2)]);
    let parent = list(vec![shared.clone(), i(3)]);
    super::recursion::dismantle(parent);
    // `shared` is intact and usable.
    assert!(values_equal(&shared, &list(vec![i(1), i(2)])));
}

#[test]
fn depth_within_reports_nesting_correctly() {
    use super::recursion::depth_within;
    assert!(depth_within(&i(0), 0));
    assert!(depth_within(&list(vec![i(1)]), 1));
    assert!(!depth_within(&list(vec![i(1)]), 0));
    let three = list(vec![list(vec![list(vec![i(1)])])]);
    assert!(depth_within(&three, 3));
    assert!(!depth_within(&three, 2));
    // A value deeper than the limit is rejected without overflowing.
    let deep = deep_list(DEEP);
    assert!(!depth_within(&deep, 1024));
    super::recursion::dismantle(deep);
}

fn dec(s: &str) -> VmValue {
    VmValue::decimal(s.parse().unwrap())
}

#[test]
fn decimal_equality_is_a_scale_insensitive_island() {
    // Equal by value, ignoring scale.
    assert!(values_equal(&dec("1.5"), &dec("1.50")));
    assert!(values_equal(&dec("0.30"), &dec("0.3")));
    // Distinct type from int/float (no cross-type coercion), so equality stays
    // transitive given the existing `Int == Float` rule.
    assert!(!values_equal(&dec("1"), &i(1)));
    assert!(!values_equal(&dec("1"), &VmValue::Float(1.0)));
    assert!(!values_equal(&i(1), &dec("1")));
}

#[test]
fn decimal_hash_key_matches_equality() {
    // `values_equal` and the structural hash key must agree: equal decimals
    // (differing only in scale) share a key, and a decimal never collides with
    // the numerically-equal int/float (which live in their own normalized key
    // space) because decimal equality is a separate island.
    assert_eq!(
        value_structural_hash_key(&dec("1.5")),
        value_structural_hash_key(&dec("1.50"))
    );
    assert_ne!(
        value_structural_hash_key(&dec("1")),
        value_structural_hash_key(&i(1))
    );
    // Dedup honors the island: equal-scale decimals collapse; decimal and int
    // do not.
    let deduped = dedup_values(&[dec("2.0"), dec("2.00"), i(2)]);
    assert_eq!(deduped.len(), 2);
}

#[test]
fn decimal_orders_only_against_decimal() {
    assert_eq!(try_compare_values(&dec("9.99"), &dec("10")), Some(-1));
    assert_eq!(try_compare_values(&dec("10.00"), &dec("10")), Some(0));
    // A decimal-vs-int/float comparison is unordered (no lossy coercion).
    assert_eq!(try_compare_values(&dec("1"), &i(1)), None);
    assert_eq!(try_compare_values(&dec("1"), &VmValue::Float(1.0)), None);
}

#[test]
fn decimal_truthiness_and_type_name() {
    assert!(dec("0.01").is_truthy());
    assert!(!dec("0").is_truthy());
    assert!(!dec("0.00").is_truthy());
    assert_eq!(dec("1.5").type_name(), "decimal");
    // Display preserves the stored scale.
    assert_eq!(dec("1.50").display(), "1.50");
}

/// Reference semantics for char-indexed slicing: materialize the chars and
/// subscript them. This is what the string methods used to do inline, and what
/// the byte-offset helpers must remain byte-for-byte equivalent to.
fn reference_char_slice(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = start.min(chars.len());
    let end = end.max(start).min(chars.len());
    chars[start..end].iter().collect()
}

#[test]
fn char_range_to_byte_range_matches_reference_slicing() {
    // Mixed widths: 1-byte ASCII, 2-byte, 3-byte, and 4-byte scalars, so an
    // off-by-one in the byte walk cannot hide behind a uniform encoding.
    for text in [
        "",
        "hello",
        "héllo wörld",
        "日本語のテキスト",
        "a🌍b🌏c",
        "<tool_call>look({ file: \"münchen.rs\" })</tool_call>",
    ] {
        let char_len = text.chars().count();
        for start in 0..=char_len + 2 {
            for end in 0..=char_len + 2 {
                let (start_byte, end_byte) = char_range_to_byte_range(text, start, end);
                assert!(
                    text.is_char_boundary(start_byte) && text.is_char_boundary(end_byte),
                    "{text:?} [{start}..{end}] produced non-boundary byte range \
                     [{start_byte}..{end_byte}]"
                );
                assert_eq!(
                    &text[start_byte..end_byte],
                    reference_char_slice(text, start, end),
                    "{text:?} [{start}..{end}] diverged from char-vector slicing"
                );
            }
        }
    }
}

#[test]
fn char_offset_conversions_round_trip() {
    for text in ["", "hello", "héllo wörld", "a🌍b", "日本語"] {
        let char_len = text.chars().count();
        for char_index in 0..=char_len + 1 {
            let byte_offset = char_to_byte_offset(text, char_index);
            assert!(
                text.is_char_boundary(byte_offset),
                "{text:?} char {char_index} produced non-boundary offset {byte_offset}"
            );
            // Past the end saturates at the string length rather than wrapping.
            let expected = text
                .char_indices()
                .nth(char_index)
                .map(|(offset, _)| offset)
                .unwrap_or(text.len());
            assert_eq!(byte_offset, expected, "{text:?} char {char_index}");
            assert_eq!(
                byte_offset_to_char_index(text, byte_offset),
                char_index.min(char_len),
                "{text:?} char {char_index} did not round-trip"
            );
        }
    }
}
