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
mod options;
mod response;
mod result;
mod thinking;
mod transport;

use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, record_cli_llm_result,
    save_fixture, LlmReplayMode,
};

// ─── Public surface (crate-wide) ────────────────────────────────────────

pub(crate) use auth::apply_auth_headers;
pub(crate) use completion::vm_call_completion_full;
pub(crate) use context_window::adapt_auto_compact_to_provider;
pub use context_window::fetch_provider_max_context;
pub(crate) use ollama::apply_ollama_runtime_settings;
pub use ollama::{
    ollama_runtime_settings_from_env, warm_ollama_model, warm_ollama_model_with_settings,
    OllamaRuntimeSettings, HARN_OLLAMA_KEEP_ALIVE_ENV, HARN_OLLAMA_NUM_CTX_ENV,
    OLLAMA_DEFAULT_KEEP_ALIVE, OLLAMA_DEFAULT_NUM_CTX, OLLAMA_HOST_ENV,
};
pub(crate) use openai_normalize::{debug_log_message_shapes, normalize_openai_style_messages};
pub(crate) use options::{
    DeltaSender, LlmCallOptions, LlmRequestPayload, LlmRouteAlternative, LlmRoutePolicy,
    LlmRoutingDecision, ThinkingConfig, ToolSearchConfig, ToolSearchMode, ToolSearchStrategy,
    ToolSearchVariant,
};
pub(crate) use result::{vm_build_llm_result, LlmResult};
pub(crate) use transport::vm_call_llm_api_with_body;

use transport::vm_call_llm_api;

/// Execute an LLM call. Always goes through the streaming path with a
/// discarding receiver so all callers share one code path for status/error
/// handling; non-streaming callers just drop the receiver.
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    vm_call_llm_full_inner(opts, Some(delta_tx)).await
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(opts, Some(delta_tx)).await
}

/// Execute provider I/O on Tokio's multithreaded scheduler while keeping
/// VM-local values and transcript assembly on the caller's LocalSet.
pub(crate) async fn vm_call_llm_full_streaming_offthread(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    let request = LlmRequestPayload::from(opts);
    tokio::task::spawn(
        async move { vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await },
    )
    .await
    .map_err(|join_err| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "llm_call background task failed: {join_err}"
        ))))
    })?
    .map_err(|message| VmError::Thrown(VmValue::String(Rc::from(message))))
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
        record_cli_llm_result(&result);
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        return Ok(result);
    }

    if crate::llm::providers::MockProvider::should_intercept(&request.provider) {
        let result = mock_llm_response(
            &request.messages,
            request.system.as_deref(),
            request.native_tools.as_deref(),
        )?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(&result);
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        return Ok(result);
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());

    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            super::trigger_predicate::note_result(request, &result);
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        )))));
    }

    super::ensure_real_llm_allowed(&request.provider)?;

    let result = vm_call_llm_api(request, delta_tx).await;

    // Surface the error as a String so it can cross the off-thread await.
    let primary_message = result.as_ref().err().map(ToString::to_string);
    let result = match (result, primary_message) {
        (Ok(r), _) => r,
        (Err(_), Some(message)) => try_fallback_provider(request, message)
            .await
            .map_err(|msg| VmError::Thrown(VmValue::String(Rc::from(msg))))?,
        (Err(_), None) => unreachable!("error branch must capture a message"),
    };

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }
    super::trigger_predicate::note_result(request, &result);
    record_cli_llm_result(&result);

    super::cost::accumulate_cost_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
    )?;

    Ok(result)
}

async fn vm_call_llm_full_inner_offthread(
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, String> {
    if let Some(result) = super::trigger_predicate::lookup_cached_result(request) {
        record_cli_llm_result(&result);
        return Ok(result);
    }

    if crate::llm::providers::MockProvider::should_intercept(&request.provider) {
        let result = mock_llm_response(
            &request.messages,
            request.system.as_deref(),
            request.native_tools.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(&result);
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
                format!("No fixture found for LLM call (hash: {hash}). Run with --record first.")
            });
    }

    super::ensure_real_llm_allowed(&request.provider).map_err(|err| err.to_string())?;

    let result = vm_call_llm_api(request, delta_tx)
        .await
        .map_err(|err| err.to_string());
    let result = match result {
        Ok(result) => result,
        Err(message) => try_fallback_provider(request, message).await?,
    };

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }
    super::trigger_predicate::note_result(request, &result);
    record_cli_llm_result(&result);

    super::cost::accumulate_cost_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
    )
    .map_err(|err| err.to_string())?;

    Ok(result)
}

/// Attempt the request on the configured fallback provider.  Returns the
/// original `primary_message` as the error if no fallback is available or
/// the fallback also fails.
async fn try_fallback_provider(
    request: &LlmRequestPayload,
    primary_message: String,
) -> Result<LlmResult, String> {
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
        return Err(primary_message);
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
            return Ok(result);
        }
    }

    Err(primary_message)
}

