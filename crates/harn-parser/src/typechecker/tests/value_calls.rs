use super::*;

#[test]
fn expression_result_call_typing() {
    let good = errors(
        r"fn make_callback() -> fn(int) -> int {
  return { value: int -> value + 1 }
}
fn apply() -> int { return make_callback()(41) }",
    );
    assert!(good.is_empty(), "unexpected chained-call errors: {good:?}");

    let bad_arg = errors(
        r#"fn make_callback() -> fn(int) -> int {
  return { value: int -> value + 1 }
}
fn apply() -> int { return (make_callback())("wrong") }"#,
    );
    assert!(
        bad_arg
            .iter()
            .any(|error| error.contains("expected int") && error.contains("found string")),
        "expected chained-call argument mismatch, got: {bad_arg:?}"
    );

    let not_callable =
        errors("fn number() -> int { return 1 }\nfn apply() -> int { return number()(2) }");
    assert!(
        not_callable
            .iter()
            .any(|error| error.contains("int") && error.contains("not callable")),
        "expected callable diagnostic, got: {not_callable:?}"
    );
}

#[test]
fn callable_parameter_shadows_same_named_module_function() {
    let shadowed = errors(
        r#"fn transform(value: string, suffix: string) -> string {
  return value + suffix
}
fn apply(transform: fn(int) -> int) -> int {
  return transform(41)
}"#,
    );
    assert!(
        shadowed.is_empty(),
        "the innermost callable binding must own the call: {shadowed:?}"
    );

    let unshadowed = errors(
        r#"fn transform(value: string, suffix: string) -> string {
  return value + suffix
}
fn apply() -> string {
  return transform("value", "-suffix")
}"#,
    );
    assert!(
        unshadowed.is_empty(),
        "the module function remains callable when no local shadows it: {unshadowed:?}"
    );
}

#[test]
fn non_callable_local_shadow_does_not_fall_through_to_module_function() {
    let diagnostics = errors(
        r#"fn transform(value: string) -> string {
  return value
}
fn apply() -> int {
  const transform = 41
  return transform(1)
}"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("int") && diagnostic.contains("not callable")),
        "the local value must own the non-callable diagnostic: {diagnostics:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("expected string, found int")),
        "the shadowed module signature must not validate the call: {diagnostics:?}"
    );
}
