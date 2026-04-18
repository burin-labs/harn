use super::*;

#[test]
fn text_tool_format_drops_native_tool_channel() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })];
    assert!(normalize_native_tools_for_format("text", Some(native_tools.clone())).is_none());
    assert!(normalize_native_tools_for_format("json", Some(native_tools)).is_none());
}

#[test]
fn native_tool_format_preserves_native_tool_channel() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })];
    let preserved = normalize_native_tools_for_format("native", Some(native_tools.clone()));
    assert_eq!(preserved, Some(native_tools));
}

#[test]
fn tool_examples_render_in_both_formats() {
    // Pre-v0.5.82 native mode dropped the text-mode examples to avoid two
    // protocols competing in the prompt. That breaks for hosts that strip
    // the native `tools` parameter (e.g. Ollama models with bare
    // `{{ .Prompt }}` chat templates) — the model ends up with no tool
    // guidance at all. Examples now flow through in both modes; the parser
    // accepts either channel.
    assert_eq!(
        normalize_tool_examples_for_format("native", Some(" edit({ path: \"a\" }) ".to_string())),
        Some("edit({ path: \"a\" })".to_string())
    );
    assert_eq!(
        normalize_tool_examples_for_format("text", Some(" edit({ path: \"a\" }) ".to_string())),
        Some("edit({ path: \"a\" })".to_string())
    );
    assert_eq!(
        normalize_tool_examples_for_format("native", Some("   ".to_string())),
        None
    );
    assert_eq!(normalize_tool_examples_for_format("native", None), None);
}

#[test]
fn native_action_stage_requires_tool_choice_when_missing() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(120),
    };
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "parameters": {"type": "object"}
        }
    })];
    let choice = normalize_tool_choice_for_format(
        "openrouter",
        "native",
        Some(&native_tools),
        None,
        Some(&policy),
    );
    assert_eq!(choice, Some(serde_json::json!("required")));
}

#[test]
fn native_action_stage_uses_provider_specific_tool_choice() {
    assert_eq!(
        required_tool_choice_for_provider("anthropic"),
        serde_json::json!({"type": "any"})
    );
}
