//! Slot-allocated locals and parameters.
//!
//! Shadowing and assignment, recursive calls through slot params, syncing a slot
//! back out for closure capture, and property/subscript assignment writing
//! through to the slot.

use super::harness::*;
#[rstest::rstest]
#[case::test_slot_locals_preserve_shadowing_and_assignment(
    r"pipeline t(harness: Harness, task: unknown) {
let x = 1
if true {
  let x = 10
  x = x + 1
  harness.stdio.log(x)
}
x = x + 2
harness.stdio.log(x)
}",
    "[harn] 11\n[harn] 3"
)]
#[case::test_slot_params_and_recursive_function_calls(
    r"pipeline t(harness: Harness, task: unknown) {
fn sum_to(n, acc = 0) {
  if n <= 0 {
    return acc
  }
  return sum_to(n - 1, acc + n)
}
harness.stdio.log(sum_to(5))
}",
    "[harn] 15"
)]
#[case::test_slot_locals_sync_for_closure_capture(
    r"pipeline t(harness: Harness, task: unknown) {
let x = 1
x = 7
const f = { -> x + 1 }
harness.stdio.log(f())
}",
    "[harn] 8"
)]
#[case::test_slot_property_assignment_updates_slot_value(
    r"pipeline t(harness: Harness, task: unknown) {
let d = {count: 1}
d.count = d.count + 2
harness.stdio.log(d.count)
}",
    "[harn] 3"
)]
#[case::test_slot_subscript_assignment_updates_slot_value(
    r#"pipeline t(harness: Harness, task: unknown) {
let d = {}
d["a"] = 1
d["b"] = 2
let xs = [0, 0]
xs[1] = 3
harness.stdio.log(d.a + d["b"] + xs[1])
}"#,
    "[harn] 6"
)]
fn output_run_output(#[case] source: &str, #[case] expected: &str) {
    assert_eq!(run_output(source), expected);
}

// --- Error handling tests ---
