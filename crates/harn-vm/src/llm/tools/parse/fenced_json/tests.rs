use super::*;

fn parse(text: &str) -> TextToolParseResult {
    parse_fenced_json_tool_calls(text)
}

fn arg<'a>(call: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    call.get("arguments")?.get(key)
}

// S1: trivial single call.
#[test]
fn parses_a_single_clean_call() {
    let out = parse("```tool\n{\"name\": \"read_file\", \"args\": {\"path\": \"a.rs\"}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "read_file");
    assert_eq!(arg(&out.calls[0], "path").unwrap(), "a.rs");
}

// S1b: tool/arguments dialect aliases (MiniMax / qwen / gpt-oss on text).
#[test]
fn parses_tool_arguments_dialect_aliases() {
    let out = parse("```tool\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "read_file");
    assert_eq!(arg(&out.calls[0], "path").unwrap(), "a.rs");
}

#[test]
fn unwraps_generic_tool_wrapper_envelope() {
    let out = parse(
            "```tool\n{\"name\":\"tool\",\"args\":{\"name\":\"look\",\"args\":{\"intent\":\"read\",\"file\":\"src/lib.rs\"}}}\n```",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(arg(&out.calls[0], "intent").unwrap(), "read");
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "src/lib.rs");
}

#[test]
fn strips_harmony_channel_suffix_from_tool_name() {
    let out = parse(
            "```tool\n{\"name\":\"run<|channel|>commentary\",\"args\":{\"command\":\"cargo test\"}}\n```",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "run");
    assert_eq!(arg(&out.calls[0], "command").unwrap(), "cargo test");
}

// S1c: canonical name/args win when both canonical and alias are present.
#[test]
fn canonical_keys_win_over_aliases() {
    let out = parse(
            "```tool\n{\"name\": \"canonical\", \"tool\": \"alias\", \"args\": {\"k\": 1}, \"arguments\": {\"k\": 2}}\n```",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "canonical");
    assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
}

// S3: delimiter soup — content contains ```, <<EOF, a bare }, and </tool>.
// All survive as \n-escaped JSON bytes; nothing fronts a real line.
#[test]
fn content_with_backticks_heredoc_brace_and_tag_survives() {
    let content = "```\nx := `raw`\n<<EOF\n}\n</tool>\n```";
    let json_content = serde_json::to_string(content).unwrap();
    let src = format!(
            "```tool\n{{\"name\": \"write_file\", \"args\": {{\"path\": \"f.go\", \"content\": {json_content}}}}}\n```"
        );
    let out = parse(&src);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(arg(&out.calls[0], "content").unwrap(), content);
}

// S4: N fences for N calls.
#[test]
fn multiple_fences_yield_multiple_calls() {
    let src = "```tool\n{\"name\": \"a\", \"args\": {}}\n```\nsome prose\n```tool\n{\"name\": \"b\", \"args\": {\"k\": 1}}\n```";
    let out = parse(src);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 2);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(out.calls[1]["name"], "b");
    assert!(out.prose.contains("some prose"));
}

// A body whose literal first line is `<<EOF` round-trips as JSON content —
// no heredoc marker exists to leak into the file.
#[test]
fn content_starting_with_heredoc_opener_is_just_a_string() {
    let content = "<<EOF\npackage main\n";
    let json_content = serde_json::to_string(content).unwrap();
    let src = format!(
        "```tool\n{{\"name\": \"write_file\", \"args\": {{\"content\": {json_content}}}}}\n```"
    );
    let out = parse(&src);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(arg(&out.calls[0], "content").unwrap(), content);
}

// A JSON array is not a batch of objects: it is a single non-object entry and
// is rejected with zero calls (a batch is consecutive objects, not an array).
#[test]
fn array_body_is_rejected_as_non_object() {
    let out = parse("```tool\n[{\"name\": \"a\", \"args\": {}}]\n```");
    assert!(out.calls.is_empty(), "calls: {:?}", out.calls);
    assert_eq!(out.errors.len(), 1);
    assert!(
        out.errors[0].contains("non-object") || out.errors[0].contains("one or more JSON"),
        "got: {}",
        out.errors[0]
    );
}

// Salvage: a complete object followed by trailing non-JSON bytes dispatches the
// object as a call and surfaces an error for the unparsed tail (never zero).
#[test]
fn trailing_bytes_after_object_salvages_the_object() {
    let out = parse("```tool\n{\"name\": \"a\", \"args\": {}} trailing\n```");
    assert_eq!(out.calls.len(), 1, "calls: {:?}", out.calls);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(out.errors.len(), 1, "errors: {:?}", out.errors);
}

// MissingName: object without a name.
#[test]
fn missing_name_rejected() {
    let out = parse("```tool\n{\"args\": {\"path\": \"a\"}}\n```");
    assert!(out.calls.is_empty());
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("missing a non-empty string `name`"));
}

