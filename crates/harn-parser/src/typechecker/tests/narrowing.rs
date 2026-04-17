//! Flow-sensitive type refinement: nil/typeof/has/schema_is narrowing, guards, while-body narrowing.

use super::*;

#[test]
fn test_nil_narrowing_then_branch() {
    // Existing behavior: x != nil narrows to string in then-branch
    let errs = errors(
        r#"pipeline t(task) {
  fn greet(name: string | nil) {
if name != nil {
  let s: string = name
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_nil_narrowing_else_branch() {
    // NEW: x != nil narrows to nil in else-branch
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x != nil {
  let s: string = x
} else {
  let n: nil = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_nil_equality_narrows_both() {
    // x == nil narrows then to nil, else to non-nil
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x == nil {
  let n: nil = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_truthiness_narrowing() {
    // Bare identifier in condition removes nil
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_negation_narrowing() {
    // !x swaps truthy/falsy
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if !x {
  let n: nil = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_narrowing() {
    // type_of(x) == "string" narrows to string
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) == "string" {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_narrowing_else() {
    // else removes the tested type
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) == "string" {
  let s: string = x
} else {
  let i: int = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_neq_narrowing() {
    // type_of(x) != "string" removes string in then, narrows to string in else
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) != "string" {
  let i: int = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_and_combines_narrowing() {
    // && combines truthy refinements
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int | nil) {
if x != nil && type_of(x) == "string" {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_or_falsy_narrowing() {
    // || combines falsy refinements
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil, y: int | nil) {
if x || y {
  // conservative: can't narrow
} else {
  let xn: nil = x
  let yn: nil = y
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_guard_narrows_outer_scope() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
guard x != nil else { return }
let s: string = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_while_narrows_body() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
while x != nil {
  let s: string = x
  break
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_early_return_narrows_after_if() {
    // if then-body returns, falsy refinements apply after
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) -> string {
if x == nil {
  return "default"
}
let s: string = x
return s
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_early_throw_narrows_after_if() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x == nil {
  throw "missing"
}
let s: string = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_no_narrowing_unknown_type() {
    // Gradual typing: untyped vars don't get narrowed
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x) {
if x != nil {
  let s: string = x
}
  }
}"#,
    );
    // No narrowing possible, so assigning untyped x to string should be fine
    // (gradual typing allows it)
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_reassignment_invalidates_narrowing() {
    // After reassigning a narrowed var, the original type should be restored
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
var y: string | nil = x
if y != nil {
  let s: string = y
  y = nil
  let s2: string = y
}
  }
}"#,
    );
    // s2 should fail because y was reassigned, invalidating the narrowing
    assert_eq!(errs.len(), 1, "expected 1 error, got: {:?}", errs);
    assert!(
        errs[0].contains("declared as"),
        "expected type mismatch, got: {}",
        errs[0]
    );
}

#[test]
fn test_let_immutable_warning() {
    let all = check_source(
        r#"pipeline t(task) {
  let x = 42
  x = 43
}"#,
    );
    let warnings: Vec<_> = all
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warning)
        .collect();
    assert!(
        warnings.iter().any(|w| w.message.contains("immutable")),
        "expected immutability warning, got: {:?}",
        warnings
    );
}

#[test]
fn test_nested_narrowing() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int | nil) {
if x != nil {
  if type_of(x) == "int" {
    let i: int = x
  }
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_match_narrows_arms() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
match x {
  "hello" -> {
    let s: string = x
  }
  42 -> {
    let i: int = x
  }
  _ -> {}
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_has_narrows_optional_field() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: {name?: string, age: int}) {
if x.has("name") {
  let n: {name: string, age: int} = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}
