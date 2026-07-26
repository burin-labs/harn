//! Inline caches: warming, invalidation, and adaptive specialization.
//!
//! Property, method, and spread-method sites warm to a cached target and a
//! polymorphic site replaces its entry. The adaptive tests cover specializing a
//! generic integer add and a named closure call, plus deoptimizing each when the
//! observed shape or binding changes.

use crate::{
    AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, DirectCallState, InlineCacheEntry,
    MethodCacheTarget, PropertyCacheTarget, VmValue,
};

use super::harness::*;
#[test]
fn test_inline_cache_warms_property_sites() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r#"pipeline t(task) {
const list = [1, 2, 3]
const text = ""
const p = pair("left", "right")
let i = 0
let total = 0
while i < 3 {
  total = total + list.count
  if text.empty {
    total = total + 1
  }
  log(p.second)
  i = i + 1
}
log(total)
}"#,
    );

    assert_eq!(
        out.trim_end(),
        "[harn] right\n[harn] right\n[harn] right\n[harn] 12"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::ListCount,
                ..
            }
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StringEmpty,
                ..
            }
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::PairSecond,
                ..
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_warms_dict_and_struct_property_sites() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
struct Point {
  x: int
  y: int
}
const record = {hot: 7}
const point = Point {x: 2, y: 3}
let i = 0
let total = 0
while i < 3 {
  total = total + record.hot + point.y
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 30");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::DictField(name),
                ..
            } if name.as_ref() == "hot"
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StructField { field_name, index },
                ..
            } if field_name.as_ref() == "y" && *index == 1
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_replaces_polymorphic_property_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r#"pipeline t(task) {
for value in [[1, 2], "ab"] {
  log(value.count)
}
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 2\n[harn] 2");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StringCount,
                ..
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_warms_method_sites() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r#"pipeline t(task) {
const list = [1, 2, 3]
const text = "abc"
const dict = {a: 1, b: 2}
const range = 1 to 3
const values = set(1, 2)
let i = 0
let total = 0
while i < 3 {
  total = total + list.count()
  total = total + text.count()
  total = total + dict.count()
  total = total + range.first()
  total = total + values.count()
  if list.contains(i + 1) { total = total + 1 }
  if text.contains("b") { total = total + 1 }
  if dict.has("a") { total = total + 1 }
  if values.contains(2) { total = total + 1 }
  i = i + 1
}
log(total)
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 45");
    for target in [
        MethodCacheTarget::ListCount,
        MethodCacheTarget::ListContains,
        MethodCacheTarget::StringCount,
        MethodCacheTarget::StringContains,
        MethodCacheTarget::DictCount,
        MethodCacheTarget::DictHas,
        MethodCacheTarget::RangeFirst,
        MethodCacheTarget::SetCount,
        MethodCacheTarget::SetContains,
    ] {
        assert!(
            entries.iter().any(|entry| matches!(
                entry,
                InlineCacheEntry::Method {
                    target: cached_target,
                    ..
                } if *cached_target == target
            )),
            "missing {target:?} in {entries:?}"
        );
    }
}

#[test]
fn test_inline_cache_warms_harness_property_and_method_sites() {
    let (entries, out, result) = run_harn_with_inline_cache_entries(
        r#"fn main(harness: Harness) {
  let i = 0
  let hits = 0
  while i < 3 {
    if harness.env.get_or("__HARN_TEST_MISSING__", "fallback") == "fallback" {
      hits = hits + 1
    }
    const _ = harness.clock.monotonic_ms()
    i = i + 1
  }
  return hits
}"#,
    );

    assert_eq!(out.trim_end(), "");
    assert!(matches!(result, VmValue::Int(3)));
    for target in [
        PropertyCacheTarget::HarnessSubHandle(crate::HarnessKind::Env),
        PropertyCacheTarget::HarnessSubHandle(crate::HarnessKind::Clock),
    ] {
        assert!(
            entries.iter().any(|entry| matches!(
                entry,
                InlineCacheEntry::Property {
                    target: cached_target,
                    ..
                } if *cached_target == target
            )),
            "missing {target:?} in {entries:?}"
        );
    }
    for target in [
        MethodCacheTarget::Harness(crate::HarnessKind::Env),
        MethodCacheTarget::Harness(crate::HarnessKind::Clock),
    ] {
        assert!(
            entries.iter().any(|entry| matches!(
                entry,
                InlineCacheEntry::Method {
                    target: cached_target,
                    ..
                } if *cached_target == target
            )),
            "missing {target:?} in {entries:?}"
        );
    }
}

#[test]
fn test_adaptive_inline_cache_specializes_generic_integer_add_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
let i = erase(0)
let total = erase(0)
while i < erase(8) {
  total = total + i
  i = i + erase(1)
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 28");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Int,
                    hits,
                    ..
                },
            } if *hits >= 3
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_deoptimizes_mixed_binary_shapes() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
const values = [erase(1), erase(2), erase(3), erase(4.0), erase(5.0)]
let acc = erase(0)
for value in values {
  acc = acc + value
}
log(acc)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 15.0");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Float,
                    misses: 1,
                    ..
                },
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_specializes_named_closure_call_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn inc(x) {
  return x + 1
}
let i = 0
let total = 0
while i < 8 {
  total = total + inc(i)
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 36");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized { hits, .. },
            } if *hits >= 3
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_deoptimizes_rebound_closure_call_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn inc(x) {
  return x + 1
}
fn dec(x) {
  return x - 1
}
let op = inc
let i = 0
let total = 0
while i < 5 {
  if i == 3 {
    op = dec
  }
  total = total + op(10)
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 51");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized { misses: 1, .. },
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_warms_spread_method_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
const list = [1, 2, 3]
const args = []
let i = 0
while i < 3 {
  log(list.count(...args))
  i = i + 1
}
}",
    );

    assert_eq!(out.trim_end(), "[harn] 3\n[harn] 3\n[harn] 3");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Method {
                target: MethodCacheTarget::ListCount,
                ..
            }
        )),
        "{entries:?}"
    );
}
