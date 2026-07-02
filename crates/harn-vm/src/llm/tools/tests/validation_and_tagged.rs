use super::{
    collect_tool_schemas, json, parse_bare_calls_in_body, parse_text_tool_calls_with_tools,
    sample_tool_registry, validate_tool_args,
};

// Regression: text/bare-mode models (OpenRouter `qwen/qwen3-coder`) wrap
// their bare `name({ ... })` calls in `<tool_call>...</tool_call>` tags
// unpredictably even when the prompt asks for bare calls. Before the fix a
// same-line wrapper hid the call entirely (`tool_calls: []`) and a trailing
// `</tool_call>` leaked into the visible prose as a `_call>` fragment.
#[test]
fn bare_parser_executes_same_line_tool_call_wrapper() {
    let tools = sample_tool_registry();
    let text = "<tool_call>run({ command: \"cargo test\" })</tool_call>";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "wrapped call must execute (violations: {:?}, errors: {:?})",
        result.violations,
        result.errors
    );
    assert_eq!(result.calls[0]["name"], "run");
    assert_eq!(
        result.calls[0]["arguments"]["command"], "cargo test",
        "inner call args preserved"
    );
    assert!(result.errors.is_empty(), "no errors: {:?}", result.errors);
    assert!(
        !result.prose.contains("tool_call"),
        "wrapper must not leak into prose: {:?}",
        result.prose
    );
}

#[test]
fn bare_parser_strips_multiline_tool_call_wrapper_and_trailing_fragment() {
    let tools = sample_tool_registry();
    // Wrapper across its own lines, plus leading prose. This is the exact
    // qwen3-coder shape from the failing transcript.
    let text = "I'll find the file.\n<tool_call>\nrun({ command: \"cargo test\" })\n</tool_call>";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(result.calls[0]["name"], "run");
    assert_eq!(result.prose, "I'll find the file.");
    assert!(
        !result.prose.contains("_call>") && !result.prose.contains("tool_call"),
        "wrapper fragments must not leak: {:?}",
        result.prose
    );

    // A bare call with only a trailing `</tool_call>` fragment (model forgot
    // the open tag). The fragment must not survive into prose.
    let text = "run({ command: \"cargo test\" })\n</tool_call>";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1);
    assert!(
        result.prose.trim().is_empty(),
        "trailing fragment must not leak: {:?}",
        result.prose
    );
}

// End-to-end through the tagged protocol the runtime actually uses: a
// `<tool_call>`-wrapped call executes with no protocol violation and clean
// visible text, regardless of which protocol the prompt selected.
#[test]
fn tagged_parser_executes_tool_call_wrapper_cleanly() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\nrun({ command: \"cargo test\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "violations: {:?}", result.violations);
    assert_eq!(result.calls[0]["name"], "run");
    assert!(
        result.violations.is_empty(),
        "no violation for a properly wrapped call: {:?}",
        result.violations
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        !result.prose.contains("tool_call"),
        "no wrapper leak: {:?}",
        result.prose
    );
}

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
fn tagged_parser_accepts_nested_xml_json_args_tool_call_body() {
    // Some OpenAI-compatible value routes emit a generic XML function wrapper
    // inside Harn's `<tool_call>` block, for example:
    // `<tool_call><edit>{"action":"create",...}</edit></tool_call>`.
    // Recover only registered tool names and JSON-object arguments.
    let tools = sample_tool_registry();
    let text = r#"<tool_call><edit>{"action":"create","path":"a.rs","content":"fn a() {}"}</edit></tool_call>"#;
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
    assert!(
        result.canonical.contains("edit({"),
        "canonical replay should use Harn syntax: {}",
        result.canonical
    );
}

#[test]
fn tagged_parser_rejects_unknown_nested_xml_tool_call_body() {
    let tools = sample_tool_registry();
    let text = r#"<tool_call><deploy>{"target":"prod"}</deploy></tool_call>"#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.calls.is_empty(), "unknown tool must not execute");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("Unknown tool 'deploy'")),
        "unknown inner tag should be an actionable parse error: {:?}",
        result.errors
    );
}

