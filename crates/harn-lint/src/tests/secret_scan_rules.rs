use super::*;

#[test]
fn push_pr_without_prior_secret_scan_warns() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let client = mcp_connect("harn", [])
  mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    );

    assert!(
        has_rule(&diags, "pr-open-without-secret-scan"),
        "expected pr-open-without-secret-scan warning, got: {diags:?}"
    );
}

#[test]
fn push_pr_after_secret_scan_is_not_flagged() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let diff = "token = ghp_123"
  let findings = secret_scan(diff)
  if len(findings) == 0 {
    let client = mcp_connect("harn", [])
    mcp_call(client, "git::push_pr", {title: "safe"})
  }
}
"#,
    );

    assert!(
        !has_rule(&diags, "pr-open-without-secret-scan"),
        "secret_scan before push_pr should satisfy the lint, got: {diags:?}"
    );
}

#[test]
fn branch_local_secret_scan_does_not_cover_outer_pr_open() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  if true {
    secret_scan("diff")
  }
  let client = mcp_connect("harn", [])
  mcp_call(client, "git::push_pr", {title: "still unsafe"})
}
"#,
    );

    assert!(
        has_rule(&diags, "pr-open-without-secret-scan"),
        "branch-local secret_scan should not satisfy a later unconditional PR-open, got: {diags:?}"
    );
}