// MissingName: empty-string name.
#[test]
fn empty_name_rejected() {
    let out = parse("```tool\n{\"name\": \"  \", \"args\": {}}\n```");
    assert!(out.calls.is_empty());
    assert!(out.errors[0].contains("`name`"));
}

// ArgsNotObject: args is a scalar.
#[test]
fn args_not_object_rejected() {
    let out = parse("```tool\n{\"name\": \"a\", \"args\": \"oops\"}\n```");
    assert!(out.calls.is_empty());
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("must be a JSON object"));
}

// Absent args -> {} (downstream validates required params).
#[test]
fn absent_args_is_empty_object() {
    let out = parse("```tool\n{\"name\": \"list_dir\"}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert!(out.calls[0]["arguments"].is_object());
    assert_eq!(out.calls[0]["arguments"].as_object().unwrap().len(), 0);
}

// Unterminated: fence opens, JSON string is truncated, no close fence.
// Must be rejected, never half-applied.
#[test]
fn truncated_string_is_unterminated_not_half_applied() {
    let out = parse("```tool\n{\"name\": \"write_file\", \"args\": {\"content\": \"half a str");
    assert!(out.calls.is_empty(), "must not dispatch a truncated call");
    assert_eq!(out.errors.len(), 1);
    assert!(
        out.errors[0].contains("Unterminated"),
        "got: {}",
        out.errors[0]
    );
}

// Implicit close: fence opens, a complete object, but EOF before the close
// fence. A balanced/complete object is accepted (implicit close).
#[test]
fn complete_object_without_close_fence_is_accepted() {
    let out = parse("```tool\n{\"name\": \"a\", \"args\": {\"k\": 1}}");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "a");
}

// ```json accept-with-warning: a valid object in a ```json fence parses,
// and emits a protocol_violation so telemetry sees the drift.
#[test]
fn json_fence_accepts_with_protocol_violation() {
    let out = parse("```json\n{\"name\": \"a\", \"args\": {}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "a");
    assert!(
        out.violations
            .iter()
            .any(|v| v.contains("protocol_violation")),
        "violations: {:?}",
        out.violations
    );
}

#[test]
fn tool_like_fence_drift_accepts_with_protocol_violation() {
    for (src, opener) in [
        (
            "```tool_code\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
            "```tool_code",
        ),
        (
            "```tool python\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
            "```tool python",
        ),
        (
            "```function_call\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
            "```function_call",
        ),
        (
            "~~~tool\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n~~~",
            "~~~tool",
        ),
    ] {
        let out = parse(src);
        assert!(
            out.errors.is_empty(),
            "errors for {opener}: {:?}",
            out.errors
        );
        assert_eq!(out.calls.len(), 1, "calls for {opener}: {:?}", out.calls);
        assert_eq!(out.calls[0]["name"], "a");
        assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
        assert!(
            out.violations.iter().any(|v| v.contains(opener)),
            "violations for {opener}: {:?}",
            out.violations
        );
    }
}

#[test]
fn invalid_tool_like_fence_drift_reports_error_and_violation() {
    let out = parse("```tool_code\nnot json\n```");
    assert!(out.calls.is_empty());
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("not valid JSON"));
    assert!(
        out.violations.iter().any(|v| v.contains("```tool_code")),
        "violations: {:?}",
        out.violations
    );
}

#[test]
fn bare_json_tool_call_accepts_with_protocol_violation() {
    let out = parse("{\"name\": \"a\", \"args\": {\"k\": 1}}");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert!(out.prose.is_empty(), "prose: {:?}", out.prose);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
    assert!(
        out.violations
            .iter()
            .any(|v| v.contains("bare JSON object")),
        "violations: {:?}",
        out.violations
    );
}

