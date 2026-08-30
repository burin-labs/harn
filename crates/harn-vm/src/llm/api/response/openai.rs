//! OpenAI-family non-streaming response adapters.
//!
//! Both native Responses and Chat Completions share the provider's reasoning,
//! logprob, and tool-call normalization here. The parent module retains the
//! provider-neutral completion contract, usage receipt, and dialect dispatch.

use crate::llm::usage::ProviderUsageReceipt;
use crate::value::{VmError, VmValue};

use super::super::openai_normalize::{
    append_paragraph, extract_openai_choice_logprobs, normalize_openai_message_text,
    parse_openai_tool_argument_json_values, parse_tool_arguments, preview_chars,
};
use super::super::result::{LlmResult, RawProviderToolCall};
use super::super::telemetry::ProviderTelemetry;
use super::boundary;
use super::{
    billed_noncommittal_completion_error, empty_generation_error, extract_cache_read_tokens,
    extract_cache_write_tokens, is_billed_noncommittal_completion, is_length_stop_reason,
    openai_message_content_block_types, openai_responses_content_block_types,
    CompletionContractSignals, ProviderResponseEnvelope,
};
use item_kinds::{is_openai_responses_hosted_tool_item, openai_responses_tool_kind};

mod item_kinds;

fn render_reasoning_summary_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                append_paragraph(&mut out, &render_reasoning_summary_value(item));
            }
            out
        }
        serde_json::Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(|value| value.as_str()) {
                return text.trim().to_string();
            }
            if let Some(summary) = object.get("summary") {
                return render_reasoning_summary_value(summary);
            }
            if let Some(content) = object.get("content") {
                return render_reasoning_summary_value(content);
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn extract_openai_reasoning_summary(
    json: &serde_json::Value,
    message: &serde_json::Value,
) -> String {
    let mut summary = String::new();
    for value in [
        message.get("reasoning_summary"),
        message.get("thinking_summary"),
        message
            .get("reasoning")
            .and_then(|value| value.get("summary")),
        json.get("reasoning_summary"),
    ]
    .into_iter()
    .flatten()
    {
        append_paragraph(&mut summary, &render_reasoning_summary_value(value));
    }

    if let Some(output) = json.get("output").and_then(|value| value.as_array()) {
        for item in output {
            if item.get("type").and_then(|value| value.as_str()) == Some("reasoning") {
                if let Some(value) = item.get("summary") {
                    append_paragraph(&mut summary, &render_reasoning_summary_value(value));
                }
            }
        }
    }
    summary
}

fn parse_openai_tool_argument_values(tool_name: &str, args_str: &str) -> Vec<serde_json::Value> {
    parse_openai_tool_argument_json_values(args_str).unwrap_or_else(|json_error| {
        vec![
            crate::llm::tools::parse_text_tool_argument_payload(args_str, tool_name)
                .unwrap_or_else(|text_error| {
                    tool_argument_parse_error(args_str, json_error, &text_error)
                }),
        ]
    })
}

fn tool_argument_parse_error(
    args_str: &str,
    json_error: serde_json::Error,
    text_error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse tool arguments as JSON or Harn text-tool arguments: JSON error: {}; Harn text-tool error: {}. Raw input: {}",
            json_error,
            text_error,
            preview_chars(args_str, 200)
        )
    })
}

fn native_tool_name_text_call_parse_error(raw_name: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse provider tool name as Harn text-tool call: {}. Raw input: {}",
            error,
            preview_chars(raw_name, 200)
        )
    })
}

fn native_tool_arguments_text_call_parse_error(
    raw_arguments: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse provider tool arguments as Harn text-tool call: {}. Raw input: {}",
            error,
            preview_chars(raw_arguments, 200)
        )
    })
}

fn openai_synthetic_tool_call_id(base_id: &str, call_index: usize, arg_index: usize) -> String {
    if base_id.is_empty() {
        format!("call_{call_index}_{}", arg_index + 1)
    } else if arg_index == 0 {
        base_id.to_string()
    } else {
        format!("{base_id}_{}", arg_index + 1)
    }
}

