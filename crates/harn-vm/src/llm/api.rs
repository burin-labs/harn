//! LLM API entry points and re-exports. The transport layer, request/
//! response parsing, auth, context-window discovery, and option/result
//! types each live in their own submodule under [`self`]; this file only
//! wires them together and hosts the `vm_call_llm_full*` chat entry
//! points that provider-specific completion / agent paths dispatch into.

mod auth;
mod completion;
mod context_window;
mod dialect;
mod errors;
mod ollama;
mod openai_normalize;
pub(crate) mod options;
mod partial_tool_args;
mod response;
pub(crate) mod result;
mod schema_stream;
mod telemetry;
mod thinking;
mod transport;

use crate::value::{ErrorCategory, VmError, VmValue};

use super::mock::{
    fixture_hash_for_request, get_replay_mode, load_fixture, mock_llm_response,
    record_cli_llm_result, save_fixture, LlmReplayMode,
};

// ─── Public surface (crate-wide) ────────────────────────────────────────

pub(crate) use auth::apply_auth_headers;
pub(crate) use completion::vm_call_completion_full;
pub use context_window::fetch_provider_max_context;
pub(crate) use dialect::{DialectContract, StreamProtocol};
pub(crate) use errors::{
    classify_llm_error, classify_provider_stream_error, err_for_non_success,
    err_for_non_success_with_dialect, parse_retry_after_value, provider_http_error,
    retry_after_header, LlmErrorInfo, LlmErrorKind, LlmErrorReason,
};
pub(crate) use ollama::apply_ollama_runtime_settings;
pub(crate) use ollama::ollama_unload_grace_duration_from_env;
pub use ollama::{
    normalize_ollama_keep_alive, ollama_readiness, ollama_runtime_settings_from_env,
    warm_ollama_model, warm_ollama_model_with_settings, OllamaReadinessOptions,
    OllamaReadinessResult, OllamaRuntimeSettings, OllamaWarmupResult, HARN_OLLAMA_KEEP_ALIVE_ENV,
    HARN_OLLAMA_NUM_CTX_ENV, OLLAMA_DEFAULT_KEEP_ALIVE, OLLAMA_DEFAULT_NUM_CTX, OLLAMA_HOST_ENV,
};
pub(crate) use openai_normalize::normalize_openai_style_messages;
pub(crate) use options::{
    push_unique_anthropic_beta_feature, DeltaSender, LlmApiMode, LlmCallOptions, LlmRequestPayload,
    LlmRouteAlternative, LlmRouteFallback, LlmRoutePolicy, LlmRoutingDecision, OutputFormat,
    PromptCacheTtl, ReasoningEffort, ReminderLifecycleEmission, ThinkingConfig, ToolSearchConfig,
    ToolSearchMode, ToolSearchVariant,
};
#[cfg(test)]
pub(crate) use response::empty_generation_error;
#[cfg(test)]
pub(crate) use response::parse_llm_response as parse_llm_response_for_provider;
pub(crate) use response::{
    extract_cache_read_tokens, extract_cache_write_tokens, parse_openai_responses_response,
};
#[cfg(test)]
pub(crate) use result::test_text_projection;
pub(crate) use result::{
    build_llm_text_projection, ensure_llm_text_projection, parse_candidate_text_tools,
    parse_text_tools_with_harn, vm_build_llm_result, LlmResult, LlmTextProjection,
    ProviderAttempts, RawProviderToolCall,
};
pub(crate) use schema_stream::{
    aborted_result_value as schema_stream_aborted_result_value, parse_schema_stream_abort,
    SchemaStreamAbort, StreamSchemaWatch,
};
pub(crate) use telemetry::elapsed_ms;
pub use telemetry::{source as telemetry_source, OllamaPsModel, ProviderTelemetry};
pub(crate) use thinking::{split_openai_thinking_blocks, ThinkingStreamSplitter};
pub(crate) use transport::premature_eof as premature_stream_eof;
pub(crate) use transport::vm_call_llm_api_with_body;

use transport::vm_call_llm_api;

/// Resolve the transport required by a native-tool request.
///
/// An omitted reasoning option preserves the provider's model default. It is
/// not the same as an explicit `effort: "none"` request.
pub(crate) fn effective_tool_api_mode(
    requested: LlmApiMode,
    provider: &str,
    caps: &crate::llm::capabilities::Capabilities,
    thinking: &ThinkingConfig,
    has_native_tools: bool,
) -> LlmApiMode {
    if caps.chat_completions_unsupported
        || (provider == "openai"
            && caps.reasoning_tools_require_responses
            && has_native_tools
            && !matches!(
                thinking,
                ThinkingConfig::Effort {
                    level: ReasoningEffort::None
                }
            ))
    {
        LlmApiMode::Responses
    } else {
        requested
    }
}

/// Send one already-normalized request through Harn's real provider adapter.
/// Provider probes use this boundary so they cannot drift into a second set of
/// endpoint, auth, request, streaming, and response rules.
pub(crate) async fn probe_llm_request(request: &LlmRequestPayload) -> Result<LlmResult, VmError> {
    vm_call_llm_api(request, None).await
}

