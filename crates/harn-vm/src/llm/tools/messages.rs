use super::handle_local::coerce_integer_like_tool_args;

/// Build an assistant message with tool_calls for the conversation history.
/// Format varies by API style (OpenAI-compatible vs Anthropic).
pub(crate) fn build_assistant_tool_message(
    text: &str,
    tool_calls: &[serde_json::Value],
    provider: &str,
) -> serde_json::Value {
    let resolved = super::super::helpers::ResolvedProvider::resolve(provider);
    let is_anthropic = resolved.is_anthropic_style;
    let is_ollama = provider == "ollama" || resolved.endpoint.contains("/api/chat");
    if is_anthropic {
        // Anthropic format: content blocks with text and tool_use
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(serde_json::json!({"type": "text", "text": text}));
        }
        for tc in tool_calls {
            content.push(serde_json::json!({
                "type": "tool_use",
                "id": tc["id"],
                "name": tc["name"],
                "input": tc["arguments"],
            }));
        }
        serde_json::json!({"role": "assistant", "content": content})
    } else if is_ollama {
        // Ollama `/api/chat` expects native tool-call history with
        // object arguments instead of OpenAI-style JSON strings.
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, tc)| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "index": idx,
                        "name": tc["name"],
                        "arguments": tc["arguments"],
                    }
                })
            })
            .collect();
        let mut msg = serde_json::json!({
            "role": "assistant",
            "tool_calls": calls,
        });
        if !text.is_empty() {
            msg["content"] = serde_json::json!(text);
        }
        msg
    } else {
        // OpenAI-compatible format: assistant message with tool_calls array
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc["id"],
                    "type": "function",
                    "function": {
                        "name": tc["name"],
                        "arguments": serde_json::to_string(&tc["arguments"]).unwrap_or_default(),
                    }
                })
            })
            .collect();
        serde_json::json!({
            "role": "assistant",
            "content": if text.is_empty() { serde_json::Value::String(String::new()) } else { serde_json::json!(text) },
            "tool_calls": calls,
        })
    }
}

/// Build a durable assistant message for transcript/run-record storage.
/// Prefer canonical structured blocks when available so hosts can restore
/// richer assistant state without reparsing visible text.
pub(crate) fn build_assistant_response_message(
    text: &str,
    blocks: &[serde_json::Value],
    tool_calls: &[serde_json::Value],
    reasoning: Option<&str>,
    provider: &str,
) -> serde_json::Value {
    let mut message = if !tool_calls.is_empty() {
        build_assistant_tool_message(text, tool_calls, provider)
    } else if !blocks.is_empty() {
        serde_json::json!({
            "role": "assistant",
            "content": blocks,
        })
    } else {
        serde_json::json!({
            "role": "assistant",
            "content": text,
        })
    };
    if let Some(reasoning) = reasoning.filter(|value| !value.is_empty()) {
        message["reasoning"] = serde_json::json!(reasoning);
    }
    message
}

/// Build a tool result message for the conversation history.
pub(crate) fn build_tool_result_message(
    tool_call_id: &str,
    tool_name: &str,
    result: &str,
    provider: &str,
) -> serde_json::Value {
    let resolved = super::super::helpers::ResolvedProvider::resolve(provider);
    let is_anthropic = resolved.is_anthropic_style;
    let is_ollama = provider == "ollama" || resolved.endpoint.contains("/api/chat");
    if is_anthropic {
        // Anthropic: tool_result inside a user message
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": result,
            }]
        })
    } else if is_ollama {
        serde_json::json!({
            "role": "tool",
            "tool_name": tool_name,
            "content": result,
        })
    } else {
        // OpenAI-compatible: distinct "tool" role
        serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": result,
        })
    }
}

/// Normalize tool call arguments before dispatch.
///
/// The VM walks the active policy's
/// `tool_annotations[name].arg_schema.arg_aliases` table and rewrites any
/// aliases present in the arguments object to their canonical keys. This
/// is purely driven by pipeline declarations — the VM has no hardcoded
/// tool-name branches. If a tool isn't annotated, no aliases are rewritten.
pub(crate) fn normalize_tool_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let mut obj = match args.as_object() {
        Some(o) => o.clone(),
        None => return args.clone(),
    };

    if let Some(annotations) = crate::orchestration::current_tool_annotations(name) {
        for (alias, canonical) in &annotations.arg_schema.arg_aliases {
            if obj.contains_key(canonical) {
                continue;
            }
            if let Some(value) = obj.remove(alias) {
                obj.insert(canonical.clone(), value);
            }
        }
    }

    let mut normalized = serde_json::Value::Object(obj);
    coerce_integer_like_tool_args(&mut normalized);
    normalized
}
