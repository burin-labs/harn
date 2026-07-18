use super::*;

#[test]
fn typed_pipeline_parameters_typecheck_the_body_and_calls() {
    let body_errors = errors(
        r"
pipeline parse_count(count: int) -> int {
  const label: string = count
  return count
}
",
    );
    assert!(
        body_errors
            .iter()
            .any(|error| error.contains("expected string") && error.contains("found int")),
        "expected typed pipeline body mismatch, got: {body_errors:?}"
    );

    let call_errors = errors(
        r#"
pipeline parse_count(count: int) -> int {
  return count
}

pipeline caller(task) {
  parse_count("not-an-int")
}
"#,
    );
    assert!(
        call_errors
            .iter()
            .any(|error| error.contains("expected int") && error.contains("found string")),
        "expected typed pipeline call mismatch, got: {call_errors:?}"
    );

    let return_errors = errors(
        r#"
pipeline parse_count(count: int) -> int {
  return "not-an-int"
}
"#,
    );
    assert!(
        return_errors
            .iter()
            .any(|error| error.contains("expected int") && error.contains("found string")),
        "expected typed pipeline return mismatch, got: {return_errors:?}"
    );
}