#[test]
fn chat_template_envelope_recovers_multiple_inline_argument_calls() {
    // Exact production shape from the Qwen chat template: one `<tool>`
    // marker introduces consecutive objects, and the template leaves its
    // arguments inline instead of nesting them under `args`.
    let out = parse(
            "<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"src/writer.zig\"}\n{\"name\":\"look\",\"file\":\"src/parser.zig\"}\n</tool_calls>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert!(out.prose.is_empty(), "prose: {:?}", out.prose);
    assert_eq!(out.calls.len(), 2);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(
        arg(&out.calls[0], "file"),
        Some(&serde_json::json!("src/writer.zig"))
    );
    assert_eq!(out.calls[1]["name"], "look");
    assert_eq!(
        arg(&out.calls[1], "file"),
        Some(&serde_json::json!("src/parser.zig"))
    );
    assert!(
        out.violations
            .iter()
            .any(|violation| violation.contains("chat-template")),
        "violations: {:?}",
        out.violations
    );
}

#[test]
fn chat_template_envelope_accepts_optional_per_call_close_markers() {
    let out = parse(
            "<tool_calls><tool>{\"name\":\"a\",\"args\":{\"k\":1}}</tool><tool>{\"tool\":\"b\",\"arguments\":{\"v\":2}}</tool></tool_calls>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 2);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(arg(&out.calls[0], "k"), Some(&serde_json::json!(1)));
    assert_eq!(out.calls[1]["name"], "b");
    assert_eq!(arg(&out.calls[1], "v"), Some(&serde_json::json!(2)));
}

#[test]
fn malformed_chat_template_envelope_never_dispatches_partial_calls() {
    let out = parse("<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"src/writer.zig\"");
    assert!(
        out.calls.is_empty(),
        "partial calls must not dispatch: {:?}",
        out.calls
    );
    assert_eq!(out.errors.len(), 1);
    assert!(
        out.errors[0].contains("<tool_calls>") && out.errors[0].contains("not executed"),
        "error: {}",
        out.errors[0]
    );
}

#[test]
fn truncated_chat_template_second_marker_never_dispatches_first_call() {
    let out =
        parse("<tool_calls><tool>{\"name\":\"look\",\"file\":\"src/lib.rs\"}<tool></tool_calls>");
    assert!(
        out.calls.is_empty(),
        "partial calls must not dispatch: {:?}",
        out.calls
    );
    assert_eq!(out.errors.len(), 1);
    assert!(
        out.errors[0].contains("ended without a complete JSON object")
            && out.errors[0].contains("not executed"),
        "error: {}",
        out.errors[0]
    );
}

#[test]
fn unmatched_chat_template_tool_close_is_rejected() {
    let out = parse("<tool_calls></tool></tool_calls>");
    assert!(out.calls.is_empty());
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("unmatched `</tool>`"));
}

#[test]
fn chat_template_without_tool_marker_is_rejected() {
    let out = parse("<tool_calls>{\"name\":\"look\",\"file\":\"src/writer.zig\"}</tool_calls>");
    assert!(out.calls.is_empty());
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("no `<tool>` marker"));
}

#[test]
fn legacy_tagged_markup_under_json_reports_protocol_violation() {
    let out = parse("<tool_call>\na({})\n</tool_call>");
    assert!(out.calls.is_empty());
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert!(
        out.violations.iter().any(|v| v.contains("<tool_call>")),
        "violations: {:?}",
        out.violations
    );
}

// A non-tool fence (```python) is left in prose, not eaten as a call.
#[test]
fn unrelated_fence_stays_in_prose() {
    let out = parse("```python\nprint('hi')\n```");
    assert!(out.calls.is_empty());
    assert!(out.errors.is_empty());
    assert!(out.prose.contains("print('hi')"));
}

// A non-tool tilde fence is also left in prose, not eaten as a call.
#[test]
fn unrelated_tilde_fence_stays_in_prose() {
    let out = parse("~~~python\nprint('hi')\n~~~");
    assert!(out.calls.is_empty());
    assert!(out.errors.is_empty());
    assert!(out.prose.contains("print('hi')"));
}

// Content containing a bare ``` line INSIDE the JSON string still parses —
// because the JSON string cannot hold a raw newline, the embedded ``` is on
// its own \n-escaped segment, never fronting a real source line.
#[test]
fn embedded_backtick_fence_does_not_close_early() {
    // Whole object on ONE source line; the ``` is inside the JSON string.
    let content = "before\n```\nafter";
    let json_content = serde_json::to_string(content).unwrap();
    let src = format!("```tool\n{{\"name\": \"w\", \"args\": {{\"c\": {json_content}}}}}\n```");
    let out = parse(&src);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(arg(&out.calls[0], "c").unwrap(), content);
}

