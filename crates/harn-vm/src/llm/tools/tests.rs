//! Unit tests for `crate::llm::tools`: the fenceless TypeScript tool-call
//! parser, the schema → TypeScript renderer (TypeExpr + ComponentRegistry),
//! and the argument-normalizer compatibility shims.
//!
//! Declared as `#[cfg(test)] mod tests;` in `tools/mod.rs`, so `super::`
//! names either items defined directly in `mod.rs` or parser symbols
//! that `mod.rs` re-exports (`pub(crate) use parse::…`,
//! `pub(crate) use handle_local::…`) for callers outside the tools
//! module. Either way the flat `use super::{…}` below is accurate.

use super::{
    apply_tool_search_native_injection, build_assistant_response_message,
    build_assistant_tool_message, build_tool_calling_contract_prompt, build_tool_result_message,
    collect_tool_schemas, collect_tool_schemas_with_registry, extract_deferred_tool_names,
    normalize_tool_args, parse_bare_calls_in_body, parse_native_json_tool_calls,
    parse_text_tool_calls_with_tools, validate_tool_args, vm_tools_to_native, ComponentRegistry,
    TEXT_RESPONSE_PROTOCOL_HELP,
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
    // Field comments carry required/optional markers and examples inline.
    assert!(prompt.contains("path: string /* required"));
    assert!(prompt.contains("content?: string /* optional"));
    assert!(prompt.contains("\"internal/manifest/parser.go\""));
    assert!(!prompt.contains("@param path"));
    // Tagged response protocol contract is included in text mode.
    assert!(prompt.contains("declare function") || prompt.contains("Response protocol"));
}

#[test]
fn contract_prompt_help_block_documents_tagged_protocol() {
    // The help constant teaches the three top-level tags, the call shape
    // inside <tool_call>, and the done-block grammar.
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("Response protocol"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("</tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<assistant_prose>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<done>##DONE##</done>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("name({ key: value })"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("heredoc"));
    // Legacy bare-call phrasing must not regress.
    assert!(!TEXT_RESPONSE_PROTOCOL_HELP.contains("contains no tool calls"));
    assert!(!TEXT_RESPONSE_PROTOCOL_HELP.contains("```call"));
}

#[test]
fn contract_prompt_native_mode_prefers_provider_channel_without_text_fallback() {
    // Native mode should stay lean: the provider already receives the
    // structured `tools` payload, so the system prompt must not inject
    // the text-mode response grammar or duplicate `declare function`
    // schemas that can confuse native-tool parsers.
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "native", true, None);
    assert!(
        prompt.contains("native tool-calling channel"),
        "native preamble missing: {prompt}"
    );
    assert!(
        prompt.contains("This turn is action-gated"),
        "action gate missing: {prompt}"
    );
    assert!(prompt.contains("## Task ledger"));
    assert!(!prompt.contains("## Response protocol"));
    assert!(!prompt.contains("declare function edit(args:"));
    assert!(!prompt.contains("## Available tools"));
    assert!(!prompt.contains("<tool_call>"));
}

#[test]
fn contract_prompt_text_mode_mentions_action_gate_before_examples() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None);
    assert!(prompt.contains("This turn is action-gated."));
    assert!(prompt.contains("`<tool_call>...</tool_call>`"));
    assert!(prompt.contains("Do not emit raw source code"));
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

// ─── normalize_tool_args alias rewriting ───────────────────────────────────
//
// The VM consults the active policy's tool_annotations registry for the
// arg_aliases table; no tool-name branches live in harn. Tool-specific
// command shaping (e.g. joining argv arrays into shell strings) now lives
// in the pipeline's tool handler, not the VM.

