//! `unreachable-code` coverage — return/throw/break/composite-exit.

use super::*;

#[test]
fn test_unreachable_code() {
    let diags = lint_source(
        r#"
pipeline default(task) {
return 1
log("never reached")
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code warning, got: {diags:?}"
    );
}

#[test]
fn test_no_unreachable_when_return_is_last() {
    let diags = lint_source(
        r#"
pipeline default(task) {
log("hello")
return 1
}
"#,
    );
    assert!(
        !has_rule(&diags, "unreachable-code"),
        "return at end should not trigger unreachable-code: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_throw() {
    let diags = lint_source("pipeline t(task) { throw \"err\"\nlog(\"unreachable\") }");
    assert!(
        diags.iter().any(|d| d.rule == "unreachable-code"),
        "expected unreachable-code after throw, got: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_break() {
    let diags = lint_source(
        r#"
pipeline default(task) {
while true {
    break
    log("unreachable")
}
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code after break, got: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_composite_exit() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 } else { throw "err" }
    log("unreachable")
}
foo(true)
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code after composite exit, got: {diags:?}"
    );
}

#[test]
fn test_no_unreachable_without_both_branches_exiting() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 }
    log("reachable")
}
foo(true)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unreachable-code"),
        "should not flag reachable code: {diags:?}"
    );
}
