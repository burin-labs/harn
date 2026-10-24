//! Match-arm patterns are not call sites.
//!
//! Call-shaped patterns (`Circle(radius)`) parse as `FunctionCall`, so the
//! undefined-function rule used to report the variant as a missing function.
//! Classifying a bare head needs the enum catalog and the imported-enum
//! surface, which only the typechecker has; these tests pin that the lint no
//! longer guesses.

use super::*;

#[rstest::rstest]
#[case::bare_variant_pattern_is_not_an_undefined_function(
    r#"
enum Shape { Circle(radius: int) Square(width: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Circle(r) -> { return "circle" }
    Square(w) -> { return "square" }
  }
}
"#,
    "undefined-function",
    false
)]
#[case::qualified_variant_pattern_is_not_an_undefined_function(
    r#"
enum Shape { Circle(radius: int) Square(width: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Shape.Circle(r) -> { return "circle" }
    Shape.Square(w) -> { return "square" }
  }
}
"#,
    "undefined-function",
    false
)]
#[case::builtin_result_variant_patterns_are_not_undefined_functions(
    r"
pub fn unwrap_or_zero(r: Result<int, string>) -> int {
  match r {
    Ok(value) -> { return value }
    Err(_e) -> { return 0 }
  }
}
",
    "undefined-function",
    false
)]
// `assert_eq` in *call* position outside a test pipeline warns. In pattern
// position the same shape names a variant, so the call-shaped rules that
// ride along with `FunctionCall` must not fire either.
#[case::variant_pattern_head_does_not_trigger_call_shaped_rules(
    r#"
enum Check { assert_eq(lhs: int) }

pub fn describe(c: Check) -> string {
  match c {
    assert_eq(v) -> { return "eq" }
  }
}
"#,
    "assert-outside-test",
    false
)]
// A call-shaped pattern head that is *not* a variant names a real
// function. Withholding the call-site record must not also withhold the
// reference, or the unused-function rule fires on a function that is used.
#[case::expression_equality_pattern_marks_its_function_used(
    r#"
fn code_red() -> int { return 3 }

pipeline test(task) {
  match 3 {
    code_red() -> { log("red") }
    _ -> { log("other") }
  }
}
"#,
    "unused-function",
    false
)]
// Only the pattern *head* is exempt. A genuine call nested in a guard
// still flows through ordinary expression linting.
#[case::undefined_call_inside_a_pattern_payload_is_still_reported(
    r#"
enum Shape { Circle(radius: int) }

pub fn describe(s: Shape) -> string {
  match s {
    Circle(r) if totally_undefined_fn(r) -> { return "circle" }
    _ -> { return "other" }
  }
}
"#,
    "undefined-function",
    true
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}
