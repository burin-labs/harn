//! Rescue parsing for chat-template tool-call markup emitted as plain
//! assistant text (#3220): the qwen3 `<function=NAME>` + `<parameter=KEY>`
//! style and the `<invoke name="NAME">` attribute spelling. Native-format
//! models fall back to this text rendering under long contexts; the parser
//! must promote well-formed markup into real calls (so the loop's native
//! fallback contract can act on them) and surface precise errors for
//! truncated markup — and must NOT fire on prose or fenced examples.

use super::{json, parse_text_tool_calls_with_tools, sample_tool_registry, vm_dict, vm_str};
use crate::value::VmValue;

/// Single-tool registry mirroring the conformance probe's `echo_marker(value)`.
fn echo_marker_tools() -> VmValue {
    let value_param = vm_dict(&[
        ("type", vm_str("string")),
        ("description", vm_str("The marker value to echo.")),
    ]);
    let params = VmValue::dict([("value", value_param)]);
    let tool = vm_dict(&[
        ("name", vm_str("echo_marker")),
        ("description", vm_str("Echo the probe marker exactly.")),
        ("parameters", params),
    ]);
    VmValue::dict([("tools", VmValue::List(vec![tool].into()))])
}

/// The live failure shape from the 2026-06-09 host eval meter run: a
/// complete, well-formed `edit` call in qwen's chat-template XML style,
/// wrapped in `<tool_call>` tags, emitted as plain assistant text.
fn live_qwen_markup() -> String {
    [
        "<tool_call>",
        "<function=edit>",
        "<parameter=action>",
        "create",
        "</parameter>",
        "<parameter=path>",
        "internal/handlers/health.go",
        "</parameter>",
        "<parameter=content>",
        "package handlers",
        "",
        "// Health returns 200.",
        "func Health() int { return 200 }",
        "</parameter>",
        "</function>",
        "</tool_call>",
    ]
    .join("\n")
}

#[test]
fn wrapped_function_markup_parses_into_edit_call() {
    let tools = sample_tool_registry();
    let parsed = parse_text_tool_calls_with_tools(&live_qwen_markup(), Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    let call = &parsed.calls[0];
    assert_eq!(call.get("name").and_then(|v| v.as_str()), Some("edit"));
    let args = call.get("arguments").expect("arguments");
    assert_eq!(args.get("action"), Some(&json!("create")));
    assert_eq!(
        args.get("path"),
        Some(&json!("internal/handlers/health.go"))
    );
    // String-typed parameter bytes survive verbatim — including blank lines.
    assert_eq!(
        args.get("content").and_then(|v| v.as_str()),
        Some("package handlers\n\n// Health returns 200.\nfunc Health() int { return 200 }")
    );
    // The canonical replay form is the tagged grammar, not the markup.
    assert!(parsed.canonical.contains("<tool_call>"));
    assert!(parsed.canonical.contains("edit({"));
}

#[test]
fn unwrapped_function_markup_recovers_with_violation() {
    let tools = sample_tool_registry();
    let text = "<function=run>\n<parameter=command>\ngo test ./...\n</parameter>\n</function>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0].get("name").and_then(|v| v.as_str()),
        Some("run")
    );
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("command")),
        Some(&json!("go test ./..."))
    );
    assert!(
        parsed
            .violations
            .iter()
            .any(|violation| violation.contains("chat-template function markup")),
        "expected a soft protocol violation steering back to the canonical form: {:?}",
        parsed.violations
    );
}

#[test]
fn invoke_attribute_markup_parses() {
    let tools = sample_tool_registry();
    let text =
        "<invoke name=\"run\">\n<parameter name=\"command\">cargo check</parameter>\n</invoke>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0].get("name").and_then(|v| v.as_str()),
        Some("run")
    );
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("command")),
        Some(&json!("cargo check"))
    );
}

#[test]
fn missing_function_close_with_closed_params_recovers() {
    // Some emissions close only the outer `</tool_call>`. Every parameter is
    // closed, so the values are complete and the call is recoverable.
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<function=run>\n<parameter=command>\nls\n</parameter>\n</tool_call>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("command")),
        Some(&json!("ls"))
    );
}

#[test]
fn unclosed_wrapper_function_markup_recovers() {
    // `<tool_call>` opened, function markup complete, `</tool_call>` never
    // emitted (truncated tail after the close tag is tolerated slop).
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<function=run>\n<parameter=command>\npwd\n</parameter>\n</function>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("command")),
        Some(&json!("pwd"))
    );
}

#[test]
fn truncated_parameter_errors_and_does_not_dispatch() {
    // A `<parameter=content>` whose close tag never arrives is a truncation:
    // dispatching a partial file body would corrupt the workspace, so the
    // turn must surface an error (which drives parse feedback) and 0 calls.
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<function=edit>\n<parameter=action>\ncreate\n</parameter>\n<parameter=content>\npackage handlers\n";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.calls.len(), 0);
    assert_eq!(parsed.errors.len(), 1);
    assert!(
        parsed.errors[0].contains("TRUNCATED"),
        "{:?}",
        parsed.errors
    );
    assert!(
        parsed.errors[0].contains("NOT executed"),
        "{:?}",
        parsed.errors
    );
}

#[test]
fn unknown_tool_in_markup_errors() {
    let tools = sample_tool_registry();
    let text = "<function=frobnicate>\n<parameter=level>\n9\n</parameter>\n</function>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.calls.len(), 0);
    assert_eq!(parsed.errors.len(), 1);
    assert!(
        parsed.errors[0].contains("Unknown tool 'frobnicate'"),
        "{:?}",
        parsed.errors
    );
}

