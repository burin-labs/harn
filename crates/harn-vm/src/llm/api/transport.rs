//! Shared LLM API transport: provider dispatch + request send + streaming
//! (SSE, NDJSON) and non-streaming response consumption. Provider-specific
//! request-body construction lives in `crate::llm::providers`; this file is
//! the wire-format layer below that.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus};
use crate::llm::capabilities::should_use_responses_transport;
use crate::value::{VmError, VmValue};

use super::openai_normalize::{
    append_paragraph, debug_log_message_shapes, extract_openai_delta_field_str,
    parse_openai_tool_argument_json_values, parse_tool_arguments,
};
use super::options::{DeltaSender, LlmApiMode, LlmRequestPayload};
use super::partial_tool_args::{project_partial, DeltaCoalescer, PartialToolArgs};
use super::response::{
    billed_noncommittal_completion_error, empty_generation_error, extract_cache_read_tokens,
    extract_cache_write_tokens, is_billed_noncommittal_completion, CompletionContractSignals,
    ProviderResponseEnvelope,
};
use super::result::{LlmResult, RawProviderToolCall};
use super::telemetry::{elapsed_ms, source as telemetry_source, ProviderTelemetry};
use super::thinking::ThinkingStreamSplitter;
use super::{DialectContract, StreamProtocol};

mod blocks;
mod capture;
mod liveness;
mod ndjson;
mod response_envelope;
mod sse;

pub(crate) use liveness::premature_eof;

use capture::{capture_stream_bytes, captured_stream_text, RawProviderCaptureTarget};

fn response_content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn response_header_id(response: &reqwest::Response, name: &'static str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(crate::egress::redact_diagnostic_text)
        .map(|value| crate::text::truncate_end(&value, 256))
}

#[derive(Debug)]
struct NonStreamResponseBody {
    status: reqwest::StatusCode,
    content_type: Option<String>,
    body: String,
}

async fn read_non_stream_response_body(
    response: reqwest::Response,
    raw_capture_context: Option<&crate::llm::agent_observe::RawProviderCaptureContext>,
    provider: &str,
    model: &str,
) -> Result<NonStreamResponseBody, VmError> {
    let status = response.status();
    let content_type = response_content_type(&response);
    let request_id = response_header_id(&response, "x-request-id");
    let generation_id = response_header_id(&response, "x-generation-id");
    match response.text().await {
        Ok(body) => Ok(NonStreamResponseBody {
            status,
            content_type,
            body,
        }),
        Err(error) => {
            let error = non_stream_body_error(provider, error);
            crate::llm::agent_observe::persist_raw_provider_response_failure(
                raw_capture_context,
                provider,
                model,
                crate::llm::agent_observe::RawProviderResponseFailureCapture {
                    transport: "json",
                    attempt: None,
                    status: status.as_u16(),
                    content_type: content_type.as_deref(),
                    request_id: request_id.as_deref(),
                    generation_id: generation_id.as_deref(),
                    error: &error,
                },
            );
            Err(error)
        }
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
        let arguments = parse_tool_arguments(function.get("arguments"));
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
    let resolved = crate::llm::helpers::ResolvedProvider::resolve(&opts.provider);
    let mut result = vm_call_llm_api_inner(opts, delta_tx).await?;
    result.telemetry.serving_base_url = resolved.telemetry_base_url();
    let declaration = resolved.cache_accounting_declaration();
    result.telemetry.cache_accounting_declared = declaration;
    // Only a declared-`false` route zeroes the parsed cache fields: those
    // entries exist precisely because the route reports nothing and a zero is
    // intentional. An undeclared route keeps whatever the response mapping
    // parsed — collapsing absent into false here silently destroyed cache
    // telemetry the provider actually reported.
    if declaration == Some(false) {
        result.cache_read_tokens = 0;
        result.cache_write_tokens = 0;
        result.cache_supported = false;
    }
    Ok(result)
}

async fn vm_call_llm_api_inner(
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
        let (capability_provider, capability_model) =
            crate::llm::managed_supply::logical_route(provider, &opts.model)?;
        crate::llm::route::Route::resolve(&capability_provider, &capability_model, &opts.thinking)
            .map_err(|err| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(err.into_message())))
            })?;
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
    //
    // Exhaustive on purpose. This used to be an `if is_ollama / else if
    // is_anthropic / else OpenAI-compat` chain, which quietly sent a
    // Gemini-dialect route's OpenAI-shaped body to the Gemini base URL instead
    // of failing. A `match` makes a new dialect a compile error here.
    let dialect = DialectContract::for_request(opts);
    let body = match dialect.stream_protocol() {
        StreamProtocol::OllamaNdjson => {
            return crate::llm::providers::OllamaProvider
                .chat_impl(opts, delta_tx)
                .await;
        }
        // Owns both Gemini live endpoint families; see `providers::gemini`.
        StreamProtocol::GeminiJson | StreamProtocol::GeminiInteractionsSse => {
            return crate::llm::providers::GeminiProvider
                .chat_impl(opts, delta_tx)
                .await;
        }
        StreamProtocol::AnthropicSse | StreamProtocol::OpenAiSse => {
            dialect.build_request_body(opts)
        }
    };

    vm_call_llm_api_with_body(opts, delta_tx, body, dialect).await
}

