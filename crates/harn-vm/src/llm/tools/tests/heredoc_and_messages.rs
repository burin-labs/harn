use super::{
    build_assistant_response_message, build_assistant_tool_message, json, known_tools_set,
    normalize_tool_args, parse_bare_calls_in_body, parse_native_json_tool_calls,
    parse_text_tool_calls_with_tools, sample_tool_registry,
};

#[test]
fn heredoc_simple() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "main.go",
    content: <<EOF
package main

import "fmt"

func main() {
    fmt.Println("hello")
}
EOF
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "should parse one call, errors: {:?}",
        result.errors
    );
    let args = &result.calls[0]["arguments"];
    let content = args["content"].as_str().unwrap();
    assert!(
        content.starts_with("package main"),
        "content should start with package: {content}"
    );
    assert!(
        content.contains("fmt.Println"),
        "content should contain fmt.Println"
    );
}

#[test]
fn heredoc_with_backticks_inside() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "parser_test.go",
    content: <<CONTENT
package manifest

import "testing"

func TestYAML(t *testing.T) {
    yaml := `
version: "1.0"
services:
  web:
    image: nginx
`
    if yaml == "" {
        t.Fatal("empty")
    }
}
CONTENT
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "should parse heredoc with backticks, errors: {:?}",
        result.errors
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("yaml := `"),
        "should preserve Go raw string backticks: {content}"
    );
    assert!(
        content.contains("image: nginx"),
        "should preserve YAML content"
    );
}

#[test]
fn heredoc_with_quotes_and_backslashes() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "test.py",
    content: <<END
def test_escaping():
    s = "hello \"world\""
    path = "C:\\Users\\test"
    raw = r"no\escaping\here"
    assert len(s) > 0
END
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains(r#""hello \"world\"""#),
        "should preserve escaped quotes raw"
    );
    assert!(
        content.contains(r"C:\\Users\\test"),
        "should preserve backslashes raw"
    );
}

#[test]
fn heredoc_mixed_with_regular_args() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "patch",
    path: "main.go",
    old_string: <<OLD
func broken() {
    return nil
}
OLD,
    new_string: <<NEW
func fixed() {
    return &Result{}
}
NEW
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors: {:?}", result.errors);
    let args = &result.calls[0]["arguments"];
    assert!(
        args["old_string"].as_str().unwrap().contains("broken"),
        "old_string should contain broken"
    );
    assert!(
        args["new_string"].as_str().unwrap().contains("fixed"),
        "new_string should contain fixed"
    );
}

#[test]
fn heredoc_close_with_brace_and_comma_on_same_line() {
    // Cheap models (e.g. Together Gemma 3n) frequently collapse the closing
    // dict/array tail onto the heredoc's closing line: `EOF },`. The parser
    // must accept that — anything after the tag on the close line is handed
    // back to the outer parser verbatim.
    let tools = sample_tool_registry();
    let text = r#"edit({ path: "internal/manifest/parser_extra_test.go", ops: [
  { op: "replace_body", function_name: "TestInvalidYaml", new_body: <<EOF
func TestInvalidYaml(t *testing.T) {
	assertParseError(t, "invalid yaml")
}
EOF },
  { op: "replace_body", function_name: "TestMissingRequiredFields", new_body: <<EOF
func TestMissingRequiredFields(t *testing.T) {
	assertParseError(t, "version: 1")
}
EOF }
] })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "same-line close tail should parse cleanly, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    let ops = result.calls[0]["arguments"]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0]["op"], json!("replace_body"));
    assert_eq!(ops[0]["function_name"], json!("TestInvalidYaml"));
    assert!(
        ops[0]["new_body"]
            .as_str()
            .unwrap()
            .contains("assertParseError(t, \"invalid yaml\")"),
        "first body should preserve the invalid yaml assertion"
    );
    assert_eq!(ops[1]["function_name"], json!("TestMissingRequiredFields"));
}

