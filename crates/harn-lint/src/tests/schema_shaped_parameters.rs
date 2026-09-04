use super::*;

const RULE: &str = "schema-shaped-tool-parameters";

#[test]
fn schema_shaped_parameters_are_reported_on_a_tool_descriptor() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  const tools = [
    {name: "read_file", parameters: {type: "object", required: ["path"]}},
    {name: "search", parameters: {type: "object"}},
  ]
  return tools
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, RULE), 2);
}

#[test]
fn a_real_parameter_map_and_an_input_schema_are_not_reported() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  const tools = [
    {name: "read_file", parameters: {path: {type: "string", required: true}}},
    {name: "search", inputSchema: {type: "object", required: ["query"]}},
    {name: "noop", parameters: {}},
  ]
  return tools
}
"#,
    );

    assert!(!has_rule(&diagnostics, RULE));
}