fn push_internal_tool_call(
    tool_calls: &mut Vec<serde_json::Value>,
    blocks: &mut Vec<serde_json::Value>,
    id: String,
    name: String,
    arguments: serde_json::Value,
) {
    tool_calls.push(serde_json::json!({
        "id": id,
        "name": name,
        "arguments": arguments,
    }));
    blocks.push(serde_json::json!({
        "type": "tool_call",
        "id": id,
        "name": name,
        "arguments": arguments,
        "visibility": "internal",
    }));
}

fn push_openai_responses_text_block(
    content: &serde_json::Value,
    text: &mut String,
    blocks: &mut Vec<serde_json::Value>,
) {
    let block_type = content.get("type").and_then(|value| value.as_str());
    let Some(value) = content
        .get("text")
        .or_else(|| content.get("content"))
        .and_then(|value| value.as_str())
    else {
        return boundary::unreadable_message_part(block_type, content);
    };
    match block_type {
        Some("output_text") | Some("text") | None => {
            text.push_str(value);
            blocks.push(serde_json::json!({
                "type": "output_text",
                "text": value,
                "visibility": "public",
            }));
        }
        Some("refusal") | Some("output_refusal") => {
            text.push_str(value);
            blocks.push(serde_json::json!({
                "type": "refusal",
                "text": value,
                "visibility": "public",
            }));
        }
        other => boundary::unhandled_message_part(other, content),
    }
}

fn push_openai_responses_hosted_tool_block(
    item: &serde_json::Value,
    item_type: &str,
    blocks: &mut Vec<serde_json::Value>,
) {
    let id = item
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let call_id = item
        .get("call_id")
        .and_then(|value| value.as_str())
        .unwrap_or(id);
    let name = item
        .get("name")
        .or_else(|| item.get("server_label"))
        .or_else(|| item.get("tool_name"))
        .and_then(|value| value.as_str())
        .unwrap_or(item_type);
    let status = item
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let mut block = serde_json::json!({
        "type": "provider_tool_call",
        "id": if call_id.is_empty() { id } else { call_id },
        "provider_tool_id": id,
        "call_id": call_id,
        "name": name,
        "provider_tool_type": item_type,
        "tool_kind": openai_responses_tool_kind(item_type),
        "executor": "provider_native",
        "status": status,
        "visibility": "internal",
        "provider_metadata": item,
    });
    // Computer use: promote the native `action`, `call_id`, and any
    // `pending_safety_checks` to top-level fields so the tool-call path can
    // lower the action and surface the safety prompt without digging into
    // `provider_metadata`. The neutral `computer` tool then executes via
    // `hostlib_computer_execute`.
    //
    // INTEGRATION SEAM — safety-ack echo: on a `safety_ack_flow` route the
    // orchestrator must, after the user/policy approves, echo these ids back on
    // the follow-up `computer_call_output` as `acknowledged_safety_checks`
    // (built in the Responses request assembly). That echo is not yet wired;
    // the ids are surfaced here so the approval + echo can be added without
    // re-parsing the response.
    if item_type == "computer_call" {
        if let Some(action) = item.get("action") {
            block["action"] = action.clone();
        }
        if let Some(checks) = item.get("pending_safety_checks") {
            block["pending_safety_checks"] = checks.clone();
        }
    }
    blocks.push(block);
}