#[test]
fn heredoc_close_with_multiple_closers_on_same_line() {
    // Tightly-collapsed tool calls sometimes end with `EOF } ] })` all on
    // one line. The word-boundary closing rule should absorb any punctuation
    // after the tag and hand control back to the outer parser.
    let tools = sample_tool_registry();
    let text = r#"edit({ path: "a.go", ops: [ { op: "replace_body", function_name: "F", new_body: <<EOF
body
EOF } ] })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "close tail with multiple closers on same line should parse cleanly, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    let ops = result.calls[0]["arguments"]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["new_body"], json!("body"));
}

#[test]
fn heredoc_word_boundary_rejects_tag_prefix_of_identifier() {
    // The close line must hit a word boundary after the tag. `EOFunction`
    // should NOT terminate the heredoc — otherwise any identifier that
    // happens to begin with the tag would corrupt content parsing.
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "a.rs",
    content: <<EOF
let EOFunction = 1;
let x = 2;
EOF
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "tag-prefixed identifier inside content should not terminate the heredoc, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls.len(), 1);
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("let EOFunction = 1;"),
        "content should still include the EOFunction line: {content}"
    );
    assert!(
        content.contains("let x = 2;"),
        "content should include the line after the identifier"
    );
}

#[test]
fn heredoc_unterminated_is_error() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "main.go",
    content: <<EOF
package main
// no closing EOF tag
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.calls.is_empty(),
        "unterminated heredoc should produce no calls"
    );
    assert!(!result.errors.is_empty(), "should have parse error");
}

#[test]
fn heredoc_missing_tag_is_error() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "main.go",
    content: <<
package main
EOF
})"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.calls.is_empty() || !result.errors.is_empty(),
        "missing tag should error"
    );
}

#[test]
fn template_literal_still_works() {
    let tools = sample_tool_registry();
    let text = "edit({\n    action: \"create\",\n    path: \"simple.txt\",\n    content: `hello world`\n})";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "template literal should still parse, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls[0]["arguments"]["content"], "hello world");
}

#[test]
fn double_quoted_string_still_works() {
    let tools = sample_tool_registry();
    let text = "run({ command: \"go test ./internal/manifest/\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "double-quoted string should parse, errors: {:?}",
        result.errors
    );
    assert_eq!(
        result.calls[0]["arguments"]["command"],
        "go test ./internal/manifest/"
    );
}

#[test]
fn multiple_calls_with_heredoc() {
    let tools = sample_tool_registry();
    let text = r#"I'll create the file and then run the tests.

edit({
    action: "create",
    path: "test.go",
    content: <<EOF
package main
EOF
})

run({ command: "go test ./..." })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        2,
        "should parse both calls, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls[0]["name"], "edit");
    assert_eq!(result.calls[1]["name"], "run");
}

#[test]
fn heredoc_go_code_with_backticks_then_run() {
    let tools = sample_tool_registry();
    let text = r#"I'll create the test file with table-driven tests.

edit({
    action: "create",
    path: "internal/manifest/parser_test.go",
    content: <<GOFILE
package manifest

import (
	"testing"
)

func TestParseManifest(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    string
		wantErr bool
	}{
		{
			name:  "basic",
			input: `{"name": "test"}`,
			want:  "test",
		},
		{
			name:    "empty",
			input:   ``,
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := Parse(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("Parse() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if got != tt.want {
				t.Errorf("Parse() = %v, want %v", got, tt.want)
			}
		})
	}
}
GOFILE
})

Now let me run the tests.

run({ command: "go test ./internal/manifest/ -v" })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        2,
        "should parse edit+run with Go backtick code, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls[0]["name"], "edit");
    assert_eq!(result.calls[1]["name"], "run");
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("func TestParseManifest"),
        "content should have the test function"
    );
    assert!(
        content.contains("`{\"name\": \"test\"}`"),
        "content should preserve Go raw string literals with backticks"
    );
    assert_eq!(
        result.calls[1]["arguments"]["command"],
        "go test ./internal/manifest/ -v"
    );
}

