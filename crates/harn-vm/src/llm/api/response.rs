//! Non-streaming LLM response parsing. Covers Anthropic's `content`-array
//! shape and the OpenAI-compatible `choices[0].message` shape; streaming
//! variants live in [`super::transport`].

use crate::value::{VmError, VmValue};

use super::openai_normalize::{append_paragraph, normalize_openai_message_text};
use super::result::LlmResult;
use super::telemetry::ProviderTelemetry;

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

fn normalize_top_logprobs(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .filter_map(|item| {
                    let logprob = item.get("logprob").and_then(|value| value.as_f64())?;
                    Some(serde_json::json!({
                        "token": item.get("token").and_then(|value| value.as_str()).unwrap_or(""),
                        "logprob": logprob,
                        "bytes": item.get("bytes").cloned().unwrap_or(serde_json::Value::Null),
                    }))
                })
                .collect(),
        ),
        serde_json::Value::Object(object) => serde_json::Value::Array(
            object
                .iter()
                .filter_map(|(token, item)| {
                    let logprob = if let Some(logprob) = item.as_f64() {
                        logprob
                    } else {
                        item.get("logprob").and_then(|value| value.as_f64())?
                    };
                    let bytes = item
                        .get("bytes")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    Some(serde_json::json!({
                        "token": token,
                        "logprob": logprob,
                        "bytes": bytes,
                    }))
                })
                .collect(),
        ),
        _ => serde_json::Value::Array(Vec::new()),
    }
}

fn normalize_logprob_entry(
    token: &str,
    logprob: f64,
    bytes: serde_json::Value,
    top_logprobs: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "token": token,
        "logprob": logprob,
        "bytes": bytes,
        "top_logprobs": top_logprobs,
    })
}

pub(super) fn extract_openai_choice_logprobs(choice: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(content) = choice
        .get("logprobs")
        .and_then(|value| value.get("content"))
        .and_then(|value| value.as_array())
    {
        return content
            .iter()
            .filter_map(|item| {
                let logprob = item.get("logprob").and_then(|value| value.as_f64())?;
                let token = item
                    .get("token")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                Some(normalize_logprob_entry(
                    token,
                    logprob,
                    item.get("bytes")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    normalize_top_logprobs(
                        item.get("top_logprobs").unwrap_or(&serde_json::Value::Null),
                    ),
                ))
            })
            .collect();
    }

    let Some(logprobs) = choice.get("logprobs") else {
        return Vec::new();
    };
    let Some(tokens) = logprobs.get("tokens").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let token_logprobs = logprobs
        .get("token_logprobs")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let top_logprobs = logprobs
        .get("top_logprobs")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    tokens
        .iter()
        .enumerate()
        .filter_map(|(idx, token)| {
            let token = token.as_str().unwrap_or("");
            let logprob = token_logprobs.get(idx).and_then(|value| value.as_f64())?;
            Some(normalize_logprob_entry(
                token,
                logprob,
                serde_json::Value::Null,
                normalize_top_logprobs(
                    top_logprobs
                        .get(idx)
                        .unwrap_or(&serde_json::Value::Array(Vec::new())),
                ),
            ))
        })
        .collect()
}

/// Char-boundary-safe preview of the first `max_chars` characters of `s`.
///
/// `&s[..s.len().min(N)]` panics when byte index `N` lands mid-UTF8-codepoint
/// — which happens whenever a model emits malformed tool-argument JSON that
/// contains multibyte characters straddling the cut. These previews only feed a
/// `__parse_error` message, so a panic here would crash response parsing for an
/// otherwise-recoverable error. Slicing by chars is always boundary-safe.
fn preview_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn parse_tool_arguments(arguments: Option<&serde_json::Value>) -> serde_json::Value {
    match arguments {
        Some(serde_json::Value::String(text)) => serde_json::from_str(text).unwrap_or_else(|err| {
            serde_json::json!({
                "__parse_error": format!(
                    "Could not parse tool arguments as JSON: {}. Raw input: {}",
                    err,
                    preview_chars(text, 200)
                )
            })
        }),
        Some(value) => value.clone(),
        None => serde_json::json!({}),
    }
}

pub(super) fn parse_openai_tool_argument_json_values(
    args_str: &str,
) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    match serde_json::from_str::<serde_json::Value>(args_str) {
        Ok(value) => Ok(vec![value]),
        Err(first_error) => {
            let mut values = Vec::new();
            for parsed in
                serde_json::Deserializer::from_str(args_str).into_iter::<serde_json::Value>()
            {
                match parsed {
                    Ok(value) => values.push(value),
                    Err(_) => {
                        return Err(first_error);
                    }
                }
            }
            if values.len() > 1
                && values
                    .iter()
                    .all(|value| matches!(value, serde_json::Value::Object(_)))
            {
                return Ok(values);
            }
            Err(first_error)
        }
    }
}

fn parse_openai_tool_argument_values(args_str: &str) -> Vec<serde_json::Value> {
    parse_openai_tool_argument_json_values(args_str)
        .unwrap_or_else(|err| vec![tool_argument_parse_error(args_str, err)])
}

