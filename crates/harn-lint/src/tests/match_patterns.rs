//! Match-arm patterns are not call sites.
//!
//! Call-shaped patterns (`Circle(radius)`) parse as `FunctionCall`, so the
//! undefined-function rule used to report the variant as a missing function.
//! Classifying a bare head needs the enum catalog and the imported-enum
//! surface, which only the typechecker has; these tests pin that the lint no
//! longer guesses.

use super::*;

#[test]
fn bare_variant_pattern_is_not_an_undefined_function() {
    let diags = lint_source(
        r#"
enum Shape { Circle(radius: int) Square(width: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Circle(r) -> { return "circle" }
    Square(w) -> { return "square" }
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "undefined-function"),
        "bare enum-variant patterns are not function calls: {diags:?}"
    );
}

#[test]
fn qualified_variant_pattern_is_not_an_undefined_function() {
    let diags = lint_source(
        r#"
enum Shape { Circle(radius: int) Square(width: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Shape.Circle(r) -> { return "circle" }
    Shape.Square(w) -> { return "square" }
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "undefined-function"),
        "qualified variant patterns are not function calls: {diags:?}"
    );
}

#[test]
fn builtin_result_variant_patterns_are_not_undefined_functions() {
    let diags = lint_source(
        r"
pub fn unwrap_or_zero(r: Result<int, string>) -> int {
  match r {
    Ok(value) -> { return value }
    Err(_e) -> { return 0 }
  }
}
",
    );
    assert!(
        !has_rule(&diags, "undefined-function"),
        "builtin Result patterns are not function calls: {diags:?}"
    );
}

#[test]
fn variant_pattern_head_does_not_trigger_call_shaped_rules() {
    // `assert_eq` in *call* position outside a test pipeline warns. In pattern
    // position the same shape names a variant, so the call-shaped rules that
    // ride along with `FunctionCall` must not fire either.
    let diags = lint_source(
        r#"
enum Check { assert_eq(lhs: int) }

pub fn describe(c: Check) -> string {
  match c {
    assert_eq(v) -> { return "eq" }
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "assert-outside-test"),
        "pattern heads are not calls, so call-shaped rules must not fire: {diags:?}"
    );
}

#[test]
fn expression_equality_pattern_marks_its_function_used() {
    // A call-shaped pattern head that is *not* a variant names a real
    // function. Withholding the call-site record must not also withhold the
    // reference, or the unused-function rule fires on a function that is used.
    let diags = lint_source(
        r#"
fn code_red() -> int { return 3 }

pipeline test(task) {
  match 3 {
    code_red() -> { log("red") }
    _ -> { log("other") }
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "a function used as an expression-equality pattern is used: {diags:?}"
    );
}

#[test]
fn undefined_call_inside_a_pattern_payload_is_still_reported() {
    // Only the pattern *head* is exempt. A genuine call nested in a guard
    // still flows through ordinary expression linting.
    let diags = lint_source(
        r#"
enum Shape { Circle(radius: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Circle(r) if totally_undefined_fn(r) -> { return "circle" }
    _ -> { return "other" }
  }
}
"#,
    );
    assert!(
        has_rule(&diags, "undefined-function"),
        "calls in guards are still calls: {diags:?}"
    );
}
