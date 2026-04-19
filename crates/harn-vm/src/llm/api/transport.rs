//! Shared LLM API transport: provider dispatch + request send + streaming
//! (SSE, NDJSON) and non-streaming response consumption. Provider-specific
//! request-body construction lives in `crate::llm::providers`; this file is
//! the wire-format layer below that.

use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::errors::classify_http_error;
use super::openai_normalize::{
    append_paragraph, debug_log_message_shapes, extract_openai_message_field_as_text,
};
use super::options::{DeltaSender, LlmRequestPayload};
use super::response::{extract_cache_read_tokens, extract_cache_write_tokens, parse_llm_response};
use super::result::LlmResult;
use super::thinking::ThinkingStreamSplitter;

fn parse_ollama_tool_arguments(arguments: &serde_json::Value) -> serde_json::Value {
    match arguments {
        serde_json::Value::Object(_) | serde_json::Value::Array(_) | serde_json::Value::Null => {
            arguments.clone()
        }
        serde_json::Value::String(text) => serde_json::from_str(text).unwrap_or_else(|err| {
            serde_json::json!({
                "__parse_error": format!(
                    "Could not parse tool arguments as JSON: {}. Raw input: {}",
                    err,
                    &text[..text.len().min(200)]
                )
            })
        }),
        other => other.clone(),
    }
}

fn append_ollama_tool_calls(
    message: &serde_json::Value,
    tool_calls: &mut Vec<serde_json::Value>,
    blocks: &mut Vec<serde_json::Value>,
) {
    let Some(calls) = message.get("tool_calls").and_then(|value| value.as_array()) else {
        return;
    };

    for (idx, call) in calls.iter().enumerate() {
        let function = call.get("function").unwrap_or(call);
        let name = function
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let arguments = parse_ollama_tool_arguments(
            function
                .get("arguments")
                .unwrap_or(&serde_json::Value::Object(Default::default())),
        );
        let id = call
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                function
                    .get("index")
                    .and_then(|value| value.as_i64())
                    .map(|index| format!("ollama_tool_{index}"))
            })
            .unwrap_or_else(|| format!("ollama_tool_{}", tool_calls.len() + idx));
        tool_calls.push(serde_json::json!({
            "id": id,
            "name": name,
            "arguments": arguments,
        }));
        blocks.push(serde_json::json!({
            "type": "tool_call",
            "id": id,
            "name": function.get("name").cloned().unwrap_or(serde_json::json!("")),
            "arguments": arguments,
            "visibility": "internal",
        }));
    }
}

/// Dispatch an LLM API call to the appropriate provider. This is the main
/// entry point that routes to provider-specific implementations via the
/// provider plugin architecture.
///
/// The dispatch order is:
/// 1. Check the thread-local provider registry (populated by `register_default_providers`)
/// 2. Fall back to config-based resolution (for dynamically-configured providers)
/// 3. Use the legacy inline dispatch as a final fallback
pub(super) async fn vm_call_llm_api(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;

    if crate::llm::provider::is_provider_registered(provider) {
        return dispatch_to_registered_provider(opts, delta_tx).await;
    }

    // Fallback for unregistered providers: dispatch by API style.
    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);
    let is_ollama = provider == "ollama" || resolved.endpoint.contains("/api/chat");
    let is_anthropic = resolved.is_anthropic_style;

    let body = if is_ollama {
        crate::llm::providers::OllamaProvider::build_request_body(opts)
    } else if is_anthropic {
        crate::llm::providers::AnthropicProvider::build_request_body(opts)
    } else {
        crate::llm::providers::OpenAiCompatibleProvider::build_request_body(opts, false)
    };

    vm_call_llm_api_with_body(opts, delta_tx, body, is_anthropic, is_ollama).await
}