// Content containing `</tool>` survives (no XML/tag channel here).
#[test]
fn content_with_close_tool_tag_survives() {
    let content = "x </tool> y";
    let json_content = serde_json::to_string(content).unwrap();
    let src = format!("```tool\n{{\"name\": \"w\", \"args\": {{\"c\": {json_content}}}}}\n```");
    let out = parse(&src);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(arg(&out.calls[0], "c").unwrap(), content);
}

// ===================================================================
// Broadened text-envelope recovery (qwen3.6 llamacpp json-lane probe).
// ===================================================================

// (a) XML envelope, three tool-tag calls, EOS before `</tool_calls>`.
#[test]
fn xml_envelope_three_calls_without_close_parses() {
    let out = parse(
        "<tool_calls>\n<look>\n<file>\nsrc/writer.zig\n</file>\n</look>\n\
             <look>\n<file>\nsrc/parser.zig\n</file>\n</look>\n\
             <look>\n<file>\nsrc/root.zig\n</file>\n</look>",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert!(out.prose.is_empty(), "prose: {:?}", out.prose);
    assert_eq!(out.calls.len(), 3);
    for (call, file) in out
        .calls
        .iter()
        .zip(["src/writer.zig", "src/parser.zig", "src/root.zig"])
    {
        assert_eq!(call["name"], "look");
        assert_eq!(arg(call, "file").unwrap(), file);
    }
    assert!(
        out.violations.iter().any(|v| v.contains("chat-template")),
        "violations: {:?}",
        out.violations
    );
}

// (a2) single XML call with two args, EOS before `</tool_calls>`.
#[test]
fn xml_envelope_single_multi_arg_call_without_close_parses() {
    let out = parse(
        "<tool_calls>\n<look>\n<file>\nsrc/writer.zig\n</file>\n<intent>\nread\n</intent>\n</look>",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "src/writer.zig");
    assert_eq!(arg(&out.calls[0], "intent").unwrap(), "read");
}

// Multi-line XML argument value is kept verbatim (inner newlines survive,
// outer whitespace trimmed).
#[test]
fn xml_envelope_multiline_arg_value_kept_verbatim() {
    let out = parse(
        "<tool_calls>\n<write>\n<content>\nline one\nline two\n</content>\n</write>\n</tool_calls>",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "write");
    assert_eq!(arg(&out.calls[0], "content").unwrap(), "line one\nline two");
}

// (b) `<tool_code>` tag wrapping a JSON object with inline arguments beside
// `name`, EOS before `</tool_code>`.
#[test]
fn tool_code_tag_with_inline_json_args_parses() {
    let out = parse(
        "<tool_code>\n{ \"name\": \"look\", \"file\": \"src/writer.zig\", \"intent\": \"read\" }",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "src/writer.zig");
    assert_eq!(arg(&out.calls[0], "intent").unwrap(), "read");
    assert!(
        out.violations.iter().any(|v| v.contains("chat-template")),
        "violations: {:?}",
        out.violations
    );
}

// `<tool_code>` wrapping multiple consecutive JSON objects.
#[test]
fn tool_code_tag_with_multiple_json_objects_parses() {
    let out = parse(
            "<tool_code>\n{\"name\":\"a\",\"args\":{\"k\":1}}\n{\"name\":\"b\",\"args\":{\"v\":2}}\n</tool_code>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 2);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
    assert_eq!(out.calls[1]["name"], "b");
    assert_eq!(arg(&out.calls[1], "v").unwrap(), 2);
}

// Bare singular `<tool_call>` tag wrapping a JSON object parses.
#[test]
fn bare_tool_call_tag_with_json_parses() {
    let out = parse("<tool_call>\n{\"name\":\"look\",\"args\":{\"file\":\"a.rs\"}}\n</tool_call>");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "a.rs");
}

// (c) prose BEFORE a `<tool_calls>` envelope: the prose stays prose, the
// envelope still parses (the prefix-only bug misrouted this to completion
// feedback).
#[test]
fn prose_before_envelope_parses_and_keeps_prose() {
    let out = parse(
            "I will read the writer next.\n<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"src/writer.zig\"}\n</tool_calls>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "look");
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "src/writer.zig");
    assert!(
        out.prose.contains("I will read the writer next."),
        "prose: {:?}",
        out.prose
    );
}

// Prose AFTER the envelope close also survives as prose.
#[test]
fn prose_after_envelope_close_survives() {
    let out = parse(
        "<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"a.rs\"}\n</tool_calls>\nDone reading.",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert!(
        out.prose.contains("Done reading."),
        "prose: {:?}",
        out.prose
    );
}

// Truncated inner XML tag at EOS -> violation, zero calls (never dispatch a
// guessed/truncated call).
#[test]
fn truncated_inner_xml_tag_is_violation_zero_calls() {
    let out = parse("<tool_calls>\n<look>\n<file>\nsrc/writer.zig");
    assert!(out.calls.is_empty(), "calls: {:?}", out.calls);
    assert_eq!(out.errors.len(), 1);
    assert!(
        out.errors[0].contains("<tool_calls>") && out.errors[0].contains("not executed"),
        "error: {}",
        out.errors[0]
    );
}

// Envelope opener with a garbage body -> violation, never silently None.
#[test]
fn envelope_with_garbage_body_is_violation_not_none() {
    let out = parse("<tool_code>\nlol this is not json at all\n</tool_code>");
    assert!(out.calls.is_empty(), "calls: {:?}", out.calls);
    assert_eq!(out.errors.len(), 1, "errors: {:?}", out.errors);
    assert!(
        out.errors[0].contains("not executed"),
        "error: {}",
        out.errors[0]
    );
}

// Prose + canonical ```tool fence still parses (regression pin: the fenced
// grammar is untouched by envelope recovery).
#[test]
fn prose_before_canonical_tool_fence_still_parses() {
    let out = parse("Here is the call:\n```tool\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert!(
        out.violations.is_empty(),
        "violations: {:?}",
        out.violations
    );
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "a");
    assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
    assert!(
        out.prose.contains("Here is the call:"),
        "prose: {:?}",
        out.prose
    );
}

// A repeated XML argument tag makes the call ambiguous -> loud violation,
// zero calls (never take the last write and dispatch a guessed value).
#[test]
fn xml_envelope_duplicate_arg_is_violation_zero_calls() {
    let out = parse(
            "<tool_calls>\n<look>\n<file>\na.rs\n</file>\n<file>\nb.rs\n</file>\n</look>\n</tool_calls>",
        );
    assert!(out.calls.is_empty(), "calls: {:?}", out.calls);
    assert_eq!(out.errors.len(), 1, "errors: {:?}", out.errors);
    assert!(
        out.errors[0].contains("<file>") && out.errors[0].contains("not executed"),
        "error: {}",
        out.errors[0]
    );
}

// A JSON string argument that contains a literal `</tool_code>` is content,
// not the envelope close: structural close recognition keeps the whole
// object intact (the raw-`find` scan truncated it before the JSON parser).
#[test]
fn tool_code_json_string_with_literal_close_tag_survives() {
    let out = parse(
            "<tool_code>\n{ \"name\": \"write\", \"args\": { \"content\": \"</tool_code>\" } }\n</tool_code>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "write");
    assert_eq!(arg(&out.calls[0], "content").unwrap(), "</tool_code>");
}

// Same for the `<tool>`-marker JSON dialect: a literal `</tool_calls>` inside
// a JSON string value survives instead of truncating the envelope.
#[test]
fn tool_marker_json_string_with_literal_envelope_close_survives() {
    let out = parse(
            "<tool_calls>\n<tool>\n{ \"name\": \"write\", \"content\": \"</tool_calls>\" }\n</tool_calls>",
        );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "write");
    assert_eq!(arg(&out.calls[0], "content").unwrap(), "</tool_calls>");
}

// An XML argument VALUE containing a literal `</tool_calls>` is content: the
// envelope close is recognized only at a call-block boundary, so the value
// (closed by its own `</content>`) is preserved verbatim.
#[test]
fn xml_arg_value_with_literal_envelope_close_survives() {
    let out = parse(
        "<tool_calls>\n<write>\n<content>\n</tool_calls>\n</content>\n</write>\n</tool_calls>",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "write");
    assert_eq!(arg(&out.calls[0], "content").unwrap(), "</tool_calls>");
}

// ===================================================================
// RC0a — turn-1 batched-multi-read robustness (harn#5033). The zig-test
// probe runs all open with a batched 3-read that parsed to zero across
// every dialect; these pin the canonical batch shapes to N calls.
// ===================================================================

// 172505: one ```tool fence containing THREE JSON objects (the canonical
// turn-1 batched grounding read) -> 3 calls, no error.
#[test]
fn batch_three_objects_in_one_tool_fence_parses() {
    let out = parse(
        "```tool\n\
         {\"name\": \"look\", \"args\": {\"file\": \"src/writer.zig\"}}\n\
         {\"name\": \"look\", \"args\": {\"file\": \"src/parser.zig\"}}\n\
         {\"name\": \"look\", \"args\": {\"file\": \"src/document.zig\"}}\n\
         ```",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 3, "calls: {:?}", out.calls);
    for (call, file) in
        out.calls
            .iter()
            .zip(["src/writer.zig", "src/parser.zig", "src/document.zig"])
    {
        assert_eq!(call["name"], "look");
        assert_eq!(arg(call, "file").unwrap(), file);
    }
    // Distinct ids so downstream dispatch keeps them ordered/separate.
    assert_eq!(out.calls[0]["id"], "tc_0");
    assert_eq!(out.calls[2]["id"], "tc_2");
}

// 180749 / 181153: double ```tool fence — the model uses the opener as a
// SEPARATOR between calls and never closes the first block -> 2 calls.
#[test]
fn batch_double_tool_fence_separator_parses() {
    let out = parse(
        "```tool\n{\"name\": \"look\", \"args\": {\"file\": \"a.zig\"}}\n\
         ```tool\n{\"name\": \"look\", \"args\": {\"file\": \"b.zig\"}}",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 2, "calls: {:?}", out.calls);
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "a.zig");
    assert_eq!(arg(&out.calls[1], "file").unwrap(), "b.zig");
}

