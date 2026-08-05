//! `redundant-nil-ternary` — ternary-to-`??` rewrite plus guards
//! against false positives where the rewrite would change semantics.

use super::*;

#[test]
fn test_fix_redundant_nil_ternary_eq_pattern() {
    let source = r"
pipeline default(task) {
  const x = 5
  const y = x == nil ? 0 : x
  log(y)
}
";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "redundant-nil-ternary");
    assert!(
        fix.is_some(),
        "expected autofix for `x == nil ? 0 : x`, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const y = x ?? 0"),
        "expected `x ?? 0`, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_fix_redundant_nil_ternary_ne_pattern() {
    let source = r"
pipeline default(task) {
  const x = 5
  const y = x != nil ? x : 0
  log(y)
}
";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "redundant-nil-ternary");
    assert!(
        fix.is_some(),
        "expected autofix for `x != nil ? x : 0`, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const y = x ?? 0"),
        "expected `x ?? 0`, got: {result}"
    );
}

#[test]
fn test_no_warn_for_unrelated_ternary() {
    let source = r"
pipeline default(task) {
  const a = 1
  const b = 2
  const c = a > b ? a : b
  log(c)
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "redundant-nil-ternary"),
        "unrelated ternary should not trigger redundant-nil-ternary, got: {diags:?}"
    );
}

#[test]
fn test_no_warn_when_non_nil_arm_differs_from_checked_var() {
    // `x != nil ? y : z` — the non-nil arm is NOT `x`, so the rewrite
    // would change semantics. Lint must stay silent.
    let source = r"
pipeline default(task) {
  const x = 1
  const y = 2
  const z = 3
  const w = x != nil ? y : z
  log(w)
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "redundant-nil-ternary"),
        "rewrite would change semantics, lint must be silent, got: {diags:?}"
    );
}
