//! `dead-code-after-return` coverage — return/throw/break/composite-exit.

use super::*;

#[rstest::rstest]
#[case::test_unreachable_code(
    r#"
pipeline default(task) {
return 1
log("never reached")
}
"#,
    "dead-code-after-return",
    true
)]
#[case::test_no_unreachable_when_return_is_last(
    r#"
pipeline default(task) {
log("hello")
return 1
}
"#,
    "dead-code-after-return",
    false
)]
#[case::test_unreachable_after_break(
    r#"
pipeline default(task) {
while true {
    break
    log("unreachable")
}
}
"#,
    "dead-code-after-return",
    true
)]
#[case::test_unreachable_after_composite_exit(
    r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 } else { throw "err" }
    log("unreachable")
}
foo(true)
}
"#,
    "dead-code-after-return",
    true
)]
#[case::test_no_unreachable_without_both_branches_exiting(
    r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 }
    log("reachable")
}
foo(true)
}
"#,
    "dead-code-after-return",
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

#[test]
fn test_unreachable_after_throw() {
    let diags = lint_source("pipeline t(task) { throw \"err\"\nlog(\"unreachable\") }");
    assert!(
        diags.iter().any(|d| d.rule == "dead-code-after-return"),
        "expected dead-code-after-return after throw, got: {diags:?}"
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
