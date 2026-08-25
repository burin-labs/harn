use super::*;

#[test]
fn push_pr_without_prior_secret_scan_warns() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  const client = mcp_connect("harn", [])
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
  const diff = "token = ghp_123"
  const findings = secret_scan(diff)
  if len(findings) == 0 {
    const client = mcp_connect("harn", [])
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
  const client = mcp_connect("harn", [])
  mcp_call(client, "git::push_pr", {title: "still unsafe"})
}
"#,
    );

    assert!(
        has_rule(&diags, "pr-open-without-secret-scan"),
        "branch-local secret_scan should not satisfy a later unconditional PR-open, got: {diags:?}"
    );
}

#[test]
fn migrated_scan_covers_legacy_pr_open() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "harn.secret_scan", {content: "diff"})
  mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    );

    assert!(
        !has_rule(&diags, "pr-open-without-secret-scan"),
        "canonical scan must cover a legacy PR-open, got: {diags:?}"
    );
}

#[test]
fn legacy_scan_covers_migrated_pr_open() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  mcp_call(client, "harn.secret_scan", {content: "diff"})
  harness.tools.mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    );

    assert!(
        !has_rule(&diags, "pr-open-without-secret-scan"),
        "legacy scan must cover a canonical PR-open, got: {diags:?}"
    );
}

#[test]
fn migrated_scan_covers_migrated_pr_open() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "harn.secret_scan", {content: "diff"})
  harness.tools.mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    );

    assert!(
        !has_rule(&diags, "pr-open-without-secret-scan"),
        "fully canonical scan and PR-open must pair, got: {diags:?}"
    );
}

#[test]
fn migrated_pr_open_without_scan_warns() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    );

    assert!(
        has_rule(&diags, "pr-open-without-secret-scan"),
        "canonical PR-open must retain the secret-scan guard, got: {diags:?}"
    );
}

#[test]
fn colliding_non_harness_scan_method_does_not_count_as_scan() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  const proxy = {}
  proxy.mcp_call(client, "harn.secret_scan", {content: "diff"})
  mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    );

    assert!(
        has_rule(&diags, "pr-open-without-secret-scan"),
        "unrelated scan method must not satisfy the guard, got: {diags:?}"
    );
}

#[test]
fn colliding_non_harness_pr_open_method_is_not_flagged() {
    let diags = lint_source(
        r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  const proxy = {}
  proxy.mcp_call(client, "git::push_pr", {title: "not a host call"})
}
"#,
    );

    assert!(
        !has_rule(&diags, "pr-open-without-secret-scan"),
        "unrelated PR-open method must not trigger the guard, got: {diags:?}"
    );
}
