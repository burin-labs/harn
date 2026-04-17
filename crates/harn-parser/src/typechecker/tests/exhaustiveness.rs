//! Match exhaustiveness, `unknown`-narrowing exhaustiveness, match-pattern type checks.

use super::*;

#[test]
fn test_exhaustive_match_no_warning() {
    let warns = warnings(
        r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c = Color.Red
  match c.variant {
"Red" -> { log("r") }
"Green" -> { log("g") }
"Blue" -> { log("b") }
  }
}"#,
    );
    let exhaustive_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive"))
        .collect();
    assert!(exhaustive_warns.is_empty());
}

#[test]
fn test_non_exhaustive_match_warning() {
    let warns = warnings(
        r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c = Color.Red
  match c.variant {
"Red" -> { log("r") }
"Green" -> { log("g") }
  }
}"#,
    );
    let exhaustive_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive"))
        .collect();
    assert_eq!(exhaustive_warns.len(), 1);
    assert!(exhaustive_warns[0].contains("Blue"));
}

#[test]
fn test_non_exhaustive_multiple_missing() {
    let warns = warnings(
        r#"pipeline t(task) {
  enum Status { Active, Inactive, Pending }
  let s = Status.Active
  match s.variant {
"Active" -> { log("a") }
  }
}"#,
    );
    let exhaustive_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive"))
        .collect();
    assert_eq!(exhaustive_warns.len(), 1);
    assert!(exhaustive_warns[0].contains("Inactive"));
    assert!(exhaustive_warns[0].contains("Pending"));
}

#[test]
fn test_tagged_shape_union_match_exhaustive_no_warning() {
    let warns = warnings(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) -> string {
    match m.kind {
      "ping" -> { return "ping" }
      "pong" -> { return "pong" }
    }
  }
}"#,
    );
    let exh: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive"))
        .collect();
    assert!(
        exh.is_empty(),
        "no non-exhaustive warning expected, got: {:?}",
        warns
    );
}

#[test]
fn test_tagged_shape_union_match_missing_arm_warns() {
    let warns = warnings(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) -> string {
    match m.kind {
      "ping" -> { return "ping" }
    }
  }
}"#,
    );
    let exh: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive match on tagged shape union"))
        .collect();
    assert_eq!(exh.len(), 1, "got: {:?}", warns);
    assert!(
        exh[0].contains("\"pong\""),
        "expected missing pong, got: {}",
        exh[0]
    );
}

#[test]
fn test_literal_union_match_exhaustive_no_warning() {
    let warns = warnings(
        r#"type Verdict = "pass" | "fail" | "unclear"

pipeline t(task) {
  fn classify(v: Verdict) -> string {
    match v {
      "pass" -> { return "ok" }
      "fail" -> { return "no" }
      "unclear" -> { return "?" }
    }
  }
}"#,
    );
    let exh: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive"))
        .collect();
    assert!(exh.is_empty(), "no warning expected, got: {:?}", warns);
}

#[test]
fn test_literal_union_match_missing_warns() {
    let warns = warnings(
        r#"type Verdict = "pass" | "fail" | "unclear"

pipeline t(task) {
  fn classify(v: Verdict) -> string {
    match v {
      "pass" -> { return "ok" }
      "fail" -> { return "no" }
    }
  }
}"#,
    );
    let exh: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Non-exhaustive match on literal union"))
        .collect();
    assert_eq!(exh.len(), 1, "got: {:?}", warns);
    assert!(exh[0].contains("\"unclear\""));
}

#[test]
fn test_unknown_exhaustive_unreachable_happy_path() {
    // All eight concrete variants covered → no warning on unreachable().
    let source = r#"pipeline t(task) {
  fn describe(v: unknown) -> string {
if type_of(v) == "string"  { return "s" }
if type_of(v) == "int"     { return "i" }
if type_of(v) == "float"   { return "f" }
if type_of(v) == "bool"    { return "b" }
if type_of(v) == "nil"     { return "n" }
if type_of(v) == "list"    { return "l" }
if type_of(v) == "dict"    { return "d" }
if type_of(v) == "closure" { return "c" }
unreachable("unknown type_of variant")
  }
  log(describe(1))
}"#;
    assert!(exhaustive_warns(source).is_empty());
}

#[test]
fn test_unknown_exhaustive_unreachable_incomplete_warns() {
    let source = r#"pipeline t(task) {
  fn describe(v: unknown) -> string {
if type_of(v) == "string" { return "s" }
if type_of(v) == "int"    { return "i" }
unreachable("unknown type_of variant")
  }
  log(describe(1))
}"#;
    let warns = exhaustive_warns(source);
    assert_eq!(warns.len(), 1, "expected one warning, got: {:?}", warns);
    let w = &warns[0];
    for missing in &["float", "bool", "nil", "list", "dict", "closure"] {
        assert!(w.contains(missing), "missing {missing} in: {w}");
    }
    assert!(!w.contains("int"));
    assert!(!w.contains("string"));
    assert!(w.contains("unreachable"));
    assert!(w.contains("v: unknown"));
}

#[test]
fn test_unknown_incomplete_normal_return_no_warning() {
    // Normal `return` fallthrough is NOT an exhaustiveness claim.
    let source = r#"pipeline t(task) {
  fn describe(v: unknown) -> string {
if type_of(v) == "string" { return "s" }
if type_of(v) == "int"    { return "i" }
return "other"
  }
  log(describe(1))
}"#;
    assert!(exhaustive_warns(source).is_empty());
}

