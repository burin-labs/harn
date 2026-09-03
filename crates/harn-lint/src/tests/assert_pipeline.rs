//! `assert` / `require` pipeline-placement rules.

use super::*;

#[test]
fn test_assert_outside_test_pipeline_warns() {
    let diags = lint_source(
        r"
pipeline default(task) {
assert(true)
}
",
    );
    assert!(
        has_rule(&diags, "assert-outside-test"),
        "expected assert-outside-test warning, got: {diags:?}"
    );
}

#[test]
fn test_assert_inside_test_pipeline_is_allowed() {
    let diags = lint_source(
        r"
pipeline test(task) {
assert_eq(1 + 1, 2)
}
",
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

// A helper `fn` under a test root, holding the exact shape that produced
// sixteen findings across nine files: the assert is lexically outside any
// `pipeline test_*`, so the rule read it as production control flow.
const TEST_HELPER_SOURCE: &str = r#"
fn expect_two(value: int) {
assert_eq(value, 2)
}

pipeline test_math(task) {
expect_two(1 + 1)
}
"#;

#[test]
fn assert_in_a_helper_under_a_test_root_is_allowed() {
    let diags = lint_source_at(TEST_HELPER_SOURCE, "tests/agent/math_test.harn");
    assert!(
        !has_rule(&diags, "assert-outside-test"),
        "a file under a test root is a test file in all of it: {diags:?}"
    );
}

#[test]
fn assert_in_the_same_helper_outside_a_test_root_still_warns() {
    // The control. Without it the case above passes on a build that dropped
    // the rule entirely, and the rule's whole job is to catch this shape in
    // production source.
    let diags = lint_source_at(TEST_HELPER_SOURCE, "src/runtime/math.harn");
    assert!(
        has_rule(&diags, "assert-outside-test"),
        "the same helper outside a test root is still production control flow: {diags:?}"
    );
}

#[test]
fn a_test_suffixed_file_is_a_test_root_on_its_own() {
    // `tests/` is not the only spelling in the tree: focused suites sit beside
    // the code they cover and are named `<subject>_test.harn`.
    let diags = lint_source_at(TEST_HELPER_SOURCE, "bench/vm/dispatch_test.harn");
    assert!(
        !has_rule(&diags, "assert-outside-test"),
        "a `_test.harn` file is test source wherever it lives: {diags:?}"
    );
}