// Salvage: complete objects before a truncated tail still dispatch, and the
// truncated tail is a loud error (never half-applied).
#[test]
fn batch_salvages_complete_objects_before_truncated_tail() {
    let out = parse(
        "```tool\n{\"name\": \"look\", \"args\": {\"file\": \"a.zig\"}}\n\
         {\"name\": \"look\", \"args\": {\"file\": \"b.zig\"}}\n\
         {\"name\": \"look\", \"args\": {\"file\": \"trunc",
    );
    assert_eq!(out.calls.len(), 2, "calls: {:?}", out.calls);
    assert_eq!(arg(&out.calls[0], "file").unwrap(), "a.zig");
    assert_eq!(arg(&out.calls[1], "file").unwrap(), "b.zig");
    assert_eq!(out.errors.len(), 1, "errors: {:?}", out.errors);
    assert!(
        out.errors[0].contains("Unterminated"),
        "error: {}",
        out.errors[0]
    );
}

// 170636 / 181632: `<tool_calls>` XML with three nested `<look>` tags (envelope
// dialect from #5015) parses the full batch to 3 calls.
#[test]
fn batch_tool_calls_xml_three_nested_looks_parses() {
    let out = parse(
        "<tool_calls>\n<look>\n<file>\nsrc/writer.zig\n</file>\n</look>\n\
         <look>\n<file>\nsrc/parser.zig\n</file>\n</look>\n\
         <look>\n<file>\nsrc/document.zig\n</file>\n</look>\n</tool_calls>",
    );
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 3, "calls: {:?}", out.calls);
    for call in &out.calls {
        assert_eq!(call["name"], "look");
    }
}