/// Dispatch to a registered provider by name.
///
/// Provider selection uses trait methods (`is_mock()`, `is_local()`,
/// `is_anthropic_style()`) instead of string comparisons so that each
/// provider owns its own dispatch semantics.
async fn dispatch_to_registered_provider(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    use crate::llm::provider::LlmProvider;

    // Providers are zero-cost unit structs constructed inline to avoid
    // RefCell-across-await conflicts on a shared registry.
    let provider = &opts.provider;
    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);

    let mock = crate::llm::providers::MockProvider;
    if mock.is_mock() && provider == mock.name() {
        return mock.chat_impl(opts, delta_tx).await;
    }

    let ollama = crate::llm::providers::OllamaProvider;
    if (provider == ollama.name() || resolved.endpoint.contains("/api/chat")) && ollama.is_local() {
        return ollama.chat_impl(opts, delta_tx).await;
    }

    if resolved.is_anthropic_style {
        let anthropic = crate::llm::providers::AnthropicProvider;
        return anthropic.chat_impl(opts, delta_tx).await;
    }

    crate::llm::providers::OpenAiCompatibleProvider::new(provider.to_string())
        .chat_impl(opts, delta_tx)
        .await
}

/// Execute an LLM API call with a pre-built request body. This is the shared
/// transport layer used by all provider implementations. It handles:
/// - Provider-specific overrides merging
/// - Stream vs non-stream transport selection
/// - HTTP error classification
/// - SSE and NDJSON response parsing
///
/// Provider implementations call this after building their provider-specific
/// request body via `build_request_body()`.
pub(crate) async fn vm_call_llm_api_with_body(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    mut body: serde_json::Value,
    is_anthropic_style: bool,
    is_ollama: bool,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;
    let model = &opts.model;
    let wants_streaming = delta_tx.is_some() && opts.stream;

    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);
    let use_stream_transport = if is_ollama && !opts.stream {
        crate::events::log_warn(
            "llm",
            "stream=false is not supported by Ollama, using streaming",
        );
        true
    } else {
        wants_streaming || is_ollama
    };

    if let Some(ref overrides) = opts.provider_overrides {
        if let Some(obj) = overrides.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
    }

    if let Some(messages) = body.get("messages").and_then(|value| value.as_array()) {
        debug_log_message_shapes(
            &format!("outbound provider={provider} model={model}"),
            messages,
        );
    }

    if use_stream_transport {
        body["stream"] = serde_json::json!(true);
        // OpenAI-style: request usage in the final streaming chunk.
        if !is_anthropic_style && !is_ollama {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
    }

    let client = if use_stream_transport {
        crate::llm::shared_streaming_client().clone()
    } else {
        crate::llm::shared_blocking_client().clone()
    };

    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(opts.resolve_timeout()))
        .json(&body);
    let req = resolved.apply_headers(req, &opts.api_key);

    if use_stream_transport {
        let tx = if let Some(tx) = delta_tx {
            tx
        } else {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            tx
        };
        let response = req.send().await.map_err(|e| {
            let kind = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else if e.is_request() {
                "request_build"
            } else if e.is_body() {
                "body"
            } else {
                "unknown"
            };
            // "unknown" uses Debug repr to surface the inner cause.
            let detail = if kind == "unknown" {
                format!("{provider} stream error ({kind}): {e:?}")
            } else {
                format!("{provider} stream error ({kind}): {e}")
            };
            VmError::Thrown(VmValue::String(Rc::from(detail)))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = response.text().await.unwrap_or_default();
            let msg = classify_http_error(provider, status, retry_after.as_deref(), &body);
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ct.contains("text/event-stream") {
            return vm_call_llm_api_sse_from_response(response, model, &resolved, tx).await;
        }
        return vm_call_llm_api_ndjson_from_response(response, model, tx).await;
    }

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{provider} API error: {e}"
        ))))
    })?;

    // Check HTTP status BEFORE parsing the body as LLM response, or error
    // responses (e.g. vLLM "prompt too long" 400) silently become malformed
    // parse results and the agent loop retries against the same bad context.
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = response.text().await.unwrap_or_default();
        let msg = classify_http_error(provider, status, retry_after.as_deref(), &body);
        return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
    }

    let json: serde_json::Value = response.json().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{provider} response parse error: {e}"
        ))))
    })?;

    parse_llm_response(&json, provider, model, &resolved)
}

