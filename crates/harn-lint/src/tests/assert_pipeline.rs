//! `assert` / `require` pipeline-placement rules.

use super::*;

#[rstest::rstest]
#[case::test_assert_outside_test_pipeline_warns(
    r"
pipeline default(task) {
assert(true)
}
",
    "assert-outside-test",
    true
)]
#[case::test_assert_inside_test_pipeline_is_allowed(
    r"
pipeline test(task) {
assert_eq(1 + 1, 2)
}
",
    "assert-outside-test",
    false
)]
#[case::test_require_inside_test_pipeline_warns(
    r#"
pipeline test_example(task) {
require 1 + 1 == 2, "math still works"
}
"#,
    "require-in-test",
    true
)]
#[case::test_require_outside_test_pipeline_is_allowed(
    r#"
pipeline default(task) {
require task != nil, "task is required"
}
"#,
    "require-in-test",
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

// A helper `fn` under a test root, holding the exact shape that produced
// sixteen findings across nine files: the assert is lexically outside any
// `pipeline test_*`, so the rule read it as production control flow.
const TEST_HELPER_SOURCE: &str = r"
fn expect_two(value: int) {
assert_eq(value, 2)
}

pipeline test_math(task) {
expect_two(1 + 1)
}
";

// The three layouts a test suite is actually written in. Before the typed
// predicate the rule knew only the first two, so a consumer repository whose
// suites sit in `pipeline-tests/` and end in `-test.harn` had every assert in
// every suite reported as production control flow.
#[test]
fn every_test_layout_is_recognised_as_test_source() {
    for path in [
        "tests/agent/math_test.harn",
        "bench/vm/dispatch_test.harn",
        "pipeline-tests/modes/review-test.harn",
    ] {
        let diags = lint_source_at(TEST_HELPER_SOURCE, path);
        assert!(
            !has_rule(&diags, "assert-outside-test"),
            "{path} is a test layout and must not warn: {diags:?}"
        );
    }
}

#[test]
fn a_non_test_layout_still_warns_under_every_neighbour() {
    // The control for the case above. Without it that test passes on a build
    // whose predicate returns true for everything, which would disable the
    // rule outright rather than widen it.
    for path in [
        "src/runtime/math.harn",
        "pipelines/modes/review.harn",
        "bench/vm/dispatch.harn",
    ] {
        let diags = lint_source_at(TEST_HELPER_SOURCE, path);
        assert!(
            has_rule(&diags, "assert-outside-test"),
            "{path} is production source and must warn: {diags:?}"
        );
    }
}

#[test]
fn a_declared_test_root_is_recognised_and_an_undeclared_one_is_not() {
    // The second half of the fix: a project whose suites live under a
    // directory that is not named `tests` declares it once in `[lint]`.
    let declared = vec!["pipeline-tests".to_string()];
    let path = "pipeline-tests/modes/review.harn";

    let with_config = lint_source_at_with_test_roots(TEST_HELPER_SOURCE, path, &declared);
    assert!(
        !has_rule(&with_config, "assert-outside-test"),
        "a declared test root must be test source: {with_config:?}"
    );

    // Control one: the same path with nothing declared still warns, so the
    // config is what changed the answer and not a widened default.
    let without_config = lint_source_at(TEST_HELPER_SOURCE, path);
    assert!(
        has_rule(&without_config, "assert-outside-test"),
        "an undeclared directory must stay production source: {without_config:?}"
    );

    // Control two: declaring one root does not turn the rule off everywhere.
    let elsewhere =
        lint_source_at_with_test_roots(TEST_HELPER_SOURCE, "src/runtime/math.harn", &declared);
    assert!(
        has_rule(&elsewhere, "assert-outside-test"),
        "declaring a root must not disable the rule elsewhere: {elsewhere:?}"
    );
}
