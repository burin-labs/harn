//! Non-streaming LLM response parsing. Covers Anthropic's `content`-array
//! shape and the OpenAI-compatible `choices[0].message` shape; streaming
//! variants live in [`super::transport`].

use std::rc::Rc;

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

/// Parse a complete (non-streaming) LLM JSON response into an `LlmResult`.
pub(crate) fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    is_anthropic_style: bool,
) -> Result<LlmResult, VmError> {
    if is_anthropic_style {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let mut text = String::new();
        let mut thinking_text = String::new();
        let mut tool_calls = Vec::new();
        let mut blocks = Vec::new();

        let content = json
            .get("content")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
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
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let input = block["input"].clone();
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "name": name,
                        "arguments": input,
                    }));
                    blocks.push(serde_json::json!({
                        "type": "tool_call",
                        "id": block["id"].clone(),
                        "name": block["name"].clone(),
                        "arguments": block["input"].clone(),
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
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "anthropic-style model {model} delivered no content, reasoning, or tool calls"
            )))));
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
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: if thinking_text.is_empty() {
                None
            } else {
                Some(thinking_text)
            },
            thinking_summary: None,
            stop_reason,
            blocks,
            logprobs: Vec::new(),
            telemetry,
        })
    } else {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let choices = json
            .get("choices")
            .and_then(|value| value.as_array())
            .filter(|choices| !choices.is_empty())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API response missing non-empty choices array"
                ))))
            })?;
        let choice = &choices[0];
        let message = choice.get("message").ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API response missing choices[0].message"
            ))))
        })?;
        let (text, extracted_thinking) = normalize_openai_message_text(message);
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
                "text": reasoning_summary.clone(),
                "visibility": "private",
            }));
        }

        let mut tool_calls = Vec::new();
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
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
                let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: serde_json::Value = match serde_json::from_str(args_str) {
                    Ok(v) => v,
                    Err(e) => {
                        serde_json::json!({
                            "__parse_error": format!(
                                "Could not parse tool arguments as JSON: {}. Raw input: {}",
                                e,
                                &args_str[..args_str.len().min(200)]
                            )
                        })
                    }
                };
                let id = call["id"].as_str().unwrap_or("").to_string();
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": arguments,
                }));
                blocks.push(serde_json::json!({
                    "type": "tool_call",
                    "id": call["id"].clone(),
                    "name": call["function"]["name"].clone(),
                    "arguments": arguments.clone(),
                    "visibility": "internal",
                }));
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = choice["finish_reason"].as_str().map(|s| s.to_string());
        let request_id = json["id"].as_str().filter(|value| !value.is_empty());
        let telemetry = ProviderTelemetry::from_openai_usage(&json["usage"], request_id);

        // OpenAI Responses-API `tool_search_call` / `tool_search_output`
        // blocks (harn#71) are server-executed and get stripped from
        // `tool_calls` during parsing; they show up only as transcript
        // blocks. Count their presence as "did deliver something" so
        // the empty-response error below doesn't trip when the
        // server's response consisted entirely of a search
        // query/result exchange.
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
        {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "openai-compatible model {model} delivered no content, reasoning, or tool calls"
            )))));
        }

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
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
    usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_i64())
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(|v| v.as_i64())
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        extract_cache_read_tokens, extract_cache_write_tokens, extract_openai_choice_logprobs,
        parse_llm_response,
    };

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

        let error = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true)
            .expect_err("missing content must be rejected");

        assert!(error.to_string().contains("missing content array"));
    }

    #[test]
    fn openai_parser_rejects_missing_choices_array() {
        let response = serde_json::json!({
            "id": "chatcmpl-bad",
            "usage": {"prompt_tokens": 1, "completion_tokens": 0}
        });

        let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false)
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

        let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false)
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
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true)
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
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false)
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
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false)
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

        let result = parse_llm_response(&response, "openai", "o3", false).expect("parser succeeds");

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
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true)
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

        let result =
            parse_llm_response(&response, "vllm", "qwen3.6", false).expect("parser succeeds");

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

        let result =
            parse_llm_response(&response, "llamacpp", "qwen-7b", false).expect("parser succeeds");

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
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true)
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
