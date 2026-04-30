//! Clippy-tier hygiene rules: redundant clones, constant/self comparisons,
//! destructuring-only unused bindings, and let-then-return.

use super::*;

#[test]
fn test_redundant_clone_passed_by_value_autofix() {
    let source = "pipeline default(task) {\n  let data = [1, 2]\n  log(data.clone())\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "redundant-clone"),
        "expected redundant-clone, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("log(data)"),
        "expected clone wrapper to be removed, got: {fixed}"
    );
}

#[test]
fn test_redundant_clone_dropped_autofix() {
    let source = "pipeline default(task) {\n  let data = [1, 2]\n  drop(data.clone())\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "redundant-clone"),
        "expected redundant-clone for dropped clone, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("drop(data)"),
        "expected dropped clone wrapper to be removed, got: {fixed}"
    );
}

#[test]
fn test_redundant_clone_ignores_named_clone_binding() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let data = [1, 2]
  let copied = data.clone()
  log(copied)
  log(data)
}
"#,
    );
    assert!(
        !has_rule(&diags, "redundant-clone"),
        "binding a clone for later use should not trigger redundant-clone: {diags:?}"
    );
}

#[test]
fn test_pointless_self_comparison_autofix() {
    let source = "pipeline default(task) {\n  let always = task == task\n  log(always)\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "pointless-comparison"),
        "expected pointless-comparison, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("let always = true"),
        "expected self-comparison to become true, got: {fixed}"
    );
}

#[test]
fn test_pointless_if_constant_condition() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  if false {
    log(task)
  }
}
"#,
    );
    assert!(
        has_rule(&diags, "pointless-comparison"),
        "expected constant if condition warning, got: {diags:?}"
    );
}

#[test]
fn test_pointless_comparison_ignores_different_expressions() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let other = "x"
  let same = task == other
  log(same)
}
"#,
    );
    assert!(
        !has_rule(&diags, "pointless-comparison"),
        "different operands should not trigger pointless-comparison: {diags:?}"
    );
}

#[test]
fn test_unused_pattern_binding_for_destructuring() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let { a, b } = { a: 1, b: 2 }
  log(a)
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-pattern-binding"),
        "expected unused-pattern-binding, got: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "destructuring names should use unused-pattern-binding, got: {diags:?}"
    );
}

#[test]
fn test_unused_pattern_binding_underscore_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let { a, b: _b } = { a: 1, b: 2 }
  log(a)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-pattern-binding"),
        "underscore-prefixed pattern binding should be ignored: {diags:?}"
    );
}

#[test]
fn test_let_then_return_autofix() {
    let source = "pipeline default(task) {\n  fn answer() {\n    let value = 1 + 2\n    return value\n  }\n  log(answer())\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "let-then-return"),
        "expected let-then-return, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("return 1 + 2"),
        "expected direct return fix, got: {fixed}"
    );
    assert!(
        !fixed.contains("let value = 1 + 2"),
        "expected temporary binding to be removed, got: {fixed}"
    );
}

#[test]
fn test_let_then_return_ignores_typed_binding() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  fn answer() {
    let value: int = 1 + 2
    return value
  }
  log(answer())
}
"#,
    );
    assert!(
        !has_rule(&diags, "let-then-return"),
        "typed temporary should preserve its annotation: {diags:?}"
    );
}
