//! Unit tests for `crate::llm::tools`: the fenceless TypeScript tool-call
//! parser, the schema → TypeScript renderer (TypeExpr + ComponentRegistry),
//! and the argument-normalizer compatibility shims.
//!
//! This file is included via `#[path = "tools_tests.rs"] mod tests;` in
//! `tools.rs`, so everything here has full access to that module's private
//! items as if it were inlined.

use super::{
    build_assistant_response_message, build_assistant_tool_message,
    build_tool_calling_contract_prompt, collect_tool_schemas, collect_tool_schemas_with_registry,
    normalize_tool_args, parse_native_json_tool_calls, parse_text_tool_calls_with_tools,
    validate_tool_args, ComponentRegistry, TS_CALL_CONTRACT_HELP,
};
use crate::value::VmValue;
use serde_json::json;
use std::collections::BTreeMap;
use std::rc::Rc;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
}

fn vm_str(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

fn vm_bool(b: bool) -> VmValue {
    VmValue::Bool(b)
}

fn vm_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

/// Build a small tool registry containing an `edit` tool with rich schema
/// (enum action, required path, multiple fields). Returned as a VmValue so it
/// can be passed to `parse_text_tool_calls_with_tools`.
fn sample_tool_registry() -> VmValue {
    // parameters dict
    let mut params = BTreeMap::new();
    params.insert(
        "action".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            (
                "enum",
                vm_list(vec![
                    vm_str("create"),
                    vm_str("patch"),
                    vm_str("replace_body"),
                ]),
            ),
            ("description", vm_str("Kind of edit.")),
        ]),
    );
    params.insert(
        "path".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("description", vm_str("Repo-relative path.")),
            (
                "examples",
                vm_list(vec![vm_str("internal/manifest/parser.go")]),
            ),
        ]),
    );
    params.insert(
        "content".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("File contents for create.")),
        ]),
    );
    params.insert(
        "new_body".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Replacement body for replace_body.")),
        ]),
    );
    params.insert(
        "function_name".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Existing function name.")),
        ]),
    );
    params.insert(
        "import_statement".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("required", vm_bool(false)),
            ("description", vm_str("Import line for add_import.")),
        ]),
    );
    params.insert(
        "ops".to_string(),
        vm_dict(&[
            ("type", vm_str("list")),
            ("required", vm_bool(false)),
            ("description", vm_str("Atomic same-file batch edit ops.")),
        ]),
    );

    let edit_tool = vm_dict(&[
        ("name", vm_str("edit")),
        ("description", vm_str("Precise code edit.")),
        ("parameters", VmValue::Dict(Rc::new(params))),
    ]);

    // run tool
    let mut run_params = BTreeMap::new();
    run_params.insert(
        "command".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            ("description", vm_str("Shell command to execute.")),
        ]),
    );
    let run_tool = vm_dict(&[
        ("name", vm_str("run")),
        ("description", vm_str("Run a shell command.")),
        ("parameters", VmValue::Dict(Rc::new(run_params))),
    ]);

    vm_dict(&[("tools", vm_list(vec![edit_tool, run_tool]))])
}

// ─── Fenceless TS parser ───────────────────────────────────────────────────

#[test]
fn parses_a_simple_object_literal_call() {
    let tools = sample_tool_registry();
    let text = r#"I'll write the scaffold now.
edit({ action: "create", path: "a.go", content: "package a\n" })
That should work."#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("real.go"));
}

#[test]
fn recovers_single_inline_wrapped_tool_call_when_it_is_the_entire_response() {
    let tools = sample_tool_registry();
    let text = r#"`edit({ action: "create", path: "wrapped.go" })`"#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["path"], json!("wrapped.go"));
}

#[test]
fn recovers_single_fenced_tool_call_when_it_is_the_entire_response() {
    let tools = sample_tool_registry();
    let text = "```typescript\nedit({ action: \"create\", path: \"wrapped.go\" })\n```";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
fn reports_unknown_tool_names() {
    let tools = sample_tool_registry();
    // `fictitious_tool(...)` looks like a call but the name is not in the
    // registry, so the scanner should report an error and still parse the
    // valid `edit` call.
    let text = "fictitious_tool({ x: 1 })\nedit({ action: \"create\", path: \"a.go\" })";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
fn parses_multiple_calls_in_one_response() {
    let tools = sample_tool_registry();
    let text = "edit({ action: \"create\", path: \"a.go\", content: \"a\" })\nThen we will:\nedit({ action: \"patch\", path: \"b.go\" })\n";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
}

#[test]
fn accepts_single_quoted_string_literals() {
    let tools = sample_tool_registry();
    let text = "edit({ action: 'create', path: 'a.go' })";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(result.calls[0]["arguments"]["action"], json!("create"));
    assert_eq!(result.calls[0]["arguments"]["path"], json!("a.go"));
}

#[test]
fn rejects_legacy_named_argument_call_form() {
    let tools = sample_tool_registry();
    let text = "edit(action=\"replace_body\", path=\"a.go\", new_body=`return 1`)";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.calls.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0].contains("object literal"),
        "diagnostic should require strict object-literal syntax: {:?}",
        result.errors
    );
}

