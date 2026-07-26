//! Slot-allocated locals and parameters.
//!
//! Shadowing and assignment, recursive calls through slot params, syncing a slot
//! back out for closure capture, and property/subscript assignment writing
//! through to the slot.

use super::harness::*;
#[test]
fn test_slot_locals_preserve_shadowing_and_assignment() {
    let out = run_output(
        r"pipeline t(task) {
let x = 1
if true {
  let x = 10
  x = x + 1
  log(x)
}
x = x + 2
log(x)
}",
    );
    assert_eq!(out, "[harn] 11\n[harn] 3");
}

#[test]
fn test_slot_params_and_recursive_function_calls() {
    let out = run_output(
        r"pipeline t(task) {
fn sum_to(n, acc = 0) {
  if n <= 0 {
    return acc
  }
  return sum_to(n - 1, acc + n)
}
log(sum_to(5))
}",
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_slot_locals_sync_for_closure_capture() {
    let out = run_output(
        r"pipeline t(task) {
let x = 1
x = 7
const f = { -> x + 1 }
log(f())
}",
    );
    assert_eq!(out, "[harn] 8");
}

#[test]
fn test_slot_property_assignment_updates_slot_value() {
    let out = run_output(
        r"pipeline t(task) {
let d = {count: 1}
d.count = d.count + 2
log(d.count)
}",
    );
    assert_eq!(out, "[harn] 3");
}

#[test]
fn test_slot_subscript_assignment_updates_slot_value() {
    let out = run_output(
        r#"pipeline t(task) {
let d = {}
d["a"] = 1
d["b"] = 2
let xs = [0, 0]
xs[1] = 3
log(d.a + d["b"] + xs[1])
}"#,
    );
    assert_eq!(out, "[harn] 6");
}

// --- Error handling tests ---