#[test]
fn heredoc_three_edits_then_run() {
    let tools = sample_tool_registry();
    let text = r#"I'll create all three files.

edit({
    action: "create",
    path: "a.go",
    content: <<EOF
package a
EOF
})

edit({
    action: "create",
    path: "b.go",
    content: <<EOF
package b
EOF
})

edit({
    action: "create",
    path: "c.go",
    content: <<EOF
package c
EOF
})

run({ command: "go build ./..." })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        4,
        "should parse 3 edits + 1 run, errors: {:?}",
        result.errors
    );
    assert_eq!(result.calls[0]["arguments"]["path"], "a.go");
    assert_eq!(result.calls[1]["arguments"]["path"], "b.go");
    assert_eq!(result.calls[2]["arguments"]["path"], "c.go");
    assert_eq!(result.calls[3]["name"], "run");
}

#[test]
fn heredoc_prose_extraction() {
    let tools = sample_tool_registry();
    let text = r#"Here's my plan.

edit({
    action: "create",
    path: "main.go",
    content: <<EOF
package main
EOF
})

That should compile. Let me verify.

run({ command: "go build" })"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 2);
    assert!(
        result.prose.contains("Here's my plan."),
        "prose should contain intro"
    );
    assert!(
        result.prose.contains("That should compile."),
        "prose should contain interstitial text"
    );
    assert!(
        !result.prose.contains("<<EOF"),
        "prose should not contain tool calls"
    );
}

#[test]
fn native_json_fallback_parses_openai_array_format() {
    let known = known_tools_set();
    let text = r#"I'll create the test file now.

[{"id":"call_001","type":"function","function":{"name":"edit","arguments":"{\"action\":\"create\",\"path\":\"test.go\",\"content\":\"package main\"}"}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty());
    assert_eq!(calls.len(), 1, "should parse one call from array");
    assert_eq!(calls[0]["name"], json!("edit"));
    assert_eq!(calls[0]["arguments"]["action"], json!("create"));
    assert_eq!(calls[0]["arguments"]["path"], json!("test.go"));
    assert_eq!(calls[0]["arguments"]["content"], json!("package main"));
}

#[test]
fn native_json_fallback_parses_pretty_printed_non_call_id() {
    // Regression: detection used three brittle substring needles
    // (`[{"id":` / a byte-identical dead duplicate / `{"id":"call_`), so a
    // pretty-printed native array with a space after `[{` AND a non-OpenAI id
    // (no `call_` prefix) matched NONE of them — the entire tool call was
    // silently dropped with zero parse feedback, reading to the loop as a
    // stall. Local vLLM/llama.cpp tool templates and pretty-printers emit
    // exactly this shape. Detection is now whitespace- and id-agnostic.
    let known = known_tools_set();
    let text =
        r#"[{ "id": "0", "function": { "name": "read", "arguments": "{\"path\":\"main.go\"}" } }]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty(), "no errors expected: {errors:?}");
    assert_eq!(calls.len(), 1, "should parse the pretty-printed call");
    assert_eq!(calls[0]["name"], json!("read"));
    assert_eq!(calls[0]["arguments"]["path"], json!("main.go"));
    assert_eq!(
        calls[0]["id"],
        json!("0"),
        "non-call_ id should be preserved"
    );
}

#[test]
fn normalize_tool_args_coerces_integer_like_string_fields() {
    let normalized = normalize_tool_args(
        "edit",
        &json!({
            "action": "replace_range",
            "path": "tests/unit/test_example.py",
            "range_start": "1",
            "range_end": "19",
            "ops": [
                {"op": "replace_range", "range_start": "3", "range_end": "5"}
            ]
        }),
    );
    assert_eq!(normalized["range_start"], json!(1));
    assert_eq!(normalized["range_end"], json!(19));
    assert_eq!(normalized["ops"][0]["range_start"], json!(3));
    assert_eq!(normalized["ops"][0]["range_end"], json!(5));
}

#[test]
fn native_json_fallback_parses_multiple_calls() {
    let known = known_tools_set();
    let text = r#"[{"id":"call_001","type":"function","function":{"name":"edit","arguments":"{\"action\":\"create\",\"path\":\"a.go\",\"content\":\"pkg a\"}"}},{"id":"call_002","type":"function","function":{"name":"run","arguments":"{\"command\":\"go test\"}"}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty());
    assert_eq!(calls.len(), 2, "should parse both calls");
    assert_eq!(calls[0]["name"], json!("edit"));
    assert_eq!(calls[1]["name"], json!("run"));
    assert_eq!(calls[1]["arguments"]["command"], json!("go test"));
}

#[test]
fn native_json_fallback_reports_unknown_tools() {
    let known = known_tools_set();
    let text = r#"[{"id":"call_001","type":"function","function":{"name":"unknown_tool","arguments":"{}"}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert_eq!(calls.len(), 0, "should not parse unknown tools");
    assert_eq!(errors.len(), 1, "should report one error");
    assert!(
        errors[0].contains("Unknown tool 'unknown_tool'"),
        "error should name the unknown tool: {}",
        errors[0]
    );
    assert!(
        errors[0].contains("Available tools:"),
        "error should list available tools: {}",
        errors[0]
    );
}

#[test]
fn native_json_fallback_reports_malformed_arguments() {
    let known = known_tools_set();
    let text = r#"[{"id":"call_001","type":"function","function":{"name":"edit","arguments":"not valid json {"}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert_eq!(calls.len(), 0, "should not produce a call with bad args");
    assert_eq!(errors.len(), 1, "should report one parse error");
    assert!(
        errors[0].contains("Could not parse arguments"),
        "error should describe the parse failure: {}",
        errors[0]
    );
}

#[test]
fn native_json_fallback_returns_empty_for_no_json() {
    let known = known_tools_set();
    let text = "Just some prose without any tool calls.";
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(calls.is_empty(), "should return empty for plain text");
    assert!(errors.is_empty());
}

#[test]
fn native_json_fallback_handles_object_arguments() {
    let known = known_tools_set();
    // Some models emit arguments as an object instead of a JSON string
    let text = r#"[{"id":"call_001","type":"function","function":{"name":"read","arguments":{"path":"main.go"}}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty());
    assert_eq!(calls.len(), 1, "should parse call with object arguments");
    assert_eq!(calls[0]["arguments"]["path"], json!("main.go"));
}

#[test]
fn native_json_fallback_handles_prose_before_json() {
    let known = known_tools_set();
    let text = r#"Let me read the file first to understand the structure.

Now I'll create the test:

[{"id":"call_0v95900000000000000002","function":{"name":"edit","arguments":"{\"action\":\"replace_body\",\"path\":\"test.go\",\"function_name\":\"TestMain\",\"new_body\":\"t.Fatal(\\\"fail\\\")\"}"}}]"#;
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty());
    assert_eq!(calls.len(), 1, "should find call after prose");
    assert_eq!(calls[0]["name"], json!("edit"));
    assert_eq!(calls[0]["arguments"]["action"], json!("replace_body"));
    assert_eq!(calls[0]["arguments"]["function_name"], json!("TestMain"));
}

