//! Request-shaping tests: what the provider payload looks like BEFORE any
//! transport runs. No sockets, no stubs — these assert prefill handling,
//! sampling-parameter stripping, and thinking-config rewrites per model.

use super::options::base_opts;
use super::test_support::ScopedEnvVar;
use super::{vm_call_llm_full, vm_call_llm_full_streaming, LlmRequestPayload, ThinkingConfig};
use crate::llm::env_guard;

#[test]
fn openai_compat_prefill_appends_assistant_and_sets_chat_template_kwargs() {
    use crate::llm::providers::OpenAiCompatibleProvider;

    let mut opts = base_opts("local");
    opts.model = "Qwen/Qwen3.5-Coder-32B".to_string();
    opts.prefill = Some("<done>##DONE##</done>".to_string());
    let payload = LlmRequestPayload::from(&opts);
    let body = OpenAiCompatibleProvider::build_request_body(&payload);

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
    let body = OpenAiCompatibleProvider::build_request_body(&payload);

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
    use crate::llm::fake::{install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeStopReason};

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