fn tool_argument_parse_error(args_str: &str, error: serde_json::Error) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse tool arguments as JSON: {}. Raw input: {}",
            error,
            preview_chars(args_str, 200)
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

fn openai_responses_tool_kind(item_type: &str) -> &'static str {
    match item_type {
        "web_search_call" => "web_search",
        "file_search_call" => "file_search",
        "code_interpreter_call" => "code_interpreter",
        "computer_call" => "computer_use",
        "image_generation_call" => "image_generation",
        "tool_search_call" | "tool_search_output" => "tool_search",
        _ if item_type.starts_with("mcp_") => "remote_mcp",
        _ => "hosted_tool",
    }
}

fn is_openai_responses_hosted_tool_item(item_type: &str) -> bool {
    item_type.ends_with("_call")
        || item_type == "tool_search_output"
        || item_type == "mcp_list_tools"
        || item_type == "mcp_approval_request"
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
        return;
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
        _ => {}
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
    blocks.push(serde_json::json!({
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
    }));
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
            _ => {}
        }
    }

    let has_blocks = !blocks.is_empty();
    if text.is_empty() && thinking_summary.is_empty() && tool_calls.is_empty() && !has_blocks {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "openai Responses model {model} delivered no content, reasoning, or tool calls"
            ),
        ))));
    }

    let usage = &json["usage"];
    let input_tokens = usage["input_tokens"]
        .as_i64()
        .or_else(|| usage["prompt_tokens"].as_i64())
        .unwrap_or(0);
    let output_tokens = usage["output_tokens"]
        .as_i64()
        .or_else(|| usage["completion_tokens"].as_i64())
        .unwrap_or(0);
    let cache_read_tokens = extract_cache_read_tokens(usage);
    let cache_write_tokens = extract_cache_write_tokens(usage);
    let stop_reason = json["status"]
        .as_str()
        .or_else(|| {
            json.get("incomplete_details")
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str())
        })
        .map(str::to_string);
    let request_id = json["id"].as_str().filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_openai_usage(usage, request_id);

    Ok(LlmResult {
        text,
        tool_calls,
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
        served_fast: crate::llm::fast_mode::served_fast(model, json),
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}

/// Structural signals describing a finished LLM turn, used by
/// [`is_billed_noncommittal_completion`]. All fields are derived from the
/// parsed response plus the outbound request; none involve model-name or
/// provider-name branching.
pub(crate) struct CompletionContractSignals<'a> {
    /// The provider-reported finish/stop reason, if any.
    pub stop_reason: Option<&'a str>,
    /// Billed completion/output tokens for this turn.
    pub output_tokens: i64,
    /// Whether the outbound request offered any tools to the model.
    pub tools_offered: bool,
    /// Number of dispatchable tool calls captured from the response.
    pub tool_call_count: usize,
    /// Whether the response carried a server-side tool-search block (which
    /// counts as the model "doing something" even with no committed text).
    pub has_tool_search_block: bool,
    /// The committed visible answer text.
    pub text: &'a str,
}

/// Deterministic detector for a finished-clean LLM turn that billed output
/// but delivered neither a dispatchable tool call nor a committed answer — an
/// upstream contract violation (the model serialized its action only onto the
/// reasoning channel, or onto no wire field at all).
///
/// Returns `true` only when ALL hold:
/// - the turn finished cleanly (stop reason is `stop` / `tool_calls` /
///   `end_turn` / absent — NOT `length`, so genuine truncation never misfires),
/// - the provider billed output tokens (`output_tokens > 0`),
/// - tools were offered in the request (so a deliberate terse text answer to a
///   tool-less prompt is never flagged),
/// - no dispatchable tool call was captured,
/// - no server-side tool-search block was present, and
/// - no committed visible text was present.
///
/// Do not use a minimum visible-text length here. Agent loops often request a
/// terse token or sentinel answer after a tool result; rejecting those in the
/// parser masks successful native tool loops. Non-empty committed text belongs
/// to `agent_loop`, which can accept it, nudge for more work, or fail required
/// tool policy.
pub(crate) fn is_billed_noncommittal_completion(signals: &CompletionContractSignals) -> bool {
    let finished_clean = !is_length_stop_reason(signals.stop_reason);
    finished_clean
        && signals.output_tokens > 0
        && signals.tools_offered
        && signals.tool_call_count == 0
        && !signals.has_tool_search_block
        && signals.text.trim().is_empty()
}

fn is_length_stop_reason(stop_reason: Option<&str>) -> bool {
    stop_reason.is_some_and(|reason| {
        reason.eq_ignore_ascii_case("length") || reason.eq_ignore_ascii_case("max_tokens")
    })
}

