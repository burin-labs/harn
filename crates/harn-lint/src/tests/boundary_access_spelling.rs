//! `HARN-LNT-029` must see a boundary call in either spelling.
//!
//! Reading a field straight off an unvalidated model response or tool result
//! is the same risk whether the source calls `llm_call(...)` or the
//! `harness.llm.call(...)` that replaced it. The list of boundary sources
//! is owned by `harn_parser::builtin_signatures`, shared with the
//! typechecker's `HARN-OWN-004`, so the two rules cannot drift apart.

use super::*;

#[test]
fn untyped_dict_access_reports_the_ambient_spelling() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = llm_call("rate this", "system").data
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
  const body = harness.llm.call("rate this", "system").data
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
  const body = harness.llm.call("rate this", "system")["data"]
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
  const proxy = {llm: {call: { prompt, system -> {data: prompt} }}}
  const body = proxy.llm.call("rate this", "system").data
  harness.stdio.log(body)
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "untyped-dict-access"),
        "`llm.call` on a plain value is not the harness method: {diagnostics:?}"
    );
}

#[test]
fn untyped_dict_access_names_the_spelling_the_source_used() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const body = harness.llm.call("rate this", "system").data
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
        message.contains("harness.llm.call()"),
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

/// A buffered HTTP response is the closed `HTTP_RESPONSE` record, so reading
/// its envelope is typed access, not a boundary read.
#[test]
fn untyped_dict_access_ignores_the_typed_http_envelope() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const response = harness.net.get("https://example.com")
  harness.stdio.log("${response.status} ${response.body}")
}
"#,
    );

    assert!(
        !has_rule(&diagnostics, "untyped-dict-access"),
        "a typed response envelope is not an untyped boundary: {diagnostics:?}"
    );
}
