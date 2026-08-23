use super::handle_local::coerce_integer_like_tool_args;

/// Durable native call shared by every provider adapter.
///
/// Provider-specific continuation data has one explicit extension envelope;
/// arbitrary provider wire fields never become conversation fields.
#[derive(Debug, serde::Serialize)]
struct ConversationToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_metadata: Option<serde_json::Value>,
}

impl ConversationToolCall {
    fn from_result(value: &serde_json::Value) -> Self {
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = value
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let mut provider_metadata = value
            .get("provider_metadata")
            .filter(|metadata| metadata.is_object())
            .cloned();
        if let Some(signature) = crate::llm::providers::gemini_tool_call_thought_signature(value) {
            let metadata = provider_metadata.get_or_insert_with(|| serde_json::json!({}));
            metadata["gemini"]["thought_signature"] = serde_json::json!(signature);
        }
        Self {
            id,
            name,
            arguments,
            provider_metadata,
        }
    }
}

/// Build Harn's provider-neutral assistant tool-call message.
///
/// Durable conversation history never stores a provider wire shape. Every
/// provider adapter projects this `{id, name, arguments}` contract at egress.
pub(crate) fn build_assistant_tool_message(
    text: &str,
    tool_calls: &[serde_json::Value],
) -> serde_json::Value {
    let tool_calls = tool_calls
        .iter()
        .map(ConversationToolCall::from_result)
        .collect::<Vec<_>>();
    serde_json::json!({
        "role": "assistant",
        "content": text,
        "tool_calls": tool_calls,
    })
}

/// Build a durable assistant message for transcript/run-record storage.
/// Prefer canonical structured blocks when available so hosts can restore
/// richer assistant state without reparsing visible text.
pub(crate) fn build_assistant_response_message(
    text: &str,
    blocks: &[serde_json::Value],
    tool_calls: &[serde_json::Value],
    reasoning: Option<&str>,
) -> serde_json::Value {
    let portable_blocks = blocks
        .iter()
        .filter(|block| !crate::llm::reasoning_history::is_signed_anthropic_block(block))
        .cloned()
        .collect::<Vec<_>>();
    let mut message = if !tool_calls.is_empty() {
        build_assistant_tool_message(text, tool_calls)
    } else if !portable_blocks.is_empty() {
        serde_json::json!({
            "role": "assistant",
            "content": portable_blocks,
        })
    } else {
        serde_json::json!({
            "role": "assistant",
            "content": text,
        })
    };
    crate::llm::reasoning_history::attach_anthropic_continuation(&mut message, blocks);
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