#[test]
fn native_json_fallback_handles_non_ascii_trailing_content() {
    // Regression: the salvage path used to byte-scan backwards
    // (`&text[start..=end]`) with no `is_char_boundary` guard, so any
    // multi-byte char (emoji/accent/CJK) in the trailing prose made `end + 1`
    // land mid-codepoint and PANIC — aborting the agent turn before it ever
    // reached the valid `]`. The forward streaming parser stops at the array's
    // structural end and ignores trailing bytes regardless of their width.
    let known = known_tools_set();
    let text = "[{\"id\":\"call_001\",\"type\":\"function\",\"function\":{\"name\":\"edit\",\
                \"arguments\":\"{\\\"action\\\":\\\"create\\\",\\\"path\\\":\\\"café.go\\\",\
                \\\"content\\\":\\\"pkg\\\"}\"}}] done ✅ — déjà vu 完了";
    let (calls, errors) = parse_native_json_tool_calls(text, &known);
    assert!(errors.is_empty(), "no errors expected: {errors:?}");
    assert_eq!(
        calls.len(),
        1,
        "should parse the call before the trailing prose"
    );
    assert_eq!(calls[0]["name"], json!("edit"));
    assert_eq!(calls[0]["arguments"]["path"], json!("café.go"));
}

#[test]
fn text_parser_falls_through_to_native_json_fallback() {
    // End-to-end: the main parse_text_tool_calls_with_tools should fall
    // through to the native JSON parser when text parsing finds nothing
    let tools = sample_tool_registry();
    let text = r#"I'll create the file.

[{"id":"call_001","type":"function","function":{"name":"edit","arguments":"{\"action\":\"create\",\"path\":\"main.go\",\"content\":\"package main\\nfunc main() {}\"}"}}]"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert!(
        result.errors.is_empty(),
        "should not produce errors: {:?}",
        result.errors
    );
    assert_eq!(
        result.calls.len(),
        1,
        "should parse native JSON as fallback"
    );
    assert_eq!(result.calls[0]["name"], json!("edit"));
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
}