#[derive(Debug, Clone)]
struct OffthreadLlmError {
    message: String,
    category: Option<ErrorCategory>,
    stream_failure: Option<Box<crate::value::ProviderStreamFailure>>,
    thrown: Option<VmValue>,
}

impl OffthreadLlmError {
    fn from_vm_error(err: VmError) -> Self {
        match err {
            VmError::ProviderStreamFailure(failure) => Self {
                message: failure.to_string(),
                category: Some(failure.category()),
                stream_failure: Some(failure),
                thrown: None,
            },
            VmError::CategorizedError { message, category } => Self {
                message,
                category: Some(category),
                stream_failure: None,
                thrown: None,
            },
            VmError::Thrown(VmValue::String(message)) => {
                Self::from_display_message(message.to_string())
            }
            VmError::Thrown(value) => Self {
                message: value.display(),
                category: None,
                stream_failure: None,
                thrown: Some(value),
            },
            other => Self::from_display_message(other.to_string()),
        }
    }

    fn from_display_message(message: String) -> Self {
        if let Some((category, stripped)) = parse_displayed_categorized_error(&message) {
            return Self {
                message: stripped.to_string(),
                category: Some(category),
                stream_failure: None,
                thrown: None,
            };
        }
        Self {
            message,
            category: None,
            stream_failure: None,
            thrown: None,
        }
    }

    fn into_vm_error(self) -> VmError {
        if let Some(failure) = self.stream_failure {
            return VmError::ProviderStreamFailure(failure);
        }
        match self.category {
            Some(category) => VmError::CategorizedError {
                message: self.message,
                category,
            },
            None => self.thrown.map_or_else(
                || VmError::Thrown(VmValue::String(arcstr::ArcStr::from(self.message))),
                VmError::Thrown,
            ),
        }
    }
}

fn parse_displayed_categorized_error(message: &str) -> Option<(ErrorCategory, &str)> {
    let body = message.strip_prefix("Error [")?;
    let (category, rest) = body.split_once("]: ")?;
    Some((ErrorCategory::parse(category), rest))
}

/// Route a logical call when policy is present. The boxed boundary breaks the
/// intentional async cycle: routing executes links through observability, which
/// reaches the explicit single-route primitives after clearing the policy.
fn routed_llm_call<'a>(
    opts: &'a LlmCallOptions,
    delta_tx: Option<DeltaSender>,
) -> Option<impl std::future::Future<Output = Result<LlmResult, VmError>> + 'a> {
    let policy = opts.routing_policy.as_ref()?;
    Some(async move {
        Box::pin(super::routing::execute_with_routing(
            policy,
            opts.clone(),
            None,
            delta_tx,
        ))
        .await
        .map(|(result, _trace)| result)
    })
}

/// Execute a logical LLM call. A configured routing policy runs first; each
/// routed link re-enters the observed single-route path with its policy
/// cleared. Calls without routing enter that same observation boundary
/// directly, so no logical entry point can dispatch an unjournalled provider
/// attempt.
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, None) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts, None, None, None, false, false, None, None,
    ))
    .await
}

/// Execute exactly one prepared provider/model route. Observability calls this
/// primitive after it emits the request receipt. The unforgeable token keeps
/// every other caller on the logical, observed entry points above.
pub(crate) async fn vm_call_llm_full_single_route_prepared(
    _observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut first_token = super::first_token::FirstTokenTimer::for_current_span();
    let mut deltas_open = true;
    let mut call = Box::pin(vm_call_llm_full_inner_request(request, Some(delta_tx)));
    let result = loop {
        tokio::select! {
            maybe_delta = delta_rx.recv(), if deltas_open => {
                match maybe_delta {
                    Some(_) => first_token.observe_delta(),
                    None => deltas_open = false,
                }
            }
            result = &mut call => break result?,
        }
    };
    while delta_rx.try_recv().is_ok() {
        first_token.observe_delta();
    }
    super::cost::record_llm_usage(&result)?;
    Ok(result)
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, Some(delta_tx.clone())) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts,
        None,
        None,
        None,
        false,
        false,
        None,
        Some(delta_tx),
    ))
    .await
}

pub(crate) async fn vm_call_llm_full_streaming_single_route_prepared(
    _observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let result = vm_call_llm_full_inner_request(request, Some(delta_tx)).await?;
    super::cost::record_llm_usage(&result)?;
    Ok(result)
}

/// Execute provider I/O on Tokio's multithreaded scheduler while keeping
/// VM-local values and transcript assembly on the caller's LocalSet.
#[cfg(test)]
pub(crate) async fn vm_call_llm_full_streaming_offthread(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, Some(delta_tx.clone())) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts,
        None,
        None,
        None,
        false,
        true,
        None,
        Some(delta_tx),
    ))
    .await
}

