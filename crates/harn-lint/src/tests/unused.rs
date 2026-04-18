//! `unused-variable` and `unused-parameter` coverage, plus their
//! autofix variants. The cross-rule `test_multiple_rules` test lives
//! here because its primary anchor is the unused-variable diagnostic.

use super::*;

#[test]
fn test_unused_variable() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let unused = 42
log("hello")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-variable"),
        "expected unused-variable warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_underscore_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let _ = 42
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "underscore variables should not trigger unused-variable: {diags:?}"
    );
}

#[test]
fn test_unused_fn_param() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn greet(name, unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused fn param, got: {diags:?}"
    );
    // Should NOT trigger unused-variable (parameters are tracked separately).
    assert!(
        !has_rule(&diags, "unused-variable"),
        "unused fn param should not trigger unused-variable: {diags:?}"
    );
}

#[test]
fn test_unused_closure_param() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let f = { x, y -> log(x) }
f(1, 2)
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused closure param, got: {diags:?}"
    );
}

#[test]
fn test_unused_param_underscore_prefix_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn greet(name, _unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "underscore-prefixed params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_used_fn_param_ok() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn add(a, b) {
    return a + b
}
log(add(1, 2))
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "used params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_multiple_rules() {
    let diags = lint_source(
        r#"
pipeline default(task) {
var unused = 1
return 0
log("dead")
}
"#,
    );
    assert!(has_rule(&diags, "unused-variable"));
    assert!(has_rule(&diags, "mutable-never-reassigned"));
    assert!(has_rule(&diags, "unreachable-code"));
    assert_eq!(count_rule(&diags, "unreachable-code"), 1);
}

#[test]
fn test_fix_unused_variable_simple_let_binding() {
    let source = "pipeline default(task) {\n  let unused_thing = 42\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    assert!(has_rule(&diags, "unused-variable"));
    let fix = get_fix(&diags, "unused-variable");
    assert!(
        fix.is_some(),
        "expected autofix for simple let binding, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let _unused_thing = 42"),
        "expected `_unused_thing` prefix, got: {result}"
    );
    assert!(
        !result.contains("let unused_thing"),
        "original name should be replaced, got: {result}"
    );
}

#[test]
fn test_fix_unused_variable_simple_let_binding_with_type() {
    // Type annotation between the name and `=` must not confuse the scan.
    // We use `let` (not `var`) so the `mutable-never-reassigned` autofix
    // doesn't also fire and combine with this one.
    let source = "pipeline default(task) {\n  let leftover: int = 3\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable").expect("expected autofix");
    assert_eq!(fix.len(), 1, "expected single-edit fix");
    let edit = &fix[0];
    let renamed = {
        let before = &source[..edit.span.start];
        let after = &source[edit.span.end..];
        format!("{before}{}{after}", edit.replacement)
    };
    assert!(
        renamed.contains("let _leftover: int = 3"),
        "expected `_leftover: int` prefix, got: {renamed}"
    );
    assert!(
        !renamed.contains("let leftover:"),
        "original name should be replaced, got: {renamed}"
    );
}

#[test]
fn test_no_fix_for_unused_variable_in_dict_destructuring() {
    // Destructuring patterns are intentionally not autofixed today — the
    // rename would need a per-field span we do not currently track. The
    // diagnostic must still fire with a suggestion so the user can fix
    // manually.
    let source = "pipeline default(task) {\n  let { a, b } = { a: 1, b: 2 }\n  log(a)\n}";
    let diags = lint_source(source);
    let unused: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "unused-variable")
        .collect();
    assert!(
        unused.iter().any(|d| d.message.contains("`b`")),
        "expected unused-variable for `b`, got: {diags:?}"
    );
    for diag in &unused {
        if diag.message.contains("`b`") {
            assert!(
                diag.fix.is_none(),
                "destructuring unused-variable must not autofix, got: {:?}",
                diag.fix
            );
            assert!(
                diag.suggestion.is_some(),
                "destructuring unused-variable must keep its suggestion"
            );
        }
    }
}

#[test]
fn test_fix_unused_variable_is_word_boundary_safe() {
    // The variable name also appears in the RHS expression. The autofix
    // must only rewrite the binding occurrence, not the reference inside
    // the initializer, so the resulting source still parses.
    let source =
        "pipeline default(task) {\n  let threshold_ms = threshold_ms_default()\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable");
    assert!(fix.is_some(), "expected autofix, got: {diags:?}");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let _threshold_ms = threshold_ms_default()"),
        "expected only the LHS binding renamed, got: {result}"
    );
}
