//! `shadow-variable` rule.

use super::*;

#[rstest::rstest]
#[case::test_shadow_variable(
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
    "shadow-variable",
    true
)]
// Re-declaration in the same scope is not shadowing (it may be a
// parser error, but the linter only checks outer-scope shadows).
#[case::test_no_shadow_same_scope(
    r"
pipeline default(task) {
const x = 1
log(x)
}
",
    "shadow-variable",
    false
)]
#[case::test_nested_harness_boundary_does_not_warn(
    r"
pipeline default(harness: Harness) {
  register({
    handler: { harness, event ->
      harness.stdio.println(event.kind)
    },
  })
}
",
    "shadow-variable",
    false
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}