#[test]
fn prose_mentioning_markup_does_not_fire() {
    // Inline prose mention — the opener is not at the start of a line, so it
    // is stray narration, never a call or a parse error.
    let tools = sample_tool_registry();
    let text = "I considered emitting <function=edit> markup but used the native channel.";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.calls.len(), 0);
    assert_eq!(parsed.errors, Vec::<String>::new());
}

#[test]
fn fenced_example_does_not_fire() {
    // A fenced code block *discussing* the markup syntax is documentation,
    // not an attempted call.
    let tools = sample_tool_registry();
    let text = [
        "<assistant_prose>",
        "The legacy markup looks like this:",
        "</assistant_prose>",
        "```text",
        "<function=edit>",
        "<parameter=action>",
        "create",
        "</parameter>",
        "</function>",
        "```",
    ]
    .join("\n");
    let parsed = parse_text_tool_calls_with_tools(&text, Some(&tools));
    assert_eq!(parsed.calls.len(), 0);
    assert_eq!(parsed.errors, Vec::<String>::new());
}

#[test]
fn string_schema_parameter_keeps_numeric_looking_value_verbatim() {
    // `content` is string-typed in the schema: a value that happens to parse
    // as JSON must NOT be coerced.
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<function=edit>\n<parameter=action>\ncreate\n</parameter>\n<parameter=path>\nversion.txt\n</parameter>\n<parameter=content>\n123\n</parameter>\n</function>\n</tool_call>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("content")),
        Some(&json!("123"))
    );
}

#[test]
fn non_string_schema_parameter_parses_as_json() {
    // `ops` is list-typed in the schema, so its raw markup value is JSON.
    let tools = sample_tool_registry();
    let text = "<tool_call>\n<function=edit>\n<parameter=action>\npatch\n</parameter>\n<parameter=path>\nmain.go\n</parameter>\n<parameter=ops>\n[{\"op\": \"a\"}]\n</parameter>\n</function>\n</tool_call>";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.errors, Vec::<String>::new());
    assert_eq!(parsed.calls.len(), 1);
    assert_eq!(
        parsed.calls[0]
            .get("arguments")
            .and_then(|args| args.get("ops")),
        Some(&json!([{"op": "a"}]))
    );
}

/// Assert a recovered `echo_marker` call round-trips the given JSON args.
fn assert_echo_marker_args(
    parsed: &crate::llm::tools::TextToolParseResult,
    expected: serde_json::Value,
) {
    assert_eq!(parsed.errors, Vec::<String>::new(), "{:?}", parsed.errors);
    assert_eq!(
        parsed.calls.len(),
        1,
        "expected one call: {:?}",
        parsed.calls
    );
    assert_eq!(
        parsed.calls[0].get("name").and_then(|v| v.as_str()),
        Some("echo_marker")
    );
    assert_eq!(
        parsed.calls[0].get("arguments"),
        Some(&expected),
        "arguments must round-trip the trailing JSON object"
    );
}

#[test]
fn function_markup_trailing_json_object_round_trips_args() {
    // Exact live dialect from #5252 / the tool-scorecard classifier miss:
    // `<function=NAME>{json}` with no `<parameter=...>` blocks and no close tag.
    let tools = echo_marker_tools();
    let text = r#"<function=echo_marker>{"value": "MK-7Q3Z"}"#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({"value": "MK-7Q3Z"}));
}

#[test]
fn function_markup_enclosed_json_object_round_trips_args() {
    // Same dialect with an explicit `</function>` close.
    let tools = echo_marker_tools();
    let text = r#"<function=echo_marker>{"value": "MK-7Q3Z"}</function>"#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({"value": "MK-7Q3Z"}));
}

#[test]
fn function_markup_wrapped_trailing_json_object_round_trips_args() {
    // Inside a `<tool_call>` wrapper (parameter-style markup's usual home).
    let tools = echo_marker_tools();
    let text = r#"<tool_call><function=echo_marker>{"value": "MK-7Q3Z"}</function></tool_call>"#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({"value": "MK-7Q3Z"}));
}

#[test]
fn function_markup_trailing_json_empty_object_is_no_arg_call() {
    let tools = echo_marker_tools();
    let text = r"<function=echo_marker>{}";
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({}));
}

#[test]
fn function_markup_trailing_json_nested_object_round_trips() {
    let tools = echo_marker_tools();
    let text = r#"<function=echo_marker>{"value": "x", "meta": {"n": 1}}"#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({"value": "x", "meta": {"n": 1}}));
}

#[test]
fn invoke_markup_trailing_json_object_round_trips_args() {
    // Attribute spelling of the same trailing-JSON dialect.
    let tools = echo_marker_tools();
    let text = r#"<invoke name="echo_marker">{"value": "MK-7Q3Z"}</invoke>"#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_echo_marker_args(&parsed, json!({"value": "MK-7Q3Z"}));
}

#[test]
fn function_markup_truncated_trailing_json_errors_and_does_not_dispatch() {
    // An opened `{` that never closes must not dispatch empty/partial args.
    let tools = echo_marker_tools();
    let text = r#"<function=echo_marker>{"value": "MK-7Q3Z""#;
    let parsed = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(parsed.calls.len(), 0);
    assert_eq!(parsed.errors.len(), 1);
    assert!(
        parsed.errors[0].contains("TRUNCATED"),
        "{:?}",
        parsed.errors
    );
}
