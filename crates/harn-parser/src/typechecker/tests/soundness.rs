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

#[rstest::rstest]
#[case::tool_body_final_expression_satisfies_return_type(
    r#"
pipeline test(task) {
  tool greet(name: string) -> string {
    "Hello, " + name
  }
}
"#,
    true
)]
#[case::exhaustive_match_return_arms_satisfy_return_type(
    r#"
type Verdict = "pass" | "fail"

fn classify(v: Verdict) -> string {
  match v {
    "pass" -> { return "ok" }
    "fail" -> { return "no" }
  }
}
"#,
    true
)]
#[case::exhaustive_match_return_arms_prevent_later_fallthrough(
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
    true
)]
#[case::schema_typed_llm_call_data_flows_through_generic_wrapper(
    r#"
type GraderOut = {verdict: "pass" | "fail", summary: string}

fn grade<T>(schema: Schema<T>) -> T {
  const r = llm_call("Grade this", nil, {output: {schema: schema, validation: "error"}})
  return r.data
}

fn use_grade() -> GraderOut {
  return grade(schema_of(GraderOut))
}
"#,
    true
)]
// Well-typed holes must not produce spurious errors.
#[case::interpolation_holes_accept_valid_expressions(
    r#"
fn p(name: string, n: int) -> string { return "hi ${name} ${n + 1}" }
"#,
    true
)]
// Assigning a non-nil value narrows the binding, so `x + 1` is fine.
#[case::arithmetic_after_assignment_narrowing_is_clean(
    r"
fn g() -> int {
  let x: int? = nil
  x = 5
  return x + 1
}
",
    true
)]
#[case::arithmetic_after_guard_narrowing_is_clean(
    r"
fn g(x: int?) -> int {
  if x == nil { return 0 }
  return x + 1
}
",
    true
)]
// Path narrowing: a nilable field/let assigned a concrete value reads as
// non-nil afterward.
#[case::property_access_after_assignment_narrowing_is_clean(
    r"
struct Foo { v: int }
fn g() -> int {
  let f: Foo? = nil
  f = Foo {v: 3}
  return f.v
}
",
    true
)]
#[case::test_optional_shape_field_write_accepts_nil(
    r#"fn f() -> int {
  const s: {n: int, m?: string} = {n: 1}
  s.m = nil
  s.m = "ok"
  return s.n
}"#,
    true
)]
// The ambient dict idiom: an unannotated dict/list local tolerates
// heterogeneous writes, matching read-side leniency.
#[case::test_unannotated_dict_literal_writes_stay_lenient(
    r#"pipeline t(task) {
  const d = {a: 1}
  d.b = "hello"
  d["c"] = true
  const xs = [1, 2]
  xs[0] = "loose"
}"#,
    true
)]
// `xs[0] += 1` type-checks: the read/modify/write lands back in the `int`
// element slot. The bare `return xs[0]` would be `int?` under index
// soundness (an out-of-bounds read is `nil`), so the sound form coalesces
// the possibly-absent element before returning it.
#[case::test_compound_assignment_to_list_element_type_checks(
    r"fn f() -> int {
  const xs: list<int> = [1, 2]
  xs[0] += 1
  return xs[0] ?? 0
}",
    true
)]
#[case::valid_methods_on_concrete_receivers_do_not_error(
    r#"struct Point { x: int }
impl Point {
  fn render(self) -> string { return "p" }
}
fn f(p: Point) {
  const s: string = "hi"
  s.upper()
  s.reversed()
  let xs: list<int> = [1, 2]
  xs.reversed()
  xs.sorted()
  xs.sorted_by({ x -> x })
  xs.push(3)
  xs.pop()
  xs.len()
  xs.iter()
  let values: set<int> = set([1, 2])
  values.add(3)
  values.remove(1)
  let mapping: dict<string, int> = {a: 1}
  mapping.merge({b: 2})
  mapping.remove("a")
  mapping.map_keys({ key -> key })
  p.render()
}"#,
    true
)]
fn diagnostics_errors(#[case] source: &str, #[case] empty: bool) {
    let diagnostics = errors(source);
    assert_eq!(diagnostics.is_empty(), empty, "{diagnostics:?}");
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
fn schema_witness_accepts_only_runtime_schema_objects() {
    let valid = errors(
        r#"
type Text = string
fn string_schema() -> Schema<string> {
  return schema_of(Text)
}
fn merge_options(schema: dict) -> dict { return schema + {description: "value"} }
fn decorated_schema() -> dict { return merge_options(string_schema()) }
"#,
    );
    assert!(
        valid.is_empty(),
        "schema dict should establish a witness: {valid:?}"
    );

    let invalid = errors(
        r#"
fn string_schema() -> Schema<string> {
  return 1
}
fn forged_schema() -> Schema<string> {
  return {type: "int"}
}
fn structural_schema() -> Schema<string> {
  return {type: "string"}
}
fn opaque_schema(raw: dict) -> Schema<string> {
  return raw
}
"#,
    );
    assert!(
        invalid
            .iter()
            .any(|error| error.contains("expected Schema<string>, found int")),
        "non-schema values must not establish a witness: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .filter(|error| error.contains("expected Schema<string>") && error.contains("type:"))
            .count()
            >= 2,
        "schema-shaped records must not forge a witness: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|error| error.contains("expected Schema<string>, found dict")),
        "an opaque dict must not forge a witness: {invalid:?}"
    );
}

#[rstest::rstest]
#[case::schema_typed_llm_call_data_stays_optional_without_error_validation(
    r#"
type GraderOut = {verdict: "pass" | "fail", summary: string}

fn grade<T>(schema: Schema<T>) -> T {
  const r = llm_call("Grade this", nil, {output: schema})
  return r.data
}
"#,
    "expected T, found T?",
    true
)]
// Only `true` is covered (no `false`, no wildcard) — still able to fall
// through, so the missing-return diagnostic must remain.
#[case::partial_bool_match_still_can_fall_through(
    r#"
