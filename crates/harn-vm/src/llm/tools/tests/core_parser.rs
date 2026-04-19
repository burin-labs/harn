use super::{json, parse_bare_calls_in_body, sample_tool_registry};

#[test]
fn parses_a_simple_object_literal_call() {
    let tools = sample_tool_registry();
    let text = r#"I'll write the scaffold now.
edit({ action: "create", path: "a.go", content: "package a\n" })
That should work."#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "unexpected errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.go"));
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("package a\n")
    );
}

#[test]
fn parses_a_template_literal_multiline_body() {
    let tools = sample_tool_registry();
    let text = "edit({\n  action: \"replace_body\",\n  path: \"a.go\",\n  new_body: `\nfunc Foo() {\n    return 1\n}\n`\n})";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    let body = result.calls[0]["arguments"]["new_body"].as_str().unwrap();
    assert!(body.contains("func Foo()"));
    assert!(body.contains("return 1"));
}

#[test]
fn escapes_inside_template_literals_are_honored() {
    let tools = sample_tool_registry();
    // \` must be passed through as a literal backtick; \\ as a backslash.
    let text = "edit({ action: \"create\", path: \"a.md\", content: `line with a \\` backtick` })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("line with a ` backtick")
    );
}

#[test]
fn parses_tool_calls_inside_markdown_code_fences() {
    let tools = sample_tool_registry();
    let text = "Here's how to use it:\n```python\nedit({ action: \"create\", path: \"fenced.go\" })\n```\nAnd another:\nedit({ action: \"create\", path: \"bare.go\" })\n";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(
        result.calls.len(),
        2,
        "both fenced and bare calls are parsed"
    );
    assert_eq!(result.calls[0]["arguments"]["path"], json!("fenced.go"));
    assert_eq!(result.calls[1]["arguments"]["path"], json!("bare.go"));
    // Fence lines should be stripped from prose when they bracket tool calls
    assert!(
        !result.prose.contains("```"),
        "fence markers should be stripped from prose: {:?}",
        result.prose
    );
}

#[test]
fn ignores_tool_name_mentions_inside_inline_code_spans() {
    let tools = sample_tool_registry();
    let text = "I considered `edit({...})` but chose a different approach.\nedit({ action: \"create\", path: \"real.go\" })\n";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("real.go"));
}

#[test]
fn recovers_single_inline_wrapped_tool_call_when_it_is_the_entire_response() {
    let tools = sample_tool_registry();
    let text = r#"`edit({ action: "create", path: "wrapped.go" })`"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("wrapped.go"));
}

#[test]
fn recovers_single_fenced_tool_call_when_it_is_the_entire_response() {
    let tools = sample_tool_registry();
    let text = "```typescript\nedit({ action: \"create\", path: \"wrapped.go\" })\n```";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("wrapped.go"));
}