// A single clean object in one fence is still exactly one call (N=1 batch).
#[test]
fn batch_single_object_is_one_call() {
    let out = parse("```tool\n{\"name\": \"look\", \"args\": {\"file\": \"a.zig\"}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
}

// ─── The verbatim content lane (harn#5033) ──────────────────────────────────

/// Wrap `payload` (byte-exact intended file content) into a ```tool block whose
/// `arg` value is the count-anchored heredoc opener `<<TAG:N` and whose matching
/// `<<TAG:N ... TAG` body trails the JSON header. `payload` must be
/// newline-terminated; `N` is exactly its newline count. Reuses the shared
/// scan_heredoc grammar — no bespoke sentinel.
fn verbatim_block(
    name: &str,
    other_args_json: &str,
    arg: &str,
    tag: &str,
    payload: &str,
) -> String {
    let n = payload.matches('\n').count();
    let sep = if other_args_json.is_empty() { "" } else { ", " };
    format!(
        "```tool\n{{ \"name\": \"{name}\", \"args\": {{ {other_args_json}{sep}\"{arg}\": \"<<{tag}:{n}\" }} }}\n\
         <<{tag}:{n}\n{payload}{tag}\n```",
    )
}

// GATE (harn#5033): tc_19's ~921-line Zig test suite — the payload the 2× JSON
// backslash transform mangled — round-trips byte-exact through the count-anchored
// heredoc. The real rejected-edit body from the P3 run (249 `\`, 611 `"`).
#[test]
fn tc19_zig_body_round_trips_byte_exact() {
    let payload = include_str!("fixtures/tc19_zig_tests.zig");
    let block = verbatim_block(
        "edit_file",
        "\"path\": \"src/writer.zig\"",
        "new_string",
        "Z",
        payload,
    );
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0]["name"], "edit_file");
    assert_eq!(arg(&out.calls[0], "path").unwrap(), "src/writer.zig");
    assert_eq!(
        arg(&out.calls[0], "new_string").unwrap().as_str().unwrap(),
        payload
    );
}