/// Build the loud, actionable error returned when
/// [`is_billed_noncommittal_completion`] fires. Names the provider/model and
/// the structural facts so eval dashboards and operators can route around the
/// broken upstream instead of silently absorbing a billed no-op.
pub(crate) fn billed_noncommittal_completion_error(
    provider: &str,
    model: &str,
    output_tokens: i64,
) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "provider {provider} model {model} returned billed output \
         (completion_tokens={output_tokens}) with no dispatchable tool call or answer \
         (upstream contract violation): the model finished cleanly but committed neither a \
         tool call nor visible text. This usually means the route serialized \
         the action only in a private reasoning channel or returned an empty committed \
         message. For OpenRouter aggregate routes, consider provider_route_denylist or \
         provider_order; for first-party routes, prefer a Harn text/json tool format or \
         disable auto reasoning when the capability row documents it."
    ))))
}

/// Parse a complete (non-streaming) LLM JSON response into an `LlmResult`.
pub(crate) fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    is_anthropic_style: bool,
    tools_offered: bool,
) -> Result<LlmResult, VmError> {
    if provider == "openai"
        && json
            .get("output")
            .and_then(|value| value.as_array())
            .is_some()
    {
        return parse_openai_responses_response(json, provider, model);
    }

    if is_anthropic_style {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("{provider} API error: {err}"),
            ))));
        }

        let mut text = String::new();
        let mut thinking_text = String::new();
        let mut tool_calls = Vec::new();
        let mut blocks = Vec::new();

        let content = json
            .get("content")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "{provider} API response missing content array"
                ))))
            })?;
        for block in content {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        text.push_str(t);
                        blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block["thinking"].as_str() {
                        thinking_text.push_str(t);
                        blocks.push(serde_json::json!({"type": "reasoning", "text": t, "visibility": "private"}));
                    }
                }
                Some("tool_use") => {
                    let raw_name = block["name"].as_str().unwrap_or("").to_string();
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let input = block["input"].clone();
                    let (name, input) =
                        crate::llm::tools::normalize_tool_call_shape(&raw_name, input);
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "name": name,
                        "arguments": input,
                    }));
                    blocks.push(serde_json::json!({
                        "type": "tool_call",
                        "id": block["id"].clone(),
                        "name": name,
                        "arguments": input,
                        "visibility": "internal",
                    }));
                }
                Some("server_tool_use") => {
                    // Anthropic's server-side tool-search tool emits
                    // a `server_tool_use` content block when it
                    // queries. The model never sees this as a
                    // dispatchable tool — Anthropic executes it for
                    // us — so we record it for transcript/replay
                    // fidelity but do NOT add it to `tool_calls`.
                    blocks.push(serde_json::json!({
                        "type": "tool_search_query",
                        "id": block["id"].clone(),
                        "name": block["name"].clone(),
                        "query": block["input"].clone(),
                        "visibility": "internal",
                    }));
                }
                Some("tool_search_tool_result") => {
                    // Server-side search results. Anthropic
                    // auto-expands the referenced tools inline on
                    // subsequent turns; we just record the event so
                    // replay/eval can see which tools were promoted
                    // and when.
                    let references: Vec<serde_json::Value> = block["content"]["tool_references"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    blocks.push(serde_json::json!({
                        "type": "tool_search_result",
                        "tool_use_id": block["tool_use_id"].clone(),
                        "tool_references": references,
                        "visibility": "internal",
                    }));
                }
                _ => {}
            }
        }

        if text.is_empty() && thinking_text.is_empty() && tool_calls.is_empty() && blocks.is_empty()
        {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "anthropic-style model {model} delivered no content, reasoning, or tool calls"
                ),
            ))));
        }

        let input_tokens = json["usage"]["input_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["output_tokens"].as_i64().unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = json["stop_reason"].as_str().map(|s| s.to_string());
        let request_id = json["id"].as_str().filter(|value| !value.is_empty());
        let telemetry = ProviderTelemetry::from_anthropic_usage(&json["usage"], request_id);

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cache_supported: true,
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: if thinking_text.is_empty() {
                None
            } else {
                Some(thinking_text)
            },
            thinking_summary: None,
            stop_reason,
            served_fast: crate::llm::fast_mode::served_fast(model, json),
            blocks,
            logprobs: Vec::new(),
            telemetry,
        })
    } else {
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
        let choice = &choices[0];
        let message = choice.get("message").ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "{provider} API response missing choices[0].message"
            ))))
        })?;
        let finish_reason = choice["finish_reason"].as_str();
        let caps = crate::llm::capabilities::lookup(provider, model);
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
                for (arg_index, arguments) in parse_openai_tool_argument_values(args_str)
                    .into_iter()
                    .enumerate()
                {
                    let (name, arguments) =
                        crate::llm::tools::normalize_tool_call_shape(&raw_name, arguments);
                    let id = openai_synthetic_tool_call_id(base_id, call_index, arg_index);
                    tool_calls.push(serde_json::json!({
                        "id": id.clone(),
                        "name": name,
                        "arguments": arguments,
                    }));
                    blocks.push(serde_json::json!({
                        "type": "tool_call",
                        "id": id,
                        "name": name,
                        "arguments": arguments.clone(),
                        "visibility": "internal",
                    }));
                }
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = finish_reason.map(|s| s.to_string());
        let request_id = json["id"].as_str().filter(|value| !value.is_empty());
        let telemetry = ProviderTelemetry::from_openai_usage(&json["usage"], request_id);
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
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                "openai-compatible model {model} delivered no content, reasoning, or tool calls"
            ),
            ))));
        }
        // Deterministic upstream contract-violation backstop. A clean,
        // tool-offered completion that billed output but committed no visible
        // text and dispatched no tool call is a billed no-op: the structured
        // action went only to a hidden reasoning channel or nowhere.
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
                output_tokens,
            ));
        }

        Ok(LlmResult {
            text,
            tool_calls,
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
            served_fast: crate::llm::fast_mode::served_fast(model, json),
            blocks,
            logprobs: extract_openai_choice_logprobs(choice),
            telemetry,
        })
    }
}

