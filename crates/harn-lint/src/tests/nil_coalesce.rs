use super::*;
use harn_parser::DiagnosticCode as Code;

#[test]
fn test_nil_coalesce_noop_fix_drops_nil_fallback() {
    let source = r"
pipeline default(task) {
  const value = task?.flag ?? nil
  log(value)
}
";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "nil-coalesce-noop"), 1, "{diags:?}");
    let fix = get_fix(&diags, "nil-coalesce-noop").expect("fix");
    assert_eq!(fix.len(), 1);
    assert_eq!(fix[0].replacement, "");

    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const value = task?.flag\n"),
        "expected nil fallback to be removed, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_nil_coalesce_noop_is_error_by_default() {
    let source = r"
pipeline default(task) {
  const value = task?.flag ?? nil
  log(value)
}
";
    let diags = lint_source(source);
    let diag = diags
        .iter()
        .find(|diag| diag.rule == "nil-coalesce-noop")
        .expect("nil coalesce diag");
    assert_eq!(diag.severity, LintSeverity::Error);
}

#[test]
fn test_nil_coalesce_noop_ignores_real_fallbacks() {
    let source = r#"
pipeline default(task) {
  const value = task?.flag ?? "off"
  log(value)
}
"#;
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "nil-coalesce-noop"),
        "non-nil fallback should not trigger: {diags:?}"
    );
}

#[test]
fn test_nil_coalesce_self_fallback_fix_drops_identity_fallback() {
    let source = r"
pipeline default(task) {
  const value = task ?? task
  log(value)
}
";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "nil-coalesce-self-fallback"),
        1,
        "{diags:?}"
    );
    let diag = diags
        .iter()
        .find(|diag| diag.rule == "nil-coalesce-self-fallback")
        .expect("self fallback diag");
    assert_eq!(diag.severity, LintSeverity::Error);
    assert_eq!(diag.code, Code::LintNilCoalesceSelfFallback);

    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const value = task\n"),
        "expected self fallback to be removed, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_nil_coalesce_self_fallback_ignores_repeated_effectful_calls() {
    let source = r"
pipeline default(task) {
  const value = load() ?? load()
  log(value)
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "nil-coalesce-self-fallback"),
        "repeated calls may have effects and should not trigger: {diags:?}"
    );
}