/// Consume an SSE streaming response from an already-sent request.
/// Parses `data: {...}` lines from the response body.
async fn vm_call_llm_api_sse_from_response(
    response: reqwest::Response,
    model: &str,
    resolved: &crate::llm::helpers::ResolvedProvider<'_>,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|r| r.map_err(std::io::Error::other)),
    ));
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();

    struct ToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut current_tool: Option<ToolBlock> = None;
    // Mirror structure for server-side tool-search queries: Anthropic
    // streams the query JSON the same way as a regular tool_use, but we
    // route it to a `tool_search_query` transcript event instead of the
    // dispatchable `tool_calls` vector.
    struct ServerToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut current_server_tool: Option<ServerToolBlock> = None;
    let mut thinking_text = String::new();
    let mut in_thinking_block = false;
    let mut stop_reason: Option<String> = None;
    let mut cache_read_tokens: i64 = 0;
    let mut cache_write_tokens: i64 = 0;

    let mut oai_tool_map: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();
    // Qwen3/3.5 via vLLM emit inline `<think>...</think>`. Strip these
    // out of the visible delta stream so the tool-call parser / progress
    // UI only see the real response.
    let mut oai_thinking_splitter = ThinkingStreamSplitter::new();

    while let Ok(Some(line)) = lines.next_line().await {
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if resolved.is_anthropic_style {
            match json["type"].as_str() {
                Some("message_start") => {
                    if let Some(n) = json["message"]["usage"]["input_tokens"].as_i64() {
                        input_tokens = n;
                    }
                    let usage = &json["message"]["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
                    }
                }
                Some("content_block_start") => {
                    let block = &json["content_block"];
                    match block["type"].as_str() {
                        Some("tool_use") => {
                            current_tool = Some(ToolBlock {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                input_json: String::new(),
                            });
                        }
                        Some("server_tool_use") => {
                            current_server_tool = Some(ServerToolBlock {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                input_json: String::new(),
                            });
                        }
                        Some("tool_search_tool_result") => {
                            // Non-streaming content: Anthropic embeds the
                            // references directly in the block_start
                            // payload. Record immediately — no deltas
                            // follow for this block type.
                            let refs: Vec<serde_json::Value> = block["content"]["tool_references"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            blocks.push(serde_json::json!({
                                "type": "tool_search_result",
                                "tool_use_id": block["tool_use_id"].clone(),
                                "tool_references": refs,
                                "visibility": "internal",
                            }));
                        }
                        Some("thinking") => {
                            in_thinking_block = true;
                        }
                        _ => {}
                    }
                }
                Some("content_block_delta") => {
                    let delta = &json["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(t) = delta["text"].as_str() {
                                text.push_str(t);
                                let _ = delta_tx.send(t.to_string());
                                blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta["thinking"].as_str() {
                                thinking_text.push_str(t);
                                blocks.push(serde_json::json!({"type": "reasoning", "text": t, "visibility": "private"}));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(ref mut tool) = current_tool {
                                if let Some(j) = delta["partial_json"].as_str() {
                                    tool.input_json.push_str(j);
                                }
                            } else if let Some(ref mut server_tool) = current_server_tool {
                                if let Some(j) = delta["partial_json"].as_str() {
                                    server_tool.input_json.push_str(j);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    if let Some(tool) = current_tool.take() {
                        let args = serde_json::from_str::<serde_json::Value>(&tool.input_json)
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        tool_calls.push(serde_json::json!({
                            "id": tool.id, "name": tool.name, "arguments": args,
                        }));
                        blocks.push(serde_json::json!({"type": "tool_call", "id": tool.id, "name": tool.name, "arguments": args, "visibility": "internal"}));
                    } else if let Some(server_tool) = current_server_tool.take() {
                        // Emit a `tool_search_query` transcript event —
                        // not dispatchable, just observability.
                        let query =
                            serde_json::from_str::<serde_json::Value>(&server_tool.input_json)
                                .unwrap_or(serde_json::Value::Object(Default::default()));
                        blocks.push(serde_json::json!({
                            "type": "tool_search_query",
                            "id": server_tool.id,
                            "name": server_tool.name,
                            "query": query,
                            "visibility": "internal",
                        }));
                    }
                    in_thinking_block = false;
                }
                Some("message_delta") => {
                    if let Some(n) = json["usage"]["output_tokens"].as_i64() {
                        output_tokens = n;
                    }
                    let usage = &json["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
                    }
                    if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(sr.to_string());
                    }
                }
                _ => {}
            }
        } else {
            let choice = &json["choices"][0];
            let delta = &choice["delta"];

            if let Some(content) = delta["content"].as_str() {
                let visible = oai_thinking_splitter.push(content);
                if !visible.is_empty() {
                    text.push_str(&visible);
                    let _ = delta_tx.send(visible.clone());
                    blocks.push(serde_json::json!({"type": "output_text", "text": visible, "visibility": "public"}));
                }
            }
            let reasoning_delta =
                extract_openai_message_field_as_text(delta, &["reasoning", "reasoning_content"]);
            if !reasoning_delta.is_empty() {
                append_paragraph(&mut thinking_text, &reasoning_delta);
                blocks.push(serde_json::json!({"type": "reasoning", "text": reasoning_delta, "visibility": "private"}));
            }

            // Only capture finish_reason once; OpenRouter can send
            // duplicates (qwen-code#2402) that truncate in-progress tool
            // calls.
            if stop_reason.is_none() {
                if let Some(fr) = choice["finish_reason"].as_str() {
                    stop_reason = Some(fr.to_string());
                }
            }

            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    // OpenAI Responses-API server-side tool_search
                    // (harn#71) streams as `tool_search_call` /
                    // `tool_search_output` entries in the tool_calls
                    // array. Record them as transcript events, never
                    // as dispatchable calls.
                    let tc_type = tc["type"].as_str().unwrap_or("");
                    if tc_type == "tool_search_call" {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let query = tc.get("query").cloned().unwrap_or_else(|| {
                            tc.get("input").cloned().unwrap_or(serde_json::Value::Null)
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
                    if tc_type == "tool_search_output" {
                        let tool_use_id = tc["call_id"]
                            .as_str()
                            .or_else(|| tc["id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let references = tc["tool_references"]
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
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let entry = oai_tool_map.entry(idx).or_insert_with(|| {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                        (id, name, String::new())
                    });
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        entry.2.push_str(args);
                    }
                }
            }

            if let Some(usage) = json.get("usage") {
                if let Some(n) = usage["prompt_tokens"].as_i64() {
                    input_tokens = n;
                }
                if let Some(n) = usage["completion_tokens"].as_i64() {
                    output_tokens = n;
                }
                let cr = extract_cache_read_tokens(usage);
                if cr > 0 {
                    cache_read_tokens = cr;
                }
                let cw = extract_cache_write_tokens(usage);
                if cw > 0 {
                    cache_write_tokens = cw;
                }
            }
        }
    }

    for (_, (id, name, args_str)) in oai_tool_map {
        let args = serde_json::from_str::<serde_json::Value>(&args_str)
            .unwrap_or(serde_json::Value::Object(Default::default()));
        tool_calls.push(serde_json::json!({
            "id": id, "name": name, "arguments": args,
        }));
        blocks.push(serde_json::json!({"type": "tool_call", "id": id, "name": name, "arguments": args, "visibility": "internal"}));
    }

    let final_visible = oai_thinking_splitter.flush();
    if !final_visible.is_empty() {
        text.push_str(&final_visible);
        let _ = delta_tx.send(final_visible.clone());
        blocks.push(serde_json::json!({"type": "output_text", "text": final_visible, "visibility": "public"}));
    }
    if !oai_thinking_splitter.thinking.is_empty() {
        append_paragraph(&mut thinking_text, &oai_thinking_splitter.thinking);
    }

    let _ = in_thinking_block; // suppress unused warning

    if text.is_empty() && !thinking_text.is_empty() {
        text = thinking_text.clone();
        blocks
            .push(serde_json::json!({"type": "output_text", "text": text, "visibility": "public"}));
    }
    let has_tool_search_block = blocks.iter().any(|b| {
        matches!(
            b.get("type").and_then(|v| v.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    if text.is_empty()
        && thinking_text.is_empty()
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
        provider: if resolved.is_anthropic_style {
            "anthropic".to_string()
        } else {
            "openai".to_string()
        },
        thinking: if thinking_text.is_empty() {
            None
        } else {
            Some(thinking_text)
        },
        stop_reason,
        blocks,
    })
}

/// Consume an NDJSON streaming response, forwarding text deltas to `delta_tx`
/// while accumulating the full result.
///
/// Supports Ollama format (one JSON object per line):
/// `{"message":{"role":"assistant","content":"Hi"},"done":false}`
/// Final line has `"done":true` with token counts.
///
/// Also supports OpenAI-compatible NDJSON where each line is `data: {...}`.
async fn vm_call_llm_api_ndjson_from_response(
    response: reqwest::Response,
    model: &str,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|r| r.map_err(std::io::Error::other)),
    ));
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut result_model = model.to_string();

    let mut thinking_text = String::new();
    let mut tool_calls = Vec::new();
    let mut blocks = Vec::new();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Ollama streams content and thinking as separate channels for
        // reasoning-capable models (gemma3/4, qwen3, etc.); we always set
        // `think: true` so thinking tokens aren't dropped.
        let content = json["message"]["content"].as_str().unwrap_or("");
        let thinking = json["message"]["thinking"].as_str().unwrap_or("");
        if !content.is_empty() {
            text.push_str(content);
            let _ = delta_tx.send(content.to_string());
            blocks.push(
                serde_json::json!({"type": "output_text", "text": content, "visibility": "public"}),
            );
        } else if !thinking.is_empty() {
            thinking_text.push_str(thinking);
            let _ = delta_tx.send(thinking.to_string());
            blocks.push(
                serde_json::json!({"type": "reasoning", "text": thinking, "visibility": "private"}),
            );
        }
        append_ollama_tool_calls(&json["message"], &mut tool_calls, &mut blocks);

        if let Some(m) = json["model"].as_str() {
            result_model = m.to_string();
        }

        if json["done"].as_bool() == Some(true) {
            if let Some(n) = json["prompt_eval_count"].as_i64() {
                input_tokens = n;
            }
            if let Some(n) = json["eval_count"].as_i64() {
                output_tokens = n;
            }
            break;
        }
    }

    let thinking = if thinking_text.is_empty() {
        None
    } else {
        Some(thinking_text.clone())
    };
    if text.is_empty() && !thinking_text.is_empty() {
        text = thinking_text;
    }

    // Guard against upstream parser bugs reporting generated tokens with
    // no visible content. Observed with `gemma4:26b` + ollama's
    // server-side `PARSER gemma4` on tool-heavy prompts: eval_count is
    // nonzero but every delta and the done chunk are empty strings.
    // Silently returning empty text would make the agent loop burn
    // iterations on a no-op.
    if text.is_empty() && tool_calls.is_empty() && output_tokens > 0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama model {model} reported eval_count={output_tokens} but delivered no content or thinking — likely a server-side parser bug; try a different model"
        )))));
    }

    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: result_model,
        provider: "ollama".to_string(),
        thinking,
        stop_reason: None,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::{append_ollama_tool_calls, parse_ollama_tool_arguments};

    #[test]
    fn ollama_tool_arguments_accept_object_shape() {
        let arguments = serde_json::json!({"path": "README.md"});
        assert_eq!(parse_ollama_tool_arguments(&arguments), arguments);
    }

    #[test]
    fn ollama_tool_arguments_parse_json_strings() {
        let parsed = parse_ollama_tool_arguments(&serde_json::json!("{\"path\":\"README.md\"}"));
        assert_eq!(parsed["path"], "README.md");
    }

    #[test]
    fn ollama_stream_chunks_surface_tool_calls() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {
                    "name": "read_file",
                    "arguments": {
                        "path": "README.md"
                    }
                }
            }]
        });
        let mut tool_calls = Vec::new();
        let mut blocks = Vec::new();
        append_ollama_tool_calls(&message, &mut tool_calls, &mut blocks);

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["name"], "read_file");
        assert_eq!(tool_calls[0]["arguments"]["path"], "README.md");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_call");
    }
}
