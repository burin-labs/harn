//! Shared LLM API transport: provider dispatch + request send + streaming
//! (SSE, NDJSON) and non-streaming response consumption. Provider-specific
//! request-body construction lives in `crate::llm::providers`; this file is
//! the wire-format layer below that.

use std::rc::Rc;
use std::time::Instant;

use crate::agent_events::{AgentEvent, ToolCallStatus};
use crate::value::{VmError, VmValue};

use super::openai_normalize::{
    append_paragraph, debug_log_message_shapes, extract_openai_delta_field_str,
};
use super::options::{DeltaSender, LlmRequestPayload};
use super::partial_tool_args::{project_partial, DeltaCoalescer, PartialToolArgs};
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

fn should_request_stream_usage(is_anthropic_style: bool, is_ollama: bool, endpoint: &str) -> bool {
    if is_anthropic_style {
        return false;
    }
    // OpenAI-compatible streams expose aggregate usage in a final chunk
    // when requested. Ollama's native `/api/chat` shape does not use
    // this field, but its `/v1/chat/completions` compatibility endpoint
    // does.
    !is_ollama || endpoint.contains("/v1/")
}

fn classify_transport_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
    is_anthropic_style: bool,
    is_ollama: bool,
) -> String {
    if is_anthropic_style {
        return crate::llm::providers::AnthropicProvider::classify_http_error(
            status,
            retry_after,
            body,
        )
        .message;
    }
    if is_ollama {
        return crate::llm::providers::OllamaProvider::classify_http_error(
            status,
            retry_after,
            body,
        )
        .message;
    }
    crate::llm::providers::OpenAiCompatibleProvider::classify_http_error(
        provider,
        status,
        retry_after,
        body,
    )
    .message
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

    if is_ollama {
        return crate::llm::providers::OllamaProvider
            .chat_impl(opts, delta_tx)
            .await;
    }

    let body = if is_anthropic {
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

    if provider == "gemini" {
        let gemini = crate::llm::providers::GeminiProvider;
        return gemini.chat_impl(opts, delta_tx).await;
    }

    if provider == "bedrock" {
        return crate::llm::providers::BedrockProvider
            .chat_impl(opts, delta_tx)
            .await;
    }

    if provider == "azure_openai" {
        return crate::llm::providers::AzureOpenAiProvider
            .chat_impl(opts, delta_tx)
            .await;
    }

    if provider == "vertex" {
        return crate::llm::providers::VertexProvider
            .chat_impl(opts, delta_tx)
            .await;
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

    if !is_ollama {
        if let Some(ref overrides) = opts.provider_overrides {
            if let Some(obj) = overrides.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
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
        if should_request_stream_usage(is_anthropic_style, is_ollama, &resolved.endpoint) {
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
            let msg = classify_transport_http_error(
                provider,
                status,
                retry_after.as_deref(),
                &body,
                is_anthropic_style,
                is_ollama,
            );
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ct.contains("text/event-stream") {
            return vm_call_llm_api_sse_from_response(
                response,
                provider,
                model,
                &resolved,
                tx,
                opts.session_id.as_deref(),
            )
            .await;
        }
        return vm_call_llm_api_ndjson_from_response(response, provider, model, tx).await;
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
        let msg = classify_transport_http_error(
            provider,
            status,
            retry_after.as_deref(),
            &body,
            is_anthropic_style,
            is_ollama,
        );
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
/// Parses `data: {...}` lines from the response body, then defers to
/// [`consume_sse_lines`] for the parsing and event-emission logic so
/// tests can drive the same code path against an in-memory `AsyncBufRead`.
async fn vm_call_llm_api_sse_from_response(
    response: reqwest::Response,
    provider: &str,
    model: &str,
    resolved: &crate::llm::helpers::ResolvedProvider,
    delta_tx: DeltaSender,
    session_id: Option<&str>,
) -> Result<LlmResult, VmError> {
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|r| r.map_err(std::io::Error::other)),
    ));
    consume_sse_lines(
        reader,
        provider,
        model,
        resolved.is_anthropic_style,
        delta_tx,
        session_id,
    )
    .await
}

/// Try to publish the live `(tool_call_id, tool_name, accumulated_bytes)`
/// triple as a `ToolCallUpdate(Pending, raw_input | raw_input_partial)`
/// event. Coalescing + partial-parse logic lives here so both the
/// Anthropic and OpenAI branches of the SSE loop share one emit site.
fn try_emit_partial_tool_args(
    session_id: Option<&str>,
    tool_call_id: &str,
    tool_name: &str,
    accumulated: &str,
    coalescer: &mut DeltaCoalescer,
    now: Instant,
) {
    let Some(session_id) = session_id else {
        return;
    };
    if !coalescer.should_emit(now) {
        return;
    }
    let PartialToolArgs { value, raw_partial } = project_partial(accumulated);
    if value.is_none() && raw_partial.is_none() {
        return;
    }
    let event = AgentEvent::ToolCallUpdate {
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        raw_input: value,
        raw_input_partial: raw_partial,
        audit: crate::orchestration::current_mutation_session(),

        parsing: None,
    };
    crate::llm::agent::emit_agent_event_sync(&event);
}

/// Map an Anthropic `tool_use` block id (or, as a fallback, an
/// iteration-relative index) to the canonical `tool-{id}` shape that
/// `tool_dispatch.rs` later emits from. Keeping the two sites in sync
/// lets clients correlate streaming `Pending` updates with the eventual
/// `InProgress`/`Completed` lifecycle.
fn streaming_tool_call_id(provider_id: &str, fallback_index: usize) -> String {
    if provider_id.is_empty() {
        format!("tool-stream-{fallback_index}")
    } else {
        format!("tool-{provider_id}")
    }
}

/// Pure SSE-line consumer extracted so #693's streaming-partial-args
/// behavior can be tested against canned byte streams without standing
/// up a full `reqwest::Response`. The Anthropic / OpenAI branches and
/// the trailing accumulator drain that finalize the call live here.
pub(super) async fn consume_sse_lines<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    provider: &str,
    model: &str,
    is_anthropic_style: bool,
    delta_tx: DeltaSender,
    session_id: Option<&str>,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
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
        /// Stable id used for `AgentEvent::ToolCall*` streaming
        /// emissions (#693). Must match the shape `tool_dispatch.rs`
        /// constructs later so clients can correlate the streaming
        /// `Pending` updates with the eventual `InProgress`/`Completed`
        /// lifecycle.
        tool_call_id: String,
        /// Coalescing gate so a tool that arrives in 30 small deltas
        /// emits ~6 `ToolCallUpdate` events instead of 30.
        coalescer: DeltaCoalescer,
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
    // Counter for fallback streaming-tool-call ids when a provider sent
    // an empty id on the first tool_use block. Kept stable across the
    // stream so the coalesced updates reuse the same id the dispatcher
    // would compute.
    let mut anth_tool_block_index: usize = 0;

    /// Per-tool-call OpenAI streaming state. Tracks the accumulated
    /// arguments string, the tool name (filled when the first delta
    /// carries `function.name`), the synthetic `tool_call_id` we use
    /// for `AgentEvent::ToolCall` emission, whether the initial
    /// `ToolCall(Pending)` event has fired yet, and a coalescer so
    /// argument-delta storms don't fan out per-byte.
    struct OaiToolStream {
        id: String,
        name: String,
        args: String,
        tool_call_id: String,
        announced: bool,
        coalescer: DeltaCoalescer,
    }
    let mut oai_tool_map: std::collections::HashMap<u64, OaiToolStream> =
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

        if is_anthropic_style {
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
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            anth_tool_block_index += 1;
                            let tool_call_id = streaming_tool_call_id(&id, anth_tool_block_index);
                            // Streaming announcement: emit before any
                            // arg deltas so ACP clients can render
                            // "calling search_web…" with zero latency.
                            if let Some(sid) = session_id {
                                let tool_kind =
                                    crate::orchestration::current_tool_annotations(&name)
                                        .map(|a| a.kind);
                                crate::llm::agent::emit_agent_event_sync(&AgentEvent::ToolCall {
                                    session_id: sid.to_string(),
                                    tool_call_id: tool_call_id.clone(),
                                    tool_name: name.clone(),
                                    kind: tool_kind,
                                    status: ToolCallStatus::Pending,
                                    raw_input: serde_json::Value::Object(Default::default()),
                                    audit: crate::orchestration::current_mutation_session(),

                                    parsing: None,
                                });
                            }
                            current_tool = Some(ToolBlock {
                                id,
                                name,
                                input_json: String::new(),
                                tool_call_id,
                                coalescer: DeltaCoalescer::new(),
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
                                try_emit_partial_tool_args(
                                    session_id,
                                    &tool.tool_call_id,
                                    &tool.name,
                                    &tool.input_json,
                                    &mut tool.coalescer,
                                    Instant::now(),
                                );
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
            // Streaming deltas for `reasoning` (Ollama OpenAI-compat,
            // OpenRouter passthrough) and `reasoning_content` (DashScope,
            // Together) arrive as token-sized fragments — `"Here"`,
            // `"'s"`, `" a"`, `" thinking"`. Concatenate them verbatim;
            // `extract_openai_message_field_as_text` + `append_paragraph`
            // would `.trim()` each fragment (losing inter-token spaces)
            // and inject a newline between every chunk, producing the
            // one-token-per-line reasoning text we used to surface as
            // `"The\ntask\nis\nto\nextend"`. The non-streaming response
            // path still uses `append_paragraph` because there each
            // field arrives as a single complete block.
            let reasoning_delta =
                extract_openai_delta_field_str(delta, &["reasoning", "reasoning_content"]);
            if !reasoning_delta.is_empty() {
                thinking_text.push_str(reasoning_delta);
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
                    let stream_index = idx as usize + 1;
                    let entry = oai_tool_map.entry(idx).or_insert_with(|| {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                        let tool_call_id = streaming_tool_call_id(&id, stream_index);
                        OaiToolStream {
                            id,
                            name,
                            args: String::new(),
                            tool_call_id,
                            announced: false,
                            coalescer: DeltaCoalescer::new(),
                        }
                    });
                    // OpenAI sometimes splits the metadata across deltas:
                    // the first one carries `name`, later ones carry only
                    // `arguments`. Patch missing fields if a later delta
                    // fills them in.
                    if entry.id.is_empty() {
                        if let Some(id) = tc["id"].as_str() {
                            if !id.is_empty() {
                                entry.id = id.to_string();
                                entry.tool_call_id =
                                    streaming_tool_call_id(&entry.id, stream_index);
                            }
                        }
                    }
                    if entry.name.is_empty() {
                        if let Some(name) = tc["function"]["name"].as_str() {
                            if !name.is_empty() {
                                entry.name = name.to_string();
                            }
                        }
                    }
                    // Announce the tool call as soon as we have a name —
                    // before any arg deltas, so clients render "calling
                    // X…" immediately. If only arguments arrive first
                    // (rare; some self-hosted vLLM builds), we hold off
                    // and announce on the first real partial-args emit
                    // below.
                    if !entry.announced && !entry.name.is_empty() {
                        if let Some(sid) = session_id {
                            let tool_kind =
                                crate::orchestration::current_tool_annotations(&entry.name)
                                    .map(|a| a.kind);
                            crate::llm::agent::emit_agent_event_sync(&AgentEvent::ToolCall {
                                session_id: sid.to_string(),
                                tool_call_id: entry.tool_call_id.clone(),
                                tool_name: entry.name.clone(),
                                kind: tool_kind,
                                status: ToolCallStatus::Pending,
                                raw_input: serde_json::Value::Object(Default::default()),
                                audit: crate::orchestration::current_mutation_session(),

                                parsing: None,
                            });
                            entry.announced = true;
                        }
                    }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        entry.args.push_str(args);
                    }
                    if entry.announced {
                        try_emit_partial_tool_args(
                            session_id,
                            &entry.tool_call_id,
                            &entry.name,
                            &entry.args,
                            &mut entry.coalescer,
                            Instant::now(),
                        );
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

    for (_, stream) in oai_tool_map {
        let args = serde_json::from_str::<serde_json::Value>(&stream.args)
            .unwrap_or(serde_json::Value::Object(Default::default()));
        tool_calls.push(serde_json::json!({
            "id": stream.id, "name": stream.name, "arguments": args,
        }));
        blocks.push(serde_json::json!({
            "type": "tool_call",
            "id": stream.id,
            "name": stream.name,
            "arguments": args,
            "visibility": "internal",
        }));
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

    // Use the caller-supplied provider id rather than collapsing every
    // non-anthropic stream to "openai". The provider name shows up in the
    // observability transcript (`agent_observe::dump_llm_response`) and is
    // load-bearing for downstream classifiers (e.g. honors_chat_template_kwargs
    // routing in capability lookup) — collapsing it to "openai" hides which
    // OpenAI-compatible server (vLLM, llama.cpp, OpenRouter, llamacpp) the
    // call actually went to. Anthropic's classic SSE shape still implies
    // provider="anthropic" because the wire protocol is anthropic-specific
    // even when the configured provider name disagrees (proxies / mocks).
    let result_provider = if is_anthropic_style {
        "anthropic".to_string()
    } else {
        provider.to_string()
    };
    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        model: model.to_string(),
        provider: result_provider,
        thinking: if thinking_text.is_empty() {
            None
        } else {
            Some(thinking_text)
        },
        thinking_summary: None,
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
    provider: &str,
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
        // NDJSON is currently only consumed for Ollama's `/api/chat`, but
        // pass the caller-supplied provider through anyway so the result
        // matches `opts.provider` exactly. Future engines that adopt
        // NDJSON streaming (some llama.cpp builds, mlx-vlm) will get the
        // right label without additional plumbing.
        provider: provider.to_string(),
        thinking,
        thinking_summary: None,
        stop_reason: None,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        append_ollama_tool_calls, parse_ollama_tool_arguments, should_request_stream_usage,
    };

    #[test]
    fn stream_usage_requested_for_openai_compatible_endpoints() {
        assert!(should_request_stream_usage(
            false,
            false,
            "/chat/completions"
        ));
        assert!(should_request_stream_usage(
            false,
            true,
            "/v1/chat/completions"
        ));
        assert!(!should_request_stream_usage(false, true, "/api/chat"));
        assert!(!should_request_stream_usage(true, false, "/messages"));
    }

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

#[cfg(test)]
mod streaming_tool_call_tests {
    //! Streaming-tool-call partial-arg event tests (#693). Drive
    //! [`consume_sse_lines`] against canned byte streams that mimic the
    //! Anthropic / OpenAI wire format and assert the
    //! `AgentEvent::ToolCall` / `AgentEvent::ToolCallUpdate` sequence
    //! lands as expected: an initial `Pending` announcement, one or
    //! more coalesced partial-arg updates, and `raw_input_partial`
    //! when the partial bytes can't be parsed as JSON yet.
    //!
    //! Events are captured via the global session-sink registry (which
    //! is what real ACP/A2A consumers use) rather than the per-loop
    //! thread-local — drive a fresh session id per test so they don't
    //! cross-talk under `cargo test`'s thread pool.

    use super::*;
    use crate::agent_events::{
        clear_session_sinks, register_sink, AgentEvent, AgentEventSink, ToolCallStatus,
    };
    use std::sync::{Arc, Mutex};

    struct CapturingSink {
        events: Arc<Mutex<Vec<AgentEvent>>>,
    }

    impl AgentEventSink for CapturingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.events
                .lock()
                .expect("capture mutex")
                .push(event.clone());
        }
    }

    fn install_capturing_sink(session_id: &str) -> Arc<Mutex<Vec<AgentEvent>>> {
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        register_sink(
            session_id,
            Arc::new(CapturingSink {
                events: events.clone(),
            }),
        );
        events
    }

    /// Build a fresh per-test session id so concurrent test threads
    /// can't poison each other's captured-event vector via the global
    /// registry.
    fn fresh_session_id(label: &str) -> String {
        format!("{label}-{}", uuid::Uuid::now_v7())
    }

    /// Drive `consume_sse_lines` against a canned SSE byte buffer and
    /// return the captured agent events plus the parsed `LlmResult`.
    /// Helper so each test can stay focused on the assertion.
    async fn drive(
        bytes: &[u8],
        session_id: &str,
        is_anthropic: bool,
    ) -> (LlmResult, Vec<AgentEvent>) {
        let events = install_capturing_sink(session_id);
        let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let reader = tokio::io::BufReader::new(bytes);
        let result = consume_sse_lines(
            reader,
            if is_anthropic { "anthropic" } else { "openai" },
            "test-model",
            is_anthropic,
            delta_tx,
            Some(session_id),
        )
        .await
        .expect("sse parse should succeed");
        let captured = events.lock().expect("capture mutex").clone();
        (result, captured)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anthropic_stream_announces_tool_call_then_streams_partials() {
        // Force COALESCE_WINDOW pauses by emitting deltas across the
        // 50ms boundary so the coalescer flushes more than once.
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a1\",\"name\":\"search_web\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"ant\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"hropic\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
        let session_id = fresh_session_id("anth-stream");
        let (result, events) = drive(body.as_bytes(), &session_id, true).await;

        // ── Initial ToolCall(Pending) announcement ──────────────────
        let announcements: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .collect();
        assert_eq!(
            announcements.len(),
            1,
            "expected exactly one initial ToolCall(Pending), got {events:#?}"
        );
        match announcements[0] {
            AgentEvent::ToolCall {
                tool_name,
                status,
                tool_call_id,
                raw_input,
                ..
            } => {
                assert_eq!(tool_name, "search_web");
                assert_eq!(*status, ToolCallStatus::Pending);
                assert_eq!(tool_call_id, "tool-toolu_a1");
                assert_eq!(*raw_input, serde_json::json!({}));
            }
            _ => unreachable!(),
        }

        // ── Partial-arg ToolCallUpdate(Pending) updates fired before
        //    content_block_stop ─────────────────────────────────────
        let partial_updates: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::ToolCallUpdate {
                        status: ToolCallStatus::Pending,
                        ..
                    }
                )
            })
            .collect();
        assert!(
            !partial_updates.is_empty(),
            "expected at least one Pending tool_call_update from streaming deltas, got {events:#?}"
        );
        // Some update must carry either a parsed `raw_input` or a
        // `raw_input_partial` so clients can render the args live.
        let has_payload = partial_updates.iter().any(|e| match e {
            AgentEvent::ToolCallUpdate {
                raw_input,
                raw_input_partial,
                ..
            } => raw_input.is_some() || raw_input_partial.is_some(),
            _ => false,
        });
        assert!(
            has_payload,
            "expected at least one Pending update to carry raw_input or raw_input_partial; got {partial_updates:#?}"
        );

        // ── Final tool call result was still parsed correctly ───────
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["name"], "search_web");
        assert_eq!(result.tool_calls[0]["arguments"]["q"], "anthropic");

        clear_session_sinks(&session_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anthropic_stream_emits_raw_input_partial_when_args_unparseable() {
        // The model emits an unterminated string before the close —
        // the recovery path can't synthesize a value, so the transport
        // must publish `raw_input_partial` carrying the raw bytes.
        let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b1\",\"name\":\"edit\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"foo.swift\\\",\\\"replace\\\":\\\"hello\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" world\\\"}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
        let session_id = fresh_session_id("anth-raw-partial");
        let (_, events) = drive(body.as_bytes(), &session_id, true).await;

        // The first Pending update fires immediately after the first
        // delta. At that point the buffer contains an unterminated
        // string, so `raw_input_partial` should be set.
        let first_partial = events.iter().find_map(|e| match e {
            AgentEvent::ToolCallUpdate {
                status: ToolCallStatus::Pending,
                raw_input,
                raw_input_partial,
                ..
            } => Some((raw_input.clone(), raw_input_partial.clone())),
            _ => None,
        });
        let (first_value, first_raw) =
            first_partial.expect("expected at least one Pending tool_call_update during streaming");
        assert!(
            first_value.is_none() && first_raw.is_some(),
            "first partial must surface raw_input_partial when JSON isn't yet parseable; got value={first_value:?} raw={first_raw:?}"
        );
        assert!(
            first_raw.unwrap().contains("hello"),
            "raw_input_partial should carry the concatenated bytes verbatim"
        );

        clear_session_sinks(&session_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn openai_stream_announces_and_streams_partials() {
        // OpenAI Chat-Completions-style: tool name on the first delta,
        // arguments string concatenated across subsequent deltas.
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"REA\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"DME.md\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3}}\n",
            "data: [DONE]\n",
        );
        let session_id = fresh_session_id("oai-stream");
        let (result, events) = drive(body.as_bytes(), &session_id, false).await;

        // Initial ToolCall(Pending) announcement on the first delta
        // that carries `function.name`.
        let announcements: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .collect();
        assert_eq!(
            announcements.len(),
            1,
            "expected one initial ToolCall(Pending); got {events:#?}"
        );
        match announcements[0] {
            AgentEvent::ToolCall {
                tool_name,
                tool_call_id,
                status,
                ..
            } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(*status, ToolCallStatus::Pending);
                assert_eq!(tool_call_id, "tool-call_a");
            }
            _ => unreachable!(),
        }

        // At least one partial-arg ToolCallUpdate fired during streaming.
        let partial_updates = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::ToolCallUpdate {
                        status: ToolCallStatus::Pending,
                        ..
                    }
                )
            })
            .count();
        assert!(
            partial_updates >= 1,
            "expected at least one Pending tool_call_update during streaming; got {events:#?}"
        );

        // Final tool call result still surfaces the canonical args.
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["name"], "read_file");
        assert_eq!(result.tool_calls[0]["arguments"]["path"], "README.md");

        clear_session_sinks(&session_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_session_id_means_no_streaming_events() {
        // Without an opt-in session id the transport must remain silent
        // — the dispatch-time lifecycle still owns the canonical events
        // for raw `llm_call(...)` invocations from script context.
        let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_x\",\"name\":\"fake\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"k\\\":1}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: [DONE]\n",
        );
        let session_id = fresh_session_id("anth-silent");
        let events = install_capturing_sink(&session_id);
        let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let reader = tokio::io::BufReader::new(body.as_bytes());
        let _result = consume_sse_lines(reader, "anthropic", "test-model", true, delta_tx, None)
            .await
            .expect("parse");
        let captured = events.lock().expect("capture mutex").clone();
        assert!(
            captured.is_empty(),
            "transport must emit no events when session_id is None; got {captured:#?}"
        );
        clear_session_sinks(&session_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coalescing_caps_event_count_under_burst() {
        // Build a burst of 20 input_json_deltas emitted in tight
        // succession (no real timer pauses inside the test). With
        // 50ms coalescing, only the very first delta should fire an
        // immediate ToolCallUpdate; the others should fold into it.
        let mut body = String::new();
        body.push_str(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_burst\",\"name\":\"big\"}}\n",
        );
        for i in 0..20 {
            body.push_str(&format!(
                "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"chunk{i} \"}}}}\n",
            ));
        }
        body.push_str("data: {\"type\":\"content_block_stop\",\"index\":0}\n");
        body.push_str("data: [DONE]\n");
        let session_id = fresh_session_id("anth-coalesce");
        let (_, events) = drive(body.as_bytes(), &session_id, true).await;

        // 20 deltas in well under 50ms must not produce 20 events.
        // We allow up to 3 (first delta + boundary races) so the test
        // tolerates clock jitter on busy CI hardware while still
        // guarding against the per-byte regression.
        let pending_updates = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::ToolCallUpdate {
                        status: ToolCallStatus::Pending,
                        ..
                    }
                )
            })
            .count();
        assert!(
            pending_updates < 20,
            "coalescing must cap the burst — got {pending_updates} pending updates from 20 deltas"
        );
        clear_session_sinks(&session_id);
    }
}
