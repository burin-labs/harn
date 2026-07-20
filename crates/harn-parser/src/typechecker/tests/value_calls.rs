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
