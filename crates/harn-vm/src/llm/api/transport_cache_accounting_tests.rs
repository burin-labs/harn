use super::{
    allow_stubbed_llm_transport, base_opts, env_guard,
    install_openai_stub_provider_with_cache_accounting, spawn_llm_stub, vm_call_llm_full, LlmStub,
};

/// OpenAI-compatible success response whose `usage` block reports cached
/// prompt tokens, including the shape an undeclared provider can return.
fn spawn_openai_cached_usage_stub() -> LlmStub {
    spawn_llm_stub("OpenAI-compatible cached-usage stub", |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16_384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        let body = serde_json::json!({
            "id": "ok",
            "object": "chat.completion",
            "created": 0,
            "model": "cached-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 1,
                "total_tokens": 201,
                "prompt_tokens_details": {"cached_tokens": 150}
            }
        })
        .to_string();
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

/// Pins the tri-state `cache_usage_accounting` contract at the transport
/// boundary. The absent case is the load-bearing one: an undeclared provider
/// must keep the cache figures its response actually carried — collapsing
/// absent into declared-`false` silently zeroed real telemetry.
#[test]
fn cache_accounting_declaration_tristate_gates_cache_zeroing() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let call =
            |provider: &'static str, model: &'static str, declaration: Option<bool>| async move {
            let server = spawn_openai_cached_usage_stub();
            let addr = server.addr();
            install_openai_stub_provider_with_cache_accounting(provider, addr, declaration);
            crate::llm_config::set_runtime_provider_endpoint_overrides(
                crate::llm_config::RuntimeProviderEndpointOverrides::single(
                    provider,
                    format!("http://{addr}/v1"),
                )
                .expect("valid stub endpoint"),
            );
            let mut opts = base_opts(provider);
            opts.model = model.to_string();
            opts.stream = false;
            opts.cache = false;
            let result = vm_call_llm_full(&opts)
                .await
                .expect("stubbed call should succeed");
            crate::llm_config::clear_user_overrides();
            crate::llm_config::clear_runtime_provider_endpoint_overrides();
            drop(server);
            result
        };

        // llama.cpp declares `true`: its standard OpenAI-compatible cache
        // fields are authoritative rather than zeroed as unsupported.
        assert_eq!(
            crate::llm_config::provider_config("llamacpp")
                .and_then(|provider| provider.cache_usage_accounting),
            Some(true)
        );
        let declared_true =
            call("llamacpp", "qwen3.6-35b-a3b-ud-q4-k-xl", Some(true)).await;
        assert_eq!(declared_true.cache_read_tokens, 150);
        assert!(declared_true.cache_supported);
        assert_eq!(
            declared_true.telemetry.cache_accounting_declared,
            Some(true)
        );

        // Declared `false`: the route reports no cache fields, so zeroing
        // whatever leaked into the response shape is intentional.
        let declared_false = call("declared-false", "cached-model", Some(false)).await;
        assert_eq!(declared_false.cache_read_tokens, 0);
        assert_eq!(declared_false.cache_write_tokens, 0);
        assert!(!declared_false.cache_supported);
        assert_eq!(
            declared_false.telemetry.cache_accounting_declared,
            Some(false)
        );

        // Absent: the provider block never declared either way. Keep this
        // control synthetic so catalog verification can promote real
        // providers without weakening the boundary contract.
        assert_eq!(
            crate::llm_config::provider_config("undeclared-cache")
                .and_then(|provider| provider.cache_usage_accounting),
            None
        );
        let undeclared = call("undeclared-cache", "cached-model", None).await;
        assert_eq!(undeclared.cache_read_tokens, 150);
        assert!(undeclared.cache_supported);
        assert_eq!(undeclared.telemetry.cache_accounting_declared, None);
    });
}
