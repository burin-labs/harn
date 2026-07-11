//! LLM API entry points and re-exports. The transport layer, request/
//! response parsing, auth, context-window discovery, and option/result
//! types each live in their own submodule under [`self`]; this file only
//! wires them together and hosts the `vm_call_llm_full*` chat entry
//! points that provider-specific completion / agent paths dispatch into.

mod auth;
mod completion;
mod context_window;
mod errors;
mod ollama;
mod openai_normalize;
pub(crate) mod options;
mod partial_tool_args;
mod response;
mod result;
mod schema_stream;
mod telemetry;
mod thinking;
mod transport;

use crate::value::{ErrorCategory, VmError, VmValue};

use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, record_cli_llm_result,
    save_fixture, LlmReplayMode,
};

// ─── Public surface (crate-wide) ────────────────────────────────────────

pub(crate) use auth::apply_auth_headers;
pub(crate) use completion::vm_call_completion_full;
pub use context_window::fetch_provider_max_context;
pub(crate) use errors::{
    classify_llm_error, classify_provider_http_error, err_for_non_success, retry_after_header,
    LlmErrorInfo, LlmErrorKind, LlmErrorReason,
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
pub(crate) use response::parse_openai_responses_response;
pub(crate) use response::{
    extract_cache_read_tokens, extract_cache_write_tokens,
    parse_llm_response as parse_llm_response_for_provider,
};
pub(crate) use result::{vm_build_llm_result, LlmResult};
pub(crate) use schema_stream::{
    aborted_result_value as schema_stream_aborted_result_value, parse_schema_stream_abort,
    SchemaStreamAbort, StreamSchemaWatch,
};
pub(crate) use telemetry::elapsed_ms;
pub use telemetry::{source as telemetry_source, OllamaPsModel, ProviderTelemetry};
pub(crate) use thinking::{split_openai_thinking_blocks, ThinkingStreamSplitter};
pub(crate) use transport::vm_call_llm_api_with_body;

use transport::vm_call_llm_api;

#[derive(Debug, Clone)]
struct OffthreadLlmError {
    message: String,
    category: Option<ErrorCategory>,
}

impl OffthreadLlmError {
    fn from_vm_error(err: VmError) -> Self {
        match err {
            VmError::CategorizedError { message, category } => Self {
                message,
                category: Some(category),
            },
            VmError::Thrown(VmValue::String(message)) => {
                Self::from_display_message(message.to_string())
            }
            other => Self::from_display_message(other.to_string()),
        }
    }

    fn from_display_message(message: String) -> Self {
        if let Some((category, stripped)) = parse_displayed_categorized_error(&message) {
            return Self {
                message: stripped.to_string(),
                category: Some(category),
            };
        }
        Self {
            message,
            category: None,
        }
    }

    fn into_vm_error(self) -> VmError {
        match self.category {
            Some(category) => VmError::CategorizedError {
                message: self.message,
                category,
            },
            None => VmError::Thrown(VmValue::String(arcstr::ArcStr::from(self.message))),
        }
    }
}

fn parse_displayed_categorized_error(message: &str) -> Option<(ErrorCategory, &str)> {
    let body = message.strip_prefix("Error [")?;
    let (category, rest) = body.split_once("]: ")?;
    Some((ErrorCategory::parse(category), rest))
}

/// Execute an LLM call. Always goes through the streaming path with a
/// discarding receiver so all callers share one code path for status/error
/// handling; non-streaming callers just drop the receiver.
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut first_token = super::first_token::FirstTokenTimer::for_current_span();
    let mut deltas_open = true;
    let mut call = Box::pin(vm_call_llm_full_inner(opts, Some(delta_tx)));
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
    super::cost::record_llm_usage_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
        result.served_fast,
    )?;
    Ok(result)
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let result = vm_call_llm_full_inner(opts, Some(delta_tx)).await?;
    super::cost::record_llm_usage_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
        result.served_fast,
    )?;
    Ok(result)
}