#[test]
fn parses_fenced_edit_batch_ops_with_nested_heredoc() {
    let tools = sample_tool_registry();
    let text = r#"```tool_code
edit({ path: "tests/test_service.py", ops: [
  { op: "replace_body", function_name: "test_handle", new_body: <<EOF
value = 1
assert value == 1
EOF
  },
  { op: "add_import", import_statement: "from app.types import RequestContext" }
]})
```"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(
        result.calls[0]["arguments"]["path"],
        json!("tests/test_service.py")
    );
    let ops = result.calls[0]["arguments"]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0]["op"], json!("replace_body"));
    assert_eq!(ops[0]["function_name"], json!("test_handle"));
    assert_eq!(ops[0]["new_body"], json!("value = 1\nassert value == 1"));
    assert_eq!(ops[1]["op"], json!("add_import"));
    assert_eq!(
        ops[1]["import_statement"],
        json!("from app.types import RequestContext")
    );
}

#[test]
fn recovers_malformed_quoted_heredoc_value_without_closing_quote() {
    let tools = sample_tool_registry();
    let text = r#"edit({
  path: "tests/unit/test_experiment_service.py",
  action: "replace_range",
  range_start: 1,
  range_end: 4,
  content: "<<EOF
import pytest

def test_create_experiment():
    assert "ok" == "ok"
EOF
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "quoted heredoc should be salvaged: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    assert_eq!(
        result.calls[0]["arguments"]["action"],
        json!("replace_range")
    );
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("import pytest\n\ndef test_create_experiment():\n    assert \"ok\" == \"ok\"")
    );
}

#[test]
fn reports_unknown_tool_names() {
    let tools = sample_tool_registry();
    // `fictitious_tool(...)` looks like a call but the name is not in the
    // registry, so the scanner should report an error and still parse the
    // valid `edit` call.
    let text = "fictitious_tool({ x: 1 })\nedit({ action: \"create\", path: \"a.go\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.errors.len(), 1, "should report unknown tool");
    assert!(
        result.errors[0].contains("Unknown tool 'fictitious_tool'"),
        "error should name the unknown tool: {}",
        result.errors[0]
    );
    assert!(
        result.errors[0].contains("Available tools:"),
        "error should list available tools: {}",
        result.errors[0]
    );
    assert_eq!(result.calls.len(), 1, "valid edit call should still parse");
    assert_eq!(result.calls[0]["name"], json!("edit"));
}

#[test]
fn strips_gemma_tool_code_prefix_so_native_format_parses() {
    // Gemma 3/4 are RL-trained to emit `tool_code: fn(args)` as their
    // native inline tool-call form. In text mode we want that to parse
    // cleanly instead of silently drifting into prose.
    let tools = sample_tool_registry();
    let text = "tool_code: run({ command: \"ls\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "tool_code: prefix should strip: {result:?}",
        result = &result.errors
    );
    assert_eq!(result.calls[0]["name"], json!("run"));
    assert_eq!(result.calls[0]["arguments"]["command"], json!("ls"));
}

#[test]
fn strips_assorted_language_and_wrapper_prefixes() {
    let tools = sample_tool_registry();
    for prefix in [
        "tool_call:",
        "tool_output:",
        "python:",
        "javascript:",
        "shell:",
        "bash:",
    ] {
        let text = format!("{prefix} run({{ command: \"ls\" }})");
        let result = parse_bare_calls_in_body(&text, Some(&tools));
        assert_eq!(
            result.calls.len(),
            1,
            "prefix `{prefix}` should be stripped and the call should parse; errors={:?}",
            result.errors
        );
        assert_eq!(result.calls[0]["name"], json!("run"));
    }
}

#[test]
fn explicit_parse_error_for_unknown_label_prefix_on_known_tool() {
    // When the line looks like `SomeLabel: known_tool(...)` and the label
    // isn't in our strip allowlist, we should surface a parse error so
    // the model gets a self-correction signal instead of a silent drop.
    // The guard is on the second identifier being a known tool, which
    // keeps us from false-positive'ing on arbitrary prose.
    let tools = sample_tool_registry();
    let text = "Hint: run({ command: \"ls\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        0,
        "ambiguous prefixed-line should not silently execute: {:?}",
        result.calls
    );
    assert_eq!(
        result.errors.len(),
        1,
        "should emit a self-correction error"
    );
    let err = &result.errors[0];
    assert!(
        err.contains("Hint:"),
        "error should name the observed prefix: {err}"
    );
    assert!(err.contains("run"), "error should name the tool: {err}");
    assert!(
        err.contains("Do not prefix"),
        "error should explain the fix: {err}"
    );
}

#[test]
fn parses_gemma_fenced_tool_code_block() {
    // The full-response `unwrap_exact_code_wrapper` path handles Gemma's
    // other native form: ```tool_code\nrun({...})\n```
    let tools = sample_tool_registry();
    let text = "```tool_code\nrun({ command: \"ls\" })\n```";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("run"));
}

#[test]
fn unknown_label_prefix_with_unknown_identifier_stays_prose() {
    // `note: make_coffee(...)` must NOT fire the near-miss diagnostic
    // because `make_coffee` isn't a known tool. This is the guardrail
    // against false positives on arbitrary prose containing colons.
    let tools = sample_tool_registry();
    let text = "Note: make_coffee({ strength: \"strong\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 0);
    // `make_coffee` followed by `(` with an object-literal arg WILL hit
    // the existing unknown-tool error path, but only because the line
    // starts with `Note: ` — our near-miss path must be a no-op here.
    // The existing unknown-tool error is fine; we just check we didn't
    // add a duplicate.
    let near_miss_errors: Vec<&String> = result
        .errors
        .iter()
        .filter(|e| e.contains("Do not prefix"))
        .collect();
    assert!(
        near_miss_errors.is_empty(),
        "near-miss error must not fire when the identifier isn't a known tool: {:?}",
        result.errors
    );
}

#[test]
fn parses_multiple_calls_in_one_response() {
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.go\", content: \"a\" })\nThen we will:\nedit({ action: \"patch\", path: \"b.go\" })\n";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.go"));
    assert_eq!(result.calls[1]["arguments"]["path"], json!("b.go"));
}

#[test]
fn parses_nested_object_and_array_literals() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "a.go",
    content: "x",
    new_body: { "nested": [1, 2, "three"], "flag": true }
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    // new_body is a string in the schema, but the parser is structural and
    // returns whatever shape the model writes; the tool handler will coerce.
    assert!(result.errors.is_empty());
    assert_eq!(result.calls.len(), 1);
    let new_body = &result.calls[0]["arguments"]["new_body"];
    assert_eq!(new_body["nested"][0], json!(1));
    assert_eq!(new_body["nested"][2], json!("three"));
    assert_eq!(new_body["flag"], json!(true));
}

