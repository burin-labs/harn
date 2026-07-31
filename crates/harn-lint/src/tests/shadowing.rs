//! `shadow-variable` rule.

use super::*;

#[test]
fn test_shadow_variable() {
    let diags = lint_source(
        r"
pipeline default(task) {
const x = 1
if true {
    const x = 2
    log(x)
}
log(x)
}
",
    );
    assert!(
        has_rule(&diags, "shadow-variable"),
        "expected shadow-variable warning, got: {diags:?}"
    );
}

#[test]
fn test_no_shadow_same_scope() {
    // Re-declaration in the same scope is not shadowing (it may be a
    // parser error, but the linter only checks outer-scope shadows).
    let diags = lint_source(
        r"
pipeline default(task) {
const x = 1
log(x)
}
",
    );
    assert!(
        !has_rule(&diags, "shadow-variable"),
        "same-scope should not trigger shadow-variable: {diags:?}"
    );
}

#[test]
fn test_nested_harness_boundary_does_not_warn() {
    let diags = lint_source(
        r"
pipeline default(harness: Harness) {
  register({
    handler: { harness, event ->
      harness.stdio.println(event.kind)
    },
  })
}
",
    );
    assert!(
        !has_rule(&diags, "shadow-variable"),
        "nested runtime-supplied Harness boundaries use the canonical name: {diags:?}"
    );
}