#[test]
fn normalize_tool_args_rewrites_declared_aliases_from_active_policy() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    use crate::tool_annotations::{ToolAnnotations, ToolArgSchema, ToolKind};

    let mut annotations = std::collections::BTreeMap::new();
    let mut arg_aliases = std::collections::BTreeMap::new();
    arg_aliases.insert("file".to_string(), "path".to_string());
    arg_aliases.insert("mode".to_string(), "action".to_string());
    annotations.insert(
        "edit".to_string(),
        ToolAnnotations {
            kind: ToolKind::Edit,
            arg_schema: ToolArgSchema {
                arg_aliases,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let policy = CapabilityPolicy {
        tool_annotations: annotations,
        ..Default::default()
    };
    push_execution_policy(policy);

    // Aliases get rewritten to their canonical keys; non-aliased fields
    // pass through untouched; canonical key already present wins over alias.
    let out = normalize_tool_args(
        "edit",
        &json!({"file": "lib/foo.rs", "mode": "replace_range", "range_start": "3"}),
    );
    assert_eq!(out["path"], json!("lib/foo.rs"));
    assert_eq!(out["action"], json!("replace_range"));
    assert!(out.get("file").is_none());
    assert!(out.get("mode").is_none());
    // range_start still coerces string-numerics to integers.
    assert_eq!(out["range_start"], json!(3));

    pop_execution_policy();
}

#[test]
fn normalize_tool_args_skips_unannotated_tool() {
    // Unannotated tools get no alias rewriting — harn has no
    // hardcoded tool-name knowledge.
    let out = normalize_tool_args("mystery_tool", &json!({"file": "x.rs"}));
    assert_eq!(out, json!({"file": "x.rs"}));
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
    );

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
}

#[test]
fn assistant_tool_message_uses_ollama_native_shape() {
    let message = build_assistant_tool_message(
        "",
        &[json!({
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
        })],
        "ollama",
    );

    assert_eq!(message["role"], "assistant");
    assert!(message.get("content").is_none());
    assert_eq!(message["tool_calls"][0]["type"], "function");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"]["path"],
        "main.rs"
    );
}

#[test]
fn tool_result_message_uses_ollama_tool_name() {
    let message = build_tool_result_message("call_001", "read", "contents", "ollama");

    assert_eq!(message["role"], "tool");
    assert_eq!(message["tool_name"], "read");
    assert_eq!(message["content"], "contents");
    assert!(message.get("tool_call_id").is_none());
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

// ─── Tagged response protocol ──────────────────────────────────────────────
//
// These tests exercise the top-level `parse_text_tool_calls_with_tools`
// grammar: responses must be composed only of <tool_call>, <assistant_prose>,
// and <done> blocks at the top level, with whitespace between them.

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

// ─── Tool Vault: defer_loading + tool_search ────────────────────────────────
//
// These exercise the pieces of the progressive-disclosure surface that can't
// easily be reached from a `.harn` conformance test without a live provider:
// payload shape for the Anthropic-style native path, the deferred-tool name
// extractor, and the synthetic `tool_search_tool_*` meta-tool injection.

fn defer_loading_registry() -> VmValue {
    let mut eager_params = BTreeMap::new();
    eager_params.insert("path".to_string(), vm_str("string"));
    let eager = vm_dict(&[
        ("name", vm_str("look")),
        ("description", vm_str("Read file contents")),
        ("parameters", VmValue::Dict(Rc::new(eager_params))),
    ]);

    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", VmValue::Dict(Rc::new(deferred_params))),
        ("defer_loading", vm_bool(true)),
    ]);

    vm_list(vec![eager, deferred])
}

#[test]
fn vm_tools_to_native_emits_defer_loading_for_anthropic() {
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "anthropic").expect("anthropic native tools");
    // Eager `look` tool has no defer_loading key.
    let look = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("look"))
        .expect("look tool present");
    assert!(look.get("defer_loading").is_none());
    // Deferred `deploy` tool carries `defer_loading: true`.
    let deploy = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("deploy"))
        .expect("deploy tool present");
    assert_eq!(
        deploy.get("defer_loading").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn vm_tools_to_native_emits_namespace_for_openai_compat() {
    // `namespace` on a tool entry (harn#71) flows through to the
    // OpenAI-shape wrapper alongside `defer_loading`. Anthropic
    // receives it too — the field is harmless there (ignored by the
    // API) and keeps replay fidelity.
    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", VmValue::Dict(Rc::new(deferred_params))),
        ("defer_loading", vm_bool(true)),
        ("namespace", vm_str("ops")),
    ]);
    let registry = vm_list(vec![deferred]);

    let openai = vm_tools_to_native(&registry, "openai").expect("openai native tools");
    assert_eq!(openai[0]["namespace"].as_str(), Some("ops"));
    assert_eq!(openai[0]["defer_loading"].as_bool(), Some(true));

    let anthropic = vm_tools_to_native(&registry, "anthropic").expect("anthropic native tools");
    assert_eq!(
        anthropic[0]["namespace"].as_str(),
        Some("ops"),
        "namespace survives Anthropic passthrough (harmlessly ignored by API)"
    );
}