// Shape 1 (dominant DeepSeek failure): the inner OPEN tag names a registered
// tool, the inner CLOSE tag is malformed (`</edit_call>`), and there is no
// outer `</tool_call>`. The JSON object is complete, so the call is recoverable
// — not truncated. Recover, canonicalize, and dispatch it.
#[test]
fn tagged_parser_recovers_nested_xml_mismatched_inner_close_no_outer_close() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<edit>\n{ \"action\": \"create\", \"path\": \"a.rs\", \"content\": \"fn a() {}\" }\n</edit_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1, "violations: {:?}", result.violations);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.contains("TRUNCATED")),
        "a complete JSON body must not be misreported as truncated: {:?}",
        result.errors
    );
    assert!(
        result.canonical.contains("edit({"),
        "canonical replay should use Harn syntax: {}",
        result.canonical
    );
}

// Shape 2: the inner tag is never closed and a duplicate/trailing
// `</tool_call>` follows the JSON object. Tolerate both; dispatch the call and
// swallow the orphan close tag without a noisy violation.
#[test]
fn tagged_parser_recovers_nested_xml_missing_inner_close_and_duplicate_outer_close() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<edit>\n{ \"action\": \"create\", \"path\": \"a.rs\" }\n</tool_call>\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1, "violations: {:?}", result.violations);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.rs"));
    assert!(
        result.violations.is_empty(),
        "duplicate trailing </tool_call> must be swallowed silently: {:?}",
        result.violations
    );
    assert!(
        result.canonical.contains("edit({"),
        "canonical replay should use Harn syntax: {}",
        result.canonical
    );
}

// Shape 3: a leading-dot float (`.100`) inside an otherwise-valid `name({ ... })`
// call. Recover `.100` -> `0.100` so the whole call is not dropped over one char.
#[test]
fn tagged_parser_recovers_leading_dot_float_in_tool_args() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\nrun({ command: \"x\", limit: .100, weight: -.5 })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1, "violations: {:?}", result.violations);
    assert_eq!(result.calls[0]["name"], json!("run"));
    assert_eq!(result.calls[0]["arguments"]["limit"], json!(0.100));
    assert_eq!(result.calls[0]["arguments"]["weight"], json!(-0.5));
}

// Negative: an unknown inner tag in the sloppy (mismatched-close, no outer
// close) shape must still be rejected — prose-with-angle-brackets must not be
// mis-parsed as a call. Mirrors the #3132 discipline through the new path.
#[test]
fn tagged_parser_rejects_unknown_nested_xml_tool_with_sloppy_close() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<frobnicate>\n{ \"target\": \"prod\" }\n</frobnicate_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.calls.is_empty(), "unknown tool must not execute");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("Unknown tool 'frobnicate'")
                || error.contains("TRUNCATED")),
        "unknown inner tag must be rejected (not dispatched): {:?}",
        result.errors
    );
}

// High-frequency DeepSeek shape: the model wraps its THINKING/narration in
// `<assistant_prose>` *inside* `<tool_call>`. This is not a malformed call —
// it took no action this turn. It must NOT be reported as a parse error
// (which wastes the turn telling the model it erred); the text is preserved as
// prose and the loop's normal no-tool-call nudge applies.
#[test]
fn tagged_parser_treats_wrapped_assistant_prose_as_narration_not_parse_error() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<assistant_prose>\nReading parser.go to understand ParseManifest.\n</assistant_prose>\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.calls.is_empty(), "narration emits no tool call");
    assert!(
        result.errors.is_empty(),
        "narration wrapped in <tool_call> must not be a parse error: {:?}",
        result.errors
    );
    assert!(
        result.violations.is_empty(),
        "narration wrapped in <tool_call> must not be a violation: {:?}",
        result.violations
    );
    assert!(
        !result
            .errors
            .iter()
            .chain(result.violations.iter())
            .any(|msg| msg.contains("could not be parsed")
                || msg.contains("did not contain a bare")
                || msg.contains("TRUNCATED")),
        "no 'could not be parsed' diagnostic for narration: errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    assert_eq!(
        result.prose, "Reading parser.go to understand ParseManifest.",
        "narration text preserved as prose"
    );
}

// A bare prose body (no inner tag, no call) the model wrapped in `<tool_call>`
// is treated identically to `<assistant_prose>` — narration, not a parse error.
#[test]
fn tagged_parser_treats_bare_prose_tool_call_body_as_narration() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\nReading the file to understand the layout.\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.calls.is_empty(), "bare prose emits no tool call");
    assert!(
        result.errors.is_empty(),
        "bare prose body must not be a parse error: {:?}",
        result.errors
    );
    assert!(
        result.violations.is_empty(),
        "bare prose body must not be a violation: {:?}",
        result.violations
    );
    assert_eq!(
        result.prose, "Reading the file to understand the layout.",
        "bare prose text preserved"
    );
}