/// Execute provider I/O on Tokio's multithreaded scheduler while keeping
/// VM-local values and transcript assembly on the caller's LocalSet.
pub(crate) async fn vm_call_llm_full_streaming_offthread(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    super::cost::check_llm_preflight_budget(opts)?;
    let request = LlmRequestPayload::from(opts);
    let cached = super::trigger_predicate::lookup_cached_result(&request).is_some();
    let intercepted = crate::llm::providers::MockProvider::should_intercept_request(&request)
        || crate::llm::fake::FakeLlmProvider::should_intercept(&request.provider);
    let replay_mode = get_replay_mode();
    if !cached && !intercepted && replay_mode == LlmReplayMode::Replay {
        let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());
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
    let result = tokio::task::spawn(async move {
        if let Some(context) = raw_capture_context {
            crate::llm::agent_observe::with_raw_provider_capture_context(context, async {
                vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await
            })
            .await
        } else {
            vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await
        }
    })
    .await
    .map_err(|join_err| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "llm_call background task failed: {join_err}"
        ))))
    })?
    .map_err(OffthreadLlmError::into_vm_error)?;
    super::cost::record_llm_usage_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
        result.served_fast,
    )?;
    Ok(result)
}

async fn vm_call_llm_full_inner(
    opts: &LlmCallOptions,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let request = LlmRequestPayload::from(opts);
    vm_call_llm_full_inner_request(&request, delta_tx).await
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
    let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());

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

    let result = match vm_call_llm_api(request, delta_tx).await {
        Ok(result) => result,
        Err(primary_error) => match try_fallback_provider(request).await {
            Some(result) => result,
            None => return Err(primary_error),
        },
    };

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
    let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());

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

    let primary_error = match vm_call_llm_api(request, delta_tx).await {
        Ok(result) => {
            if replay_mode == LlmReplayMode::Record {
                save_fixture(&hash, &result);
            }
            super::trigger_predicate::note_result(request, &result);
            record_cli_llm_result(request, &result);
            return Ok(result);
        }
        Err(primary_error) => OffthreadLlmError::from_vm_error(primary_error),
    };

    let result = match try_fallback_provider(request).await {
        Some(result) => result,
        None => return Err(primary_error),
    };

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }
    super::trigger_predicate::note_result(request, &result);
    record_cli_llm_result(request, &result);

    Ok(result)
}

