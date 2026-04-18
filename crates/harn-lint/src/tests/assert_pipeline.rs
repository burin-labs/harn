//! `assert` / `require` pipeline-placement rules.

use super::*;

#[test]
fn test_assert_outside_test_pipeline_warns() {
    let diags = lint_source(
        r#"
pipeline default(task) {
assert(true)
}
"#,
    );
    assert!(
        has_rule(&diags, "assert-outside-test"),
        "expected assert-outside-test warning, got: {diags:?}"
    );
}

#[test]
fn test_assert_inside_test_pipeline_is_allowed() {
    let diags = lint_source(
        r#"
pipeline test(task) {
assert_eq(1 + 1, 2)
}
"#,
    );
    assert!(
        !has_rule(&diags, "assert-outside-test"),
        "asserts inside test pipelines should be allowed: {diags:?}"
    );
}

#[test]
fn test_require_inside_test_pipeline_warns() {
    let diags = lint_source(
        r#"
pipeline test_example(task) {
require 1 + 1 == 2, "math still works"
}
"#,
    );
    assert!(
        has_rule(&diags, "require-in-test"),
        "expected require-in-test warning, got: {diags:?}"
    );
}

#[test]
fn test_require_outside_test_pipeline_is_allowed() {
    let diags = lint_source(
        r#"
pipeline default(task) {
require task != nil, "task is required"
}
"#,
    );
    assert!(
        !has_rule(&diags, "require-in-test"),
        "require outside tests should be allowed: {diags:?}"
    );
}
