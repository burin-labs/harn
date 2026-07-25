//! Shared LLM API transport: provider dispatch + request send + streaming
//! (SSE, NDJSON) and non-streaming response consumption. Provider-specific
//! request-body construction lives in `crate::llm::providers`; this file is
//! the wire-format layer below that.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus};
use crate::llm::capabilities::{should_use_responses_transport, WireDialect};
use crate::value::{VmError, VmValue};

use super::openai_normalize::{
    append_paragraph, debug_log_message_shapes, extract_openai_delta_field_str,
};
use super::options::{DeltaSender, LlmApiMode, LlmRequestPayload};
use super::partial_tool_args::{project_partial, DeltaCoalescer, PartialToolArgs};
use super::response::{
    billed_noncommittal_completion_error, empty_generation_error, extract_cache_read_tokens,
    extract_cache_write_tokens, is_billed_noncommittal_completion, parse_llm_response,
    parse_openai_tool_argument_json_values, CompletionContractSignals,
};
use super::result::{LlmResult, RawProviderToolCall};
use super::telemetry::{elapsed_ms, source as telemetry_source, ProviderTelemetry};
use super::thinking::ThinkingStreamSplitter;

mod capture;
mod ndjson;
mod sse;

use capture::{capture_stream_bytes, captured_stream_text, RawProviderCaptureTarget};

