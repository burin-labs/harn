use crate::value::VmValue;

use super::{
    dict_get, json_to_vm, list_items, vm_to_json, with_session, HOST_SESSION_RECORD_ASSISTANT,
    HOST_SESSION_RECORD_TOOL_RESULTS,
};
use crate::llm::tools::{
    assistant_prose_block, build_assistant_response_message, render_canonical_call,
    text_tool_call_block,
};

pub(super) fn record_dispatch_receipt(
    session_id: &str,
    calls: Vec<serde_json::Value>,
    provider: String,
    model: String,
    tool_format: Option<String>,
) {
    let _ = with_session(session_id, HOST_SESSION_RECORD_ASSISTANT, |session| {
        session.tool_calls.extend(calls);
        if !provider.is_empty() {
            session.last_provider = Some(provider);
        }
        if !model.is_empty() {
            session.last_model = Some(model);
        }
        if let Some(format) = tool_format {
            session.last_tool_format = Some(format);
        }
        Ok(())
    });
}

pub(super) fn last_dispatch_receipt(session_id: &str) -> (String, String, Option<String>) {
    with_session(session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
        Ok((
            session.last_provider.clone().unwrap_or_default(),
            session.last_model.clone().unwrap_or_default(),
            session.last_tool_format.clone(),
        ))
    })
    .unwrap_or_default()
}

pub(super) fn effective_session_tool_format(
    provider: &str,
    model: &str,
    requested: &str,
) -> String {
    // Mock/replay fixtures deliberately bypass live capability admission so
    // they can exercise every wire shape against one deterministic provider.
    if crate::llm::providers::is_internal_simulator(provider) {
        return requested.to_string();
    }
    let caps = crate::llm::managed_supply::capabilities_for(provider, model);
    crate::llm::capabilities::validate_tool_format_with_caps(provider, model, requested, &caps)
        .effective
}

pub(super) fn effective_history_tool_format(
    llm_result: &VmValue,
    provider: &str,
    model: &str,
) -> String {
    if let Some(effective) = dict_get(llm_result, "_effective_tool_format")
        .map(|value| value.display())
        .filter(|format| !format.trim().is_empty())
    {
        return effective;
    }
    // Legacy/replay fixtures without private route metadata historically mean
    // native structured history. Live results carry the admitted format; the
    // capability decision still repairs a missing-field Fireworks surprise.
    let requested = dict_get(llm_result, "_agent_tool_format")
        .map(|value| value.display())
        .filter(|format| !format.trim().is_empty())
        .unwrap_or_else(|| "native".to_string());
    effective_session_tool_format(provider, model, &requested)
}

pub(super) fn supports_native_history(llm_result: &VmValue, provider: &str, model: &str) -> bool {
    effective_history_tool_format(llm_result, provider, model) == "native"
        && crate::llm::managed_supply::capabilities_for(provider, model).native_tools
}

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
    tool_format: &str,
) -> Option<String> {
    let mut parts = Vec::new();
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        parts.push(if tool_format == "json" {
            trimmed.to_string()
        } else {
            assistant_prose_block(trimmed)
        });
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
        parts.push(if tool_format == "json" {
            let call = serde_json::json!({"name": name, "args": args});
            format!(
                "```tool\n{}\n```",
                serde_json::to_string_pretty(&call)
                    .expect("JSON tool-history values must serialize")
            )
        } else {
            text_tool_call_block(&render_canonical_call(name, &args))
        });
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub(super) fn text_channel_provider_surprise_message(
    llm_result: &VmValue,
    provider: &str,
    model: &str,
    text: &str,
    tool_calls: &[serde_json::Value],
    thinking: Option<&str>,
) -> Option<VmValue> {
    let format = effective_history_tool_format(llm_result, provider, model);
    if tool_calls.is_empty()
        || crate::llm_config::tool_format_channel(&format)
            != Some(crate::llm_config::ToolFormatChannel::Text)
    {
        return None;
    }
    let history = canonical_text_history_for_tool_calls(text, tool_calls, &format)?;
    Some(json_to_vm(&build_assistant_response_message(
        &history,
        &[],
        &[],
        thinking,
    )))
}