// Mixed shape: the SAME `<tool_call>` wrapper carries an `<assistant_prose>`
// narration block AND a real call. Recover and dispatch the call; keep the
// narration as prose; emit no parse error.
#[test]
fn tagged_parser_recovers_real_call_alongside_wrapped_prose() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<assistant_prose>\nReading parser.go first.\n</assistant_prose>\nrun({ command: \"cat parser.go\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(
        result.errors.is_empty(),
        "mixed prose+call must not error: {:?}",
        result.errors
    );
    assert_eq!(
        result.calls.len(),
        1,
        "the real call must be recovered (violations: {:?})",
        result.violations
    );
    assert_eq!(result.calls[0]["name"], json!("run"));
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        json!("cat parser.go")
    );
    assert_eq!(
        result.prose, "Reading parser.go first.",
        "narration preserved alongside the recovered call"
    );
    assert!(
        result.canonical.contains("run({"),
        "canonical replay should carry the recovered call: {}",
        result.canonical
    );
}

// Negative: an unknown inner tag that LOOKS like an attempted call must STILL
// be rejected — the narration allowance is scoped to the narration allowlist,
// it does not turn every unknown wrapped tag into silent prose.
#[test]
fn tagged_parser_still_rejects_unknown_wrapped_tag_as_not_narration() {
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<frobnicate>\n{ \"target\": \"prod\" }\n</frobnicate>\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    assert!(result.calls.is_empty(), "unknown tool must not execute");
    assert!(
        result.prose.trim().is_empty(),
        "an unknown attempted call is not narration: {:?}",
        result.prose
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("Unknown tool 'frobnicate'")),
        "unknown inner tag must be rejected with an actionable error: {:?}",
        result.errors
    );
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
fn tagged_parser_reports_unclosed_user_response_as_unclosed_not_unknown() {
    let tools = sample_tool_registry();
    // A trailing <done> block keeps this NON-terminal: an unclosed response
    // tag whose remainder still carries another top-level block keeps the
    // strict violation (the terminal shape — nothing after the prose — is now
    // accepted as the block body; see corpus_conformance).
    let text = "<user_response>Visible answer without a close tag.\n<done>##DONE##</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result
            .violations
            .iter()
            .any(|violation| violation.contains("Unclosed <user_response> block")),
        "unclosed user_response should get a precise close-tag diagnostic: {:?}",
        result.violations
    );
    assert!(
        !result
            .violations
            .iter()
            .any(|violation| violation.contains("Unknown top-level tag")),
        "unclosed accepted tags must not be misreported as unknown: {:?}",
        result.violations
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
fn tagged_parser_keeps_tool_call_after_unbalanced_fence() {
    // Regression: the fence-parity check counted every ``` marker before the
    // cursor and called an odd count "inside a fence". A single UNCLOSED ```
    // earlier in the response (common when a model opens a code block in its
    // narration but never closes it) therefore made a later, legitimate
    // <tool_call> look fenced — the call was dropped and a spurious
    // protocol_violation injected. An unbalanced *trailing* fence must not
    // swallow a real block.
    let tools = sample_tool_registry();
    // The narration opens a ``` fence and never closes it before the real call.
    // It sits inside <assistant_prose> so the only top-level block the parser
    // must keep is the trailing <tool_call>.
    let text = "<assistant_prose>\nHere is the rough shape:\n```\nfn sketch() {}\n</assistant_prose>\n\
                <tool_call>\nedit({ action: \"create\", path: \"a.rs\", content: \"x\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "tool call after an unclosed fence must still parse: {:?}",
        result.calls
    );
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert!(
        result.violations.is_empty(),
        "no spurious protocol violation should be injected: {:?}",
        result.violations
    );
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

// Regression (#3011 follow-up): a large `<tool_call>edit({ content: … })`
// turn that the model truncated mid-argument (it hit its max output token
// limit before emitting `</tool_call>`). Before the fix the unclosed open
// tag fell through to the generic "unknown top-level tag" path and the
// `edit(...)` body became stray text, so the turn parsed to ZERO tool calls
// and the agent loop silently stalled as if the model had produced only
// prose. The parser must now surface an actionable TOOL CALL TRUNCATED
// error (naming the recovered tool) instead of dropping the signal.
#[test]
fn tagged_parser_flags_truncated_unclosed_tool_call() {
    let tools = sample_tool_registry();
    // Prose, then an opened-but-never-closed tool call cut off mid-string.
    let text = "Now I'll create the error module.\n\
                <tool_call>\n\
                edit({ action: \"create\", path: \"src/error.rs\", content: \"//! Unified storage error types.\\nuse std::io;\\n\
                pub enum StorageError {\\n    IoError(";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));

    // The truncated call cannot be dispatched (the argument string is
    // unterminated), so there are no recovered calls...
    assert!(
        result.calls.is_empty(),
        "truncated call must not dispatch: {:?}",
        result.calls
    );
    // ...but the loop must see a precise, recoverable truncation error that
    // names the tool, NOT a generic "unknown top-level tag" / "stray text".
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("TOOL CALL TRUNCATED") && e.contains("edit")),
        "expected a TOOL CALL TRUNCATED error naming `edit`, got errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    assert!(
        !result
            .violations
            .iter()
            .any(|v| v.contains("Unknown top-level tag")),
        "must not misreport the truncated open tag as an unknown tag: {:?}",
        result.violations
    );
}