fn response_content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

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
        let (name, arguments) = crate::llm::tools::normalize_tool_call_shape(&name, arguments);
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
/// 1. Route providers declared with `protocol = "acp"` to the ACP adapter
/// 2. Check the thread-local provider registry (populated by `register_default_providers`)
/// 3. Fall back to config-based resolution (for dynamically-configured providers)
/// 4. Use the legacy inline dispatch as a final fallback
pub(super) async fn vm_call_llm_api(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;

    if crate::llm::providers::AcpProvider::is_configured_acp(provider) {
        return crate::llm::providers::AcpProvider::new(provider.clone())
            .chat_impl(opts, delta_tx)
            .await;
    }

    // Reject a structurally-broken route before any HTTP call. A thinking-enabled
    // Anthropic-family model resolved to the OpenAI-compatible transport is
    // billed-but-empty (Anthropic's compat surface never streams extended
    // thinking); erroring here surfaces the upstream provider-drop instead of
    // serving an empty completion far downstream (harn#3956). Valid routes are
    // dispatched byte-identically below. The `fake` test double never touches
    // the real transport, so it is exempt (`mock` already resolves to the
    // anthropic dialect for Claude ids and so never trips the guard).
    if !crate::llm::fake::FakeLlmProvider::should_intercept(provider) {
        crate::llm::route::Route::resolve(provider, &opts.model, &opts.thinking).map_err(
            |err| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(err.into_message()))),
        )?;
    }

    // Route explicit Responses requests through providers that advertise the
    // transport. OpenAI models that reject Chat Completions (such as Codex)
    // also select Responses implicitly through the model capability matrix.
    if should_use_responses_transport(
        provider,
        &opts.model,
        opts.api_mode == LlmApiMode::Responses,
    ) {
        return crate::llm::providers::OpenAiResponsesProvider::call(opts, delta_tx).await;
    }

    if crate::llm::provider::is_provider_registered(provider) {
        return dispatch_to_registered_provider(opts, delta_tx).await;
    }

    // Fallback for unregistered providers: dispatch by wire dialect. A single
    // capability lookup yields the typed dialect instead of two independent
    // predicate lookups that could disagree.
    let dialect = crate::llm::capabilities::lookup(provider, &opts.model).message_wire_format;

    if dialect.is_ollama() {
        return crate::llm::providers::OllamaProvider
            .chat_impl(opts, delta_tx)
            .await;
    }

    let body = if dialect.is_anthropic() {
        crate::llm::providers::AnthropicProvider::build_request_body(opts)
    } else {
        crate::llm::providers::OpenAiCompatibleProvider::build_request_body(opts, false)
    };

    vm_call_llm_api_with_body(opts, delta_tx, body, dialect).await
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

    let mock = crate::llm::providers::MockProvider;
    if mock.is_mock() && provider == mock.name() {
        return mock.chat_impl(opts, delta_tx).await;
    }

    if crate::llm::fake::FakeLlmProvider::should_intercept(provider) {
        return crate::llm::fake::FakeLlmProvider
            .chat_impl(opts, delta_tx)
            .await;
    }

    let ollama = crate::llm::providers::OllamaProvider;
    if (provider == ollama.name()
        || crate::llm::provider::provider_uses_ollama_messages(provider, &opts.model))
        && ollama.is_local()
    {
        return ollama.chat_impl(opts, delta_tx).await;
    }

    let gemini = crate::llm::providers::GeminiProvider;
    if provider == gemini.name() {
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

    if crate::llm::provider::provider_uses_anthropic_messages(provider, &opts.model) {
        let anthropic = crate::llm::providers::AnthropicProvider;
        return anthropic.chat_impl(opts, delta_tx).await;
    }

    crate::llm::providers::OpenAiCompatibleProvider::new(provider.clone())
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
    body: serde_json::Value,
    dialect: WireDialect,
) -> Result<LlmResult, VmError> {
    let started = Instant::now();
    let mut result = vm_call_llm_api_with_body_inner(opts, delta_tx, body, dialect).await?;
    // Reserved-token tool-call delimiter remap (single boundary).
    //
    // For models that reserve `<tool_call>`/`</tool_call>` as special tokens
    // (`reserved_tool_call_token` in capabilities.toml) the prompt is sent with
    // the delimiters swapped for a non-special wire form (`[[CALL]]`; see
    // `build_request_body` + `tool_delimiter`). The completion comes back in
    // that same wire form and MUST be mapped back to canonical before the
    // transcript and tagged tool-call parser ever see it.
    //
    // This lives in the shared transport funnel — not in `chat_impl` — so it
    // fires identically for every route into this function: the registered
    // OpenAI-compat path (`chat_impl`), the *unregistered* OpenAI-compat
    // fallback in `vm_call_llm_api` (e.g. a `llamacpp` provider configured via
    // providers.toml but never `provider_register`-ed), and both the streaming
    // (SSE/NDJSON) and non-streaming transports. Without this shared remap an
    // unregistered `llamacpp` qwen3.6 route can return raw `[[CALL]]` text, the
    // parser finds zero `<tool_call>` blocks, and the agent dispatches no tools
    // (convergence-fatal). The streamed live deltas are canonicalized
    // separately by `canonicalizing_delta_tx`; this remaps the assembled
    // `result.text` that the parser/transcript consume.
    if crate::llm::capabilities::lookup(&opts.provider, &opts.model).reserved_tool_call_token {
        let wire_open = result.text.matches("[[CALL]]").count();
        result.text = crate::llm::tool_delimiter::wire_to_canonical(&result.text);
        let canon_open = result.text.matches("<tool_call>").count();
        tracing::debug!(
            target: "harn::llm::tool_delimiter",
            provider = %opts.provider,
            model = %opts.model,
            wire_open_markers = wire_open,
            canonical_open_blocks = canon_open,
            "reserved-token wire->canonical remap applied to assembled completion",
        );
    }
    // Preserve a per-call wall clock regardless of provider. Server-side
    // timings (when available) cover only the model's view; client_wall_ms
    // captures network + streaming overhead the server cannot see, so eval
    // dashboards can decompose total latency end-to-end.
    if result.telemetry.client_wall_ms.is_none() {
        result.telemetry.client_wall_ms = Some(elapsed_ms(started));
    }
    if result.telemetry.source.is_empty() {
        result.telemetry.source = telemetry_source::UNKNOWN.to_string();
    }
    Ok(result)
}