#[test]
fn text_parser_prefers_text_format_over_native_json() {
    // If both text-format and native JSON are present, text format wins
    let tools = sample_tool_registry();
    let text = r#"edit({ action: "create", path: "a.go", content: "pkg a" })

[{"id":"call_001","type":"function","function":{"name":"run","arguments":"{\"command\":\"go test\"}"}}]"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    // Text parser should find the edit call and NOT fall through to native
    assert_eq!(result.calls.len(), 1, "text format should take priority");
    assert_eq!(result.calls[0]["name"], json!("edit"));
}

#[test]
fn assistant_tool_message_includes_empty_content_for_openai_style() {
    let message = build_assistant_tool_message(
        "",
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
        })],
        "together",
        "moonshotai/Kimi-K2.5",
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
}

#[test]
fn assistant_tool_message_stringifies_ollama_arguments() {
    let message = build_assistant_tool_message(
        "",
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
        })],
        "ollama",
        "devstral-small-2:24b",
    );

    assert_eq!(message["role"], "assistant");
    assert!(message.get("content").is_none());
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
    assert_eq!(message["tool_calls"][0]["type"], "function");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "read");
    let arguments = message["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("ollama tool arguments should be a JSON string");
    let parsed_arguments: serde_json::Value =
        serde_json::from_str(arguments).expect("ollama tool arguments should parse as JSON");
    assert_eq!(parsed_arguments["path"], "main.rs");
}

#[test]
fn assistant_tool_message_uses_gemini_parts_for_gemini_models() {
    let message = build_assistant_tool_message(
        "checking",
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
            "thought_signature": "opaque-signature",
        })],
        "gemini",
        "gemini-2.5-flash",
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"][0], json!({"text": "checking"}));
    assert_eq!(
        message["content"][1]["functionCall"],
        json!({"id": "call_001", "name": "read", "args": {"path": "main.rs"}})
    );
    assert_eq!(
        message["content"][1]["thoughtSignature"],
        "opaque-signature"
    );
}

#[test]
fn assistant_response_message_preserves_gemini_block_signatures() {
    let message = build_assistant_response_message(
        "checking",
        &[
            json!({
                "type": "output_text",
                "text": "checking",
                "provider_metadata": {
                    "gemini": {"thought_signature": "text-signature"}
                }
            }),
            json!({
                "type": "tool_call",
                "id": "call_001",
                "name": "read",
                "arguments": {"path": "main.rs"},
                "thought_signature": "tool-signature",
            }),
        ],
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
            "thought_signature": "tool-signature",
        })],
        None,
        "gemini",
        "gemini-2.5-flash",
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"][0]["text"], "checking");
    assert_eq!(message["content"][0]["thoughtSignature"], "text-signature");
    assert_eq!(
        message["content"][1]["functionCall"],
        json!({"id": "call_001", "name": "read", "args": {"path": "main.rs"}})
    );
    assert_eq!(message["content"][1]["thoughtSignature"], "tool-signature");
}

#[test]
fn assistant_tool_message_uses_model_capability_shape_for_bedrock_claude() {
    let message = build_assistant_tool_message(
        "using a tool",
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
        })],
        "bedrock",
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"][0]["type"], "text");
    assert_eq!(message["content"][1]["type"], "tool_use");
    assert_eq!(message["content"][1]["name"], "read");
}

#[test]
fn assistant_response_message_preserves_reasoning() {
    let message = build_assistant_response_message(
        "",
        &[],
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
        })],
        Some("inspect the file before editing"),
        "together",
        "moonshotai/Kimi-K2.5",
    );

    assert_eq!(message["reasoning"], "inspect the file before editing");
    assert_eq!(message["content"], "");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
}

