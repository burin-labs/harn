use super::*;

#[test]
fn contract_prompt_renders_edit_signature_with_enum_and_required_markers() {
    let tools = sample_tool_registry();
    let prompt = build_tool_calling_contract_prompt(Some(&tools), None, "text", true, None);
    assert!(
        prompt.contains("declare function edit(args:"),
        "missing TS declaration: {prompt}"
    );
    assert!(
        prompt.contains("\"create\" | \"patch\" | \"replace_body\""),
        "enum should render as literal union: {prompt}"
    );
    let obj_start = prompt.find("args: {").unwrap();
    let obj_end = prompt[obj_start..].find("})").unwrap() + obj_start;
    let obj_body = &prompt[obj_start..obj_end];
    let path_idx = obj_body.find("path:").unwrap();
    let content_idx = obj_body.find("content?:").unwrap();
    assert!(
        path_idx < content_idx,
        "required `path` should appear before optional `content?`: {obj_body}"
    );
    assert!(obj_body.contains("content?: string"));
    assert!(obj_body.contains("new_body?: string"));
    assert!(prompt.contains("path: string /* required"));
    assert!(prompt.contains("content?: string /* optional"));
    assert!(prompt.contains("\"internal/manifest/parser.go\""));
    assert!(!prompt.contains("@param path"));
    assert!(prompt.contains("declare function") || prompt.contains("Response protocol"));
}

#[test]
fn contract_prompt_help_block_documents_tagged_protocol() {
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("Response protocol"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("</tool_call>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<assistant_prose>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("<done>##DONE##</done>"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("name({ key: value })"));
    assert!(TEXT_RESPONSE_PROTOCOL_HELP.contains("heredoc"));
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
    assert!(
        prompt.contains("## Tool call examples"),
        "missing examples header: {prompt}"
    );
    assert!(
        prompt.contains("read({ path: \"src/main.rs\" })"),
        "missing example content: {prompt}"
    );
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
