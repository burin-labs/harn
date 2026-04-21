use super::*;

#[test]
fn request_approval_result_must_be_handled() {
    let diags = lint_source(
        r#"
pipeline deploy(task) {
  request_approval("deploy prod", {reviewers: ["alice"]})
}
"#,
    );
    assert!(
        has_rule(&diags, "unhandled-approval-result"),
        "expected unhandled approval result warning, got: {diags:?}"
    );
}

#[test]
fn request_approval_bound_result_is_handled() {
    let diags = lint_source(
        r#"
pipeline deploy(task) {
  let approval = request_approval("deploy prod", {reviewers: ["alice"]})
  println(approval.approved)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unhandled-approval-result"),
        "bound approval records should not warn: {diags:?}"
    );
}

#[test]
fn request_approval_as_try_value_is_handled() {
    let diags = lint_source(
        r#"
pipeline deploy(task) {
  let result = try {
    request_approval("deploy prod", {reviewers: ["alice"]})
  }
  println(is_ok(result))
}
"#,
    );
    assert!(
        !has_rule(&diags, "unhandled-approval-result"),
        "approval returned from try expression should not warn: {diags:?}"
    );
}