// A properly closed `<tool_call>` is unaffected by the truncation branch:
// it still parses and dispatches normally.
#[test]
fn tagged_parser_closed_tool_call_unaffected_by_truncation_branch() {
    let tools = sample_tool_registry();
    let text =
        "<tool_call>\nedit({ action: \"create\", path: \"a.rs\", content: \"x\" })\n</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors={:?}", result.errors);
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert!(
        !result.errors.iter().any(|e| e.contains("TRUNCATED")),
        "closed call must not be flagged as truncated: {:?}",
        result.errors
    );
}

// ---------------------------------------------------------------------------
// Shape 1: template literals + `+`-concatenated string fragments in a value.
// ---------------------------------------------------------------------------

// A multi-line backtick template literal as a value (Go-ish content) parses to
// the literal body. (Single-line backticks were already covered; this guards
// the multi-line body the corpus emits.)
#[test]
fn value_accepts_multiline_template_literal() {
    let tools = sample_tool_registry();
    let text =
        "edit({ action: \"create\", path: \"a.go\", content: `package main\n\nfunc main() {}\n` })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("package main\n\nfunc main() {}\n")
    );
}

// Plain `+`-concatenation of two quoted strings collapses to one value.
#[test]
fn value_folds_string_concatenation() {
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.txt\", content: \"hello \" + \"world\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("hello world")
    );
}

// The dominant corpus shape: a Go file body where a backtick struct tag forces
// the model into ``…` + "`json:\"x\"`" + `…`` concatenation. The fragments
// (template + quoted + template) must collapse into one content string with the
// literal backtick struct tag preserved.
#[test]
fn value_folds_go_backtick_struct_tag_concatenation() {
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"status.go\", content: `package main\n\ntype S struct {\n\tServices []ServiceStatus ` + \"`json:\\\"services\\\"`\" + ` // tail\n}\n` })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("`json:\"services\"`"),
        "backtick struct tag must survive concatenation: {content:?}"
    );
    assert!(
        content.starts_with("package main") && content.contains("type S struct {"),
        "surrounding template fragments must be joined: {content:?}"
    );
}

// Negative: a `+` whose right operand is NOT a string is malformed
// concatenation — the parser must reject loudly, never guess.
#[test]
fn value_rejects_non_string_concatenation_operand() {
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.txt\", content: \"x\" + 1 })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.calls.is_empty(),
        "non-string concat operand must not dispatch: {:?}",
        result.calls
    );
    assert!(
        !result.errors.is_empty(),
        "should error: {:?}",
        result.errors
    );
}

// ---------------------------------------------------------------------------
// Shape 3: `=` accepted as a synonym for `:` as the object key/value separator.
// ---------------------------------------------------------------------------

#[test]
fn object_accepts_equals_as_key_value_separator() {
    let tools = sample_tool_registry();
    // `=` before a scalar and before a template body, mixed with a normal `:`.
    let text = "edit({ action= \"create\", path: \"a.go\", content= `x` })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.go"));
    assert_eq!(result.calls[0]["arguments"]["content"], json!("x"));
}

#[test]
fn object_accepts_equals_before_heredoc_value() {
    let tools = sample_tool_registry();
    let text = "edit({\n    action: \"create\",\n    path: \"a.go\",\n    content= <<EOF\npackage main\nEOF\n})";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"],
        json!("package main")
    );
}
