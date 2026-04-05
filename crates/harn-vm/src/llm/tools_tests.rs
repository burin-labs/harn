//! Unit tests for `crate::llm::tools`: the fenceless TypeScript tool-call
//! parser, the schema → TypeScript renderer (TypeExpr + ComponentRegistry),
//! and the argument-normalizer compatibility shims.
//!
//! This file is included via `#[path = "tools_tests.rs"] mod tests;` in
//! `tools.rs`, so everything here has full access to that module's private
//! items as if it were inlined.

use super::{
    build_tool_calling_contract_prompt, collect_tool_schemas_with_registry, normalize_tool_args,
    parse_text_tool_calls_with_tools, ComponentRegistry, TS_CALL_CONTRACT_HELP,
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

    let tool = vm_dict(&[
        ("name", vm_str("edit")),
        ("description", vm_str("Precise code edit.")),
        ("parameters", VmValue::Dict(Rc::new(params))),
    ]);

    vm_dict(&[("tools", vm_list(vec![tool]))])
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
fn ignores_tool_name_mentions_inside_markdown_code_fences() {
    let tools = sample_tool_registry();
    let text = "Here's how to use it in prose:\n```\nedit({ action: \"create\", path: \"oops.go\" })\n```\nNow the real call:\nedit({ action: \"create\", path: \"real.go\" })\n";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(
        result.calls.len(),
        1,
        "only the call outside the fence counts"
    );
    assert_eq!(result.calls[0]["arguments"]["path"], json!("real.go"));
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
fn ignores_unknown_tool_names() {
    let tools = sample_tool_registry();
    // `fictitious_tool(...)` looks like a call but the name is not in the
    // registry, so the scanner should leave it alone.
    let text = "fictitious_tool({ x: 1 })\nedit({ action: \"create\", path: \"a.go\" })";
    let result = parse_text_tool_calls_with_tools(text, Some(&tools));
    assert!(result.errors.is_empty());
    assert_eq!(result.calls.len(), 1);
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
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true);
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
    assert!(TS_CALL_CONTRACT_HELP.contains("template literal"));
    assert!(!TS_CALL_CONTRACT_HELP.contains("```call"));
    assert!(!TS_CALL_CONTRACT_HELP.contains("<<'EOF'"));
}

#[test]
fn contract_prompt_native_mode_omits_text_help() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "native", true);
    assert!(prompt.contains("native tool-calling channel"));
    assert!(!prompt.contains("How to call tools"));
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
    let prompt = build_tool_calling_contract_prompt(None, Some(&native_tools), "text", false);
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