/// Extract cache-read token count from a provider `usage` JSON value,
/// covering Anthropic, OpenAI (and OpenAI-compatibles), and OpenRouter
/// passthrough field shapes. Returns 0 when the provider doesn't report it.
pub(super) fn extract_cache_read_tokens(usage: &serde_json::Value) -> i64 {
    // Anthropic / OpenRouter passthrough: usage.cache_read_input_tokens
    if let Some(n) = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenAI (and vLLM/SGLang when configured): usage.prompt_tokens_details.cached_tokens
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter variants: cache_read_tokens / cached_prompt_tokens.
    if let Some(n) = usage.get("cache_read_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    if let Some(n) = usage.get("cached_prompt_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    // DeepSeek (and a few OpenRouter passthrough shapes):
    // usage.prompt_cache_hit_tokens. Falling through to 0 silently hides
    // genuine cache hits when this is the only field the provider sets.
    if let Some(n) = usage
        .get("prompt_cache_hit_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter `cache_discount` shape: `usage.cache.read_input_tokens`
    // (newer 2026-04 wire format their docs reference under "Caching →
    // Anthropic / Claude").
    if let Some(n) = usage
        .get("cache")
        .and_then(|d| d.get("read_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    0
}

/// Extract cache-write (creation) token count from a provider `usage` JSON.
/// Anthropic reports this at top level; OpenRouter/OpenAI-compatible
/// providers may nest it under `prompt_tokens_details`.
pub(super) fn extract_cache_write_tokens(usage: &serde_json::Value) -> i64 {
    if let Some(n) = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter newer `cache.write_input_tokens` shape — matches the
    // counterpart added to `extract_cache_read_tokens` above.
    if let Some(n) = usage
        .get("cache")
        .and_then(|d| d.get("write_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::{
        extract_cache_read_tokens, extract_cache_write_tokens, extract_openai_choice_logprobs,
        is_billed_noncommittal_completion, parse_llm_response, parse_openai_responses_response,
        parse_tool_arguments, preview_chars, CompletionContractSignals,
    };

    #[test]
    fn parse_tool_arguments_preview_does_not_panic_mid_utf8() {
        // 199 ASCII bytes + a 3-byte char means byte index 200 lands INSIDE the
        // multibyte char. The old `&text[..text.len().min(200)]` slice panicked
        // here; the char-safe preview must not. The body is invalid JSON so we
        // hit the __parse_error path that builds the preview.
        let mut malformed = "a".repeat(199);
        malformed.push('→'); // 3 bytes (E2 86 92), straddles byte 200
        malformed.push_str(" not json");
        let value = parse_tool_arguments(Some(&serde_json::Value::String(malformed.clone())));
        let preview = value["__parse_error"]
            .as_str()
            .expect("parse error preview");
        assert!(preview.contains("Could not parse tool arguments"));
    }

    #[test]
    fn preview_chars_is_char_boundary_safe_and_caps_chars() {
        let s = format!("{}é", "x".repeat(199)); // 199 ASCII + 2-byte char
        let out = preview_chars(&s, 200);
        assert_eq!(out.chars().count(), 200);
        // Multibyte content survives intact without a panic.
        assert!(preview_chars("→→→", 1).chars().count() <= 1);
    }

    #[test]
    fn contract_violation_fires_on_billed_noop_tool_turn() {
        // Hidden-action shape: clean `stop`, billed tokens, tools offered, no
        // visible text, no tool call, no tool-search.
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 342,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_normal_tool_call() {
        let signals = CompletionContractSignals {
            stop_reason: Some("tool_calls"),
            output_tokens: 800,
            tools_offered: true,
            tool_call_count: 2,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_committed_text_answer() {
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 120,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "pong:catalog-refresh",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_truncation() {
        // A `length`/truncation finish must never misfire even when short.
        let signals = CompletionContractSignals {
            stop_reason: Some("length"),
            output_tokens: 4096,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_max_tokens_truncation() {
        let signals = CompletionContractSignals {
            stop_reason: Some("max_tokens"),
            output_tokens: 4096,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_when_no_tools_offered() {
        // A deliberately terse text reply to a tool-less prompt is fine.
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 6,
            tools_offered: false,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "Yes.",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_tool_search_block() {
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 200,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: true,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn parse_llm_response_rejects_ambient_billed_noop() {
        // End-to-end through the openai-compat parser: clean stop, billed
        // hidden reasoning, no visible content, and empty tool_calls must
        // surface a loud contract-violation error rather than a silent empty
        // success.
        let response = serde_json::json!({
            "id": "gen-ambient",
            "model": "qwen/qwen3.6-35b-a3b",
            "provider": "Ambient",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "creating files"
                }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 342 }
        });
        let err = parse_llm_response(&response, "openrouter", "qwen/qwen3.6-35b-a3b", false, true)
            .expect_err("billed no-op tool turn must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("upstream contract violation"),
            "error must name the contract violation: {message}"
        );
    }

    #[test]
    fn parse_llm_response_allows_billed_empty_length_truncation() {
        // Some reasoning routes consume the output cap in a hidden channel and
        // return no visible content or reasoning string, only a length stop and
        // billed completion tokens. The parser must hand this shape to the
        // agent loop so it can auto-continue with a raised cap.
        let response = serde_json::json!({
            "id": "gen-hidden-truncated",
            "choices": [{
                "index": 0,
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": ""
                }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 342 }
        });
        let result = parse_llm_response(&response, "openrouter", "hidden-reasoning", false, true)
            .expect("billed length truncation is recoverable");

        assert_eq!(result.text, "");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.output_tokens, 342);
        assert_eq!(result.stop_reason.as_deref(), Some("length"));
        assert_eq!(result.thinking, None);
        assert_eq!(result.thinking_summary, None);
    }

    #[test]
    fn parse_llm_response_allows_short_answer_when_no_tools_offered() {
        // Same short content, but no tools were offered: this is a legitimate
        // terse answer and must parse cleanly.
        let response = serde_json::json!({
            "id": "gen-terse",
            "model": "qwen/qwen3.6-35b-a3b",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "creating files\n\n" }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 6 }
        });
        let result = parse_llm_response(
            &response,
            "openrouter",
            "qwen/qwen3.6-35b-a3b",
            false,
            false,
        )
        .expect("short answer with no tools offered parses cleanly");
        assert_eq!(result.text.trim(), "creating files");
    }

    #[test]
    fn parse_llm_response_allows_short_committed_answer_when_tools_were_offered() {
        // Agent loops often request a terse final token after a tool result.
        // The parser must not guess that a short visible answer is a no-op.
        let response = serde_json::json!({
            "id": "gen-terse-tool-answer",
            "model": "claude-haiku-4-5-20251001",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "pong:catalog-refresh" }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 9 }
        });
        let result = parse_llm_response(
            &response,
            "anthropic",
            "claude-haiku-4-5-20251001",
            false,
            true,
        )
        .expect("short committed answer with tools offered parses cleanly");
        assert_eq!(result.text.trim(), "pong:catalog-refresh");
    }

    #[test]
    fn cache_write_tokens_supports_openrouter_prompt_details_shape() {
        let usage = serde_json::json!({
            "prompt_tokens": 194,
            "completion_tokens": 2,
            "prompt_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 100
            }
        });

        assert_eq!(extract_cache_write_tokens(&usage), 100);
    }

    #[test]
    fn cache_tokens_support_openai_responses_details_shape() {
        let usage = serde_json::json!({
            "input_tokens": 194,
            "output_tokens": 2,
            "input_tokens_details": {
                "cached_tokens": 120,
                "cache_creation_input_tokens": 40
            }
        });

        assert_eq!(extract_cache_read_tokens(&usage), 120);
        assert_eq!(extract_cache_write_tokens(&usage), 40);
    }

    #[test]
    fn cache_tokens_support_deepseek_prompt_cache_hit_field() {
        // DeepSeek (and some OpenRouter passthrough shapes for it) reports
        // cache hits as `prompt_cache_hit_tokens` instead of the
        // Anthropic-style top-level or OpenAI-style nested field. Without
        // this row a real cache hit reads as 0 (harn#2320).
        let usage = serde_json::json!({
            "prompt_tokens": 9100,
            "completion_tokens": 42,
            "prompt_cache_hit_tokens": 8800
        });
        assert_eq!(extract_cache_read_tokens(&usage), 8800);
    }

    #[test]
    fn cache_tokens_support_openrouter_cache_subobject_shape() {
        // OpenRouter's newer 2026-04 "Caching → Anthropic" wire shape
        // surfaces cache attribution under a `cache` sub-object instead
        // of mirroring Anthropic's top-level fields verbatim.
        let usage = serde_json::json!({
            "prompt_tokens": 9100,
            "completion_tokens": 42,
            "cache": {
                "read_input_tokens": 8800,
                "write_input_tokens": 220
            }
        });
        assert_eq!(extract_cache_read_tokens(&usage), 8800);
        assert_eq!(extract_cache_write_tokens(&usage), 220);
    }

    #[test]
    fn parses_openai_responses_structured_output() {
        let json = serde_json::json!({
            "id": "resp_123",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg_123",
                "content": [{
                    "type": "output_text",
                    "text": "{\"ok\":true}"
                }]
            }],
            "usage": {
                "input_tokens": 9,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": 3}
            }
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert_eq!(result.text, "{\"ok\":true}");
        assert_eq!(result.input_tokens, 9);
        assert_eq!(result.output_tokens, 5);
        assert_eq!(result.cache_read_tokens, 3);
        assert_eq!(result.telemetry.request_id.as_deref(), Some("resp_123"));
        assert_eq!(result.blocks[0]["type"], "output_text");
    }

    #[test]
    fn parses_openai_responses_hosted_tool_metadata() {
        let json = serde_json::json!({
            "id": "resp_456",
            "status": "completed",
            "output": [{
                "type": "web_search_call",
                "id": "ws_123",
                "call_id": "call_ws_123",
                "status": "completed",
                "action": {"query": "Harn orchestration"}
            }],
            "usage": {"input_tokens": 4, "output_tokens": 1}
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert!(result.tool_calls.is_empty());
        let block = &result.blocks[0];
        assert_eq!(block["type"], "provider_tool_call");
        assert_eq!(block["provider_tool_id"], "ws_123");
        assert_eq!(block["call_id"], "call_ws_123");
        assert_eq!(block["provider_tool_type"], "web_search_call");
        assert_eq!(block["tool_kind"], "web_search");
        assert_eq!(block["executor"], "provider_native");
        assert_eq!(
            block["provider_metadata"]["action"]["query"],
            "Harn orchestration"
        );
    }

    #[test]
    fn parses_openai_responses_compaction_metadata() {
        let json = serde_json::json!({
            "id": "resp_compact",
            "status": "completed",
            "output": [{
                "type": "compaction",
                "id": "cmp_123",
                "encrypted_content": "opaque-state"
            }],
            "usage": {"input_tokens": 20, "output_tokens": 0}
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert!(result.text.is_empty());
        let block = &result.blocks[0];
        assert_eq!(block["type"], "compaction");
        assert_eq!(block["provider_id"], "cmp_123");
        assert_eq!(block["encrypted_content"], "opaque-state");
        assert_eq!(block["visibility"], "private");
    }

    #[test]
    fn extracts_chat_completion_logprobs() {
        let choice = serde_json::json!({
            "logprobs": {
                "content": [
                    {
                        "token": "safe",
                        "logprob": -0.1,
                        "bytes": [115, 97, 102, 101],
                        "top_logprobs": [
                            {"token": "safe", "logprob": -0.1},
                            {"token": "risky", "logprob": -2.4}
                        ]
                    }
                ]
            }
        });

        let logprobs = extract_openai_choice_logprobs(&choice);

        assert_eq!(logprobs.len(), 1);
        assert_eq!(logprobs[0]["token"].as_str(), Some("safe"));
        assert_eq!(logprobs[0]["logprob"].as_f64(), Some(-0.1));
        let top = logprobs[0]["top_logprobs"]
            .as_array()
            .expect("top logprobs array");
        assert_eq!(top.len(), 2);
        assert_eq!(top[1]["token"].as_str(), Some("risky"));
    }

    #[test]
    fn extracts_legacy_completion_logprobs() {
        let choice = serde_json::json!({
            "logprobs": {
                "tokens": ["safe"],
                "token_logprobs": [-0.1],
                "top_logprobs": [
                    {"safe": -0.1, "risky": -2.4}
                ]
            }
        });

        let logprobs = extract_openai_choice_logprobs(&choice);

        assert_eq!(logprobs.len(), 1);
        assert_eq!(logprobs[0]["token"].as_str(), Some("safe"));
        assert_eq!(logprobs[0]["logprob"].as_f64(), Some(-0.1));
        let top = logprobs[0]["top_logprobs"]
            .as_array()
            .expect("top logprobs array");
        assert_eq!(top.len(), 2);
        assert!(top.iter().any(|item| {
            item.get("token").and_then(|value| value.as_str()) == Some("risky")
                && item.get("logprob").and_then(|value| value.as_f64()) == Some(-2.4)
        }));
    }

    #[test]
    fn anthropic_parser_rejects_missing_content_array() {
        let response = serde_json::json!({
            "id": "msg_bad",
            "usage": {"input_tokens": 1, "output_tokens": 0}
        });

        let error = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect_err("missing content must be rejected");

        assert!(error.to_string().contains("missing content array"));
    }

    #[test]
    fn openai_parser_rejects_missing_choices_array() {
        let response = serde_json::json!({
            "id": "chatcmpl-bad",
            "usage": {"prompt_tokens": 1, "completion_tokens": 0}
        });

        let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect_err("missing choices must be rejected");

        assert!(error
            .to_string()
            .contains("missing non-empty choices array"));
    }

    #[test]
    fn openai_parser_rejects_empty_message_without_content() {
        let response = serde_json::json!({
            "choices": [{
                "message": {"content": ""},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 0}
        });

        let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect_err("empty provider message must be rejected");

        assert!(error.to_string().contains("delivered no content"));
    }

    #[test]
    fn anthropic_parser_records_server_tool_use_as_tool_search_query() {
        // Build a minimal Anthropic Messages API response containing a
        // server_tool_use block (the model calling the search tool).
        let response = serde_json::json!({
            "content": [
                {"type": "text", "text": "searching now"},
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_01",
                    "name": "tool_search_tool_bm25",
                    "input": {"query": "weather"}
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect("parser succeeds");

        // tool_calls is for *dispatchable* user tools — server-side tools
        // must never appear there.
        assert!(result.tool_calls.is_empty());

        // The tool_search_query event is on the blocks list.
        let has_query_event = result.blocks.iter().any(|b| {
            b.get("type").and_then(|v| v.as_str()) == Some("tool_search_query")
                && b.get("name").and_then(|v| v.as_str()) == Some("tool_search_tool_bm25")
        });
        assert!(
            has_query_event,
            "expected tool_search_query block; got {:#?}",
            result.blocks
        );
    }

    #[test]
    fn openai_parser_keeps_finish_reason_on_empty_args_tool_call() {
        // IDE-host bug-report evidence shape (non-streaming): the provider
        // boundary delivers a named tool call with literally "{}" arguments.
        // `finish_reason` must surface as `stop_reason` so downstream
        // feedback can distinguish a length-truncated call from a clean-stop
        // provider drop.
        for finish_reason in ["length", "tool_calls"] {
            let response = serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "tool_calls": [
                            {
                                "id": "chatcmpl-tool-1",
                                "type": "function",
                                "function": {"name": "edit", "arguments": "{}"}
                            }
                        ]
                    },
                    "finish_reason": finish_reason
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 549}
            });
            let result = parse_llm_response(&response, "openrouter", "or-qwen", false, false)
                .expect("parser succeeds");
            assert_eq!(result.stop_reason.as_deref(), Some(finish_reason));
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0]["name"], "edit");
            assert_eq!(result.tool_calls[0]["arguments"], serde_json::json!({}));
        }
    }

    #[test]
    fn openai_parser_splits_concatenated_tool_argument_objects() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "chatcmpl-tool-1",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"a.rs\"}{\"path\":\"b.rs\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(&response, "openrouter", "google/gemma-4", false, false)
            .expect("parser succeeds");

        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0]["id"], "chatcmpl-tool-1");
        assert_eq!(result.tool_calls[0]["name"], "read");
        assert_eq!(result.tool_calls[0]["arguments"]["path"], "a.rs");
        assert_eq!(result.tool_calls[1]["id"], "chatcmpl-tool-1_2");
        assert_eq!(result.tool_calls[1]["name"], "read");
        assert_eq!(result.tool_calls[1]["arguments"]["path"], "b.rs");
        let tool_blocks = result
            .blocks
            .iter()
            .filter(|block| block["type"] == "tool_call")
            .collect::<Vec<_>>();
        assert_eq!(tool_blocks.len(), 2);
        assert_eq!(tool_blocks[1]["id"], "chatcmpl-tool-1_2");
    }

    #[test]
    fn openai_parser_splits_concatenated_tool_arguments_without_source_id() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"a.rs\"}{\"path\":\"b.rs\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(&response, "openrouter", "google/gemma-4", false, false)
            .expect("parser succeeds");

        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0]["id"], "call_0_1");
        assert_eq!(result.tool_calls[1]["id"], "call_0_2");
    }

    #[test]
    fn openai_parser_normalizes_harmony_wrapper_tool_call() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "chatcmpl-tool-1",
                            "type": "function",
                            "function": {
                                "name": "tool",
                                "arguments": "{\"name\":\"look\",\"args\":{\"intent\":\"read\",\"file\":\"src/lib.rs\"}}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(
            &response,
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            false,
            false,
        )
        .expect("parser succeeds");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["name"], "look");
        assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
        assert_eq!(result.tool_calls[0]["arguments"]["file"], "src/lib.rs");
    }

    #[test]
    fn openai_parser_strips_harmony_channel_suffix_from_tool_name() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "chatcmpl-tool-1",
                            "type": "function",
                            "function": {
                                "name": "run<|channel|>commentary",
                                "arguments": "{\"command\":\"cargo test\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(
            &response,
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            false,
            false,
        )
        .expect("parser succeeds");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["name"], "run");
        assert_eq!(result.tool_calls[0]["arguments"]["command"], "cargo test");
    }

    #[test]
    fn openai_parser_records_tool_search_call_as_query_event() {
        // OpenAI's Responses API (harn#71) surfaces the server-hosted
        // tool_search as a `tool_search_call` entry in the `tool_calls`
        // array. The parser must NOT add it to the dispatchable
        // `tool_calls` vector — OpenAI runs the search on their side —
        // but must record a `tool_search_query` transcript block so
        // replay lines up with the Anthropic path.
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "searching",
                    "tool_calls": [
                        {
                            "id": "tsc_01",
                            "type": "tool_search_call",
                            "query": {"q": "weather"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect("parser succeeds");

        assert!(
            result.tool_calls.is_empty(),
            "tool_search_call is server-executed; must not be dispatchable"
        );
        let query = result
            .blocks
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_query"))
            .expect("tool_search_query block present");
        assert_eq!(query["id"].as_str(), Some("tsc_01"));
        assert_eq!(query["query"]["q"].as_str(), Some("weather"));
    }

    #[test]
    fn openai_parser_records_tool_search_output_as_result_event() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "tso_01",
                            "type": "tool_search_output",
                            "call_id": "tsc_01",
                            "tool_references": [
                                {"tool_name": "get_weather"}
                            ]
                        }
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        });
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect("parser succeeds");

        assert!(result.tool_calls.is_empty());
        let result_block = result
            .blocks
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_result"))
            .expect("tool_search_result block present");
        assert_eq!(result_block["tool_use_id"].as_str(), Some("tsc_01"));
        let refs = result_block["tool_references"]
            .as_array()
            .expect("tool_references array");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["tool_name"].as_str(), Some("get_weather"));
    }

    #[test]
    fn openai_parser_surfaces_reasoning_summary_separate_from_text() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Final answer.",
                    "reasoning_summary": [
                        {"type": "summary_text", "text": "Checked the constraints."},
                        {"type": "summary_text", "text": "Chose the direct answer."}
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7}
        });

        let result =
            parse_llm_response(&response, "openai", "o3", false, false).expect("parser succeeds");

        assert_eq!(result.text, "Final answer.");
        assert_eq!(
            result.thinking_summary.as_deref(),
            Some("Checked the constraints.\nChose the direct answer.")
        );
        assert_eq!(result.thinking, None);
        assert!(result.blocks.iter().any(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("reasoning_summary")
                && block.get("text").and_then(|value| value.as_str())
                    == Some("Checked the constraints.\nChose the direct answer.")
        }));
    }

    #[test]
    fn anthropic_parser_records_tool_search_tool_result_as_event() {
        let response = serde_json::json!({
            "content": [
                {
                    "type": "tool_search_tool_result",
                    "tool_use_id": "srvtoolu_01",
                    "content": {
                        "type": "tool_search_tool_search_result",
                        "tool_references": [
                            {"type": "tool_reference", "tool_name": "get_weather"}
                        ]
                    }
                },
                {"type": "text", "text": "ok"}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 1}
        });
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect("parser succeeds");

        let result_block = result
            .blocks
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_result"))
            .expect("tool_search_result block present");
        let refs = result_block["tool_references"]
            .as_array()
            .expect("tool_references array");
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0]["tool_name"].as_str(),
            Some("get_weather"),
            "reference name preserved"
        );
    }

    #[test]
    fn openai_parser_preserves_partial_usage_in_telemetry() {
        // OpenAI-compatible local servers (vLLM, MLX) often report only
        // `prompt_tokens` and `completion_tokens`. The parser must still
        // surface those values in the telemetry envelope rather than
        // dropping them on the floor — otherwise eval dashboards see
        // empty per-call accounting and have to fall back to
        // wall-clock heuristics.
        let response = serde_json::json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "message": {"content": "done"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 314, "completion_tokens": 27}
        });

        let result = parse_llm_response(&response, "vllm", "qwen3.6", false, false)
            .expect("parser succeeds");

        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::OPENAI_USAGE
        );
        assert_eq!(result.telemetry.server_prompt_tokens, Some(314));
        assert_eq!(result.telemetry.server_output_tokens, Some(27));
        assert_eq!(result.telemetry.server_prompt_eval_ms, None);
        assert_eq!(result.telemetry.request_id.as_deref(), Some("chatcmpl-abc"));
    }

    #[test]
    fn openai_parser_lifts_llamacpp_timings_into_telemetry() {
        // llama.cpp's OpenAI-compatible server extends `usage` with a
        // `timings` block. Preserve the millisecond fields verbatim and
        // promote the source to `llamacpp_timings` so eval scripts can
        // route on them.
        let response = serde_json::json!({
            "choices": [{
                "message": {"content": "answer"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 17,
                "timings": {
                    "prompt_n": 200,
                    "prompt_ms": 145.4,
                    "predicted_n": 17,
                    "predicted_ms": 89.1
                }
            }
        });

        let result = parse_llm_response(&response, "llamacpp", "qwen-7b", false, false)
            .expect("parser succeeds");

        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::LLAMACPP_TIMINGS
        );
        assert_eq!(result.telemetry.server_prompt_eval_ms, Some(145));
        assert_eq!(result.telemetry.server_generation_ms, Some(89));
        assert_eq!(result.telemetry.server_total_ms, Some(234));
    }

    #[test]
    fn anthropic_parser_captures_request_id_in_telemetry() {
        let response = serde_json::json!({
            "id": "msg_01ABC",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 5, "output_tokens": 2},
            "stop_reason": "end_turn"
        });
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect("parser succeeds");
        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::ANTHROPIC_USAGE
        );
        assert_eq!(result.telemetry.request_id.as_deref(), Some("msg_01ABC"));
        assert_eq!(result.telemetry.server_prompt_tokens, Some(5));
        assert_eq!(result.telemetry.server_output_tokens, Some(2));
    }
}