fn classify(b: bool) -> string {
  match b {
    true -> { return "yes" }
  }
}
"#,
    "fall through",
    true
)]
// `x + 1` where `x: int?` is a definite runtime fault — flag it at check
// time rather than letting `nil + 1` throw.
#[case::arithmetic_on_nullable_operand_is_flagged(
    "fn g(x: int?) -> int { return x + 1 }",
    "may be nil",
    true
)]
#[case::test_list_subscript_write_checks_element_type(
    r#"fn f() -> int {
  const xs: list<int> = [1, 2]
  xs[0] = "not an int"
  return xs[0]
}"#,
    "expected int, found string",
    true
)]
#[case::test_shape_field_write_checks_field_type(
    r#"fn f() -> int {
  const s: {n: int} = {n: 1}
  s.n = "nope"
  return s.n
}"#,
    "expected int, found string",
    true
)]
fn diagnostic_message_errors(#[case] source: &str, #[case] message: &str, #[case] present: bool) {
    let diagnostics = errors(source);
    assert_eq!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains(message)),
        present,
        "{message}: {diagnostics:?}"
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
    let source = r#"
fn needs_int(p: int) -> int { return p + 1 }
pipeline p() { log("${needs_int("nope")}") }
"#;
    let diagnostics = check_source_with_source(source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected int, found string")),
        "expected argument-type error inside interpolation, got: {diagnostics:?}"
    );
    let mismatch = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("expected int, found string"))
        .expect("interpolation argument mismatch");
    assert_eq!(
        mismatch.span.map(|span| span.start),
        source.find("\"nope\""),
        "interpolation diagnostics must carry absolute file byte offsets"
    );
}

#[test]
fn assert_condition_narrows_like_a_guard() {
    // `assert(cond, msg?)` throws when `cond` is falsy, so code after it may
    // rely on the truthy refinement. A direct nilable value and that value
    // coalesced with `false` have the same continuing-scope refinement.
    for condition in ["x", "x ?? false", "x != nil"] {
        let errs = errors(&format!(
            "fn g(x: float?) -> float {{\n  assert({condition}, \"nn\")\n  return x - 1.0\n}}"
        ));
        assert!(
            errs.is_empty(),
            "assert({condition}) should narrow, got: {errs:?}"
        );
    }

    for condition in ["x ?? true", "!(x ?? false)"] {
        let errs = errors(&format!(
            "fn g(x: float?) -> float {{\n  assert({condition})\n  return x - 1.0\n}}"
        ));
        assert!(
            !errs.is_empty(),
            "assert({condition}) must not prove x non-nil, got: {errs:?}"
        );
    }
}

#[test]
fn shadowed_assert_does_not_apply_builtin_refinement() {
    let source_shadow = errors(
        r"
fn assert(value: float?) -> bool { return true }
fn g(x: float?) -> float {
  assert(x ?? false)
  return x - 1.0
}
",
    );
    assert!(
        source_shadow
            .iter()
            .any(|error| error.contains("may be nil")),
        "source assert must not inherit builtin narrowing: {source_shadow:?}"
    );

    let import_shadow = check_source_with_imports(
        r"
fn g(x: float?) -> float {
  assert(x ?? false)
  return x - 1.0
}
",
        &["assert"],
    );
    assert!(
        import_shadow.iter().any(|diagnostic| {
            diagnostic.severity == DiagnosticSeverity::Error
                && diagnostic.message.contains("may be nil")
        }),
        "imported assert must not inherit builtin narrowing: {import_shadow:?}"
    );

    let binding_shadow = errors(
        r"
fn g(x: float?) -> float {
  const assert = { value -> true }
  assert(x ?? false)
  return x - 1.0
}
",
    );
    assert!(
        binding_shadow
            .iter()
            .any(|error| error.contains("may be nil")),
        "closure-bound assert must not inherit builtin narrowing: {binding_shadow:?}"
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
  s.lowr()
}"#,
    );
    assert_eq!(diags.len(), 1, "got: {diags:?}");
    assert!(diags[0].contains("did you mean `lower`"), "{diags:?}");
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
fn unknown_method_on_inferred_builtin_binding_is_rejected() {
    // An unannotated binding's inferred type is a binding contract, and the
    // VM crashes on an unknown method of a closed builtin, so inference alone
    // must hold the receiver to the method set (harn#9307).
    let diags = nam_005(
        r#"fn f() {
  const xs = [1, 2, 3]
  xs.take_last(2)
  let s = "hi"
  s.frobnicate()
  const n = 5
  n.frobnicate()
  const d = {a: 1}
  d.frobnicate()
}"#,
    );
    assert_eq!(diags.len(), 3, "got: {diags:?}");
    for name in ["take_last", "frobnicate"] {
        assert!(diags.iter().any(|d| d.contains(name)), "{diags:?}");
    }
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
fn recognized_method_name_on_number_is_flagged() {
    // A method on another builtin does not make it available on numbers.
    let diags = nam_005(
        r"fn f() {
  const n: int = 5
  n.to_string()
}",
    );
    assert!(
        diags.len() == 1 && diags[0].contains("method `to_string` does not exist on `int`"),
        "number methods must agree with runtime dispatch: {diags:?}"
    );
}