/// Attempt the request on the configured fallback provider.
async fn try_fallback_provider(request: &LlmRequestPayload) -> Option<LlmResult> {
    for route in &request.route_fallbacks {
        if route.provider == request.provider && route.model == request.model {
            continue;
        }
        let Ok(fb_key) = super::helpers::resolve_api_key(&route.provider) else {
            continue;
        };

        let mut fb_request = request.clone();
        fb_request.provider = route.provider.clone();
        fb_request.model = route.model.clone();
        fb_request.api_key = fb_key;
        if super::ensure_real_llm_allowed(&fb_request.provider).is_err() {
            continue;
        }
        if let Ok(result) = vm_call_llm_api(&fb_request, None).await {
            return Some(result);
        }
    }

    let mut fallback_providers = Vec::<String>::new();
    for provider in &request.fallback_chain {
        if provider != &request.provider && !fallback_providers.contains(provider) {
            fallback_providers.push(provider.clone());
        }
    }
    if let Some(pdef) = crate::llm_config::provider_config(&request.provider) {
        if let Some(fallback_provider) = pdef.fallback {
            if fallback_provider != request.provider
                && !fallback_providers.contains(&fallback_provider)
            {
                fallback_providers.push(fallback_provider);
            }
        }
    }
    if fallback_providers.is_empty() {
        return None;
    }

    for fallback_provider in fallback_providers {
        let Ok(fb_key) = super::helpers::resolve_api_key(&fallback_provider) else {
            continue;
        };

        let mut fb_request = request.clone();
        fb_request.provider = fallback_provider;
        fb_request.api_key = fb_key;
        if super::ensure_real_llm_allowed(&fb_request.provider).is_err() {
            continue;
        }
        if let Ok(result) = vm_call_llm_api(&fb_request, None).await {
            return Some(result);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::options::base_opts;
    use super::{
        vm_call_llm_full, vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread,
        LlmRequestPayload, ThinkingConfig,
    };
    use crate::llm::env_guard;

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn allow_stubbed_llm_transport() -> ScopedEnvVar {
        ScopedEnvVar::remove(crate::llm::LLM_CALLS_DISABLED_ENV)
    }

    #[test]
    fn openai_compat_prefill_appends_assistant_and_sets_chat_template_kwargs() {
        use crate::llm::providers::OpenAiCompatibleProvider;

        let mut opts = base_opts("local");
        opts.model = "Qwen/Qwen3.5-Coder-32B".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        let messages = body["messages"].as_array().expect("messages array");
        let last = messages.last().expect("at least one message");
        assert_eq!(last["role"].as_str(), Some("assistant"));
        assert_eq!(last["content"].as_str(), Some("<done>##DONE##</done>"));

        let kw = &body["chat_template_kwargs"];
        assert_eq!(kw["add_generation_prompt"].as_bool(), Some(false));
        assert_eq!(kw["continue_final_message"].as_bool(), Some(true));
    }

    #[test]
    fn openai_compat_without_prefill_omits_continue_flags() {
        use crate::llm::providers::OpenAiCompatibleProvider;

        let opts = base_opts("openai");
        let payload = LlmRequestPayload::from(&opts);
        let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

        let kw = &body["chat_template_kwargs"];
        assert!(kw.get("add_generation_prompt").is_none());
        assert!(kw.get("continue_final_message").is_none());
    }

    #[test]
    fn anthropic_prefill_appends_assistant_for_legacy_model() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-sonnet-4-20250514".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let messages = body["messages"].as_array().expect("messages array");
        let last = messages.last().expect("at least one message");
        assert_eq!(last["role"].as_str(), Some("assistant"));
        assert_eq!(last["content"].as_str(), Some("<done>##DONE##</done>"));
    }

    #[test]
    fn anthropic_prefill_skipped_for_deprecated_4_6_model() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-6".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let messages = body["messages"].as_array().expect("messages array");
        // User message only; prefill dropped silently.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn anthropic_prefill_skipped_for_opus_4_7() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-7".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn anthropic_sampling_params_stripped_for_opus_4_7() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-7".to_string();
        // base_opts already supplies temperature/top_p/top_k.
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        assert!(
            body.get("temperature").is_none(),
            "Opus 4.7 body must omit temperature (returns HTTP 400 otherwise)"
        );
        assert!(body.get("top_p").is_none(), "Opus 4.7 body must omit top_p");
        assert!(body.get("top_k").is_none(), "Opus 4.7 body must omit top_k");
    }

    #[test]
    fn anthropic_sampling_params_preserved_for_opus_4_6() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-6".to_string();
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        assert_eq!(body["temperature"].as_f64(), Some(0.2));
        assert_eq!(body["top_p"].as_f64(), Some(0.8));
        assert_eq!(body["top_k"].as_i64(), Some(40));
    }

    #[test]
    fn disabled_llm_calls_reject_real_provider_before_transport() {
        let _guard = env_guard();
        let _disabled = ScopedEnvVar::set(crate::llm::LLM_CALLS_DISABLED_ENV, "1");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let err = runtime
            .block_on(vm_call_llm_full(&base_opts("local")))
            .expect_err("local provider should be blocked before transport");
        let message = err.to_string();
        assert!(message.contains("HARN_LLM_CALLS_DISABLED"), "{message}");
        assert!(message.contains("provider `local`"), "{message}");
    }

    #[test]
    fn offthread_error_preserves_schema_stream_abort_category() {
        let abort = super::SchemaStreamAbort {
            provider: "openrouter".to_string(),
            model: "mistralai/devstral-small".to_string(),
            reason: "expected JSON value, got '`'".to_string(),
            path: "$".to_string(),
            chunks_consumed: 1,
        };

        let err = super::OffthreadLlmError::from_vm_error(abort.into_vm_error()).into_vm_error();
        let parsed = super::parse_schema_stream_abort(&err)
            .expect("schema stream abort must survive off-thread conversion");

        assert_eq!(parsed.provider, "openrouter");
        assert_eq!(parsed.model, "mistralai/devstral-small");
        assert_eq!(parsed.path, "$");
        assert_eq!(parsed.chunks_consumed, 1);
    }

    #[test]
    fn disabled_llm_calls_still_allow_mock_provider() {
        let _guard = env_guard();
        let _disabled = ScopedEnvVar::set(crate::llm::LLM_CALLS_DISABLED_ENV, "1");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = runtime
            .block_on(vm_call_llm_full(&base_opts("mock")))
            .expect("mock provider remains available");
        assert_eq!(result.provider, "mock");
    }

    #[test]
    fn fake_provider_routes_through_full_pipeline_with_streaming_deltas() {
        use crate::llm::fake::{
            install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeStopReason,
        };

        let _guard = env_guard();
        // Even with HARN_LLM_CALLS_DISABLED, the fake must pass through —
        // it never hits the network, so it must not be gated by that env.
        let _disabled = ScopedEnvVar::set(crate::llm::LLM_CALLS_DISABLED_ENV, "1");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let _script = install_fake_llm_script(FakeLlmScript::streaming(vec![
            FakeLlmEvent::Token("alpha".into()),
            FakeLlmEvent::Token(" beta".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ]));

        runtime.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let result = vm_call_llm_full_streaming(&base_opts("fake"), tx)
                .await
                .expect("fake provider routes through dispatch");
            assert_eq!(result.provider, "fake");
            assert_eq!(result.text, "alpha beta");
            let mut deltas = Vec::new();
            while let Ok(delta) = rx.try_recv() {
                deltas.push(delta);
            }
            assert_eq!(deltas, vec!["alpha".to_string(), " beta".to_string()]);
        });
    }

    #[test]
    fn anthropic_thinking_rewritten_to_adaptive_for_opus_4_7() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-7".to_string();
        opts.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let thinking = &body["thinking"];
        assert_eq!(thinking["type"].as_str(), Some("adaptive"));
        assert!(
            thinking.get("budget_tokens").is_none(),
            "Opus 4.7 adaptive thinking must not carry budget_tokens"
        );
    }

    #[test]
    fn anthropic_thinking_budget_discarded_for_opus_4_7() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-7".to_string();
        opts.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(32000),
        };
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let thinking = &body["thinking"];
        assert_eq!(thinking["type"].as_str(), Some("adaptive"));
        assert!(thinking.get("budget_tokens").is_none());
    }

    #[test]
    fn anthropic_thinking_preserves_extended_for_opus_4_6() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-6".to_string();
        opts.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(16000),
        };
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let thinking = &body["thinking"];
        assert_eq!(thinking["type"].as_str(), Some("enabled"));
        assert_eq!(thinking["budget_tokens"].as_i64(), Some(16000));
    }

    #[test]
    fn anthropic_prefill_preserved_for_or_opus_dotted_older_generations() {
        use crate::llm::providers::AnthropicProvider;

        // Dotted "claude-opus-4.5" style should NOT hit the 4.6 gate.
        let mut opts = base_opts("anthropic");
        opts.model = "anthropic/claude-opus-4.5".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages.last().unwrap()["role"].as_str(), Some("assistant"));
    }

    #[test]
    fn anthropic_prefill_skipped_for_or_opus_4_7_dotted() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "anthropic/claude-opus-4.7".to_string();
        opts.prefill = Some("<done>##DONE##</done>".to_string());
        let payload = LlmRequestPayload::from(&opts);
        let body = AnthropicProvider::build_request_body(&payload);

        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"].as_str(), Some("user"));
    }

    /// Cooperative accept: blocks the stub thread on a real
    /// `accept()` call until a client connects, then returns the
    /// stream. Shutdown wakes the thread by self-connecting to the
    /// listener (see [`LlmStub::drop`]) — when the resulting `accept`
    /// returns, the shutdown flag is checked and the synthetic stream
    /// is discarded.
    ///
    /// This replaces a previous nonblocking polling loop with a 5ms
    /// sleep tick. Polling introduced two flake modes under nextest's
    /// 50× concurrent flake-detection profile: (1) a real client
    /// connection could land between two polls and reqwest could time
    /// out on the SYN-ACK before the stub thread woke; (2) under
    /// heavy CPU contention the 5ms tick could stretch to tens of
    /// milliseconds, compounding (1). Blocking accept removes the
    /// scheduling-latency variable entirely.
    fn accept_with_shutdown(
        listener: &std::net::TcpListener,
        label: &str,
        shutdown: &std::sync::atomic::AtomicBool,
    ) -> Option<std::net::TcpStream> {
        let (stream, _peer) = listener
            .accept()
            .unwrap_or_else(|e| panic!("{label}: accept failed: {e}"));
        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
            drop(stream);
            return None;
        }
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .ok();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(30)))
            .ok();
        Some(stream)
    }

    /// Wake a stub thread blocked in [`accept_with_shutdown`] by
    /// opening a one-shot self-connection to its listener. The thread
    /// then observes the shutdown flag and exits without serving the
    /// connection. The connect uses a short timeout so a deferred
    /// shutdown (e.g. drop during panic unwind on a saturated CI
    /// worker) cannot wedge the test process.
    fn wake_accept_for_shutdown(addr: std::net::SocketAddr) {
        let _ = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500));
    }

    /// RAII guard for an in-process LLM stub. Dropping signals the stub
    /// thread to exit and joins it so no FDs leak past the test, even on
    /// panic.
    struct LlmStub {
        addr: std::net::SocketAddr,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
        /// Maximum number of `accept()` calls the stub thread can be
        /// parked on. Single-shot stubs use 1; `spawn_llm_stub_many`
        /// uses its connection count. Drop fires that many self-
        /// connections so every parked accept observes shutdown.
        pending_accepts: usize,
    }

    impl LlmStub {
        fn addr(&self) -> std::net::SocketAddr {
            self.addr
        }
    }

    impl Drop for LlmStub {
        fn drop(&mut self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Release);
            // Self-connect to unblock any thread parked inside
            // `accept_with_shutdown`. Multiple stubs in
            // `spawn_llm_stub_many` may need waking, so issue one
            // wake per outstanding accept slot.
            for _ in 0..self.pending_accepts.max(1) {
                wake_accept_for_shutdown(self.addr);
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Bind a localhost listener and run `body` on a background thread once
    /// a client connects. Wraps the listener in an [`LlmStub`] guard whose
    /// lifetime bounds the stub thread.
    fn spawn_llm_stub<F>(label: &'static str, body: F) -> LlmStub
    where
        F: FnOnce(&mut std::net::TcpStream) + Send + 'static,
    {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind llm stub");
        let addr = listener.local_addr().expect("stub addr");
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let Some(mut stream) = accept_with_shutdown(&listener, label, &shutdown_thread) else {
                return;
            };
            body(&mut stream);
        });
        LlmStub {
            addr,
            shutdown,
            handle: Some(handle),
            pending_accepts: 1,
        }
    }

    fn spawn_llm_stub_many<F>(label: &'static str, connections: usize, mut body: F) -> LlmStub
    where
        F: FnMut(usize, &mut std::net::TcpStream) + Send + 'static,
    {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind llm stub");
        let addr = listener.local_addr().expect("stub addr");
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();
        let handle = std::thread::spawn(move || {
            for attempt in 0..connections {
                let Some(mut stream) = accept_with_shutdown(&listener, label, &shutdown_thread)
                else {
                    return;
                };
                body(attempt, &mut stream);
            }
        });
        LlmStub {
            addr,
            shutdown,
            handle: Some(handle),
            pending_accepts: connections,
        }
    }

    fn spawn_ollama_stub() -> LlmStub {
        spawn_llm_stub("ollama stub", |stream| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

            let body = concat!(
                "{\"message\":{\"role\":\"assistant\",\"content\":\"hello \"},\"done\":false,\"model\":\"stub-model\"}\n",
                "{\"message\":{\"role\":\"assistant\",\"content\":\"world\"},\"done\":false}\n",
                "{\"done\":true,\"prompt_eval_count\":3,\"eval_count\":2,\"model\":\"stub-model\"}\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        })
    }

    fn spawn_ollama_empty_then_success_stub(
        request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> LlmStub {
        spawn_llm_stub_many("ollama retry stub", 2, move |attempt, stream| {
            use std::io::{Read, Write};
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

            let body = if attempt == 0 {
                "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":5,\"eval_count\":3}\n"
            } else {
                concat!(
                    "{\"message\":{\"role\":\"assistant\",\"content\":\"retried\"},\"done\":false,\"model\":\"stub-model\"}\n",
                    "{\"done\":true,\"prompt_eval_count\":5,\"eval_count\":1,\"model\":\"stub-model\"}\n"
                )
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        })
    }

    fn spawn_ollama_stub_with_body_capture(
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> LlmStub {
        spawn_llm_stub("ollama stub (capture)", move |stream| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or_default()
                .to_string();
            *captured.lock().expect("capture body") = Some(body);

            let body = concat!(
                "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":false}\n",
                "{\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        })
    }

    fn spawn_ollama_raw_generate_stub(
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> LlmStub {
        spawn_llm_stub("ollama raw stub", move |stream| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(request.starts_with("POST /api/generate HTTP/1.1\r\n"));
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or_default()
                .to_string();
            *captured.lock().expect("capture body") = Some(body);

            let body = concat!(
                "{\"response\":\"<tool_call>\\nedit({ path: \\\"a.rs\\\" })\\n</tool_call>\",\"done\":false,\"model\":\"qwen3.5:stub\"}\n",
                "{\"done\":true,\"prompt_eval_count\":7,\"eval_count\":11,\"model\":\"qwen3.5:stub\",\"done_reason\":\"stop\"}\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        })
    }

    fn spawn_anthropic_stub_with_request_capture(
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> LlmStub {
        spawn_llm_stub("anthropic stub (capture)", move |stream| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            *captured.lock().expect("capture request") = Some(request);

            let body = concat!(
                r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-opus-4-6","#,
                r#""content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","#,
                r#""usage":{"input_tokens":1,"output_tokens":1}}"#
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        })
    }

    #[test]
    fn anthropic_interleaved_thinking_beta_header_is_sent_for_supported_model() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let server = spawn_anthropic_stub_with_request_capture(captured.clone());
            let mut overlay = crate::llm_config::ProvidersConfig::default();
            overlay.providers.insert(
                "anthropic".to_string(),
                crate::llm_config::ProviderDef {
                    base_url: format!("http://{}", server.addr()),
                    auth_style: "none".to_string(),
                    auth_env: crate::llm_config::AuthEnv::None,
                    extra_headers: std::collections::BTreeMap::from([(
                        "anthropic-version".to_string(),
                        "2023-06-01".to_string(),
                    )]),
                    chat_endpoint: "/messages".to_string(),
                    ..Default::default()
                },
            );
            crate::llm_config::set_user_overrides(Some(overlay));
            crate::llm::helpers::reset_provider_key_cache();

            let mut opts = base_opts("anthropic");
            opts.model = "claude-opus-4-6".to_string();
            opts.stream = false;
            opts.thinking = ThinkingConfig::Enabled {
                budget_tokens: Some(8000),
            };
            let result = vm_call_llm_full(&opts)
                .await
                .expect("stubbed Anthropic response");

            crate::llm_config::clear_user_overrides();
            crate::llm::helpers::reset_provider_key_cache();
            drop(server);

            assert_eq!(result.text, "ok");
            let request = captured
                .lock()
                .expect("captured request")
                .clone()
                .expect("request captured")
                .to_lowercase();
            assert!(
                request.contains("anthropic-beta: interleaved-thinking-2025-05-14\r\n"),
                "{request}"
            );
        });
    }

    #[test]
    fn offthread_streaming_completes_inside_localset() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let server = spawn_ollama_stub();
            let addr = server.addr();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                        .await
                        .expect("llm call should succeed");

                    let mut deltas = Vec::new();
                    while let Ok(delta) = rx.try_recv() {
                        deltas.push(delta);
                    }
                    (result, deltas)
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe {
                    std::env::set_var("OLLAMA_HOST", value);
                },
                None => unsafe {
                    std::env::remove_var("OLLAMA_HOST");
                },
            }

            drop(server);

            let (result, deltas) = result;
            assert_eq!(result.text, "hello world");
            assert_eq!(result.model, "stub-model");
            assert_eq!(result.input_tokens, 3);
            assert_eq!(result.output_tokens, 2);
            assert_eq!(deltas.join(""), "hello world");
        });
    }

    #[test]
    fn ollama_empty_content_done_frame_retries_once() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let server = spawn_ollama_empty_then_success_stub(request_count.clone());
            let addr = server.addr();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                        .await
                        .expect("retry should recover from empty done frame");

                    let mut deltas = Vec::new();
                    while let Ok(delta) = rx.try_recv() {
                        deltas.push(delta);
                    }
                    (result, deltas)
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
                None => unsafe { std::env::remove_var("OLLAMA_HOST") },
            }

            drop(server);

            let (result, deltas) = result;
            assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert_eq!(result.text, "retried");
            assert_eq!(deltas.join(""), "retried");
        });
    }

    #[test]
    fn ollama_chat_applies_env_runtime_overrides() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let server = spawn_ollama_stub_with_body_capture(captured.clone());
            let addr = server.addr();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            let prev_num_ctx = std::env::var("HARN_OLLAMA_NUM_CTX").ok();
            let prev_keep_alive = std::env::var("HARN_OLLAMA_KEEP_ALIVE").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
                std::env::set_var("HARN_OLLAMA_NUM_CTX", "131072");
                std::env::set_var("HARN_OLLAMA_KEEP_ALIVE", "forever");
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    vm_call_llm_full_streaming_offthread(&opts, tx)
                        .await
                        .expect("llm call should succeed")
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
                None => unsafe { std::env::remove_var("OLLAMA_HOST") },
            }
            match prev_num_ctx {
                Some(value) => unsafe { std::env::set_var("HARN_OLLAMA_NUM_CTX", value) },
                None => unsafe { std::env::remove_var("HARN_OLLAMA_NUM_CTX") },
            }
            match prev_keep_alive {
                Some(value) => unsafe { std::env::set_var("HARN_OLLAMA_KEEP_ALIVE", value) },
                None => unsafe { std::env::remove_var("HARN_OLLAMA_KEEP_ALIVE") },
            }

            drop(server);
            assert_eq!(result.text, "ok");
            let body = captured
                .lock()
                .expect("captured body")
                .clone()
                .expect("request body");
            let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
            assert_eq!(json["keep_alive"].as_i64(), Some(-1));
            assert_eq!(json["options"]["num_ctx"].as_u64(), Some(131072));
        });
    }

    #[test]
    fn ollama_qwen_text_tool_route_bypasses_chat_parser_with_raw_generate() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let server = spawn_ollama_raw_generate_stub(captured.clone());
            let addr = server.addr();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let mut opts = base_opts("ollama");
                    opts.model = "qwen3.5:35b-a3b-coding-nvfp4".to_string();
                    opts.native_tools = None;
                    opts.output_format = crate::llm::api::OutputFormat::Text;
                    opts.response_format = None;
                    opts.json_schema = None;
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                        .await
                        .expect("raw-generate route should succeed");
                    let mut deltas = Vec::new();
                    while let Ok(delta) = rx.try_recv() {
                        deltas.push(delta);
                    }
                    (result, deltas)
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
                None => unsafe { std::env::remove_var("OLLAMA_HOST") },
            }

            drop(server);
            let (result, deltas) = result;
            assert_eq!(
                result.text,
                "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>"
            );
            assert_eq!(deltas.join(""), result.text);
            assert_eq!(result.model, "qwen3.5:stub");
            assert_eq!(result.input_tokens, 7);
            assert_eq!(result.output_tokens, 11);
            assert_eq!(result.stop_reason.as_deref(), Some("stop"));

            let body = captured
                .lock()
                .expect("captured body")
                .clone()
                .expect("request body");
            let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
            assert_eq!(json["raw"].as_bool(), Some(true));
            assert!(json["prompt"]
                .as_str()
                .unwrap_or_default()
                .contains("<|im_start|>assistant\n"));
            assert!(json.get("chat_template_kwargs").is_none());
        });
    }

    #[test]
    fn ollama_warmup_applies_shared_runtime_settings() {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let server = spawn_ollama_stub_with_body_capture(captured.clone());
            let addr = server.addr();
            let _num_ctx = ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "65536");
            let _keep_alive = ScopedEnvVar::set("HARN_OLLAMA_KEEP_ALIVE", "forever");

            super::ollama::warm_ollama_model("qwen3.5:35b", Some(&format!("http://{addr}")))
                .await
                .expect("warmup should succeed");

            drop(server);
            let body = captured
                .lock()
                .expect("captured body")
                .clone()
                .expect("request body");
            let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
            assert_eq!(json["model"].as_str(), Some("qwen3.5:35b"));
            assert_eq!(json["keep_alive"].as_i64(), Some(-1));
            assert_eq!(json["options"]["num_ctx"].as_u64(), Some(65536));
        });
    }

    /// Bind a stub listener that serves one canned HTTP error response.
    /// The returned [`LlmStub`] guard owns the listener and the worker
    /// thread, so dropping it (test exit, panic) signals shutdown and
    /// joins — a stuck or misrouted client can never wedge the suite.
    fn spawn_openai_error_stub(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> LlmStub {
        spawn_llm_stub("openai error stub", move |stream| {
            use std::io::{Read, Write};
            let mut buf = vec![0u8; 16384];
            let _ = stream.read(&mut buf);
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra_headers}connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        })
    }

    /// Single-entrypoint helper that serializes env-var mutation and the
    /// LLM call behind `env_lock`, so parallel streaming error tests can't
    /// clobber each other's `LOCAL_LLM_BASE_URL` and leak an unconnected
    /// stub whose `join()` would hang the test binary.
    fn run_streaming_error_case(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> String {
        let _guard = env_guard();
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let server = spawn_openai_error_stub(status_line, extra_headers, body);
        let addr = server.addr();
        let prev = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::set_var("LOCAL_LLM_BASE_URL", format!("http://{addr}"));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");
        let err = runtime.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut opts = base_opts("local");
                    opts.tools = None;
                    opts.native_tools = None;
                    opts.tool_choice = None;
                    opts.output_format = crate::llm::api::OutputFormat::Text;
                    opts.response_format = None;
                    opts.json_schema = None;
                    opts.output_schema = None;
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    let call = tokio::time::timeout(
                        // Must stay inside the stub's accept window.
                        std::time::Duration::from_secs(30),
                        vm_call_llm_full_streaming_offthread(&opts, tx),
                    )
                    .await;
                    match call {
                        Ok(Ok(_)) => panic!("expected streaming call to fail"),
                        Ok(Err(err)) => err.to_string(),
                        Err(elapsed) => panic!("streaming call timed out ({elapsed})"),
                    }
                })
                .await
        });
        match prev {
            Some(v) => unsafe { std::env::set_var("LOCAL_LLM_BASE_URL", v) },
            None => unsafe { std::env::remove_var("LOCAL_LLM_BASE_URL") },
        }
        drop(server);
        err
    }

    #[test]
    fn streaming_path_classifies_context_overflow() {
        let err = run_streaming_error_case(
            "HTTP/1.1 400 Bad Request",
            "",
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your prompt is too long."}}"#,
        );
        assert!(err.contains("[context_overflow]"), "err was: {err}");
        assert!(err.contains("local HTTP 400"), "err was: {err}");
    }

    #[test]
    fn streaming_path_classifies_rate_limit_with_retry_after() {
        let err = run_streaming_error_case(
            "HTTP/1.1 429 Too Many Requests",
            "retry-after: 7\r\n",
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(err.contains("[rate_limited]"), "err was: {err}");
        assert!(err.contains("(retry-after: 7)"), "err was: {err}");
    }

    #[test]
    fn streaming_path_classifies_opaque_500_as_http_error() {
        let err = run_streaming_error_case(
            "HTTP/1.1 500 Internal Server Error",
            "",
            r#"{"error":"upstream exploded"}"#,
        );
        assert!(err.contains("[http_error]"), "err was: {err}");
        assert!(err.contains("upstream exploded"), "err was: {err}");
    }
}