// ADVERSARIAL: the body contains a line that is exactly the close tag. The count
// is authoritative, so the interior tag line is content, not an early close.
#[test]
fn verbatim_body_containing_the_tag_line_round_trips() {
    let payload = "before\nEND\nmiddle\nEND\nafter\n";
    let block = verbatim_block(
        "write_file",
        "\"path\": \"a.txt\"",
        "content",
        "END",
        payload,
    );
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        payload
    );
}

// ADVERSARIAL: every #5015-promised literal — a bare ``` fence, `<<EOF`, `}`,
// `</tool>`, `</tool_calls>`, backticks — appears in the body and survives.
#[test]
fn verbatim_body_with_all_promised_literals_round_trips() {
    let payload = "```\n<<EOF\n}\n</tool>\n</tool_calls>\nconst raw = `go run`;\n```\nEOF\n";
    let block = verbatim_block("write_file", "\"path\": \"b.txt\"", "content", "T", payload);
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        payload
    );
}

// ADVERSARIAL: an empty body (`<<T:0`) is a valid segment.
#[test]
fn verbatim_empty_body_round_trips() {
    let block = verbatim_block("write_file", "\"path\": \"empty.txt\"", "content", "T", "");
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(arg(&out.calls[0], "content").unwrap().as_str().unwrap(), "");
}

// ADVERSARIAL: a CRLF body keeps every `\r` (scan_heredoc's byte-exact count slice).
#[test]
fn verbatim_crlf_body_round_trips() {
    let payload = "line one\r\nline two\r\n\r\nline four\r\n";
    let block = verbatim_block(
        "write_file",
        "\"path\": \"crlf.txt\"",
        "content",
        "T",
        payload,
    );
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        payload
    );
}

// A ```tool separator inside a count-anchored body is content, NOT the #5037
// double-fence split — the count protects the body interior.
#[test]
fn verbatim_tool_fence_in_body_is_content_not_split() {
    let payload = "line a\n```tool\nline c\n";
    let block = verbatim_block("write_file", "\"path\": \"d.txt\"", "content", "T", payload);
    let out = parse(&block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        payload
    );
}

// Two verbatim arguments in one call bind by their opener string; the trailing
// bodies are matched to the args regardless of JSON key order.
#[test]
fn verbatim_two_bodies_matched_by_opener() {
    let block = "```tool\n{ \"name\": \"edit_file\", \"args\": { \"path\": \"x.rs\", \
         \"old_string\": \"<<OLD:1\", \"new_string\": \"<<NEW:2\" } }\n\
         <<NEW:2\nNEW a\\b\nNEW c\nNEW\n\
         <<OLD:1\nOLD \"q\"\nOLD\n```";
    let out = parse(block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        arg(&out.calls[0], "new_string").unwrap().as_str().unwrap(),
        "NEW a\\b\nNEW c\n"
    );
    assert_eq!(
        arg(&out.calls[0], "old_string").unwrap().as_str().unwrap(),
        "OLD \"q\"\n"
    );
}

// Verbatim coexists with #5037 batching: a batched read object plus a verbatim
// edit object in one block both dispatch, the body binding only the edit.
#[test]
fn verbatim_coexists_with_batched_object() {
    let block = "```tool\n{ \"name\": \"read_file\", \"args\": { \"path\": \"a.zig\" } }\n\
         { \"name\": \"write_file\", \"args\": { \"path\": \"b.zig\", \"content\": \"<<C:1\" } }\n\
         <<C:1\nconst x = \"a\\tb\";\nC\n```";
    let out = parse(block);
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 2, "calls: {:?}", out.calls);
    assert_eq!(out.calls[0]["name"], "read_file");
    assert_eq!(out.calls[1]["name"], "write_file");
    assert_eq!(
        arg(&out.calls[1], "content").unwrap().as_str().unwrap(),
        "const x = \"a\\tb\";\n"
    );
}