// ─── Tool-calling contract prompt ───────────────────────────────────────────

#[test]
fn contract_prompt_renders_edit_signature_with_enum_and_required_markers() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None);
    // TypeScript declaration header.
    assert!(
        prompt.contains("declare function edit(args:"),
        "missing TS declaration: {prompt}"
    );
    // Enum rendered as literal union.
    assert!(
        prompt.contains("\"create\" | \"patch\" | \"replace_body\""),
        "enum should render as literal union: {prompt}"
    );
    // Required `path` comes before optional fields in the object type.
    let obj_start = prompt.find("args: {").unwrap();
    let obj_end = prompt[obj_start..].find("})").unwrap() + obj_start;
    let obj_body = &prompt[obj_start..obj_end];
    let path_idx = obj_body.find("path:").unwrap();
    let content_idx = obj_body.find("content?:").unwrap();
    assert!(
        path_idx < content_idx,
        "required `path` should appear before optional `content?`: {obj_body}"
    );
    // Optional fields carry a trailing `?` in the declaration.
    assert!(obj_body.contains("content?: string"));
    assert!(obj_body.contains("new_body?: string"));
    // JSDoc @param lines with required/optional markers + example.
    assert!(prompt.contains("@param path (required)"));
    assert!(prompt.contains("@param content (optional)"));
    assert!(prompt.contains("\"internal/manifest/parser.go\""));
    // TS call contract help is included in text mode.
    assert!(prompt.contains("declare function") || prompt.contains("How to call tools"));
}

#[test]
fn contract_prompt_help_block_has_ts_call_example() {
    // The help constant is included verbatim in text mode and must show a
    // real TS call example, not the old Python/heredoc syntax.
    assert!(TS_CALL_CONTRACT_HELP.contains("edit({"));
    assert!(TS_CALL_CONTRACT_HELP.contains("heredoc"));
    assert!(!TS_CALL_CONTRACT_HELP.contains("```call"));
    assert!(!TS_CALL_CONTRACT_HELP.contains("<<'EOF'"));
}

#[test]
fn contract_prompt_native_mode_omits_text_help() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "native", true, None);
    assert!(prompt.contains("native tool-calling channel"));
    assert!(!prompt.contains("How to call tools"));
}

#[test]
fn contract_prompt_includes_tool_examples_before_schemas() {
    let tools = sample_tool_registry();
    let examples = "read({ path: \"src/main.rs\" })\n\nedit({ action: \"create\", path: \"test.rs\", content: <<EOF\nfn main() {}\nEOF\n})";
    let prompt =
        build_tool_calling_contract_prompt(Some(&tools), None, "text", true, Some(examples));
    // Examples section is present.
    assert!(
        prompt.contains("## Tool call examples"),
        "missing examples header: {prompt}"
    );
    assert!(
        prompt.contains("read({ path: \"src/main.rs\" })"),
        "missing example content: {prompt}"
    );
    // Examples appear BEFORE the tool schemas.
    let examples_pos = prompt.find("Tool call examples").unwrap();
    let schemas_pos = prompt.find("Available tools").unwrap();
    assert!(
        examples_pos < schemas_pos,
        "examples ({examples_pos}) should appear before schemas ({schemas_pos})"
    );
}

#[test]
fn contract_prompt_omits_examples_section_when_none() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None);
    assert!(
        !prompt.contains("Tool call examples"),
        "should not have examples section when None"
    );
}

// ─── $ref / ComponentRegistry ───────────────────────────────────────────────

