use super::*;

#[test]
fn mcp_tools_warns_for_unannotated_tool_define() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
  })
  mcp_tools(tools)
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, "mcp-tool-annotations"), 1);
}

#[test]
fn mcp_tools_accepts_annotated_tool_define() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
    annotations: {readOnlyHint: true, idempotentHint: true, openWorldHint: false},
  })
  mcp_tools(tools)
}
"#,
    );

    assert!(!has_rule(&diagnostics, "mcp-tool-annotations"));
}

#[test]
fn non_mcp_tool_define_does_not_warn() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
  })
  tool_count(tools)
}
"#,
    );

    assert!(!has_rule(&diagnostics, "mcp-tool-annotations"));
}

#[test]
fn mcp_tools_warns_for_inline_unannotated_tool_define() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  mcp_tools(tool_define(tool_registry(), "echo", "Echo input", {
    parameters: {text: "string"},
    handler: { args -> args.text },
  }))
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, "mcp-tool-annotations"), 1);
}

#[test]
fn mcp_tools_reports_only_unannotated_entries_in_chain() {
    let diagnostics = lint_source(
        r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(tools, "read", "Read input", {
    parameters: {},
    handler: { _args -> "ok" },
    annotations: {readOnlyHint: true},
  })
  tools = tool_define(tools, "write", "Write output", {
    parameters: {},
    handler: { _args -> "ok" },
  })
  mcp_tools(tools)
}
"#,
    );

    assert_eq!(count_rule(&diagnostics, "mcp-tool-annotations"), 1);
}
