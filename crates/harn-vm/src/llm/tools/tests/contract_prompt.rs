use super::{
    build_tool_calling_contract_prompt, collect_tool_schemas_with_registry, json,
    normalize_tool_args, sample_tool_registry, ComponentRegistry, TEXT_RESPONSE_PROTOCOL_HELP,
};

#[test]
fn contract_prompt_renders_edit_signature_with_enum_and_required_markers() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None, false);
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
    // The help constant teaches the top-level tags, the call shape
    // inside <tool_call>, and the done-block grammar.
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("Response protocol"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("</tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<assistant_prose>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<user_response>"));
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
    let prompt =
        build_tool_calling_contract_prompt(Some(&tools), None, "native", true, None, false);
    assert!(
        prompt.contains("native tool-calling channel"),
        "native preamble missing: {prompt}"
    );
    assert!(
        prompt.contains("This turn is action-gated"),
        "action gate missing: {prompt}"
    );
    assert!(!prompt.contains("## Task ledger"));
    assert!(!prompt.contains("## Response protocol"));
    assert!(!prompt.contains("declare function edit(args:"));
    assert!(!prompt.contains("## Available tools"));
    assert!(!prompt.contains("<tool_call>"));
}

#[test]
fn contract_prompt_ledger_help_requires_visible_task_ledger_ids() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "native", true, None, true);
    assert!(
        prompt.contains(
            "Only use the `ledger` tool if that `<task_ledger>` block is actually present"
        ),
        "missing guarded ledger guidance: {prompt}"
    );
    assert!(
        prompt.contains("do not invent ids such as `deliverable-N`"),
        "missing invented-id warning: {prompt}"
    );
    assert!(
        prompt.contains("deliverable-id-from-task-ledger"),
        "missing concrete id placeholder: {prompt}"
    );
}

#[test]
fn contract_prompt_text_mode_mentions_action_gate_before_examples() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None, false);
    assert!(prompt.contains("This turn is action-gated."));
    assert!(prompt.contains("`<tool_call>...</tool_call>`"));
    assert!(prompt.contains("Do not emit raw source code"));
}

#[test]
fn contract_prompt_includes_tool_examples_before_schemas() {
    let tools = sample_tool_registry();
    let examples = "read({ path: \"src/main.rs\" })\n\nedit({ action: \"create\", path: \"test.rs\", content: <<EOF\nfn main() {}\nEOF\n})";
    let prompt =
        build_tool_calling_contract_prompt(Some(&tools), None, "text", true, Some(examples), false);
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
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None, false);
    assert!(
        !prompt.contains("Tool call examples"),
        "should not have examples section when None"
    );
}

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
    let prompt =
        build_tool_calling_contract_prompt(None, Some(&native_tools), "text", false, None, false);
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
    let _ty = super::super::json_schema_to_type_expr(&node_schema, &root, &mut registry);
    // Alias rendering must not panic or infinite-loop.
    let _ = registry.render_aliases();
}

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