/// Parse OpenAI's native Responses API output into Harn's normal result shape.
pub(crate) fn parse_openai_responses_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
) -> Result<LlmResult, VmError> {
    if let Some(err) = json["error"]["message"].as_str() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{provider} API error: {err}"),
        ))));
    }

    let output = json
        .get("output")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "{provider} Responses API response missing output array"
            ))))
        })?;

    let mut text = String::new();
    let mut thinking_summary = String::new();
    let mut tool_calls = Vec::new();
    let mut raw_tool_calls = Vec::new();
    let mut blocks = Vec::new();

    for item in output {
        let item_type = item
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match item_type {
            "message" => {
                if let Some(content) = item.get("content").and_then(|value| value.as_array()) {
                    for content in content {
                        push_openai_responses_text_block(content, &mut text, &mut blocks);
                    }
                }
            }
            "reasoning" => {
                if let Some(summary) = item.get("summary") {
                    let rendered = render_reasoning_summary_value(summary);
                    append_paragraph(&mut thinking_summary, &rendered);
                    if !rendered.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "reasoning_summary",
                            "text": rendered,
                            "provider_id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                            "visibility": "private",
                        }));
                    }
                }
            }
            "function_call" => {
                raw_tool_calls.push(RawProviderToolCall::new(item.clone()).map_err(|error| {
                    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error)))
                })?);
                let provider_id = item
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let id = item
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or(provider_id)
                    .to_string();
                let raw_name = item
                    .get("name")
                    .or_else(|| item.get("function").and_then(|value| value.get("name")))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = parse_tool_arguments(item.get("arguments").or_else(|| {
                    item.get("function")
                        .and_then(|value| value.get("arguments"))
                }));
                let (name, arguments) =
                    crate::llm::tools::normalize_tool_call_shape(&raw_name, arguments);
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "provider_id": provider_id,
                    "name": name,
                    "arguments": arguments,
                }));
                blocks.push(serde_json::json!({
                    "type": "tool_call",
                    "id": id,
                    "provider_id": provider_id,
                    "name": name,
                    "arguments": arguments,
                    "visibility": "internal",
                }));
            }
            "tool_search_call" => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let query = item
                    .get("query")
                    .or_else(|| item.get("input"))
                    .or_else(|| item.get("action").and_then(|action| action.get("query")))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                blocks.push(serde_json::json!({
                    "type": "tool_search_query",
                    "id": id,
                    "provider_tool_id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "name": "tool_search",
                    "query": query,
                    "executor": "provider_native",
                    "visibility": "internal",
                }));
                push_openai_responses_hosted_tool_block(item, item_type, &mut blocks);
            }
            "tool_search_output" => {
                let tool_use_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let references = item
                    .get("tool_references")
                    .or_else(|| item.get("results"))
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                blocks.push(serde_json::json!({
                    "type": "tool_search_result",
                    "tool_use_id": tool_use_id,
                    "tool_references": references,
                    "executor": "provider_native",
                    "visibility": "internal",
                }));
                push_openai_responses_hosted_tool_block(item, item_type, &mut blocks);
            }
            "compaction" => {
                let id = item.get("id").cloned().unwrap_or(serde_json::Value::Null);
                blocks.push(serde_json::json!({
                    "type": "compaction",
                    "id": id.clone(),
                    "provider_id": id,
                    "encrypted_content": item.get("encrypted_content").cloned().unwrap_or(serde_json::Value::Null),
                    "visibility": "private",
                    "provider_metadata": item,
                }));
            }
            other if is_openai_responses_hosted_tool_item(other) => {
                push_openai_responses_hosted_tool_block(item, other, &mut blocks);
            }
            other => boundary::unhandled_output_item(other, item),
        }
    }

    let usage = &json["usage"];
    let reported_input_tokens = usage["input_tokens"]
        .as_i64()
        .or_else(|| usage["prompt_tokens"].as_i64());
    let reported_output_tokens = usage["output_tokens"]
        .as_i64()
        .or_else(|| usage["completion_tokens"].as_i64());
    let input_tokens = reported_input_tokens.unwrap_or(0);
    let output_tokens = reported_output_tokens.unwrap_or(0);
    let cache_read_tokens = extract_cache_read_tokens(usage);
    let cache_write_tokens = extract_cache_write_tokens(usage);
    let request_id = json["id"].as_str().filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_openai_response(json, request_id);
    let served_fast = crate::llm::serving_tiers::served_fast(model, json);
    let provider_usage = ProviderUsageReceipt::new(
        reported_input_tokens,
        reported_output_tokens,
        telemetry.provider_cost_usd,
        served_fast,
    )
    .with_cache(
        cache_read_tokens,
        cache_write_tokens,
        telemetry.cache_accounting_declared,
        true,
    );
    // `status: "incomplete"` says only that generation stopped early. Prefer
    // its nested reason and retain status as the completed-response fallback.
    let stop_reason = json
        .get("incomplete_details")
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
        .or_else(|| json["status"].as_str())
        .map(str::to_string);
    let has_blocks = !blocks.is_empty();
    if text.is_empty() && thinking_summary.is_empty() && tool_calls.is_empty() && !has_blocks {
        return Err(empty_generation_error(
            provider,
            model,
            ProviderResponseEnvelope::new(
                request_id,
                stop_reason.as_deref(),
                openai_responses_content_block_types(output),
                provider_usage,
            ),
            format!(
                "openai Responses model {model} delivered no content, reasoning, or tool calls"
            ),
        ));
    }

    Ok(LlmResult {
        attempts: Default::default(),
        text_projection: None,
        text,
        tool_calls,
        raw_tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_supported: true,
        model: model.to_string(),
        provider: provider.to_string(),
        thinking: None,
        thinking_summary: if thinking_summary.is_empty() {
            None
        } else {
            Some(thinking_summary)
        },
        stop_reason,
        served_fast,
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}

pub(super) fn parse_chat_completions_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    tools_offered: bool,
) -> Result<LlmResult, VmError> {
    if let Some(err) = json["error"]["message"].as_str() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{provider} API error: {err}"),
        ))));
    }

    let choices = json
        .get("choices")
        .and_then(|value| value.as_array())
        .filter(|choices| !choices.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "{provider} API response missing non-empty choices array"
            ))))
        })?;
    boundary::discarded_choices(provider, choices);
    let choice = &choices[0];
    let message = choice.get("message").ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "{provider} API response missing choices[0].message"
        ))))
    })?;
    let finish_reason = choice["finish_reason"].as_str();
    let caps = crate::llm::managed_supply::capabilities_for(provider, model);
    let promote_reasoning_to_text = caps.reasoning_text_promotable && !tools_offered;
    let (text, extracted_thinking) =
        normalize_openai_message_text(message, finish_reason, promote_reasoning_to_text);
    let reasoning_summary = extract_openai_reasoning_summary(json, message);
    let mut blocks = if text.is_empty() {
        Vec::new()
    } else {
        vec![serde_json::json!({"type": "output_text", "text": text, "visibility": "public"})]
    };
    if !extracted_thinking.is_empty() {
        blocks.insert(
            0,
            serde_json::json!({
                "type": "reasoning",
                "text": extracted_thinking,
                "visibility": "private",
            }),
        );
    }
    if !reasoning_summary.is_empty() {
        blocks.push(serde_json::json!({
            "type": "reasoning_summary",
            "text": reasoning_summary,
            "visibility": "private",
        }));
    }

    let mut tool_calls = Vec::new();
    let raw_tool_calls = RawProviderToolCall::array_from_value(&message["tool_calls"])
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    if let Some(calls) = message["tool_calls"].as_array() {
        for (call_index, call) in calls.iter().enumerate() {
            // OpenAI Responses-API tool_search (harn#71) emits
            // `tool_search_call` blocks when the server-hosted
            // search runs. These are NOT dispatchable tools — the
            // server executes them for us — so we record the query
            // as a transcript event and continue without touching
            // tool_calls. `tool_search_output` blocks on the
            // response carry server results and are recorded
            // symmetrically.
            let call_type = call["type"].as_str().unwrap_or("");
            if call_type == "tool_search_call" {
                let id = call["id"].as_str().unwrap_or("").to_string();
                let query = call.get("query").cloned().unwrap_or_else(|| {
                    call.get("input")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                });
                blocks.push(serde_json::json!({
                    "type": "tool_search_query",
                    "id": id,
                    "name": "tool_search",
                    "query": query,
                    "visibility": "internal",
                }));
                continue;
            }
            if call_type == "tool_search_output" {
                let tool_use_id = call["call_id"]
                    .as_str()
                    .or_else(|| call["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let references = call["tool_references"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                blocks.push(serde_json::json!({
                    "type": "tool_search_result",
                    "tool_use_id": tool_use_id,
                    "tool_references": references,
                    "visibility": "internal",
                }));
                continue;
            }
            let raw_name = call["function"]["name"].as_str().unwrap_or("").to_string();
            let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
            let base_id = call["id"].as_str().unwrap_or("");
            match crate::llm::tools::parse_text_tool_call_from_native_name(&raw_name) {
                crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
                    let (name, arguments) =
                        crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                    let id = openai_synthetic_tool_call_id(base_id, call_index, 0);
                    push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                    continue;
                }
                crate::llm::tools::NativeToolNameTextCall::Malformed { name, error } => {
                    let arguments = native_tool_name_text_call_parse_error(&raw_name, &error);
                    let (name, arguments) =
                        crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                    let id = openai_synthetic_tool_call_id(base_id, call_index, 0);
                    push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                    continue;
                }
                crate::llm::tools::NativeToolNameTextCall::NotCall => {}
            }
            if crate::llm::tools::is_generic_wrapper_name(&raw_name) {
                match crate::llm::tools::parse_text_tool_call_from_native_arguments(args_str) {
                    crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
                        let (name, arguments) =
                            crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                        let id = openai_synthetic_tool_call_id(base_id, call_index, 0);
                        push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                        continue;
                    }
                    crate::llm::tools::NativeToolNameTextCall::Malformed { name, error } => {
                        let arguments =
                            native_tool_arguments_text_call_parse_error(args_str, &error);
                        let (name, arguments) =
                            crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                        let id = openai_synthetic_tool_call_id(base_id, call_index, 0);
                        push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                        continue;
                    }
                    crate::llm::tools::NativeToolNameTextCall::NotCall => {}
                }
            }
            for (arg_index, arguments) in parse_openai_tool_argument_values(&raw_name, args_str)
                .into_iter()
                .enumerate()
            {
                let (name, arguments) =
                    crate::llm::tools::normalize_tool_call_shape(&raw_name, arguments);
                let id = openai_synthetic_tool_call_id(base_id, call_index, arg_index);
                push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
            }
        }
    }

    let reported_input_tokens = json["usage"]["prompt_tokens"].as_i64();
    let reported_output_tokens = json["usage"]["completion_tokens"].as_i64();
    let input_tokens = reported_input_tokens.unwrap_or(0);
    let output_tokens = reported_output_tokens.unwrap_or(0);
    let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
    let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
    let stop_reason = finish_reason.map(|s| s.to_string());
    let request_id = json["id"].as_str().filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_openai_response(json, request_id);
    let served_fast = crate::llm::serving_tiers::served_fast(model, json);
    let provider_usage = ProviderUsageReceipt::new(
        reported_input_tokens,
        reported_output_tokens,
        telemetry.provider_cost_usd,
        served_fast,
    )
    .with_cache(
        cache_read_tokens,
        cache_write_tokens,
        telemetry.cache_accounting_declared,
        true,
    );
    let billed_length_truncation =
        is_length_stop_reason(stop_reason.as_deref()) && output_tokens > 0;

    // OpenAI Responses-API `tool_search_call` / `tool_search_output`
    // blocks (harn#71) are server-executed and get stripped from
    // `tool_calls` during parsing; they show up only as transcript
    // blocks. Count their presence as "did deliver something" so
    // the empty-response error below doesn't trip when the
    // server's response consisted entirely of a search
    // query/result exchange. Also let billed length-truncated turns
    // through so the agent loop can raise max_tokens and continue them:
    // hidden reasoning can consume the whole cap without leaving a
    // provider-visible reasoning string to preserve here.
    let has_tool_search_block = blocks.iter().any(|b| {
        matches!(
            b.get("type").and_then(|v| v.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    if text.is_empty()
        && extracted_thinking.is_empty()
        && reasoning_summary.is_empty()
        && tool_calls.is_empty()
        && !has_tool_search_block
        && !billed_length_truncation
    {
        return Err(empty_generation_error(
            provider,
            model,
            ProviderResponseEnvelope::new(
                request_id,
                stop_reason.as_deref(),
                openai_message_content_block_types(message),
                provider_usage,
            ),
            format!(
                "openai-compatible model {model} delivered no content, reasoning, or tool calls"
            ),
        ));
    }
    // Reject billed tool-offered completions with neither text nor a tool call.
    if is_billed_noncommittal_completion(&CompletionContractSignals {
        stop_reason: stop_reason.as_deref(),
        output_tokens,
        tools_offered,
        tool_call_count: tool_calls.len(),
        has_tool_search_block,
        text: &text,
    }) {
        return Err(billed_noncommittal_completion_error(
            provider,
            model,
            provider_usage,
        ));
    }

    Ok(LlmResult {
        attempts: Default::default(),
        text_projection: None,
        text,
        tool_calls,
        raw_tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_supported: true,
        model: model.to_string(),
        provider: provider.to_string(),
        thinking: if extracted_thinking.is_empty() {
            None
        } else {
            Some(extracted_thinking)
        },
        thinking_summary: if reasoning_summary.is_empty() {
            None
        } else {
            Some(reasoning_summary)
        },
        stop_reason,
        served_fast,
        blocks,
        logprobs: extract_openai_choice_logprobs(choice),
        telemetry,
    })
}