#[test]
fn read_file_offset_and_limit() {
    use super::super::handle_tool_locally;
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_offset.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=20 {
            writeln!(f, "line {i}").unwrap();
        }
    }
    let path_str = path.to_str().unwrap();

    // Full read — should get all 20 lines.
    let result = handle_tool_locally("read_file", &json!({"path": path_str})).unwrap();
    assert!(result.contains("1\tline 1"), "first line numbered");
    assert!(result.contains("20\tline 20"), "last line numbered");
    assert!(!result.contains("more lines not shown"), "no truncation");

    // Offset 5, limit 3 — lines 5, 6, 7.
    let result = handle_tool_locally(
        "read_file",
        &json!({"path": path_str, "offset": 5, "limit": 3}),
    )
    .unwrap();
    assert!(result.contains("5\tline 5"), "starts at line 5");
    assert!(result.contains("7\tline 7"), "ends at line 7");
    assert!(!result.contains("4\tline 4"), "no line 4");
    assert!(!result.contains("8\tline 8"), "no line 8");
    assert!(result.contains("more lines not shown"), "truncation hint");
    assert!(result.contains("offset=8"), "hint says offset=8");

    // Offset past end — empty result, no panic.
    let result =
        handle_tool_locally("read_file", &json!({"path": path_str, "offset": 100})).unwrap();
    assert!(!result.contains("line"), "no content past end");
}

#[test]
fn tagged_tool_call_keeps_literal_close_tag_inside_heredoc() {
    // The response protocol tells models to write multiline string fields as
    // `<<TAG ... TAG` heredocs with "raw content, no escaping" — so a heredoc
    // body can legitimately contain the literal `</tool_call>`. The close-tag
    // scan must step over the heredoc body and honor only the real terminator,
    // otherwise the whole call is shredded into stray text.
    let tools = sample_tool_registry();
    let text = "<tool_call>edit({\n    action: \"create\",\n    path: \"doc.md\",\n    content: <<EOF\nWrap each call like </tool_call> here.\nMore body lines.\nEOF\n})</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "expected exactly one edit call, errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("</tool_call>"),
        "heredoc body should retain the literal close tag: {content:?}"
    );
    assert!(
        content.contains("More body lines."),
        "heredoc body should not be truncated mid-stream: {content:?}"
    );
}

#[test]
fn over_closed_object_string_recovers_embedded_raw_string() {
    // The create-heavy local-model failure: a quoted `content` body holds a
    // Rust raw string `r#"..."#` whose inner quotes the model left unescaped.
    // The strict scan closes the string at the first bare `"`, leaving the
    // object continuation on content (`s`/`#`) instead of `,`/`}` — historically
    // dropping the whole create and stranding the model in a re-emit loop. The
    // greedy object-string recovery must absorb the embedded quotes and keep the
    // call dispatchable.
    let tools = sample_tool_registry();
    let text = "<tool_call>edit({ action: \"create\", path: \"t.rs\", content: \"let q = parse_query(r#\"sel\"#);\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "embedded-quote create should recover to one call, errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert_eq!(
        content, "let q = parse_query(r#\"sel\"#);",
        "recovered content must keep the embedded raw-string quotes verbatim"
    );
    assert_eq!(
        result.calls[0]["arguments"]["action"].as_str().unwrap(),
        "create"
    );
}

#[test]
fn over_closed_recovery_stops_at_true_boundary_preserving_later_keys() {
    // Over-capture guard: when the embedded-quote value is NOT the last key, the
    // greedy scan must stop at the first close whose continuation validates (a
    // `,`), leaving subsequent keys intact rather than swallowing them. The
    // embedded quotes are balanced (a full `r#"a"#`) so the upstream call-
    // boundary scanner stays in sync and the value parser sees the whole args
    // object — isolating MY recovery's boundary behavior.
    let tools = sample_tool_registry();
    let text =
        "<tool_call>edit({ content: \"x=r#\"a\"#y\", action: \"create\", path: \"t.rs\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "recovery should preserve later keys, errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    assert_eq!(
        result.calls[0]["arguments"]["content"].as_str().unwrap(),
        "x=r#\"a\"#y"
    );
    assert_eq!(
        result.calls[0]["arguments"]["action"].as_str().unwrap(),
        "create",
        "the key after the recovered string must survive"
    );
    assert_eq!(
        result.calls[0]["arguments"]["path"].as_str().unwrap(),
        "t.rs"
    );
}

