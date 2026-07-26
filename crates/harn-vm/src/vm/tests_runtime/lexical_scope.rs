//! Block scoping for `if`/`else`, loop, and catch bindings.
//!
//! Sibling modules `tests_lexical_block` and `tests_lexical_capture` cover the
//! rest of lexical scoping; these two arrived with the runtime tests and moved
//! with them.

use super::harness::*;
#[test]
fn test_if_else_has_lexical_block_scope() {
    let out = run_output(
        r#"pipeline t(task) {
const x = "outer"
if true {
  const x = "inner"
  log(x)
} else {
  const x = "other"
  log(x)
}
log(x)
}"#,
    );
    assert_eq!(out, "[harn] inner\n[harn] outer");
}

#[test]
fn test_loop_and_catch_bindings_are_block_scoped() {
    let out = run_output(
        r#"pipeline t(task) {
const label = "outer"
for item in [1, 2] {
  const label = "loop ${item}"
  log(label)
}
try {
  throw("boom")
} catch (label) {
  log(label)
}
log(label)
}"#,
    );
    assert_eq!(
        out,
        "[harn] loop 1\n[harn] loop 2\n[harn] boom\n[harn] outer"
    );
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
