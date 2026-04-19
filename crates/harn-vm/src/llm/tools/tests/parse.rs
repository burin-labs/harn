use super::*;

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
    let tools = sample_tool_registry();
    let text = "```tool_code\nrun({ command: \"ls\" })\n```";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["name"], json!("run"));
}

#[test]
fn unknown_label_prefix_with_unknown_identifier_stays_prose() {
    let tools = sample_tool_registry();
    let text = "Note: make_coffee({ strength: \"strong\" })";
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 0);
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
fn text_parser_falls_through_to_native_json_fallback() {
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
    let tools = sample_tool_registry();
    let text = r#"edit({ action: "create", path: "a.go", content: "pkg a" })

[{"id":"call_001","type":"function","function":{"name":"run","arguments":"{\"command\":\"go test\"}"}}]"#;
    let result = parse_bare_calls_in_body(text, Some(&tools));
    assert_eq!(result.calls.len(), 1, "text format should take priority");
    assert_eq!(result.calls[0]["name"], json!("edit"));
}

#[test]
fn text_parser_reports_unknown_tool_in_native_json_fallback() {
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
    assert_eq!(result.prose, "Creating the file.");
    assert_eq!(result.done_marker.as_deref(), Some("##DONE##"));
    assert!(
        !result.canonical.is_empty(),
        "canonical must be populated so history replays the tagged shape"
    );
    assert!(result.canonical.contains("<tool_call>"));
    assert!(result.canonical.contains("<done>##DONE##</done>"));
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
    assert_eq!(result.calls.len(), 1);
}

#[test]
fn tagged_parser_executes_bare_tool_call_with_soft_violation() {
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
            .and_then(|n| n.as_str())
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
        result.violations.iter().any(|v| v.contains("Unknown")),
        "unknown top-level tag should be flagged: {:?}",
        result.violations
    );
    assert_eq!(result.calls.len(), 1, "known <tool_call> still executes");
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
            .any(|v| v.contains("<done> block is empty")),
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
    assert_eq!(
        result.canonical.trim(),
        "<assistant_prose>\nNarration.\n</assistant_prose>"
    );
    assert!(!result.canonical.contains("leading garbage"));
}

#[test]
fn tagged_parser_accepts_configured_done_body() {
    let tools = sample_tool_registry();
    let text = "<done>PLAN_READY</done>";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.done_marker.as_deref(), Some("PLAN_READY"));
    assert!(result.violations.is_empty());
}
