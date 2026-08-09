use super::{assert_roundtrip, format_source};

#[test]
fn nil_coalescing_with_comparison_gets_clarifying_parens() {
    let source = r"pipeline default(task) {
  let x = classified == fixture?.expect_missing_dep ?? false
  let y = value ?? 0 > 0
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("classified == (fixture?.expect_missing_dep ?? false)"),
        "Expected comparison rhs nil-coalescing to be parenthesized, got:\n{result}"
    );
    assert!(
        result.contains("(value ?? 0) > 0"),
        "Expected comparison lhs nil-coalescing to be parenthesized, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn binary_operand_ternary_keeps_required_parentheses() {
    let source = r#"pipeline default(task) {
  let fallback = profiles ?? (supports_apps ? ["mcp-app"] : [])
  let selected = (enabled ? primary : secondary) || backup
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("profiles ?? (supports_apps ? [\"mcp-app\"] : [])"),
        "Expected ternary rhs to remain parenthesized, got:\n{result}"
    );
    assert!(
        result.contains("(enabled ? primary : secondary) || backup"),
        "Expected ternary lhs to remain parenthesized, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn unparenthesized_pipeline_ternary_keeps_its_parse_shape() {
    let source = r#"pipeline default(task) {
  let size = 3 |> _ > 2 ? "big" : "small"
}"#;
    let result = format_source(source).unwrap();
    assert!(
        result.contains("3 |> (_ > 2 ? \"big\" : \"small\")"),
        "Expected formatting to show the ternary inside the pipeline, got:\n{result}"
    );
    assert_roundtrip(source);
}
