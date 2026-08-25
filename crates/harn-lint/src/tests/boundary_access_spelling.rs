//! `HARN-LNT-029` must see a boundary call in either spelling.
//!
//! Reading a field straight off an unvalidated network body, model response,
//! or tool result is the same risk whether the source calls `http_get(...)` or
//! the `harness.net.get(...)` that replaced it. The list of boundary sources
//! is owned by `harn_parser::builtin_signatures`, shared with the
//! typechecker's `HARN-OWN-004`, so the two rules cannot drift apart.

use super::*;

#[test]
fn untyped_dict_access_reports_the_ambient_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = http_get("https://example.com").body
  harness.stdio.log(body)
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, "untyped-dict-access"), 1);
}

#[test]
fn untyped_dict_access_reports_the_harness_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = harness.net.get("https://example.com").body
  harness.stdio.log(body)
}
"#,
    );

    assert_eq!(
        count_rule(&diagnostics, "untyped-dict-access"),
        1,
        "migrating the call site must not silence the rule: {diagnostics:?}"
    );
}

#[test]
fn untyped_dict_access_reports_a_harness_subscript() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = harness.net.get("https://example.com")["body"]
  harness.stdio.log(body)
}
"#,
    );

    assert_eq!(
        count_rule(&diagnostics, "untyped-dict-access"),
        1,
        "subscript access is the same risk as property access: {diagnostics:?}"
    );
}

#[test]
fn untyped_dict_access_ignores_a_get_method_on_another_receiver() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const proxy = {net: {get: { url -> {body: url} }}}
  const body = proxy.net.get("https://example.com").body
  harness.stdio.log(body)
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "untyped-dict-access"),
        "`net.get` on a plain value is not the harness method: {diagnostics:?}"
    );
}

#[test]
fn untyped_dict_access_names_the_spelling_the_source_used() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = harness.net.get("https://example.com").body
  harness.stdio.log(body)
}
"#,
    );

    let message = &diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule == "untyped-dict-access")
        .expect("expected an untyped-dict-access diagnostic")
        .message;
    assert!(
        message.contains("harness.net.get()"),
        "the diagnostic should quote the call as written, got: {message}"
    );
}

/// `mcp_call` was in the linter's list but not the typechecker's, and
/// `host_tool_call` was the reverse. Both now resolve from the one list, in
/// both spellings.
#[test]
fn untyped_dict_access_covers_the_previously_divergent_names() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const a = host_tool_call("read_file", {path: "x"}).content
  const b = harness.tools.mcp_call(nil, "srv::tool", {}).content
  harness.stdio.log("${a} ${b}")
}
"#,
    );

    assert_eq!(
        count_rule(&diagnostics, "untyped-dict-access"),
        2,
        "both previously one-sided names should report: {diagnostics:?}"
    );
}