pub(crate) async fn vm_call_llm_full_streaming_offthread_single_route_prepared(
    _observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: LlmRequestPayload,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let cached = super::trigger_predicate::lookup_cached_result(&request).is_some();
    let intercepted = crate::llm::providers::MockProvider::should_intercept_request(&request)
        || crate::llm::fake::FakeLlmProvider::should_intercept(&request.provider);
    let replay_mode = get_replay_mode();
    if !cached && !intercepted && replay_mode == LlmReplayMode::Replay {
        let hash = fixture_hash_for_request(&request);
        if load_fixture(&hash).is_none() {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("No fixture found for LLM call (hash: {hash}). Run with --record first."),
            ))));
        }
    }
    if !cached && !intercepted && replay_mode != LlmReplayMode::Replay {
        super::ensure_real_llm_allowed(&request.provider)?;
    }
    request.emit_reminder_lifecycle();
    let raw_capture_context = crate::llm::agent_observe::current_raw_provider_capture_context();
    let result = tokio::task::spawn(crate::orchestration::scope_inline_subtask(async move {
        if let Some(context) = raw_capture_context {
            crate::llm::agent_observe::with_raw_provider_capture_context(context, async {
                vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await
            })
            .await
        } else {
            vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await
        }
    }))
    .await
    .map_err(|join_err| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "llm_call background task failed: {join_err}"
        ))))
    })?
    .map_err(OffthreadLlmError::into_vm_error)?;
    super::cost::record_llm_usage(&result)?;
    Ok(result)
}

async fn vm_call_llm_full_inner_request(
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    if let Some(result) = super::trigger_predicate::lookup_cached_result(request) {
        request.emit_reminder_lifecycle();
        record_cli_llm_result(request, &result);
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        return Ok(result);
    }

    if crate::llm::providers::MockProvider::should_intercept_request(request) {
        request.emit_reminder_lifecycle();
        let result = mock_llm_response(request)?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(request, &result);
        if let Some(tx) = delta_tx {
            // A mock may script an ordered chunk sequence to emulate a real
            // token stream; otherwise fall back to a single full-text delta so
            // streaming callers still see the visible text (the graceful
            // non-streaming path). `stream_chunks.concat() == result.text`.
            if let Some(chunks) = super::mock::take_mock_stream_chunks() {
                for chunk in chunks {
                    let _ = tx.send(chunk);
                }
                return Ok(result);
            }
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
            return Ok(result);
        }
        return Ok(result);
    }

    if crate::llm::fake::FakeLlmProvider::should_intercept(&request.provider) {
        // Bypass fixture/replay so the script-driven fake never collides
        // with HARN_LLM_REPLAY/RECORD being set from an outer harness.
        request.emit_reminder_lifecycle();
        let result = crate::llm::fake::FakeLlmProvider
            .chat_impl(request, delta_tx)
            .await?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(request, &result);
        return Ok(result);
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash_for_request(request);

    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            request.emit_reminder_lifecycle();
            super::trigger_predicate::note_result(request, &result);
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("No fixture found for LLM call (hash: {hash}). Run with --record first."),
        ))));
    }

    super::ensure_real_llm_allowed(&request.provider)?;
    request.emit_reminder_lifecycle();

    // Provider/model failover is owned by `routing::execute_with_routing`.
    // This layer executes exactly one route so no attempt can bypass the
    // canonical ledger, quarantine, or exhaustion contract.
    let result = vm_call_llm_api(request, delta_tx).await?;

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }
    super::trigger_predicate::note_result(request, &result);
    record_cli_llm_result(request, &result);

    Ok(result)
}

async fn vm_call_llm_full_inner_offthread(
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, OffthreadLlmError> {
    if let Some(result) = super::trigger_predicate::lookup_cached_result(request) {
        record_cli_llm_result(request, &result);
        return Ok(result);
    }

    if crate::llm::providers::MockProvider::should_intercept_request(request) {
        let result = mock_llm_response(request).map_err(OffthreadLlmError::from_vm_error)?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(request, &result);
        return Ok(result);
    }

    if crate::llm::fake::FakeLlmProvider::should_intercept(&request.provider) {
        let result = crate::llm::fake::FakeLlmProvider
            .chat_impl(request, delta_tx)
            .await
            .map_err(OffthreadLlmError::from_vm_error)?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(request, &result);
        return Ok(result);
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash_for_request(request);

    if replay_mode == LlmReplayMode::Replay {
        return load_fixture(&hash)
            .inspect(|result| {
                super::trigger_predicate::note_result(request, result);
            })
            .ok_or_else(|| {
                OffthreadLlmError::from_display_message(format!(
                    "No fixture found for LLM call (hash: {hash}). Run with --record first."
                ))
            });
    }

    super::ensure_real_llm_allowed(&request.provider).map_err(OffthreadLlmError::from_vm_error)?;

    // Keep the off-thread transport primitive single-route as well. The caller
    // routing executor owns all retries across provider/model alternatives.
    let result = vm_call_llm_api(request, delta_tx)
        .await
        .map_err(OffthreadLlmError::from_vm_error)?;

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }
    super::trigger_predicate::note_result(request, &result);
    record_cli_llm_result(request, &result);

    Ok(result)
}

#[cfg(test)]
mod request_shaping_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod transport_stub_tests;
