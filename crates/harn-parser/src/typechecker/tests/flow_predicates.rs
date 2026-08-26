use super::*;

#[test]
fn const_condition_alias_preserves_typeof_narrowing() {
    let errs = errors(
        r#"fn check(x: string | int) {
  const is_text = type_of(x) == "string"
  if is_text {
    const text: string = x
  } else {
    const number: int = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn const_typeof_result_alias_preserves_narrowing() {
    let errs = errors(
        r#"fn check(x: string | int) {
  const kind = type_of(x)
  if kind == "string" {
    const text: string = x
  } else {
    const number: int = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn const_value_guard_narrows_the_const_not_its_initializer() {
    let errs = errors(
        r#"fn maybe_text(value: string?) -> string? {
  return value
}

fn check(value: string?) -> string {
  const result = maybe_text(value)
  if result == nil {
    return "missing"
  }
  return result
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn truthy_const_guard_narrows_the_const_not_its_initializer() {
    let errs = errors(
        r"fn maybe_text(value: string?) -> string? {
  return value
}

fn check(value: string?) -> int {
  const result = maybe_text(value)
  if result && len(result) > 0 {
    return len(result)
  }
  return 0
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn const_condition_aliases_compose_transitively() {
    let errs = errors(
        r#"fn check(x: string | int | nil) {
  const present = x != nil
  const text = type_of(x) == "string"
  const present_text = present && text
  if present_text {
    const value: string = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn const_condition_alias_does_not_narrow_a_mutable_source() {
    let errs = errors(
        r#"fn check(seed: string | int) {
  let value: string | int = seed
  const is_text = type_of(value) == "string"
  value = 1
  if is_text {
    const text: string = value
  }
}"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected stale alias to stay untrusted: {errs:?}"
    );
    assert!(errs[0].contains("expected string"), "got: {errs:?}");
}

#[test]
fn declared_type_predicate_narrows_both_branches() {
    let errs = errors(
        r#"fn is_text(value: string | int) -> value is string {
  return type_of(value) == "string"
}

fn check(value: string | int) {
  if is_text(value) {
    const text: string = value
  } else {
    const number: int = value
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn generic_type_predicate_is_rejected() {
    let diagnostics = diagnostics_with_code(
        r"fn is_present<T>(value: T | nil) -> implies value is T {
  return value != nil
}",
        Code::InvalidTypePredicate,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
    assert!(diagnostics[0]
        .message
        .contains("cannot prove every call substitution"));
}

#[test]
fn schema_type_predicate_proves_both_branches() {
    let errs = errors(
        r"type Named = {name: string}

fn is_named(value: unknown) -> value is Named {
  return schema_is(value, Named)
}

fn check(value: unknown) {
  if is_named(value) {
    const name: string = value.name
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn type_predicate_can_wrap_another_two_sided_predicate() {
    let errs = errors(
        r#"fn is_text(value: unknown) -> value is string {
  return type_of(value) == "string"
}

fn is_string(value: unknown) -> value is string {
  return is_text(value)
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn circular_type_predicates_cannot_prove_each_other() {
    let diagnostics = diagnostics_with_code(
        r"fn first(value: unknown) -> value is string {
  return second(value)
}

fn second(value: unknown) -> value is string {
  return first(value)
}",
        Code::InvalidTypePredicate,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 2, "got: {diagnostics:?}");
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic.message.contains("has not been validated")));
}

#[test]
fn one_sided_type_predicate_keeps_the_false_branch_wide() {
    let errs = errors(
        r#"fn is_nonempty_text(value: string | int) -> implies value is string {
  return type_of(value) == "string" && len(value) > 0
}

fn check(value: string | int) {
  if is_nonempty_text(value) {
    const text: string = value
  } else {
    const number: int = value
  }
}"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "the false branch must keep string | int: {errs:?}"
    );
    assert!(errs[0].contains("expected int"), "got: {errs:?}");
}

#[test]
fn two_sided_type_predicate_rejects_a_one_sided_body() {
    let diagnostics = diagnostics_with_code(
        r#"fn is_nonempty_text(value: string | int) -> value is string {
  return type_of(value) == "string" && len(value) > 0
}"#,
        Code::InvalidTypePredicate,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
    assert!(diagnostics[0]
        .message
        .contains("use `implies value is string`"));
}

#[test]
fn type_predicate_rejects_an_unrelated_body() {
    let diagnostics = diagnostics_with_code(
        r"fn lies(value: string | int) -> value is string {
  return true
}",
        Code::InvalidTypePredicate,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
    assert!(diagnostics[0].message.contains("does not prove"));
}
