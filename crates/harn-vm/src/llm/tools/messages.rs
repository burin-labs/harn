use super::handle_local::coerce_integer_like_tool_args;

/// Build an assistant message with tool_calls for the conversation history.
/// Format varies by API style (OpenAI-compatible vs Anthropic).
pub(crate) fn build_assistant_tool_message(
    text: &str,
    tool_calls: &[serde_json::Value],
    provider: &str,
    model: &str,
) -> serde_json::Value {
    let is_anthropic = super::super::provider::provider_uses_anthropic_messages(provider, model);
    let is_gemini = super::super::provider::provider_uses_gemini_messages(provider, model);
    let is_ollama = super::super::provider::provider_uses_ollama_messages(provider, model);
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
    } else if is_gemini {
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(serde_json::json!({"text": text}));
        }
        for tc in tool_calls {
            let mut function_call = serde_json::json!({
                "name": tc["name"],
                "args": tc["arguments"],
            });
            if let Some(id) = tc.get("id").and_then(|value| value.as_str()) {
                if !id.is_empty() {
                    function_call["id"] = serde_json::json!(id);
                }
            }
            let mut part = serde_json::json!({ "functionCall": function_call });
            if let Some(signature) = crate::llm::providers::gemini_tool_call_thought_signature(tc) {
                part["thoughtSignature"] = serde_json::json!(signature);
            }
            parts.push(part);
        }
        serde_json::json!({"role": "assistant", "content": parts})
    } else if is_ollama {
        // Ollama's chat API uses the OpenAI-style `function` envelope but
        // its request schema expects `function.arguments` to be a string.
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, tc)| {
                serde_json::json!({
                    "id": tc["id"],
                    "type": "function",
                    "function": {
                        "index": idx,
                        "name": tc["name"],
                        "arguments": serde_json::to_string(&tc["arguments"]).unwrap_or_default(),
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
    model: &str,
) -> serde_json::Value {
    let mut message = if !tool_calls.is_empty() {
        if super::super::provider::provider_uses_gemini_messages(provider, model)
            && !blocks.is_empty()
        {
            let content =
                crate::llm::content::gemini_parts(&serde_json::Value::Array(blocks.to_vec()));
            if content
                .iter()
                .any(|part| part.get("functionCall").is_some())
            {
                serde_json::json!({"role": "assistant", "content": content})
            } else {
                build_assistant_tool_message(text, tool_calls, provider, model)
            }
        } else {
            build_assistant_tool_message(text, tool_calls, provider, model)
        }
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

/// Normalize tool call arguments before dispatch.
///
/// The VM walks the active policy's
/// `tool_annotations[name].arg_schema.arg_aliases` table and rewrites any
/// aliases present in the arguments object to their canonical keys. This
/// is purely driven by pipeline declarations — the VM has no hardcoded
/// tool-name branches. If a tool isn't annotated, no aliases are rewritten.
///
/// When `tools` (the registered tool schemas) is provided, schema-declared
/// tagged argument envelopes are flattened and string values are coerced onto
/// unambiguous schema expectations — `"True"` → `true` for a bool param, a
/// JSON-array string → list for a list param. This is the single chokepoint
/// every dispatched call passes through, so it covers the native and text
/// channels alike.
pub(crate) fn normalize_tool_args(
    name: &str,
    args: serde_json::Value,
    tools: Option<&crate::value::VmValue>,
) -> serde_json::Value {
    let schema = super::collect_tool_schemas(tools, None)
        .into_iter()
        .find(|schema| schema.name == name);
    let args = super::compat::unwrap_single_key_discriminator_envelope(args, schema.as_ref());
    let mut obj = match args {
        serde_json::Value::Object(obj) => obj,
        other => return other,
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

    // Strip a leaked tool-call heredoc wrapper from any string argument that is
    // *entirely* a `<<TAG\n...\nTAG` heredoc. The model is taught the
    // `content: <<EOF\n...\nEOF` envelope, then sometimes delivers the value
    // through a channel that never ran the heredoc grammar — a native JSON
    // string `"<<EOF\n...\nEOF"`, or chat-template/DSML markup — so the opener
    // and closing sentinel leak verbatim into the written file (e.g. Zig:
    // `expected type expression, found '<<'`). This is the single chokepoint
    // every dispatched call passes through, so it covers native and text
    // channels alike; `unwrap_fully_wrapping_heredoc` is strict enough to leave
    // a value that merely contains `<<` (a shift operator, a real mid-file
    // `<<EOF`) byte-identical.
    for value in obj.values_mut() {
        strip_wrapping_heredoc_in_place(value);
    }

    let mut normalized = serde_json::Value::Object(obj);
    coerce_integer_like_tool_args(&mut normalized);
    super::compat::coerce_args_to_schema(normalized, schema.as_ref())
}

/// Recursively unwrap a leaked, fully-wrapping `<<TAG\n...\nTAG` heredoc from
/// every string leaf of a tool-argument value. Recurses into nested
/// objects/arrays so a batched `ops: [{ new_body: "<<EOF\n...\nEOF" }]` (the
/// shape weak models emit through the native/markup channels) is healed the same
/// way a top-level `content` is. Non-string, non-container leaves are untouched.
fn strip_wrapping_heredoc_in_place(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(unwrapped) = crate::llm::tools::unwrap_fully_wrapping_heredoc(text) {
                *text = unwrapped;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_wrapping_heredoc_in_place(item);
            }
        }
        serde_json::Value::Object(map) => {
            for nested in map.values_mut() {
                strip_wrapping_heredoc_in_place(nested);
            }
        }
        _ => {}
    }
}
