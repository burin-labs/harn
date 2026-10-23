//! `break-outside-loop` rule.

use super::*;

#[rstest::rstest]
#[case::test_break_outside_loop(
    r"
pipeline default(task) {
break
}
",
    "break-outside-loop",
    true
)]
#[case::test_break_inside_loop_ok(
    r"
pipeline default(task) {
while true {
    break
}
}
",
    "break-outside-loop",
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
