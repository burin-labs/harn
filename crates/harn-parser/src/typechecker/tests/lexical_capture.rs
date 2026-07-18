use super::*;

#[test]
fn enum_payload_closure_reassignment_does_not_poison_outer_narrowing() {
    let errs = errors(
        r#"enum Option<T> {
  Some(value: T),
  None
}

fn f(value: Option<string>) {
  let pin: string | nil = "outer"
  match value {
    Some(pin) -> {
      const replace = { -> pin = "inner" }
      replace()
    }
    None -> {}
  }
  if pin != nil {
    let narrowed: string = pin
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "enum payload reassignment poisoned outer narrowing: {errs:?}"
    );
}

#[test]
fn parameter_default_resolves_before_current_parameter_binding() {
    let errs = errors(
        r#"const value: string = "outer"
fn read(value: int = value) -> int { return value }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected int") && err.contains("found string")),
        "self-named default must resolve the outer string binding: {errs:?}"
    );

    let earlier_param_errs =
        errors(r"fn read(first: string, second: string = first) -> string { return second }");
    assert!(earlier_param_errs.is_empty(), "{earlier_param_errs:?}");

    let closure_errs = errors(
        r#"const value: string = "outer"
const read: fn(int) -> int = fn(value = value) -> int { value }"#,
    );
    assert!(
        closure_errs
            .iter()
            .any(|err| err.contains("expected int") && err.contains("found string")),
        "contextual closure defaults must use declaration scope: {closure_errs:?}"
    );

    let closure_earlier_errs =
        errors(r"const read = fn(first: string, second: string = first) -> string { second }");
    assert!(closure_earlier_errs.is_empty(), "{closure_earlier_errs:?}");

    for (kind, declaration) in [
        (
            "function default",
            "fn read(value: int = value) -> int { return value }",
        ),
        (
            "tool default",
            "tool read(value: int = value) -> int { return value }",
        ),
        ("function body", "fn read() -> int { return value }"),
    ] {
        let nested_errs = errors(&format!(
            "pipeline default(task) {{\n  const value: string = \"outer\"\n  {declaration}\n}}"
        ));
        assert!(
            nested_errs
                .iter()
                .any(|err| err.contains("expected int") && err.contains("found string")),
            "nested {kind} must resolve in declaration scope: {nested_errs:?}"
        );
    }
}

#[test]
fn ambiguous_bare_variant_pattern_requires_qualification() {
    let errs = errors(
        r"enum First { Shared(value: int) }
enum Second { Shared(value: int) }
fn inspect(value: First) -> int {
  match value {
    Shared(payload) -> { return payload }
  }
}",
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("variant `Shared` is declared by enums First, Second")),
        "typechecker must reject the same ambiguity as codegen: {errs:?}"
    );
}

#[test]
fn nested_enum_is_absent_from_outer_bare_variant_catalog() {
    let errs = errors(
        r"enum Outer { Shared(value: int) }
fn nested_declaration() {
  enum Inner { Shared(value: int) }
}
pipeline default(task) {
  match Outer.Shared(1) {
    Shared(payload) -> { log(payload) }
  }
}",
    );
    assert!(
        errs.is_empty(),
        "nested enum leaked into outer scope: {errs:?}"
    );
}

#[test]
fn duplicate_pipeline_enums_shadow_in_source_order() {
    let errs = errors(
        r"pipeline default(task) {
  enum Event { First(value: int) }
  match Event.First(1) {
    First(payload) -> { log(payload) }
  }
  enum Event { Second(value: int) }
}",
    );
    assert!(errs.is_empty(), "pipeline enum shadowing drifted: {errs:?}");
}

#[test]
fn inherited_pipeline_enum_does_not_change_child_catalog() {
    let errs = errors(
        r"pipeline base(task) {
  enum Event { Base(value: int) }
}
pipeline default(task) extends base {
  match Event.Child(1) {
    Child(payload) -> { log(payload) }
  }
  enum Event { Child(value: int) }
}",
    );
    assert!(
        errs.is_empty(),
        "parent enum changed child catalog: {errs:?}"
    );
}