#[test]
fn well_formed_escaped_string_is_untouched_by_recovery() {
    // The recovery engages only after the strict continuation already failed, so
    // a cleanly-escaped multi-key value must parse identically (no greedy
    // re-interpretation, no merged keys).
    let tools = sample_tool_registry();
    let text = "<tool_call>edit({ action: \"create\", path: \"t.rs\", content: \"line1\\nline2\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "errors={:?}", result.errors);
    assert_eq!(
        result.calls[0]["arguments"]["content"].as_str().unwrap(),
        "line1\nline2"
    );
}

#[test]
fn tagged_tool_calls_get_turn_unique_ids() {
    // Two `<tool_call>` blocks in one turn must not collide on `tc_0`; each
    // per-body parser only sees its local (single-call) vector, so the
    // turn-global renumber is what keeps result correlation unambiguous.
    let tools = sample_tool_registry();
    let text = "<tool_call>run({ command: \"echo a\" })</tool_call>\n\
                <tool_call>run({ command: \"echo b\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        2,
        "expected two run calls, errors={:?}",
        result.errors
    );
    let id0 = result.calls[0]["id"].as_str().unwrap();
    let id1 = result.calls[1]["id"].as_str().unwrap();
    assert_eq!(id0, "tc_0");
    assert_eq!(id1, "tc_1");
    assert_ne!(id0, id1, "tool-call ids must be unique within a turn");
}

#[test]
fn tagged_tool_call_with_bash_heredoc_inside_a_string_arg() {
    // A complete, well-formed call whose *string* argument contains a bash
    // `<<EOF ... EOF` heredoc. That `<<EOF` is string content, not a Harn
    // heredoc, so the close-tag scan must not treat it as one (else the call is
    // dropped and falsely reported as truncated).
    let tools = sample_tool_registry();
    let text = "<tool_call>run({ command: \"cat <<EOF\nsome heredoc body\nEOF\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "expected one run call, errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    let command = result.calls[0]["arguments"]["command"].as_str().unwrap();
    assert!(
        command.contains("<<EOF") && command.contains("some heredoc body"),
        "the string argument should retain its bash heredoc verbatim: {command:?}"
    );
}

#[test]
fn tagged_tool_call_with_close_tag_inside_a_string_arg() {
    // A `</tool_call>` literal inside a quoted string argument is content, not
    // the structural close — the call parses and the string keeps the literal.
    let tools = sample_tool_registry();
    let text = "<tool_call>run({ command: \"echo </tool_call> done\" })</tool_call>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "expected one run call, errors={:?} violations={:?}",
        result.errors,
        result.violations
    );
    let command = result.calls[0]["arguments"]["command"].as_str().unwrap();
    assert_eq!(command, "echo </tool_call> done");
}

// --- JSON-escaped heredoc recovery (Change B) -------------------------------
// Cheap models (e.g. qwen3.6) emit the heredoc body as a JSON-escaped one-liner
// where the line breaks are the two literal bytes `\` + `n`, not real newlines.
// The parser must recover those calls (dispatch with a real body) while leaving
// genuine real-newline heredocs byte-for-byte unchanged.

