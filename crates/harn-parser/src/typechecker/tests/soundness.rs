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
const x: int = 1
const y: string = x
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
  const x = 1
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
  const x = 1
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
  const r = llm_call("Grade this", nil, {output_schema: schema, output_validation: "error"})
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
  const r = llm_call("Grade this", nil, {output_schema: schema})
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
  const data = json_parse("{}")
  log(data.name)
}"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let diagnostics = TypeChecker::new().check_strict_with_source(&program, source);
    assert!(
        diagnostics.iter().any(
            |diagnostic| diagnostic.severity == DiagnosticSeverity::Error
                && diagnostic.message.contains("unvalidated")
        ),
        "expected strict unvalidated error, got: {diagnostics:?}"
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

#[test]
fn arithmetic_on_nullable_operand_is_flagged() {
    // `x + 1` where `x: int?` is a definite runtime fault — flag it at check
    // time rather than letting `nil + 1` throw.
    let errs = errors("fn g(x: int?) -> int { return x + 1 }");
    assert!(
        errs.iter().any(|e| e.contains("may be nil")),
        "expected nilable-operand error, got: {errs:?}"
    );
}

#[test]
fn arithmetic_after_assignment_narrowing_is_clean() {
    // Assigning a non-nil value narrows the binding, so `x + 1` is fine.
    let errs = errors(
        r"
fn g() -> int {
  let x: int? = nil
  x = 5
  return x + 1
}
",
    );
    assert!(
        errs.is_empty(),
        "expected no errors after narrowing, got: {errs:?}"
    );
}

#[test]
fn arithmetic_after_guard_narrowing_is_clean() {
    let errs = errors(
        r"
fn g(x: int?) -> int {
  if x == nil { return 0 }
  return x + 1
}
",
    );
    assert!(
        errs.is_empty(),
        "expected no errors after guard, got: {errs:?}"
    );
}

#[test]
fn property_access_after_assignment_narrowing_is_clean() {
    // Path narrowing: a nilable field/let assigned a concrete value reads as
    // non-nil afterward.
    let errs = errors(
        r"
struct Foo { v: int }
fn g() -> int {
  let f: Foo? = nil
  f = Foo {v: 3}
  return f.v
}
",
    );
    assert!(
        errs.is_empty(),
        "expected no errors after path narrowing, got: {errs:?}"
    );
}

#[test]
fn assert_condition_narrows_like_a_guard() {
    // `assert(cond, msg?)` throws when `cond` is falsy, so code after it may
    // rely on the truthy refinement — `assert(x != nil)` then `x - 1` is fine.
    let errs =
        errors("fn g(x: float?) -> float {\n  assert(x != nil, \"nn\")\n  return x - 1.0\n}");
    assert!(errs.is_empty(), "assert should narrow, got: {errs:?}");
}

#[test]
fn require_condition_narrows_after_statement() {
    let errs = errors("fn g(x: int?) -> int { require x != nil\n  return x + 1 }");
    assert!(errs.is_empty(), "require should narrow, got: {errs:?}");
}

#[test]
fn arithmetic_without_assert_guard_still_flagged() {
    // Sanity: the narrowing is gated on the assert/require actually being
    // present — a bare nilable operand is still an error.
    let errs = errors("fn g(x: int?) -> int { return x + 1 }");
    assert!(
        errs.iter().any(|e| e.contains("may be nil")),
        "expected nilable error without a guard, got: {errs:?}"
    );
}

#[test]
fn test_list_subscript_write_checks_element_type() {
    let errs = errors(
        r#"fn f() -> int {
  const xs: list<int> = [1, 2]
  xs[0] = "not an int"
  return xs[0]
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "expected element-type mismatch, got: {errs:?}"
    );
}

#[test]
fn test_dict_subscript_write_checks_value_and_key_types() {
    let errs = errors(
        r#"fn f() -> int {
  const d: dict<string, int> = {a: 1}
  d["b"] = "nope"
  d[0] = 2
  return 0
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "expected value-type mismatch, got: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("subscript index") && e.contains("expected string, found int")),
        "expected index-type mismatch, got: {errs:?}"
    );
}

#[test]
fn test_shape_field_write_checks_field_type() {
    let errs = errors(
        r#"fn f() -> int {
  const s: {n: int} = {n: 1}
  s.n = "nope"
  return s.n
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "expected field-type mismatch, got: {errs:?}"
    );
}

#[test]
fn test_struct_field_write_checks_type_and_existence() {
    let errs = errors(
        r#"struct Point { x: int, y: int }

fn f() -> int {
  const p = Point({x: 1, y: 2})
  p.x = "nope"
  p.z = 1
  return p.x
}"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("expected int, found string")),
        "expected field-type mismatch, got: {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("field `z` does not exist")),
        "expected unknown-field error, got: {errs:?}"
    );
}

#[test]
fn test_optional_shape_field_write_accepts_nil() {
    let errs = errors(
        r#"fn f() -> int {
  const s: {n: int, m?: string} = {n: 1}
  s.m = nil
  s.m = "ok"
  return s.n
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_unannotated_dict_literal_writes_stay_lenient() {
    // The ambient dict idiom: an unannotated dict/list local tolerates
    // heterogeneous writes, matching read-side leniency.
    let errs = errors(
        r#"pipeline t(task) {
  const d = {a: 1}
  d.b = "hello"
  d["c"] = true
  const xs = [1, 2]
  xs[0] = "loose"
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_compound_assignment_to_list_element_type_checks() {
    let errs = errors(
        r"fn f() -> int {
  const xs: list<int> = [1, 2]
  xs[0] += 1
  return xs[0]
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

// --- HARN-NAM-005: method existence on concrete receivers -----------------

/// Diagnostics carrying the method-existence code, by message, for the given
/// source. Asserting on `Code::UnknownMethod` keeps these deterministic and
/// free of line-number / prose coupling.
fn nam_005(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.code == crate::diagnostic_codes::Code::UnknownMethod)
        .map(|d| d.message)
        .collect()
}

#[test]
fn unknown_method_on_annotated_float_is_rejected() {
    let diags = nam_005(
        r"fn f() {
  const x: float = 3.14
  x.frobnicate()
}",
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
    assert!(diags[0].contains("frobnicate"), "{diags:?}");
}

#[test]
fn unknown_method_on_int_field_is_rejected() {
    let diags = nam_005(
        r"struct User { age: int }
fn f(u: User) {
  u.age.frobnicate()
}",
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
}

#[test]
fn unknown_method_on_string_is_rejected_with_suggestion() {
    let diags = nam_005(
        r#"fn f() {
  const s: string = "hi"
  s.uppercas()
}"#,
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
    assert!(diags[0].contains("did you mean `uppercase`"), "{diags:?}");
}

#[test]
fn unknown_method_on_list_is_rejected() {
    let diags = nam_005(
        r"fn f() {
  const xs: list<int> = [1, 2]
  xs.frobnicate()
}",
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
}

#[test]
fn unknown_method_on_struct_is_rejected_with_suggestion() {
    let diags = nam_005(
        r"struct Point { x: int, y: int }
impl Point {
  fn distance(self) -> int { return self.x }
}
fn f(p: Point) {
  p.distanse()
}",
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
    assert!(diags[0].contains("did you mean `distance`"), "{diags:?}");
}

#[test]
fn valid_methods_on_concrete_receivers_do_not_error() {
    let errs = errors(
        r#"struct Point { x: int }
impl Point {
  fn render(self) -> string { return "p" }
}
fn f(p: Point) {
  const s: string = "hi"
  s.uppercase()
  const xs: list<int> = [1, 2]
  xs.reverse()
  xs.count()
  xs.iter()
  p.render()
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn method_on_gradual_receiver_defers_to_runtime() {
    // Unannotated / unknown receivers stay gradual: no static rejection.
    let diags = nam_005(
        r"fn f(x) {
  x.frobnicate()
}",
    );
    assert!(diags.is_empty(), "gradual receiver should defer: {diags:?}");
}

#[test]
fn method_on_dict_receiver_defers_to_runtime() {
    // A dict can hold a callable under a key and be invoked as `d.field()`,
    // so dict receivers are never statically rejected.
    let diags = nam_005(
        r"fn f(d: dict<string, string>) {
  d.frobnicate()
}",
    );
    assert!(diags.is_empty(), "dict receiver should defer: {diags:?}");
}

#[test]
fn recognized_method_name_on_number_is_not_flagged() {
    // The permissive number tier only rejects names unknown on every builtin;
    // a real builtin method name (even if odd on a number) is tolerated.
    let diags = nam_005(
        r"fn f() {
  const n: int = 5
  n.to_string()
}",
    );
    assert!(
        diags.is_empty(),
        "known method name should defer: {diags:?}"
    );
}
