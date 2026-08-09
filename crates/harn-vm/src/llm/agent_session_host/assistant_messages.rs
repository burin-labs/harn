use crate::value::VmValue;

use super::{dict_get, list_items, vm_to_json};
use crate::llm::tools::{assistant_prose_block, render_canonical_call, text_tool_call_block};

pub(super) fn durable_anthropic_blocks(
    llm_result: &VmValue,
    provider: &str,
    model: &str,
) -> Vec<serde_json::Value> {
    let blocks = dict_get(llm_result, "blocks")
        .map(list_items)
        .unwrap_or_default()
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    crate::llm::reasoning_history::capture_anthropic_blocks(
        &blocks,
        crate::llm::provider::provider_uses_anthropic_messages(provider, model),
    )
}

pub(super) fn canonical_text_history_for_tool_calls(
    text: &str,
    tool_calls: &[serde_json::Value],
) -> Option<String> {
    let mut parts = Vec::new();
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        parts.push(assistant_prose_block(trimmed));
    }
    for call in tool_calls {
        let name = call
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let args = call
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        parts.push(text_tool_call_block(&render_canonical_call(name, &args)));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}
