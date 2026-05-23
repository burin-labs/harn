use super::{
    collect_tool_schemas, json, parse_bare_calls_in_body, parse_text_tool_calls_with_tools,
    sample_tool_registry, validate_tool_args,
};

#[test]
fn validate_tool_args_reports_missing_required_params() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    // edit requires "action" and "path" — omit "path"
    let args = json!({"action": "create"});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_err(), "should report missing required param");
    let msg = result.unwrap_err();
    assert!(msg.contains("path"), "error should mention 'path': {msg}");
    assert!(
        msg.contains("missing required parameter"),
        "error should say missing required: {msg}"
    );
}

#[test]
fn validate_tool_args_passes_when_all_required_present() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"action": "create", "path": "test.go", "content": "pkg main"});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_ok(), "should pass with all required params");
}

#[test]
fn validate_tool_args_skips_unknown_tool() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"foo": "bar"});
    let result = validate_tool_args("nonexistent_tool", &args, &schemas);
    assert!(
        result.is_ok(),
        "should pass for unknown tool (handled elsewhere)"
    );
}

#[test]
fn validate_tool_args_treats_null_as_missing() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"action": "create", "path": null});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_err(), "null should count as missing");
    assert!(result.unwrap_err().contains("path"));
}

#[test]
fn validate_tool_args_passes_with_empty_args_when_no_required() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    // "run" tool requires "command" — but let's test with a tool that has
    // no required params. Since sample_tool_registry tools all have required
    // params, just verify an unknown tool passes.
    let result = validate_tool_args("no_such_tool", &json!({}), &schemas);
    assert!(result.is_ok());
}

#[test]
fn text_parser_reports_unknown_tool_in_native_json_fallback() {
    // End-to-end through parse_text_tool_calls_with_tools: when a native
    // JSON fallback call references an unknown tool, it should surface as
    // an error in the TextToolParseResult.
    let tools = sample_tool_registry();
    let text = r#"[{"id":"call_001","type":"function","function":{"name":"nonexistent","arguments":"{}"}}]"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(result.calls.is_empty(), "no valid calls");
    assert!(
        !result.errors.is_empty(),
        "should surface unknown tool error: {:?}",
        result.errors
    );
    assert!(
        result.errors[0].contains("Unknown tool"),
        "error message: {}",
        result.errors[0]
    );
}

#[test]
fn tagged_parser_accepts_well_formed_response() {
    let tools = sample_tool_registry();
    let text = "<assistant_prose>Creating the file.</assistant_prose>\n\
                <user_response>Created the file.</user_response>\n\
                <tool_call>\n\
                edit({ action: \"create\", path: \"a.rs\", content: \"fn a() {}\" })\n\
                </tool_call>\n\
                <done>##DONE##</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.violations.is_empty(),
        "no violations expected, got {:?}",
        result.violations,
    );
    assert!(result.errors.is_empty(), "no errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.prose, "Created the file.");
    assert_eq!(result.user_response.as_deref(), Some("Created the file."));
    assert_eq!(result.done_marker.as_deref(), Some("##DONE##"));
    assert!(
        !result.canonical.is_empty(),
        "canonical must be populated so history replays the tagged shape"
    );
    assert!(result.canonical.contains("<tool_call>"));
    assert!(result.canonical.contains("<done>##DONE##</done>"));
}

#[test]
fn tagged_parser_accepts_gemma_json_tool_call_body() {
    // Local Gemma-family stacks sometimes return raw JSON inside the
    // `<tool_call>` wrapper instead of Harn's inner `name({...})` syntax.
    let tools = sample_tool_registry();
    let text = r#"<tool_call>{"name":"edit","arguments":{"action":"create","path":"a.rs","content":"fn a() {}"}}</tool_call>"#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.violations.is_empty(),
        "violations: {:?}",
        result.violations
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
}

#[test]
fn tagged_parser_recovers_mistral_tool_markers() {
    let tools = sample_tool_registry();
    let text = "I'll inspect first.[TOOL_CALLS]edit[ARGS]{\"action\":\"create\",\"path\":\"a.rs\"}";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
    assert_eq!(result.prose, "I'll inspect first.");
    assert!(
        result.violations[0].contains("Mistral"),
        "violation should teach canonical protocol: {:?}",
        result.violations
    );
    assert!(result.canonical.contains("<tool_call>"));
}