/// Dispatch to a registered provider by name.
///
/// Mock and enterprise transports keep their concrete auth envelopes. Every
/// ordinary registered route selects its provider-wire adapter from the same
/// dialect contract used for request and response semantics.
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

    match DialectContract::for_request(opts).stream_protocol() {
        StreamProtocol::OllamaNdjson => {
            crate::llm::providers::OllamaProvider
                .chat_impl(opts, delta_tx)
                .await
        }
        StreamProtocol::GeminiJson | StreamProtocol::GeminiInteractionsSse => {
            crate::llm::providers::GeminiProvider
                .chat_impl(opts, delta_tx)
                .await
        }
        StreamProtocol::AnthropicSse => {
            crate::llm::providers::AnthropicProvider
                .chat_impl(opts, delta_tx)
                .await
        }
        StreamProtocol::OpenAiSse => {
            crate::llm::providers::OpenAiCompatibleProvider::new(provider.clone())
                .chat_impl(opts, delta_tx)
                .await
        }
    }
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
    dialect: DialectContract,
) -> Result<LlmResult, VmError> {
    let started = Instant::now();
    // Same origin as `started`, in `tokio` form so the first-frame stamp is
    // subtractable from `client_wall_ms` and so virtual-time tests can advance
    // it. Both therefore span the whole call including any retried attempts.
    let request_origin = tokio::time::Instant::now();
    // Provider data controls resolve once, here, so the body half, the header
    // half, and the receipt describing them come from one plan and cannot
    // disagree. Under the `default` posture the plan is empty and the request
    // is untouched. The writes land inside, after every other body mutation.
    let data_controls = crate::llm::api::data_controls::resolve(
        &opts.provider,
        crate::llm::api::data_controls::dialect_of(dialect.stream_protocol()),
        opts.data_controls,
    );
    let data_controls_receipt = data_controls.receipt.clone();
    let mut result = vm_call_llm_api_with_body_inner(
        opts,
        delta_tx,
        body,
        dialect,
        request_origin,
        &data_controls,
    )
    .await;
    // The receipt describes what Harn sent, so it must survive a provider
    // error: "we asked for the strict posture and the call failed" is a
    // different fact from "we never asked".
    if let Ok(result) = result.as_mut() {
        result.telemetry.data_controls = Some(Box::new(data_controls_receipt));
    }
    let mut result = result?;
    crate::llm::managed_supply::apply_terminal_receipt(&mut result, &opts.provider, &opts.model)?;
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
    if crate::llm::managed_supply::capabilities_for(&opts.provider, &opts.model)
        .reserved_tool_call_token
    {
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
    dialect: DialectContract,
    request_origin: tokio::time::Instant,
    data_controls: &crate::llm::api::data_controls::DataControlsPlan,
) -> Result<LlmResult, VmError> {
    let stream_protocol = dialect.stream_protocol();
    let provider = &opts.provider;
    let model = &opts.model;
    crate::llm::managed_supply::attach_request_extension(&mut body, provider, model)?;
    let raw_capture_context = crate::llm::agent_observe::current_raw_provider_capture_context();
    // `stream` selects the provider transport. A delta receiver only decides
    // whether a caller observes incremental text; probe calls intentionally
    // collect the same stream without one.
    let wants_streaming = opts.stream;
    // Whether this request offered any tools to the model. Used by the
    // billed-no-op contract guard so a deliberately terse text answer to a
    // tool-less prompt is never misclassified as a missing tool call.
    let tools_offered = body
        .get("tools")
        .and_then(|tools| tools.as_array())
        .map(|tools| !tools.is_empty())
        .unwrap_or(false);

    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);
    let caps = crate::llm::managed_supply::capabilities_for(provider, model);
    let use_stream_transport = if stream_protocol == StreamProtocol::OllamaNdjson && !opts.stream {
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
        wants_streaming
            || stream_protocol == StreamProtocol::OllamaNdjson
            || caps.requires_streaming
    };

    if stream_protocol != StreamProtocol::OllamaNdjson {
        crate::llm::provider::apply_provider_wire_overrides(
            &mut body,
            opts.provider_overrides.as_ref(),
        );
    }
    if stream_protocol == StreamProtocol::AnthropicSse {
        crate::llm::providers::anthropic::reconcile_request_body(
            &mut body,
            model,
            &opts.thinking,
            opts.provider_contract_probe,
        );
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

    dialect.apply_stream_transport_fields(
        &mut body,
        provider,
        &resolved.endpoint,
        use_stream_transport,
    );

    // Last write wins, deliberately. A declared retention/training control
    // must survive the caller's `provider_overrides` escape hatch, or the
    // receipt would claim a control the wire does not carry.
    data_controls.write_body(&mut body);

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
    for (name, value) in &data_controls.headers {
        req = req.header(name.as_str(), value.as_str());
    }
    if stream_protocol == StreamProtocol::AnthropicSse && !opts.anthropic_beta_features.is_empty() {
        req = req.header("anthropic-beta", opts.anthropic_beta_features.join(","));
    }

    if use_stream_transport {
        let tx = if let Some(tx) = delta_tx {
            tx
        } else {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            tx
        };
        // Observed calls own every recovery. Retrying here would hide a native
        // response from the canonical provider-attempt ledger before the
        // observer can retain its measured usage.
        let attempt = 0;
        let unload_grace = if stream_protocol == StreamProtocol::OllamaNdjson {
            crate::llm::api::ollama_unload_grace_duration_from_env()
        } else {
            Duration::ZERO
        };
        let mut ollama_warmup_gate = false;
        crate::llm::agent_observe::persist_raw_provider_request(
            raw_capture_context.as_ref(),
            provider,
            model,
            dialect.wire().as_str(),
            Some(attempt),
            &body,
        );
        let response = send_stream_request_with_ollama_warmup(
            req,
            provider,
            model,
            stream_protocol,
            unload_grace,
            &mut ollama_warmup_gate,
        )
        .await?;
        if !response.status().is_success() {
            let status = response.status();
            let headers = response.headers().clone();
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
            return Err(super::provider_http_error(
                Some(dialect),
                provider,
                status,
                &headers,
                &body,
            ));
        }
        let provider_request_id = response_header_id(&response, "x-generation-id")
            .or_else(|| response_header_id(&response, "x-request-id"));
        // The outer observer creates a new transport attempt for each retry.
        // This watcher therefore belongs to this one physical request.
        let schema_watch = super::schema_stream::StreamSchemaWatch::from_payload(opts);
        let deadline_policy = liveness::StreamDeadlinePolicy::from_payload(opts);
        match stream_protocol {
            StreamProtocol::AnthropicSse | StreamProtocol::OpenAiSse => {
                return vm_call_llm_api_sse_from_response(
                    response,
                    provider,
                    model,
                    dialect,
                    tx,
                    opts.session_id.as_deref(),
                    schema_watch,
                    tools_offered,
                    deadline_policy,
                    RawProviderCaptureTarget::new(raw_capture_context.clone(), Some(attempt)),
                    provider_request_id.as_deref(),
                    request_origin,
                )
                .await;
            }
            StreamProtocol::OllamaNdjson => {}
            StreamProtocol::GeminiJson | StreamProtocol::GeminiInteractionsSse => {
                unreachable!("Gemini streaming is owned by the Gemini provider transport")
            }
        }
        return vm_call_llm_api_ndjson_from_response(
            response,
            provider,
            model,
            tx.clone(),
            unload_grace,
            &mut ollama_warmup_gate,
            schema_watch,
            deadline_policy,
            RawProviderCaptureTarget::new(raw_capture_context.clone(), Some(attempt)),
            request_origin,
        )
        .await;
    }

    crate::llm::agent_observe::persist_raw_provider_request(
        raw_capture_context.as_ref(),
        provider,
        model,
        dialect.wire().as_str(),
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
        let headers = response.headers().clone();
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
        return Err(super::provider_http_error(
            Some(dialect),
            provider,
            status,
            &headers,
            &body,
        ));
    }

    let response =
        read_non_stream_response_body(response, raw_capture_context.as_ref(), provider, model)
            .await?;
    crate::llm::agent_observe::persist_raw_provider_response(
        raw_capture_context.as_ref(),
        provider,
        model,
        "json",
        None,
        response.status.as_u16(),
        response.content_type.as_deref(),
        &response.body,
    );
    let json: serde_json::Value = serde_json::from_str(&response.body).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "{provider} response parse error: {e}"
        ))))
    })?;

    // Reuse the complete contract resolved for this dispatch instead of
    // re-looking up any response-shape predicates.
    dialect.parse_response(&json, opts, tools_offered)
}

use ndjson::{emit_ollama_warmup_progress, vm_call_llm_api_ndjson_from_response};
use sse::{
    non_stream_body_error, non_stream_send_error, send_stream_request_with_ollama_warmup,
    vm_call_llm_api_sse_from_response,
};

#[cfg(test)]
mod dialect_golden_stream_tests;
#[cfg(test)]
mod liveness_tests;
#[cfg(test)]
mod non_stream_body_error_tests;
#[cfg(test)]
mod schema_stream_abort_tests;
#[cfg(test)]
mod sse_read_error_tests;
#[cfg(test)]
mod sse_telemetry_tests;
#[cfg(test)]
mod stream_block_coalesce_tests;
#[cfg(test)]
mod streaming_tool_call_tests;
#[cfg(test)]
mod tests;
