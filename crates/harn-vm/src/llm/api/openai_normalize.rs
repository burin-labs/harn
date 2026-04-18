//! Message-shape normalization for OpenAI-style providers. Handles both
//! string and structured `content` payloads, surfaces hidden reasoning
//! fields (`reasoning` / `reasoning_content`), and splits inline
//! `<think>...</think>` blocks via [`super::thinking`].

use super::thinking::split_openai_thinking_blocks;

pub(super) fn render_openai_message_content_as_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => {
            let mut rendered = String::new();
            for block in blocks {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" | "output_text" => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            rendered.push_str(text);
                        }
                    }
                    "tool_result" => {
                        let content = block
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if !rendered.is_empty() {
                            rendered.push_str("\n\n");
                        }
                        rendered.push_str("[Result] ");
                        rendered.push_str(content);
                    }
                    "reasoning" | "thinking" => {
                        if let Some(text) = block
                            .get("text")
                            .and_then(|v| v.as_str())
                            .or_else(|| block.get("thinking").and_then(|v| v.as_str()))
                        {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(text);
                        }
                    }
                    _ => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(text);
                        } else if !block.is_null() {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(&block.to_string());
                        }
                    }
                }
            }
            rendered
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(super) fn extract_openai_message_field_as_text(
    message: &serde_json::Value,
    field_names: &[&str],
) -> String {
    let mut combined = String::new();
    for field_name in field_names {
        let field_text = message
            .get(*field_name)
            .map(render_openai_message_content_as_text)
            .unwrap_or_default();
        if field_text.trim().is_empty() {
            continue;
        }
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(field_text.trim());
    }
    combined
}

pub(super) fn append_paragraph(target: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text.trim());
}

pub(super) fn normalize_openai_message_text(message: &serde_json::Value) -> (String, String) {
    let raw_text = extract_openai_message_field_as_text(message, &["content"]);
    let reasoning_text =
        extract_openai_message_field_as_text(message, &["reasoning", "reasoning_content"]);
    // Qwen3/3.5 emit inline `<think>...</think>` when
    // `chat_template_kwargs.enable_thinking` is set. Split them out so the
    // agent loop doesn't treat reasoning as output or parse tool calls
    // inside them.
    let (mut text, inline_thinking) = split_openai_thinking_blocks(&raw_text);
    let mut extracted_thinking = String::new();
    append_paragraph(&mut extracted_thinking, &reasoning_text);
    append_paragraph(&mut extracted_thinking, &inline_thinking);
    if text.is_empty() && !extracted_thinking.is_empty() {
        text = extracted_thinking.clone();
    }
    (text, extracted_thinking)
}

pub(crate) fn normalize_openai_style_messages(
    messages: Vec<serde_json::Value>,
    force_string_content: bool,
) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|message| {
            let Some(object) = message.as_object() else {
                return message;
            };
            let mut normalized = object.clone();
            if force_string_content {
                let content = normalized
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::String(String::new()));
                normalized.insert(
                    "content".to_string(),
                    serde_json::Value::String(render_openai_message_content_as_text(&content)),
                );
            }
            serde_json::Value::Object(normalized)
        })
        .collect()
}

fn should_debug_message_shapes() -> bool {
    std::env::var("HARN_DEBUG_MESSAGE_SHAPES")
        .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub(crate) fn debug_log_message_shapes(label: &str, messages: &[serde_json::Value]) {
    if !should_debug_message_shapes() {
        return;
    }
    let summary = messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let role = message
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let content_kind = match message.get("content") {
                Some(serde_json::Value::String(_)) => "string",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Array(_)) => "array",
                Some(serde_json::Value::Object(_)) => "object",
                Some(_) => "other",
                None => "missing",
            };
            let has_tool_call_id = message.get("tool_call_id").is_some();
            let tool_calls = message
                .get("tool_calls")
                .and_then(|value| value.as_array())
                .map(|calls| calls.len())
                .unwrap_or(0);
            let has_reasoning = message
                .get("reasoning")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            format!(
                "#{idx}:{role}:content={content_kind}:tool_call_id={has_tool_call_id}:tool_calls={tool_calls}:reasoning={has_reasoning}"
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    crate::events::log_info("llm.message_shape", &format!("{label}: {summary}"));
}

#[cfg(test)]
mod tests {
    use super::normalize_openai_message_text;

    #[test]
    fn normalize_openai_message_text_uses_reasoning_when_content_missing() {
        let message = serde_json::json!({
            "reasoning": "hello from reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message);
        assert_eq!(visible, "hello from reasoning");
        assert_eq!(thinking, "hello from reasoning");
    }

    #[test]
    fn normalize_openai_message_text_merges_reasoning_and_inline_think_blocks() {
        let message = serde_json::json!({
            "content": "<think>inline reasoning</think>visible answer",
            "reasoning": "separate reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message);
        assert_eq!(visible, "visible answer");
        assert_eq!(thinking, "separate reasoning\ninline reasoning");
    }
}