#[test]
fn reports_unclosed_object_literal_with_precise_diagnostic() {
    let tools = sample_tool_registry();
    // Truncated mid-object: model hit token limit before closing brace.
    let text = "edit({ action: \"create\", path: \"a.go\", content: `hello";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert_eq!(result.errors.len(), 1);
    let err = &result.errors[0];
    assert!(
        err.contains("edit"),
        "diagnostic should name the tool: {err}"
    );
}

#[test]
fn reports_bare_scalar_argument_as_needing_object_wrapper() {
    let tools = sample_tool_registry();
    let text = "edit(\"a.go\")";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert_eq!(result.errors.len(), 1);
    let err = &result.errors[0];
    assert!(
        err.contains("object literal") || err.contains("{ key: value }"),
        "diagnostic should nudge towards object literal syntax: {err}"
    );
}

#[test]
fn parses_trailing_commas() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "a.go",
    content: "x",
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
}

#[test]
fn parses_line_and_block_comments() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    // Create a new file
    action: "create", /* must be a valid action */
    path: "a.go",
    content: "package a"
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
}

#[test]
fn accepts_single_quoted_string_literals() {
    let tools = sample_tool_registry();
    let text = "edit({ action: 'create', path: 'a.go' })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.go"));
}

#[test]
fn rejects_legacy_named_argument_call_form() {
    let tools = sample_tool_registry();
    let text = "edit(action=\"replace_body\", path=\"a.go\", new_body=`return 1`)";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0].contains("object literal"),
        "diagnostic should require strict object-literal syntax: {:?}",
        result.errors
    );
}

#[test]
fn ignores_non_tool_function_calls_inside_code_examples() {
    let tools = sample_tool_registry();
    let text =
        "Here is the target test body:\n\n    assert divide(10, 2) == 5.0\n    multiply(2, 3)\n";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert!(
        result.errors.is_empty(),
        "unexpected parse errors: {:?}",
        result.errors
    );
}
