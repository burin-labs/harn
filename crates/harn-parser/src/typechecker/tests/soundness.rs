//! Regression tests for type-system holes that accepted programs the VM
//! would later execute with a different value shape than the annotation
//! promised.

use super::*;
use crate::Parser;
use harn_lexer::Lexer;

#[test]
fn top_level_bindings_replace_forward_placeholders_with_real_types() {
    let errs = errors(
        r"
let x: int = 1
let y: string = x
",
    );
    assert_eq!(errs.len(), 1, "expected top-level mismatch, got: {errs:?}");
    assert!(errs[0].contains("expected string, found int"), "{errs:?}");
}

#[test]
fn unconstrained_generic_param_cannot_flow_to_concrete_return() {
    let errs = errors(
        r"
fn bad<T>(x: T) -> int {
  return x
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected generic return mismatch, got: {errs:?}"
    );
    assert!(errs[0].contains("expected int, found T"), "{errs:?}");
}

#[test]
fn concrete_value_cannot_flow_to_unconstrained_generic_return() {
    let errs = errors(
        r"
fn bad<T>() -> T {
  return 1
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected generic return mismatch, got: {errs:?}"
    );
    assert!(errs[0].contains("expected T, found int"), "{errs:?}");
}

#[test]
fn bare_return_rejects_non_nil_return_type() {
    let errs = errors(
        r"
fn bad() -> int {
  return
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected bare-return mismatch, got: {errs:?}"
    );
    assert!(errs[0].contains("expected int, found nil"), "{errs:?}");
}

#[test]
fn non_nil_return_type_rejects_fallthrough() {
    let errs = errors(
        r"
fn bad() -> int {
  let x = 1
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected missing-return error, got: {errs:?}"
    );
    assert!(
        errs[0].contains("can fall through without returning int"),
        "{errs:?}"
    );
}

#[test]
fn partial_return_path_rejects_fallthrough() {
    let errs = errors(
        r"
fn bad(flag: bool) -> int {
  if flag {
    return 1
  }
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected missing-return error, got: {errs:?}"
    );
    assert!(
        errs[0].contains("can fall through without returning int"),
        "{errs:?}"
    );
}

#[test]
fn typed_pipeline_checks_final_expression_type() {
    let errs = errors(
        r#"
pipeline test(task) -> int {
  "wrong"
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected pipeline result mismatch, got: {errs:?}"
    );
    assert!(
        errs[0].contains("pipeline result: expected int, found string"),
        "{errs:?}"
    );
}

#[test]
fn typed_pipeline_rejects_nil_fallthrough() {
    let errs = errors(
        r"
pipeline test(task) -> int {
  let x = 1
}
",
    );
    assert_eq!(
        errs.len(),
        1,
        "expected pipeline nil result mismatch, got: {errs:?}"
    );
    assert!(
        errs[0].contains("pipeline result: expected int, found nil"),
        "{errs:?}"
    );
}

#[test]
fn tool_body_final_expression_satisfies_return_type() {
    let errs = errors(
        r#"
pipeline test(task) {
  tool greet(name: string) -> string {
    "Hello, " + name
  }
}
"#,
    );
    assert!(errs.is_empty(), "unexpected tool result error: {errs:?}");
}

#[test]
fn tool_body_final_expression_type_is_checked() {
    let errs = errors(
        r#"
pipeline test(task) {
  tool bad() -> int {
    "wrong"
  }
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected tool result mismatch, got: {errs:?}"
    );
    assert!(
        errs[0].contains("tool result: expected int, found string"),
        "{errs:?}"
    );
}

#[test]
fn return_type_checks_recurse_into_match_arms() {
    let errs = errors(
        r#"
fn bad(value: string) -> int {
  match value {
    _ -> { return "wrong" }
  }
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected match-arm return mismatch, got: {errs:?}"
    );
    assert!(errs[0].contains("expected int, found string"), "{errs:?}");
}

#[test]
fn return_type_checks_recurse_into_try_expr_body() {
    let errs = errors(
        r#"
fn bad() -> int {
  try {
    return "wrong"
  }
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected try-body return mismatch, got: {errs:?}"
    );
    assert!(errs[0].contains("expected int, found string"), "{errs:?}");
}

#[test]
fn exhaustive_match_return_arms_satisfy_return_type() {
    let errs = errors(
        r#"
type Verdict = "pass" | "fail"

fn classify(v: Verdict) -> string {
  match v {
    "pass" -> { return "ok" }
    "fail" -> { return "no" }
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "unexpected exhaustive-match error: {errs:?}"
    );
}

#[test]
fn exhaustive_match_return_arms_prevent_later_fallthrough() {
    let errs = errors(
        r#"
type Verdict = "pass" | "fail"

fn classify(v: Verdict) -> string {
  match v {
    "pass" -> { return "ok" }
    "fail" -> { return "no" }
  }
  log("unreachable")
}
"#,
    );
    assert!(
        errs.is_empty(),
        "unexpected fallthrough after exhaustive match: {errs:?}"
    );
}

#[test]
fn non_exhaustive_match_return_arms_do_not_hide_fallthrough() {
    let errs = errors(
        r#"
fn classify(v: string) -> int {
  match v {
    "one" -> { return 1 }
  }
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected missing-return error, got: {errs:?}"
    );
    assert!(
        errs[0].contains("can fall through without returning int"),
        "{errs:?}"
    );
}

#[test]
fn generic_enum_match_requires_all_variants() {
    let errs = errors(
        r"
fn unwrap_ok<T, E>(result: Result<T, E>) -> T {
  match result {
    Result.Ok(value) -> { return value }
  }
}
",
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("Non-exhaustive match") && err.contains("\"Err\"")),
        "expected generic enum exhaustiveness error, got: {errs:?}"
    );
}

#[test]
fn never_tail_expression_satisfies_return_type() {
    let errs = errors(
        r#"
fn exhaustive(x: string | int) -> string {
  if type_of(x) == "string" {
    return "string"
  }
  if type_of(x) == "int" {
    return "int"
  }
  unreachable(x)
}
"#,
    );
    assert!(errs.is_empty(), "unexpected never-tail error: {errs:?}");
}

#[test]
fn schema_typed_llm_call_data_flows_through_generic_wrapper() {
    let errs = errors(
        r#"
type GraderOut = {verdict: "pass" | "fail", summary: string}

fn grade<T>(schema: Schema<T>) -> T {
  let r = llm_call("Grade this", nil, {output_schema: schema, output_validation: "error"})
  return r.data
}

fn use_grade() -> GraderOut {
  return grade(schema_of(GraderOut))
}
"#,
    );
    assert!(errs.is_empty(), "unexpected schema wrapper error: {errs:?}");
}

#[test]
fn schema_typed_llm_call_data_stays_optional_without_error_validation() {
    let errs = errors(
        r#"
type GraderOut = {verdict: "pass" | "fail", summary: string}

fn grade<T>(schema: Schema<T>) -> T {
  let r = llm_call("Grade this", nil, {output_schema: schema})
  return r.data
}
"#,
    );
    assert!(
        errs.iter().any(|err| err.contains("expected T, found T?")),
        "expected optional data mismatch, got: {errs:?}"
    );
}

#[test]
fn check_strict_with_source_enables_strict_mode() {
    let source = r#"pipeline t(task) {
  let data = json_parse("{}")
  log(data.name)
}"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let diagnostics = TypeChecker::new().check_strict_with_source(&program, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unvalidated")),
        "expected strict unvalidated warning, got: {diagnostics:?}"
    );
}

#[test]
fn interpolation_holes_are_type_checked() {
    let errs = errors(
        r#"
fn needs_int(p: int) -> int { return p + 1 }
pipeline p() { log("${needs_int("nope")}") }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "expected argument-type error inside interpolation, got: {errs:?}"
    );
}

#[test]
fn interpolation_holes_accept_valid_expressions() {
    // Well-typed holes must not produce spurious errors.
    let errs = errors(
        r#"
fn p(name: string, n: int) -> string { return "hi ${name} ${n + 1}" }
"#,
    );
    assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
}

#[test]
fn exhaustive_bool_match_does_not_report_fallthrough() {
    // A `match` on a bool covering both `true` and `false` with returning
    // arms is exhaustive and terminating — no wildcard required.
    let errs = errors(
        r#"
fn classify(b: bool) -> string {
  match b {
    true -> { return "yes" }
    false -> { return "no" }
  }
}
"#,
    );
    assert!(
        !errs.iter().any(|e| e.contains("fall through")),
        "exhaustive bool match should not report fall-through, got: {errs:?}"
    );
}

#[test]
fn partial_bool_match_still_can_fall_through() {
    // Only `true` is covered (no `false`, no wildcard) — still able to fall
    // through, so the missing-return diagnostic must remain.
    let errs = errors(
        r#"
fn classify(b: bool) -> string {
  match b {
    true -> { return "yes" }
  }
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("fall through")),
        "partial bool match should still report fall-through, got: {errs:?}"
    );
}