// A wrong declared count fails loud (the close tag is not on line N+1) and never
// dispatches a half-applied call.
#[test]
fn verbatim_wrong_count_fails_loud() {
    // Declares 5 lines but only two precede the close tag.
    let block = "```tool\n{ \"name\": \"write_file\", \"args\": { \"path\": \"a\", \"content\": \"<<C:5\" } }\n\
         <<C:5\nl1\nl2\nC\n```";
    let out = parse(block);
    assert!(out.calls.is_empty(), "must not dispatch: {:?}", out.calls);
    assert!(
        out.errors
            .iter()
            .any(|e| e.contains("recount") || e.contains("did not close")),
        "expected actionable recount error, got {:?}",
        out.errors
    );
}

fn assert_verbatim_rejected(block: &str, expected_error_count: usize) {
    let out = parse(block);
    assert_eq!(out.calls, Vec::<serde_json::Value>::new());
    assert_eq!(out.errors.len(), expected_error_count, "{:?}", out.errors);
}

#[test]
fn verbatim_count_overflow_never_downgrades_or_dispatches() {
    assert_verbatim_rejected(
        "```tool\n{ \"name\": \"write_file\", \"args\": { \"content\": \"<<C:184467440737095516160\" } }\n\
         <<C:184467440737095516160\nbody\nC\n```",
        1,
    );
}

#[test]
fn verbatim_counted_close_with_suffix_never_drops_bytes_or_dispatches() {
    assert_verbatim_rejected(
        "```tool\n{ \"name\": \"write_file\", \"args\": { \"content\": \"<<C:1\" } }\n\
         <<C:1\nbody\nC trailing payload\n```",
        1,
    );
}

#[test]
fn verbatim_counted_declaration_without_body_never_dispatches() {
    assert_verbatim_rejected(
        "```tool\n{\"name\":\"write_file\",\"args\":{\"content\":\"<<C:1\"}}\n```",
        1,
    );
}

#[test]
fn verbatim_mismatched_counted_body_never_dispatches() {
    assert_verbatim_rejected(
        "```tool\n{\"name\":\"write_file\",\"args\":{\"content\":\"<<C:1\"}}\n\
         <<D:1\nbody\nD\n```",
        1,
    );
}

#[test]
fn verbatim_one_of_two_missing_counted_bodies_never_dispatches() {
    assert_verbatim_rejected(
        "```tool\n{\"name\":\"edit_file\",\"args\":{\"old\":\"<<OLD:1\",\"new\":\"<<NEW:1\"}}\n\
         <<OLD:1\nbefore\nOLD\n```",
        1,
    );
}

#[test]
fn verbatim_duplicate_counted_declarations_need_distinct_bodies() {
    assert_verbatim_rejected(
        "```tool\n{\"name\":\"edit_file\",\"args\":{\"old\":\"<<BODY:1\",\"new\":\"<<BODY:1\"}}\n\
         <<BODY:1\nonly one\nBODY\n```",
        1,
    );
}

#[test]
fn verbatim_counted_crlf_close_round_trips() {
    let out = parse(
        "```tool\r\n{\"name\":\"write_file\",\"args\":{\"content\":\"<<C:1\"}}\r\n\
         <<C:1\r\nbody\r\n  C\r\n```",
    );
    assert_eq!(out.errors, Vec::<String>::new());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(arg(&out.calls[0], "content").unwrap(), "body\r\n");
}

// #5015 PRESERVATION: an argument whose value is literally `<<EOF` with NO
// trailing heredoc body stays a literal string — never misread as a declaration.
#[test]
fn literal_heredoc_opener_value_without_body_survives() {
    let out = parse("```tool\n{\"name\": \"write_file\", \"args\": {\"path\": \"a\", \"content\": \"<<EOF\"}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        "<<EOF"
    );
}

// A trailing heredoc no argument declared is a fail-loud mismatch, never dropped.
#[test]
fn orphan_trailing_heredoc_fails_loud() {
    let block = "```tool\n{ \"name\": \"write_file\", \"args\": { \"path\": \"a\", \"content\": \"x\" } }\n\
         <<ORPHAN:1\nstray\nORPHAN\n```";
    let out = parse(block);
    assert!(
        !out.errors.is_empty(),
        "expected a fail-loud mismatch, got {:?}",
        out.errors
    );
}

// A pure-JSON block with no heredoc-opener argument is byte-identical #5015/
// #5037 behavior — the verbatim lane is strictly additive.
#[test]
fn pure_json_block_is_unchanged_by_verbatim_lane() {
    let out = parse("```tool\n{\"name\": \"write_file\", \"args\": {\"path\": \"a\", \"content\": \"x\\ny\"}}\n```");
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        arg(&out.calls[0], "content").unwrap().as_str().unwrap(),
        "x\ny"
    );
}
