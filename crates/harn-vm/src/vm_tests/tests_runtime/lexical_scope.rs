//! Block scoping for `if`/`else`, loop, and catch bindings.
//!
//! Sibling modules `tests_lexical_block` and `tests_lexical_capture` cover the
//! rest of lexical scoping; these two arrived with the runtime tests and moved
//! with them.

use super::harness::*;
#[rstest::rstest]
#[case::test_if_else_has_lexical_block_scope(
    r#"pipeline t(harness: Harness, task: unknown) {
const x = "outer"
if true {
  const x = "inner"
  harness.stdio.log(x)
} else {
  const x = "other"
  harness.stdio.log(x)
}
harness.stdio.log(x)
}"#,
    "[harn] inner\n[harn] outer"
)]
#[case::test_loop_and_catch_bindings_are_block_scoped(
    r#"pipeline t(harness: Harness, task: unknown) {
const label = "outer"
for item in [1, 2] {
  const label = "loop ${item}"
  harness.stdio.log(label)
}
try {
  throw("boom")
} catch (label) {
  harness.stdio.log(label)
}
harness.stdio.log(label)
}"#,
    "[harn] loop 1\n[harn] loop 2\n[harn] boom\n[harn] outer"
)]
fn output_run_output(#[case] source: &str, #[case] expected: &str) {
    assert_eq!(run_output(source), expected);
}

// --- Closure late-bind walk skip (Chunk::references_outer_names) ---
//
// The runtime fast path in `Vm::closure_call_env` skips the
// caller-scope late-bind walk for callees whose bodies never read
// outer names. These tests pin the corner cases that would regress
// if the static check ever stopped flagging an env-reading opcode:
// self-recursion, mutual recursion, sibling-fn references, var
// rebinding from inside a closure, and inline lambdas as callback
// arguments.