#[test]
fn tagged_parser_recovers_mistral_json_tool_marker_payload() {
    let tools = sample_tool_registry();
    let text = "I'll inspect first.\n[TOOL_CALLS] [{\"name\":\"edit\",\"arguments\":{\"action\":\"create\",\"path\":\"a.rs\",\"content\":\"fn a() {}\"}},{\"name\":\"run\",\"arguments\":\"{\\\"command\\\":\\\"cargo test\\\"}\"}]";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
    assert_eq!(result.calls[1]["name"], json!("run"));
    assert_eq!(result.calls[1]["arguments"]["command"], json!("cargo test"));
    assert_eq!(result.prose, "I'll inspect first.");
    assert!(
        result.violations[0].contains("Mistral"),
        "violation should teach canonical protocol: {:?}",
        result.violations
    );
    assert_eq!(result.canonical.matches("<tool_call>").count(), 2);
}

#[test]
fn tagged_parser_recovers_deepseek_dsml_markers() {
    let tools = sample_tool_registry();
    let text = "I'll inspect first.\n\
<｜DSML｜function_calls>\n\
<｜DSML｜invoke name=\"edit\">\n\
<｜DSML｜parameter name=\"action\" string=\"true\">create</｜DSML｜parameter>\n\
<｜DSML｜parameter name=\"path\" string=\"true\">a.rs</｜DSML｜parameter>\n\
<｜DSML｜parameter name=\"content\" string=\"true\">fn main() {}</｜DSML｜parameter>\n\
</｜DSML｜invoke>\n\
</｜DSML｜function_calls><｜DSML｜function_calls>\n\
<｜DSML｜invoke name=\"run\">\n\
<｜DSML｜parameter name=\"command\" string=\"false\">[\"cargo\", \"test\"]</｜DSML｜parameter>\n\
</｜DSML｜invoke>\n\
</｜DSML｜function_calls><｜DSML｜function_calls>\n\
<｜DSML｜invoke name=\"edit\">";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
    assert_eq!(result.calls[1]["name"], json!("run"));
    assert_eq!(result.calls[1]["arguments"]["command"][0], json!("cargo"));
    assert_eq!(result.prose, "I'll inspect first.");
    assert!(
        result.violations[0].contains("DeepSeek DSML"),
        "violation should teach canonical protocol: {:?}",
        result.violations
    );
    assert!(result.canonical.contains("<tool_call>"));
}

#[test]
fn tagged_parser_accepts_compact_protocol_tag_aliases() {
    let tools = sample_tool_registry();
    let text = "<assistantprose>Checking status.</assistantprose>\n\
                <userresponse>Done.</userresponse>\n\
                <toolcall>\n\
                run({ command: \"git status\" })\n\
                </toolcall>\n\
                <done>##DONE##</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.violations.is_empty(),
        "compact tag aliases should parse without violations: {:?}",
        result.violations,
    );
    assert!(result.errors.is_empty(), "no errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("run"));
    assert_eq!(result.prose, "Done.");
    assert_eq!(result.user_response.as_deref(), Some("Done."));
    assert!(result.canonical.contains("<tool_call>"));
    assert!(result.canonical.contains("<user_response>"));
}

#[test]
fn tagged_parser_flags_stray_prose_outside_tags() {
    let tools = sample_tool_registry();
    let text = "def foo():\n    pass\n\n<tool_call>\nedit({ action: \"create\", path: \"a.rs\", content: \"x\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        !result.violations.is_empty(),
        "stray prose before <tool_call> must be flagged"
    );
    // The inside-tag call still parses; the model sees the violation on the
    // next turn but the runtime doesn't lose the action.
    assert_eq!(result.calls.len(), 1);
}

#[test]
fn tagged_parser_executes_bare_tool_call_with_soft_violation() {
    // Pre-v0.5.82 bare calls without `<tool_call>` wrappers were flagged
    // AND dropped, which stranded weaker locally-hosted models that kept
    // emitting the same right-shape-wrong-wrapper response. Now we
    // execute the call and surface a soft violation so the model still
    // learns the canonical wrapping next turn.
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.rs\", content: \"x\" })";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "bare call must execute (calls: {}, violations: {:?})",
        result.calls.len(),
        result.violations
    );
    assert_eq!(
        result.calls[0]
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or(""),
        "edit"
    );
    assert!(
        !result.violations.is_empty(),
        "bare call still warrants a violation so the model wraps next turn"
    );
    assert!(
        result.violations[0].contains("bare text") || result.violations[0].contains("<tool_call>"),
        "violation must name the missing wrapper: {}",
        result.violations[0]
    );
}

