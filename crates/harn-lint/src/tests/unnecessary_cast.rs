//! `unnecessary-cast` — flags conversion-builtin calls whose argument is
//! already syntactically of the target type, plus chained identity
//! conversions like `to_string(to_string(x))`. Includes guards against
//! firing on legitimate conversions (`to_int("42")`, `to_float(5)`).

use super::*;

#[test]
fn warn_to_string_on_string_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let s = to_string("hello")
  log(s)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
}

#[test]
fn warn_to_int_on_int_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = to_int(42)
  log(n)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
}

#[test]
fn warn_to_float_on_float_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = to_float(1.5)
  log(n)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
}

#[test]
fn warn_to_list_on_list_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let xs = to_list([1, 2, 3])
  log(xs)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
}

#[test]
fn warn_to_dict_on_dict_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let d = to_dict({a: 1, b: 2})
  log(d)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
}

#[test]
fn warn_to_string_on_interpolated_string() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let name = "world"
  let s = to_string("hello ${name}")
  log(s)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast on interpolated string, got: {diags:?}"
    );
}

#[test]
fn warn_chained_to_string_calls() {
    // The OUTER call is the redundant one — the inner `to_string(x)` may
    // be load-bearing, but wrapping it again is always a no-op.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = 5
  let s = to_string(to_string(n))
  log(s)
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast on chained to_string calls, got: {diags:?}"
    );
}

#[test]
fn no_warn_on_genuine_int_to_string() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = 5
  let s = to_string(n)
  log(s)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "should not warn on genuine identifier-to-string cast: {diags:?}"
    );
}

#[test]
fn no_warn_on_string_to_int() {
    // `to_int("42")` is a real parse, not a cast. Must not fire.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = to_int("42")
  log(n)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "should not warn on string-to-int parse: {diags:?}"
    );
}

#[test]
fn no_warn_on_int_to_float() {
    // `to_float(5)` widens int to float. Not a no-op.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = to_float(5)
  log(n)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "should not warn on int-to-float widening: {diags:?}"
    );
}

#[test]
fn no_warn_on_float_to_int() {
    // `to_int(1.5)` truncates. Not a no-op.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let n = to_int(1.5)
  log(n)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "should not warn on float-to-int truncation: {diags:?}"
    );
}

#[test]
fn no_warn_on_to_list_of_set_call() {
    // `to_list(set(...))` materializes a set as a list. Real conversion.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let xs = to_list(set([1, 2, 3]))
  log(xs)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "should not warn on set-to-list materialization: {diags:?}"
    );
}

#[test]
fn fix_to_string_on_string_literal() {
    let source = "pipeline default(task) {\n  let s = to_string(\"hello\")\n  log(s)\n}";
    let diags = lint_source(source);
    assert!(
        get_fix(&diags, "unnecessary-cast").is_some(),
        "expected autofix, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let s = \"hello\""),
        "expected `let s = \"hello\"`, got: {result}"
    );
    assert!(
        !result.contains("to_string"),
        "to_string should be removed, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn fix_to_int_on_int_literal() {
    let source = "pipeline default(task) {\n  let n = to_int(42)\n  log(n)\n}";
    let diags = lint_source(source);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let n = 42"),
        "expected `let n = 42`, got: {result}"
    );
}

#[test]
fn fix_to_list_on_list_literal_preserves_inner_formatting() {
    let source = "pipeline default(task) {\n  let xs = to_list([1, 2, 3])\n  log(xs)\n}";
    let diags = lint_source(source);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let xs = [1, 2, 3]"),
        "expected list literal with original spacing preserved, got: {result}"
    );
}

#[test]
fn fix_chained_to_string_collapses_one_layer() {
    // Both calls are flagged (outer collapses to inner; if the lint
    // re-runs on the result the now-outer call is no longer redundant
    // because its argument is a bare identifier). The right-to-left,
    // overlap-dropping fix application keeps only the outer one.
    let source =
        "pipeline default(task) {\n  let n = 5\n  let s = to_string(to_string(n))\n  log(s)\n}";
    let diags = lint_source(source);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let s = to_string(n)"),
        "expected outer to_string removed, got: {result}"
    );
}

#[test]
fn no_warn_on_zero_or_multi_arg_calls() {
    // Defensive: a wrong-arity call is not a cast — leave it alone.
    let diags = lint_source(
        r#"
pipeline default(task) {
  let s = to_string()
  let t = to_string(1, 2)
  log(s)
  log(t)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-cast"),
        "wrong-arity calls must not trigger unnecessary-cast: {diags:?}"
    );
}
