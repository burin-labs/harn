//! Non-streaming LLM response parsing. Covers Anthropic's `content`-array
//! shape and the OpenAI-compatible `choices[0].message` shape; streaming
//! variants live in [`super::transport`].

use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::openai_normalize::normalize_openai_message_text;
use super::result::LlmResult;

/// Parse a complete (non-streaming) LLM JSON response into an `LlmResult`.
pub(crate) fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    resolved: &crate::llm::helpers::ResolvedProvider,
) -> Result<LlmResult, VmError> {
    if resolved.is_anthropic_style {
        let mut text = String::new();
        let mut thinking_text = String::new();
        let mut tool_calls = Vec::new();
        let mut blocks = Vec::new();

        if let Some(content) = json["content"].as_array() {
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
                        let references: Vec<serde_json::Value> = block["content"]
                            ["tool_references"]
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
        }

        if text.is_empty() && tool_calls.is_empty() {
            if let Some(err) = json["error"]["message"].as_str() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {err}"
                )))));
            }
        }

        let input_tokens = json["usage"]["input_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["output_tokens"].as_i64().unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = json["stop_reason"].as_str().map(|s| s.to_string());

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
            stop_reason,
            blocks,
        })
    } else {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let message = &json["choices"][0]["message"];
        let (text, extracted_thinking) = normalize_openai_message_text(message);
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
        let stop_reason = json["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| s.to_string());

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
            && output_tokens > 0
            && tool_calls.is_empty()
            && !has_tool_search_block
        {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "openai-compatible model {model} reported completion_tokens={output_tokens} but delivered no content, reasoning, or tool calls"
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
            stop_reason,
            blocks,
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
    use super::{extract_cache_read_tokens, extract_cache_write_tokens, parse_llm_response};

    // Build a ResolvedProvider for the Anthropic path without going through
    // the thread-local provider registry — these parser tests only need the
    // is_anthropic_style flag set.
    fn anthropic_resolved() -> crate::llm::helpers::ResolvedProvider {
        crate::llm::helpers::ResolvedProvider::resolve("anthropic")
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
    fn anthropic_parser_records_server_tool_use_as_tool_search_query() {
        let resolved = anthropic_resolved();
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
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", &resolved)
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
        let resolved = crate::llm::helpers::ResolvedProvider::resolve("openai");
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
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", &resolved)
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
        let resolved = crate::llm::helpers::ResolvedProvider::resolve("openai");
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
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", &resolved)
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
    fn anthropic_parser_records_tool_search_tool_result_as_event() {
        let resolved = anthropic_resolved();
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
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", &resolved)
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
}
