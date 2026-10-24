use super::*;

#[rstest::rstest]
#[case::push_pr_without_prior_secret_scan_warns(
    r#"
pipeline default(task) {
  const client = mcp_connect("harn", [])
  mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    "pr-open-without-secret-scan",
    true
)]
#[case::push_pr_after_secret_scan_is_not_flagged(
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
    "pr-open-without-secret-scan",
    false
)]
#[case::branch_local_secret_scan_does_not_cover_outer_pr_open(
    r#"
pipeline default(task) {
  if true {
    secret_scan("diff")
  }
  const client = mcp_connect("harn", [])
  mcp_call(client, "git::push_pr", {title: "still unsafe"})
}
"#,
    "pr-open-without-secret-scan",
    true
)]
#[case::migrated_scan_covers_legacy_pr_open(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "harn.secret_scan", {content: "diff"})
  mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    "pr-open-without-secret-scan",
    false
)]
#[case::legacy_scan_covers_migrated_pr_open(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  mcp_call(client, "harn.secret_scan", {content: "diff"})
  harness.tools.mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    "pr-open-without-secret-scan",
    false
)]
#[case::migrated_scan_covers_migrated_pr_open(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "harn.secret_scan", {content: "diff"})
  harness.tools.mcp_call(client, "git::push_pr", {title: "safe"})
}
"#,
    "pr-open-without-secret-scan",
    false
)]
#[case::migrated_pr_open_without_scan_warns(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  harness.tools.mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    "pr-open-without-secret-scan",
    true
)]
#[case::colliding_non_harness_scan_method_does_not_count_as_scan(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  const proxy = {}
  proxy.mcp_call(client, "harn.secret_scan", {content: "diff"})
  mcp_call(client, "git::push_pr", {title: "unsafe"})
}
"#,
    "pr-open-without-secret-scan",
    true
)]
#[case::colliding_non_harness_pr_open_method_is_not_flagged(
    r#"
fn main(harness: Harness) {
  const client = mcp_connect("harn", [])
  const proxy = {}
  proxy.mcp_call(client, "git::push_pr", {title: "not a host call"})
}
"#,
    "pr-open-without-secret-scan",
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
