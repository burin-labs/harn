use super::{assert_roundtrip, format_source};

#[rstest::rstest]
#[case::nil_coalescing_with_comparison_gets_clarifying_parens(r"pipeline default(task) {
  let x = classified == fixture?.expect_missing_dep ?? false
  let y = value ?? 0 > 0
}", &["classified == (fixture?.expect_missing_dep ?? false)", "(value ?? 0) > 0"])]
#[case::binary_operand_ternary_keeps_required_parentheses(r#"pipeline default(task) {
  let fallback = profiles ?? (supports_apps ? ["mcp-app"] : [])
  let selected = (enabled ? primary : secondary) || backup
}"#, &["profiles ?? (supports_apps ? [\"mcp-app\"] : [])", "(enabled ? primary : secondary) || backup"])]
#[case::unparenthesized_pipeline_ternary_keeps_its_parse_shape(r#"pipeline default(task) {
  let size = 3 |> _ > 2 ? "big" : "small"
}"#, &["3 |> (_ > 2 ? \"big\" : \"small\")"])]
#[case::nil_coalescing_groups_with_swift_precedence(r"pipeline default(task) {
  let a = count ?? 0 + 1
  let b = (count ?? 0) + 1
  let c = count ?? (0 + 1)
}", &["let a = count ?? 0 + 1", "let b = (count ?? 0) + 1", "let c = count ?? 0 + 1"])]
fn formatting_preserves_text(#[case] source: &str, #[case] expected: &[&str]) {
    let result = format_source(source).unwrap();
    for text in expected {
        assert!(result.contains(text), "missing {text:?} in {result:?}");
    }
    assert_roundtrip(source);
}
