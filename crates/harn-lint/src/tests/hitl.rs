use super::*;

#[rstest::rstest]
#[case::request_approval_result_must_be_handled(
    r#"
pipeline deploy(task) {
  request_approval("deploy prod", {reviewers: ["alice"]})
}
"#,
    "unhandled-approval-result",
    true
)]
#[case::request_approval_bound_result_is_handled(
    r#"
pipeline deploy(task) {
  const approval = request_approval("deploy prod", {reviewers: ["alice"]})
  __io_println(approval.approved)
}
"#,
    "unhandled-approval-result",
    false
)]
#[case::request_approval_as_try_value_is_handled(
    r#"
pipeline deploy(task) {
  const result = try {
    request_approval("deploy prod", {reviewers: ["alice"]})
  }
  __io_println(is_ok(result))
}
"#,
    "unhandled-approval-result",
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