async fn vm_call_llm_api_with_body_inner(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    mut body: serde_json::Value,
    dialect: WireDialect,
) -> Result<LlmResult, VmError> {
    // Derive the transport-shape booleans once from the single typed dialect.
    // The `(true, true)` state was never valid; a single `WireDialect` makes
    // it unrepresentable and removes the re-derivation that used to happen at
    // the response-parse boundary below.
    let is_anthropic_style = dialect.is_anthropic();
    let is_ollama = dialect.is_ollama();
    let provider = &opts.provider;
    let model = &opts.model;
    let raw_capture_context = crate::llm::agent_observe::current_raw_provider_capture_context();
    let wants_streaming = delta_tx.is_some() && opts.stream;
    // Whether this request offered any tools to the model. Used by the
    // billed-no-op contract guard so a deliberately terse text answer to a
    // tool-less prompt is never misclassified as a missing tool call.
    let tools_offered = body
        .get("tools")
        .and_then(|tools| tools.as_array())
        .map(|tools| !tools.is_empty())
        .unwrap_or(false);

    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);
    let caps = crate::llm::capabilities::lookup(provider, model);
    let use_stream_transport = if is_ollama && !opts.stream {
        crate::events::log_warn(
            "llm",
            "stream=false is not supported by Ollama, using streaming",
        );
        true
    } else if caps.requires_streaming && !opts.stream {
        crate::events::log_warn(
            "llm",
            &format!("{provider} model {model} requires streaming, using stream=true"),
        );
        true
    } else {
        wants_streaming || is_ollama || caps.requires_streaming
    };

    if !is_ollama {
        crate::llm::provider::apply_provider_wire_overrides(
            &mut body,
            opts.provider_overrides.as_ref(),
        );
    }
    if is_anthropic_style {
        crate::llm::providers::anthropic::reconcile_request_body(&mut body, model, &opts.thinking);
    }
    if provider == "openrouter"
        && (body.get("response_format").is_some() || body.get("top_k").is_some())
    {
        crate::llm::providers::openai_compat::ensure_openrouter_require_parameters(&mut body);
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
        crate::llm::streaming_client_for_base_url(&resolved.base_url)
    } else {
        crate::llm::blocking_client_for_base_url(&resolved.base_url)
    };

    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(opts.resolve_timeout()))
        .json(&body);
    let mut req = resolved.apply_headers(req, &opts.api_key);
    if is_anthropic_style && !opts.anthropic_beta_features.is_empty() {
        req = req.header("anthropic-beta", opts.anthropic_beta_features.join(","));
    }

    if use_stream_transport {
        let tx = if let Some(tx) = delta_tx {
            tx
        } else {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            tx
        };
        let max_attempts = if is_ollama { 2 } else { 1 };
        let unload_grace = if is_ollama {
            crate::llm::api::ollama_unload_grace_duration_from_env()
        } else {
            Duration::ZERO
        };
        let mut ollama_warmup_gate = false;
        for attempt in 0..max_attempts {
            crate::llm::agent_observe::persist_raw_provider_request(
                raw_capture_context.as_ref(),
                provider,
                model,
                dialect.as_str(),
                Some(attempt),
                &body,
            );
            let req = client
                .post(resolved.url())
                .header("Content-Type", "application/json")
                .timeout(std::time::Duration::from_secs(opts.resolve_timeout()))
                .json(&body);
            let mut req = resolved.apply_headers(req, &opts.api_key);
            if is_anthropic_style && !opts.anthropic_beta_features.is_empty() {
                req = req.header("anthropic-beta", opts.anthropic_beta_features.join(","));
            }
            let response = send_stream_request_with_ollama_warmup(
                req,
                provider,
                model,
                is_ollama,
                unload_grace,
                &mut ollama_warmup_gate,
            )
            .await?;
            if !response.status().is_success() {
                let status = response.status();
                let retry_after = super::retry_after_header(response.headers());
                let content_type = response_content_type(&response);
                let body = response.text().await.unwrap_or_default();
                crate::llm::agent_observe::persist_raw_provider_response(
                    raw_capture_context.as_ref(),
                    provider,
                    model,
                    "stream-error",
                    Some(attempt),
                    status.as_u16(),
                    content_type.as_deref(),
                    &body,
                );
                let msg = classify_transport_http_error(
                    provider,
                    status,
                    retry_after.as_deref(),
                    &body,
                    is_anthropic_style,
                    is_ollama,
                );
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
            }
            let is_sse = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("text/event-stream"));
            // Build a fresh schema watch per attempt so an Ollama-retry
            // restart doesn't see chunks from the previous run.
            let schema_watch = super::schema_stream::StreamSchemaWatch::from_payload(opts);
            if is_sse {
                return vm_call_llm_api_sse_from_response(
                    response,
                    provider,
                    model,
                    is_anthropic_style,
                    tx,
                    opts.session_id.as_deref(),
                    schema_watch,
                    tools_offered,
                    RawProviderCaptureTarget::new(raw_capture_context.clone(), Some(attempt)),
                )
                .await;
            }
            match vm_call_llm_api_ndjson_from_response(
                response,
                provider,
                model,
                tx.clone(),
                unload_grace,
                &mut ollama_warmup_gate,
                schema_watch,
                RawProviderCaptureTarget::new(raw_capture_context.clone(), Some(attempt)),
            )
            .await
            {
                // A `done_reason == "length"` truncation returns Ok with an
                // empty body and `stop_reason: Some("length")`, bypassing the
                // retry guard below. A deterministic token-cap cut would just
                // re-truncate on every retry. Only the genuine empty-content
                // parser bug (done_reason stop/absent) is retried.
                Ok(result) => return Ok(result),
                Err(err)
                    if is_ollama
                        && attempt + 1 < max_attempts
                        && is_ollama_empty_content_parser_bug(&err) =>
                {
                    crate::events::log_warn(
                        "llm",
                        &format!(
                            "ollama model {model} returned empty content with eval_count; retrying once"
                        ),
                    );
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        unreachable!("streaming LLM attempt loop exhausted without returning");
    }

    crate::llm::agent_observe::persist_raw_provider_request(
        raw_capture_context.as_ref(),
        provider,
        model,
        dialect.as_str(),
        None,
        &body,
    );
    let response = req
        .send()
        .await
        .map_err(|e| non_stream_send_error(provider, e))?;

    // Check HTTP status BEFORE parsing the body as LLM response, or error
    // responses (e.g. vLLM "prompt too long" 400) silently become malformed
    // parse results and the agent loop retries against the same bad context.
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = super::retry_after_header(response.headers());
        let content_type = response_content_type(&response);
        let body = response.text().await.unwrap_or_default();
        crate::llm::agent_observe::persist_raw_provider_response(
            raw_capture_context.as_ref(),
            provider,
            model,
            "json",
            None,
            status.as_u16(),
            content_type.as_deref(),
            &body,
        );
        let msg = classify_transport_http_error(
            provider,
            status,
            retry_after.as_deref(),
            &body,
            is_anthropic_style,
            is_ollama,
        );
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
    }

    let status = response.status();
    let content_type = response_content_type(&response);
    let body = response.text().await.map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "{provider} response parse error: {e}"
        ))))
    })?;
    crate::llm::agent_observe::persist_raw_provider_response(
        raw_capture_context.as_ref(),
        provider,
        model,
        "json",
        None,
        status.as_u16(),
        content_type.as_deref(),
        &body,
    );
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "{provider} response parse error: {e}"
        ))))
    })?;

    // Reuse the dialect resolved for this dispatch instead of re-looking it up
    // (the previous re-lookup passed the same `(provider, model)` and could
    // only ever agree with `is_anthropic_style` above).
    parse_llm_response(&json, provider, model, is_anthropic_style, tools_offered)
}

use ndjson::{
    emit_ollama_warmup_progress, is_ollama_empty_content_parser_bug,
    vm_call_llm_api_ndjson_from_response,
};
use sse::{
    non_stream_send_error, send_stream_request_with_ollama_warmup,
    vm_call_llm_api_sse_from_response,
};

#[cfg(test)]
mod schema_stream_abort_tests;
#[cfg(test)]
mod sse_read_error_tests;
#[cfg(test)]
mod streaming_tool_call_tests;
#[cfg(test)]
mod tests;