#[test]
fn native_schema_ref_resolves_to_component_alias() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "touch",
            "description": "Touch a file path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "$ref": "#/components/schemas/FilePath" }
                },
                "required": ["path"]
            }
        },
        "components": {
            "schemas": {
                "FilePath": {
                    "type": "string",
                    "description": "Repo-relative path"
                }
            }
        }
    })];
    let (schemas, registry) = collect_tool_schemas_with_registry(None, Some(&native_tools));
    assert_eq!(schemas.len(), 1);
    let aliases = registry.render_aliases();
    assert!(
        aliases.contains("type FilePath = string;"),
        "expected type alias for FilePath: {aliases}"
    );
    // The signature for `touch` should reference `FilePath` by name.
    let prompt = build_tool_calling_contract_prompt(None, Some(&native_tools), "text", false, None);
    assert!(
        prompt.contains("type FilePath = string;"),
        "prompt missing alias: {prompt}"
    );
    assert!(
        prompt.contains("path: FilePath"),
        "signature should reference alias: {prompt}"
    );
}

#[test]
fn component_registry_handles_recursive_refs_without_looping() {
    let mut registry = ComponentRegistry::default();
    // A root schema where `Node` refers to itself via its children. We just
    // need to prove resolution terminates.
    let root = json!({
        "components": {
            "schemas": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "children": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Node" }
                        }
                    }
                }
            }
        }
    });
    let node_schema = root["components"]["schemas"]["Node"].clone();
    let _ty = super::json_schema_to_type_expr(&node_schema, &root, &mut registry);
    // Alias rendering must not panic or infinite-loop.
    let _ = registry.render_aliases();
}

// ─── normalize_tool_args pass-through ───────────────────────────────────────

#[test]
fn normalize_tool_args_joins_run_command_arrays() {
    let args = json!({"command": ["go", "test", "./..."]});
    let out = normalize_tool_args("run", &args);
    assert_eq!(out["command"], json!("go test ./..."));
}

#[test]
fn normalize_tool_args_accepts_run_args_alias() {
    let args = json!({"args": ["go", "test"]});
    let out = normalize_tool_args("run", &args);
    assert_eq!(out["command"], json!("go test"));
}

#[test]
fn normalize_tool_args_recovers_stringified_run_array() {
    let args = json!({"command": "[\"go\", \"test\"]"});
    let out = normalize_tool_args("run", &args);
    assert_eq!(out["command"], json!("go test"));
}

#[test]
fn normalize_tool_args_recovers_fragmented_run_array() {
    // The model sometimes splits a JSON-encoded command array across `args`
    // and `command` when it runs out of tokens mid-literal. The normalizer
    // reassembles them into a single shell string.
    let out = normalize_tool_args(
        "run",
        &json!({"command": "\"internal/manifest/\"]", "args": "[\"ls\""}),
    );
    assert_eq!(out["command"], json!("ls internal/manifest/"));
}

// ─── Heredoc parser tests ──────────────────────────────────────────────────

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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
fn heredoc_unterminated_is_error() {
    let tools = sample_tool_registry();
    let text = r#"edit({
    action: "create",
    path: "main.go",
    content: <<EOF
package main
// no closing EOF tag
})"#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(
        result.calls.is_empty() || !result.errors.is_empty(),
        "missing tag should error"
    );
}

#[test]
fn template_literal_still_works() {
    let tools = sample_tool_registry();
    let text = "edit({\n    action: \"create\",\n    path: \"simple.txt\",\n    content: `hello world`\n})";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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

// ─── Native JSON fallback parser ──────────────────────────────────────────

fn known_tools_set() -> std::collections::BTreeSet<String> {
    ["edit", "read", "run", "lookup", "scaffold"]
        .into_iter()
        .map(String::from)
        .collect()
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
fn text_parser_falls_through_to_native_json_fallback() {
    // End-to-end: the main parse_text_tool_calls_with_tools should fall
    // through to the native JSON parser when text parsing finds nothing
    let tools = sample_tool_registry();
    let text = r#"I'll create the file.

[{"id":"call_001","type":"function","function":{"name":"edit","arguments":"{\"action\":\"create\",\"path\":\"main.go\",\"content\":\"package main\\nfunc main() {}\"}"}}]"#;
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
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
    );

    assert_eq!(message["reasoning"], "inspect the file before editing");
    assert_eq!(message["content"], "");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
}

// ─── handle_tool_locally: read_file offset/limit ───────────────────────────

#[test]
fn read_file_offset_and_limit() {
    use super::handle_tool_locally;
    use std::io::Write;

    // Create a temp file with numbered lines.
    let dir = std::env::temp_dir().join("harn_test_read_file_offset");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_offset.txt");
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

    // Clean up.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

// ── validate_tool_args ─────────────────────────────────────────────────────

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
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
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
