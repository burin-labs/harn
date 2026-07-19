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
#[test]
fn unknown_value_tails_are_not_treated_as_nil() {
    let errs = errors(
        r#"
pipeline render() -> string {
  exec("printf", "pipeline").stdout ?? ""
}

pipeline test(task) {
  tool render_tool() -> string {
    exec("printf", "tool").stdout ?? ""
  }
}
"#,
    );
    assert!(errs.is_empty(), "unknown value tail became nil: {errs:?}");
}

#[test]
fn no_value_tool_tail_is_still_nil() {
    let errs = errors(
        r#"
pipeline test(task) {
  tool no_result() -> string {
    const result = exec("printf", "tool")
  }
}
"#,
    );
    assert!(
        errs.iter()
            .any(|error| error.contains("tool result: expected string, found nil")),
        "expected nil result mismatch, got: {errs:?}"
    );
}