#[test]
fn vm_tools_to_native_emits_defer_loading_for_openai_compat() {
    // OpenAI-shape tools place the flag at the wrapper level (not inside
    // `function`) so harn#71's Responses-API path can read it uniformly
    // without re-walking. Non-Anthropic providers that don't understand
    // the flag will never actually see it — the capability gate in
    // options.rs blocks them before the request leaves the VM.
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "openai").expect("openai native tools");
    let deploy = tools
        .iter()
        .find(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                == Some("deploy")
        })
        .expect("deploy tool present");
    assert_eq!(
        deploy.get("defer_loading").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn extract_deferred_tool_names_walks_both_wire_shapes() {
    let anthropic = vec![
        json!({"name": "look"}),
        json!({"name": "deploy", "defer_loading": true}),
    ];
    assert_eq!(
        extract_deferred_tool_names(&anthropic),
        vec!["deploy".to_string()]
    );

    let openai = vec![
        json!({"type": "function", "function": {"name": "look"}}),
        json!({
            "type": "function",
            "function": {"name": "deploy"},
            "defer_loading": true,
        }),
    ];
    assert_eq!(
        extract_deferred_tool_names(&openai),
        vec!["deploy".to_string()]
    );
}

#[test]
fn apply_tool_search_native_injection_prepends_meta_tool() {
    let mut tools: Option<Vec<serde_json::Value>> =
        Some(vec![json!({"name": "look"}), json!({"name": "deploy"})]);
    apply_tool_search_native_injection(&mut tools, "anthropic", "bm25");
    let tools = tools.expect("tools still set");
    assert_eq!(tools.len(), 3, "search tool prepended");
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_bm25_20251119"),
        "bm25 variant uses the documented type string"
    );
    assert_eq!(tools[0]["name"].as_str(), Some("tool_search_tool_bm25"));
}

#[test]
fn apply_tool_search_native_injection_regex_variant() {
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![json!({"name": "look"})]);
    apply_tool_search_native_injection(&mut tools, "anthropic", "regex");
    let tools = tools.unwrap();
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_regex_20251119")
    );
    assert_eq!(tools[0]["name"].as_str(), Some("tool_search_tool_regex"));
}

#[test]
fn apply_tool_search_native_injection_emits_openai_shape_for_non_anthropic() {
    // OpenAI's native `tool_search` meta-tool (harn#71) uses a flat
    // `{"type": "tool_search", "mode": "hosted"}` shape, distinct from
    // Anthropic's versioned `tool_search_tool_*_20251119` block.
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![json!({"name": "look"})]);
    apply_tool_search_native_injection(&mut tools, "openai", "bm25");
    let tools = tools.unwrap();
    assert_eq!(tools.len(), 2, "OpenAI meta-tool prepended");
    assert_eq!(tools[0]["type"].as_str(), Some("tool_search"));
    assert_eq!(tools[0]["mode"].as_str(), Some("hosted"));
    assert!(
        tools[0].get("name").is_none(),
        "OpenAI meta-tool has no `name` field (that's an Anthropic detail)"
    );
    assert_eq!(tools[1]["name"].as_str(), Some("look"));
}

#[test]
fn apply_tool_search_native_injection_openai_collects_namespaces() {
    // When deferred tools declare a `namespace`, OpenAI's meta-tool
    // carries the distinct set so the server can group them.
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![
        json!({
            "type": "function",
            "function": {"name": "deploy_api"},
            "namespace": "ops",
        }),
        json!({
            "type": "function",
            "function": {"name": "deploy_web"},
            "namespace": "ops",
        }),
        json!({
            "type": "function",
            "function": {"name": "lookup_account"},
            "namespace": "crm",
        }),
    ]);
    apply_tool_search_native_injection(&mut tools, "openai", "bm25");
    let tools = tools.unwrap();
    let namespaces = tools[0]["namespaces"]
        .as_array()
        .expect("namespaces present");
    let names: Vec<&str> = namespaces.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec!["crm", "ops"], "sorted + deduped");
}

#[test]
fn apply_tool_search_native_injection_creates_list_when_empty() {
    let mut tools: Option<Vec<serde_json::Value>> = None;
    apply_tool_search_native_injection(&mut tools, "anthropic", "bm25");
    let tools = tools.expect("tools populated");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_bm25_20251119")
    );
}