#[test]
fn test_unknown_exhaustive_throw_incomplete_warns() {
    let source = r#"pipeline t(task) {
  fn describe(v: unknown) -> string {
if type_of(v) == "string" { return "s" }
throw "unhandled"
  }
  log(describe("x"))
}"#;
    let warns = exhaustive_warns(source);
    assert_eq!(warns.len(), 1, "expected one warning, got: {:?}", warns);
    assert!(warns[0].contains("throw"));
    assert!(warns[0].contains("int"));
}

#[test]
fn test_unknown_throw_without_narrowing_no_warning() {
    // A bare throw with no preceding `type_of` narrowing is not
    // an exhaustiveness claim — stay silent.
    let source = r#"pipeline t(task) {
  fn crash(v: unknown) -> string {
throw "nope"
  }
  log(crash(1))
}"#;
    assert!(exhaustive_warns(source).is_empty());
}

#[test]
fn test_unknown_exhaustive_nested_branch() {
    // Nested if inside a single branch: inner exhaustiveness doesn't
    // escape to the outer scope, and incomplete outer coverage warns.
    let source = r#"pipeline t(task) {
  fn describe(v: unknown, x: int) -> string {
if type_of(v) == "string" {
  if x > 0 { return v.upper() } else { return "s" }
}
if type_of(v) == "int" { return "i" }
unreachable("unknown type_of variant")
  }
  log(describe(1, 1))
}"#;
    let warns = exhaustive_warns(source);
    assert_eq!(warns.len(), 1, "expected one warning, got: {:?}", warns);
    assert!(warns[0].contains("float"));
}

#[test]
fn test_unknown_exhaustive_negated_check() {
    // `type_of(v) != "T"` guards the happy path, so the then-branch
    // accumulates coverage on the truthy side via inversion.
    let source = r#"pipeline t(task) {
  fn describe(v: unknown) -> string {
if type_of(v) != "string" {
  // v still unknown here, but "string" is NOT ruled out on this path
  return "non-string"
}
// v: string here
return v.upper()
  }
  log(describe("x"))
}"#;
    // No unreachable/throw so no warning regardless.
    assert!(exhaustive_warns(source).is_empty());
}

#[test]
fn test_enum_construct_type_inference() {
    let errs = errors(
        r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c: Color = Color.Red
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_nil_coalescing_strips_nil() {
    // After ??, nil should be stripped from the type
    let errs = errors(
        r#"pipeline t(task) {
  let x: string | nil = nil
  let y: string = x ?? "default"
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_shape_mismatch_detail_missing_field() {
    let errs = errors(
        r#"pipeline t(task) {
  let x: {name: string, age: int} = {name: "hello"}
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(
        errs[0].contains("missing field 'age'"),
        "expected detail about missing field, got: {}",
        errs[0]
    );
}

#[test]
fn test_shape_mismatch_detail_wrong_type() {
    let errs = errors(
        r#"pipeline t(task) {
  let x: {name: string, age: int} = {name: 42, age: 10}
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(
        errs[0].contains("field 'name' has type int, expected string"),
        "expected detail about wrong type, got: {}",
        errs[0]
    );
}

#[test]
fn test_match_pattern_string_against_int() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: int = 42
  match x {
"hello" -> { log("bad") }
42 -> { log("ok") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert_eq!(pattern_warns.len(), 1);
    assert!(pattern_warns[0].contains("matching int against string literal"));
}

#[test]
fn test_match_pattern_int_against_string() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: string = "hello"
  match x {
42 -> { log("bad") }
"hello" -> { log("ok") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert_eq!(pattern_warns.len(), 1);
    assert!(pattern_warns[0].contains("matching string against int literal"));
}

#[test]
fn test_match_pattern_bool_against_int() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: int = 42
  match x {
true -> { log("bad") }
42 -> { log("ok") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert_eq!(pattern_warns.len(), 1);
    assert!(pattern_warns[0].contains("matching int against bool literal"));
}

#[test]
fn test_match_pattern_float_against_string() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: string = "hello"
  match x {
3.14 -> { log("bad") }
"hello" -> { log("ok") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert_eq!(pattern_warns.len(), 1);
    assert!(pattern_warns[0].contains("matching string against float literal"));
}

#[test]
fn test_match_pattern_int_against_float_ok() {
    // int and float are compatible for match patterns
    let warns = warnings(
        r#"pipeline t(task) {
  let x: float = 3.14
  match x {
42 -> { log("ok") }
_ -> { log("default") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert!(pattern_warns.is_empty());
}

#[test]
fn test_match_pattern_float_against_int_ok() {
    // float and int are compatible for match patterns
    let warns = warnings(
        r#"pipeline t(task) {
  let x: int = 42
  match x {
3.14 -> { log("close") }
_ -> { log("default") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert!(pattern_warns.is_empty());
}

#[test]
fn test_match_pattern_correct_types_no_warning() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: int = 42
  match x {
1 -> { log("one") }
2 -> { log("two") }
_ -> { log("other") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert!(pattern_warns.is_empty());
}

#[test]
fn test_match_pattern_wildcard_no_warning() {
    let warns = warnings(
        r#"pipeline t(task) {
  let x: int = 42
  match x {
_ -> { log("catch all") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert!(pattern_warns.is_empty());
}

#[test]
fn test_match_pattern_untyped_no_warning() {
    // When value has no known type, no warning should be emitted
    let warns = warnings(
        r#"pipeline t(task) {
  let x = some_unknown_fn()
  match x {
"hello" -> { log("string") }
42 -> { log("int") }
  }
}"#,
    );
    let pattern_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("Match pattern type mismatch"))
        .collect();
    assert!(pattern_warns.is_empty());
}