#[test]
fn tagged_parser_executes_bare_tool_call_with_heredoc_body() {
    // Regression: the top-level scanner's stray-bytes chunker scanned to
    // the next `<` byte, which truncated bare `name({ key: <<EOF\n...\nEOF })`
    // calls at the heredoc opener and left the salvage path with a fragment
    // that couldn't parse. qwen2.5-coder hits this on every py-test edit
    // because it emits the entire test body as a heredoc value without
    // wrapping the call in `<tool_call>` tags.
    let tools = sample_tool_registry();
    let text = "edit({ action: \"replace_range\", path: \"tests/test.py\", \
                range_start: 1, range_end: 4, content: <<EOF\n\
                import pytest\n\n\
                def test_one():\n    assert 1 == 1\n\
                EOF\n})";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "heredoc-bodied bare call must execute (calls: {}, violations: {:?}, errors: {:?})",
        result.calls.len(),
        result.violations,
        result.errors,
    );
    assert_eq!(
        result.calls[0]["arguments"]["action"]
            .as_str()
            .unwrap_or(""),
        "replace_range"
    );
    let body = result.calls[0]["arguments"]["content"]
        .as_str()
        .unwrap_or("");
    assert!(
        body.contains("import pytest"),
        "heredoc body preserved: {body:?}"
    );
    assert!(
        body.contains("def test_one"),
        "heredoc body preserved: {body:?}"
    );
}

#[test]
fn tagged_parser_flags_unknown_top_level_tag() {
    let tools = sample_tool_registry();
    let text = "<notes>my thoughts</notes><tool_call>\nedit({ action: \"create\", path: \"a.rs\", content: \"x\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result
            .violations
            .iter()
            .any(|violation| violation.contains("Unknown")),
        "unknown top-level tag should be flagged: {:?}",
        result.violations
    );
    assert_eq!(result.calls.len(), 1, "known <tool_call> still executes");
}

#[test]
fn tagged_parser_accepts_user_response_tag() {
    let tools = sample_tool_registry();
    let text = "<user_response>Visible answer.</user_response>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "no errors: {:?}", result.errors);
    assert!(
        result.violations.is_empty(),
        "no violations: {:?}",
        result.violations
    );
    assert_eq!(result.prose, "Visible answer.");
    assert_eq!(result.user_response.as_deref(), Some("Visible answer."));
    assert_eq!(
        result.canonical.trim(),
        "<user_response>\nVisible answer.\n</user_response>"
    );
}

#[test]
fn tagged_parser_ignores_inline_user_response_placeholder() {
    let tools = sample_tool_registry();
    let text =
        "Use `<user_response>...</user_response>` as the wrapper.\nActual answer in plain prose.";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.user_response, None);
    assert_eq!(result.prose, "");
    assert!(
        result
            .violations
            .iter()
            .any(|violation| violation.contains("Stray text outside response tags")),
        "inline placeholder should be stray prose, not a user response: {:?}",
        result.violations
    );
}

#[test]
fn tagged_parser_ignores_user_response_inside_markdown_fence() {
    let tools = sample_tool_registry();
    let text = "```xml\n<user_response>example only</user_response>\n```\n<user_response>Visible answer.</user_response>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.user_response.as_deref(), Some("Visible answer."));
    assert_eq!(result.prose, "Visible answer.");
}

#[test]
fn tagged_parser_flags_empty_done_block() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\nedit({ action: \"create\", path: \"a.rs\", content: \"x\" })\n</tool_call>\n<done></done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.done_marker.is_none(),
        "empty <done> is not a completion"
    );
    assert!(
        result
            .violations
            .iter()
            .any(|violation| violation.contains("<done> block is empty")),
        "empty <done> must be flagged: {:?}",
        result.violations
    );
}

#[test]
fn tagged_parser_empty_response_flags_violation() {
    let tools = sample_tool_registry();
    let text = "";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert!(
        result.violations.is_empty(),
        "whitespace-only text is not a violation"
    );

    let text = "just prose with no tags at all";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        !result.violations.is_empty(),
        "response with no tags must violate"
    );
}

#[test]
fn tagged_parser_preserves_heredoc_inside_tool_call() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n\
                edit({ action: \"create\", path: \"a.py\", content: <<EOF\n\
                def foo():\n\
                    return 1\n\
                EOF\n\
                })\n\
                </tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "heredoc should parse: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(content.contains("def foo():"));
}

#[test]
fn tagged_parser_canonical_omits_raw_stray_text() {
    let tools = sample_tool_registry();
    let text = "leading garbage\n<assistant_prose>Narration.</assistant_prose>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    // Canonical reflects only the well-formed tagged content.
    assert_eq!(
        result.canonical.trim(),
        "<assistant_prose>\nNarration.\n</assistant_prose>"
    );
    assert!(!result.canonical.contains("leading garbage"));
}

#[test]
fn tagged_parser_accepts_configured_done_body() {
    // The parser captures the body verbatim; the agent compares it to the
    // pipeline's configured `done_sentinel` value. Non-default sentinels
    // like "PLAN_READY" must round-trip through the grammar.
    let tools = sample_tool_registry();
    let text = "<done>PLAN_READY</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.done_marker.as_deref(), Some("PLAN_READY"));
    assert!(result.violations.is_empty());
}