#[test]
fn heredoc_recovers_json_escaped_literal_newline_body() {
    let tools = sample_tool_registry();
    // `\\n`, `\\t`, `\\"` here are the literal backslash-escape *bytes* — this
    // mirrors the on-the-wire degraded form, e.g.
    //   content: <<EOF\npackage manifest\n\nimport (\n\t"strings"\n)\n...EOF
    let text = "edit({ path: \"parser_test.go\", content: <<EOF\\npackage manifest\\n\\nimport (\\n\\t\\\"strings\\\"\\n\\t\\\"testing\\\"\\n)\\n\\nfunc TestX(t *testing.T) {\\n\\tif 1 != 1 {\\n\\t\\tt.Fatal(\\\"bad\\\")\\n\\t}\\n}\\nEOF\\n })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "escaped heredoc must yield exactly one call, errors: {:?}",
        result.errors
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    // No literal backslash-n / backslash-t survives — they were unescaped.
    assert!(
        !content.contains("\\n") && !content.contains("\\t"),
        "literal escapes must be decoded, got: {content:?}"
    );
    // Real newlines and tabs are present.
    assert!(content.contains('\n'), "must contain real newlines");
    assert!(content.contains('\t'), "must contain real tabs");
    // Escaped quotes were decoded to bare quotes (valid Go source).
    assert!(content.contains("\"strings\""), "must contain \"strings\"");
    assert!(
        content.starts_with("package manifest"),
        "content should start with the package clause: {content:?}"
    );
    // The closing `EOF` tag is not part of the body.
    assert!(
        !content.contains("EOF"),
        "closing tag must not leak into the body: {content:?}"
    );
}

#[test]
fn heredoc_real_newline_body_is_not_unescaped() {
    // GUARD: a heredoc whose body uses REAL newlines must parse exactly as
    // before — a literal `\n` typed inside such a body (e.g. a Go format string
    // `"%d\n"`) must be preserved verbatim, never collapsed to a newline.
    let tools = sample_tool_registry();
    let text = "edit({ path: \"main.go\", content: <<EOF\npackage main\nimport \"fmt\"\nfunc main() { fmt.Printf(\"%d\\n\", 1) }\nEOF\n})";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "real-newline heredoc must parse, errors: {:?}",
        result.errors
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    // The literal `\n` inside the format string survives untouched.
    assert!(
        content.contains("%d\\n"),
        "literal \\n in a real-newline body must be preserved verbatim: {content:?}"
    );
    assert!(content.starts_with("package main"));
}

#[test]
fn heredoc_escaped_body_with_escaped_backslash_n_is_not_a_line_break() {
    // An escaped-backslash sequence `\\n` on the wire is a single decoded `\`
    // followed by `n` (e.g. a Go format string `"%d\n"`), NOT the escaped line
    // separator. The closing-tag scan must not split there, and the decoded body
    // must keep the literal `\n`.
    let tools = sample_tool_registry();
    // Wire bytes: <<EOF \n package main \n s := "x\n" \n EOF
    // where the inner `"x\\n"` is an escaped-backslash + n (decodes to `x\n`).
    let text =
        "edit({ path: \"m.go\", content: <<EOF\\npackage main\\nvar s = \\\"x\\\\n\\\"\\nEOF\\n})";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        1,
        "escaped-backslash body must still parse to one call, errors: {:?}",
        result.errors
    );
    let content = result.calls[0]["arguments"]["content"].as_str().unwrap();
    assert!(
        content.contains("\"x\\n\""),
        "decoded body must keep the literal backslash-n from `\\\\n`: {content:?}"
    );
    assert!(
        content.starts_with("package main"),
        "body must not be truncated at the false separator: {content:?}"
    );
    assert!(
        !content.contains("EOF"),
        "close tag must not leak: {content:?}"
    );
}

#[test]
fn heredoc_genuinely_missing_newline_still_errors() {
    // `<<EOF` followed by end-of-input (no newline, no literal `\n`) is a
    // genuinely-malformed heredoc and must still surface a parse error.
    let tools = sample_tool_registry();
    let text = "edit({ path: \"x.go\", content: <<EOF";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        0,
        "truncated heredoc opener must not produce a call"
    );
    assert!(
        !result.errors.is_empty(),
        "truncated heredoc opener must surface a parse error"
    );
}

#[test]
fn heredoc_escaped_body_unterminated_errors() {
    // Literal-`\n` form that never reaches a closing tag line must error, not
    // silently swallow the rest of the call.
    let tools = sample_tool_registry();
    let text = "edit({ path: \"x.go\", content: <<EOF\\npackage main\\nfunc main() {}";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(
        result.calls.len(),
        0,
        "unterminated escaped heredoc must not produce a call, calls: {:?}",
        result.calls
    );
    assert!(
        !result.errors.is_empty(),
        "unterminated escaped heredoc must surface a parse error"
    );
}
