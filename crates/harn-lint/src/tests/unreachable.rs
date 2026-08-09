//! `dead-code-after-return` coverage — return/throw/break/composite-exit.

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
        has_rule(&diags, "dead-code-after-return"),
        "expected dead-code-after-return warning, got: {diags:?}"
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
        !has_rule(&diags, "dead-code-after-return"),
        "return at end should not trigger dead-code-after-return: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_throw() {
    let diags = lint_source("pipeline t(task) { throw \"err\"\nlog(\"unreachable\") }");
    assert!(
        diags.iter().any(|d| d.rule == "dead-code-after-return"),
        "expected dead-code-after-return after throw, got: {diags:?}"
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
        has_rule(&diags, "dead-code-after-return"),
        "expected dead-code-after-return after break, got: {diags:?}"
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
        has_rule(&diags, "dead-code-after-return"),
        "expected dead-code-after-return after composite exit, got: {diags:?}"
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
        !has_rule(&diags, "dead-code-after-return"),
        "should not flag reachable code: {diags:?}"
    );
}

#[test]
fn test_legacy_unreachable_code_disabled_rule_alias() {
    let source = "pipeline default(task) {\n  return 1\n  log(\"never reached\")\n}";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let diags =
        lint_with_config_and_source(&program, &["unreachable-code".to_string()], Some(source));
    assert!(
        !has_rule(&diags, "dead-code-after-return"),
        "legacy disabled rule should suppress renamed diagnostic: {diags:?}"
    );
}