#[cfg(test)]
mod tests {
    use super::options::base_opts;
    use super::{
        vm_call_llm_full, vm_call_llm_full_streaming_offthread, LlmRequestPayload, ThinkingConfig,
    };
    use crate::llm::env_lock;

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

        let mut opts = base_opts("openai");
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
        let _guard = env_lock().lock().expect("env lock");
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
    fn disabled_llm_calls_still_allow_mock_provider() {
        let _guard = env_lock().lock().expect("env lock");
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
    fn anthropic_thinking_rewritten_to_adaptive_for_opus_4_7() {
        use crate::llm::providers::AnthropicProvider;

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-7".to_string();
        opts.thinking = Some(ThinkingConfig::Enabled);
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
        opts.thinking = Some(ThinkingConfig::WithBudget(32000));
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
        opts.thinking = Some(ThinkingConfig::WithBudget(16000));
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

    /// Accept a single connection with a bounded deadline so a buggy client
    /// can't wedge the test runner. Used by all localhost stubs in this
    /// module. Historical note: blocking `listener.accept()` has taken down
    /// the test suite at least twice.
    fn accept_with_deadline(listener: &std::net::TcpListener, label: &str) -> std::net::TcpStream {
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("restore blocking mode");
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                        .ok();
                    stream
                        .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                        .ok();
                    return stream;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        panic!("{label}: no client within 3s");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("{label}: accept failed: {e}"),
            }
        }
    }

    fn spawn_ollama_stub() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ollama stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            let mut stream = accept_with_deadline(&listener, "ollama stub");
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
        });
        (addr, handle)
    }

    fn spawn_ollama_stub_with_body_capture(
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ollama stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            let mut stream = accept_with_deadline(&listener, "ollama stub (capture)");
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
        });
        (addr, handle)
    }

    #[test]
    fn offthread_streaming_completes_inside_localset() {
        let _guard = env_lock().lock().expect("env lock");
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let (addr, server) = spawn_ollama_stub();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        vm_call_llm_full_streaming_offthread(&opts, tx),
                    )
                    .await
                    .expect("llm call timed out")
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

            server.join().expect("stub server");

            let (result, deltas) = result;
            assert_eq!(result.text, "hello world");
            assert_eq!(result.model, "stub-model");
            assert_eq!(result.input_tokens, 3);
            assert_eq!(result.output_tokens, 2);
            assert_eq!(deltas.join(""), "hello world");
        });
    }

    #[test]
    fn ollama_chat_applies_env_runtime_overrides() {
        let _guard = env_lock().lock().expect("env lock");
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let (addr, server) = spawn_ollama_stub_with_body_capture(captured.clone());
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

            server.join().expect("stub server");
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
    fn ollama_warmup_applies_shared_runtime_settings() {
        let _guard = env_lock().lock().expect("env lock");
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let (addr, server) = spawn_ollama_stub_with_body_capture(captured.clone());
            let _num_ctx = ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "65536");
            let _keep_alive = ScopedEnvVar::set("HARN_OLLAMA_KEEP_ALIVE", "forever");

            super::ollama::warm_ollama_model("qwen3.5:35b", Some(&format!("http://{addr}")))
                .await
                .expect("warmup should succeed");

            server.join().expect("stub server");
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

    /// Bind a stub listener + spawn a responder that serves a single canned
    /// HTTP error response, then returns its join handle. The listener uses
    /// a bounded accept so a misrouted client can never hang the test
    /// process — any failure to connect within 3s causes the thread to
    /// exit, unblocking `join()`.
    fn spawn_openai_error_stub(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind openai stub");
        let addr = listener.local_addr().expect("stub addr");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let handle = std::thread::spawn(move || {
            // Fail fast if the client never reaches the stub listener.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            };
            // Bounded read/write so a stuck client can't wedge the suite.
            stream
                .set_nonblocking(false)
                .expect("restore blocking mode on accepted stream");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            let mut buf = vec![0u8; 16384];
            let _ = stream.read(&mut buf);
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra_headers}connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (addr, handle)
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
        let _guard = env_lock().lock().expect("env lock");
        let _allow_llm_transport = allow_stubbed_llm_transport();
        let (addr, server) = spawn_openai_error_stub(status_line, extra_headers, body);
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
                    opts.response_format = None;
                    opts.json_schema = None;
                    opts.output_schema = None;
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    let call = tokio::time::timeout(
                        // Must stay inside the stub's fail-fast accept window.
                        std::time::Duration::from_secs(2),
                        vm_call_llm_full_streaming_offthread(&opts, tx),
                    )
                    .await;
                    match call {
                        Ok(Ok(_)) => panic!("expected streaming call to fail"),
                        Ok(Err(err)) => err.to_string(),
                        Err(_) => panic!("streaming call timed out"),
                    }
                })
                .await
        });
        match prev {
            Some(v) => unsafe { std::env::set_var("LOCAL_LLM_BASE_URL", v) },
            None => unsafe { std::env::remove_var("LOCAL_LLM_BASE_URL") },
        }
        let _ = server.join();
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
