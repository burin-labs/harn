use super::*;
use crate::diagnostic_codes::Code;
use crate::typechecker::DiagnosticDetails;
use crate::TypeExpr;

#[test]
fn skill_and_eval_pack_bindings_stay_in_their_declaring_body() {
    for declaration in [
        r#"skill local_binding { description "Local"; prompt "Follow the runbook." }"#,
        r#"eval_pack local_binding "local-pack" {}"#,
    ] {
        let source = format!(
            "pipeline declares(harness: Harness) {{\n{declaration}\n\
             harness.stdio.println(local_binding)\n}}\n\
             pipeline reads(harness: Harness) {{\n\
             harness.stdio.println(local_binding)\n}}"
        );
        let diagnostics = check_source_with_imports(&source, &[]);
        let unresolved: Vec<_> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::UndefinedVariable)
            .collect();
        assert_eq!(
            unresolved.len(),
            1,
            "only the sibling reference must fail for {declaration}: {diagnostics:?}"
        );
        assert!(matches!(
            unresolved[0].details.as_ref(),
            Some(DiagnosticDetails::UnresolvedName { name }) if name == "local_binding"
        ));
        let span = unresolved[0]
            .span
            .expect("unresolved name must have a source span");
        assert_eq!(&source[span.start..span.end], "local_binding");
        assert!(span.start > source.find("pipeline reads").unwrap());
    }
}

#[test]
fn module_skill_and_eval_pack_bindings_keep_forward_references() {
    for declaration in [
        r#"skill shared_binding { description "Shared"; prompt "Follow the runbook." }"#,
        r#"eval_pack shared_binding "shared-pack" {}"#,
    ] {
        let diagnostics = check_source_with_imports(
            &format!(
                "pipeline reads(harness: Harness) {{\n\
                 harness.stdio.println(shared_binding)\n}}\n{declaration}"
            ),
            &[],
        );
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
            "module binding must resolve for {declaration}: {diagnostics:?}"
        );
    }
}

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
    let diagnostics = check_source(
        r#"const value: string = "outer"
fn read(value: int = value) -> int { return value }"#,
    );
    let mismatch = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == Code::VariableTypeMismatch)
        .unwrap_or_else(|| {
            panic!("self-named default must resolve the outer string binding: {diagnostics:?}")
        });
    match mismatch.details.as_ref() {
        Some(DiagnosticDetails::TypeMismatch { expected, actual }) => {
            assert_eq!(expected, &TypeExpr::Named("int".to_string()));
            assert_eq!(actual, &TypeExpr::Named("string".to_string()));
        }
        details => panic!("expected typed mismatch details, got {details:?}"),
    }
    assert_eq!(
        mismatch.message,
        "parameter default `value`: expected int, found string"
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

/// The exact repro from #8459.
///
/// `harn check` accepted a reference to a name bound only inside a different
/// function, and the VM then refused it at runtime with an undefined
/// variable. A green check has to mean "this resolves here", not "this name
/// exists somewhere in the file".
#[test]
fn a_name_bound_only_in_another_function_does_not_resolve() {
    let diagnostics = check_source_with_imports(
        r"fn first() -> int {
  let only_in_first = 1
  return only_in_first
}

fn second() -> int {
  return only_in_first
}",
        &[],
    );
    let unresolved: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::UndefinedVariable)
        .collect();
    assert_eq!(
        unresolved.len(),
        1,
        "a local of another function must not resolve: {diagnostics:?}"
    );
    assert!(
        matches!(
            unresolved[0].details.as_ref(),
            Some(DiagnosticDetails::UnresolvedName { name }) if name == "only_in_first"
        ),
        "the diagnostic must name the unresolved binding: {:?}",
        unresolved[0]
    );
}

/// Negative control: a name bound in an enclosing scope still resolves.
#[test]
fn a_name_bound_in_an_enclosing_scope_still_resolves() {
    let diagnostics = check_source_with_imports(
        r"const at_module_scope = 1

fn reader() -> int {
  let outer = 2
  if true {
    return outer + at_module_scope
  }
  return outer
}",
        &[],
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
        "an enclosing binding must still resolve: {diagnostics:?}"
    );
}

/// Negative control: a closure still captures from its defining scope.
#[test]
fn a_closure_capture_still_resolves() {
    let diagnostics = check_source_with_imports(
        r"fn reader() -> int {
  let captured = 1
  const read = { -> captured }
  return read()
}",
        &[],
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
        "a closure capture must still resolve: {diagnostics:?}"
    );
}

/// Negative control: an imported name still resolves.
#[test]
fn an_imported_name_still_resolves() {
    let diagnostics = check_source_with_imports(
        r"fn reader() -> int {
  return from_another_module()
}",
        &["from_another_module"],
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
        "an imported name must still resolve: {diagnostics:?}"
    );
}

/// The forward reference the placeholder pass exists for, kept deliberately.
///
/// Callables are still registered wherever they are declared, so a call that
/// precedes its declaration, and a recursive call, both still resolve. Narrow
/// the hoist too far and this is what breaks.
#[test]
fn forward_and_recursive_calls_still_resolve() {
    let diagnostics = check_source_with_imports(
        r"fn caller() -> int {
  return declared_later(3)
}

fn declared_later(n: int) -> int {
  if n <= 0 {
    return 0
  }
  return declared_later(n - 1)
}",
        &[],
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
        "a forward or recursive call must still resolve: {diagnostics:?}"
    );
}

/// A module-scope binding referenced before its declaration still resolves.
#[test]
fn a_module_scope_binding_still_resolves_before_its_declaration() {
    let diagnostics = check_source_with_imports(
        r"fn reader() -> int {
  return declared_below
}

const declared_below = 7",
        &[],
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::UndefinedVariable),
        "a module-scope binding must still resolve: {diagnostics:?}"
    );
}
