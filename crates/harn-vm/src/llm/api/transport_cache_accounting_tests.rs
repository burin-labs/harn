use super::{
    allow_stubbed_llm_transport, base_opts, env_guard,
    install_openai_stub_provider_with_cache_accounting, spawn_llm_stub, vm_call_llm_full, LlmStub,
};

/// OpenAI-compatible success response whose `usage` block reports cached
/// prompt tokens, as an undeclared provider may return.
fn spawn_openai_cached_usage_stub() -> LlmStub {
    spawn_openai_usage_stub(Some(serde_json::json!({"cached_tokens": 150})), false)
}

fn spawn_openai_usage_stub(details: Option<serde_json::Value>, streaming: bool) -> LlmStub {
    spawn_llm_stub("OpenAI-compatible cached-usage stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16_384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        let mut body = serde_json::json!({
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
                "total_tokens": 201
            }
        });
        if let Some(details) = &details {
            body["usage"]["prompt_tokens_details"] = details.clone();
        }
        let (content_type, body) = if streaming {
            let content = serde_json::json!({
                "id": "ok", "model": "cached-model",
                "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": null}]
            });
            let finish = serde_json::json!({
                "id": "ok", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            });
            let receipt = serde_json::json!({"id": "ok", "choices": [], "usage": body["usage"]});
            (
                "text/event-stream",
                format!("data: {content}\n\ndata: {finish}\n\ndata: {receipt}\n\ndata: [DONE]\n\n"),
            )
        } else {
            ("application/json", body.to_string())
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    })
}

#[test]
fn conditional_cache_receipts_preserve_omission_zero_and_created_tokens() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        for streaming in [false, true] {
            let call = |details| async move {
                let server = spawn_openai_usage_stub(details, streaming);
                let provider = "conditional-cache";
                let addr = server.addr();
                install_openai_stub_provider_with_cache_accounting(provider, addr, Some(true));
                crate::llm_config::set_runtime_provider_endpoint_overrides(
                    crate::llm_config::RuntimeProviderEndpointOverrides::single(
                        provider,
                        format!("http://{addr}/v1"),
                    )
                    .expect("valid stub endpoint"),
                );
                let mut opts = base_opts(provider);
                opts.model = "cached-model".into();
                opts.stream = streaming;
                opts.cache = false;
                let result = vm_call_llm_full(&opts).await.expect("provider response");
                let usage = crate::llm::usage::LlmUsage::from_result(&result);
                crate::llm_config::clear_user_overrides();
                crate::llm_config::clear_runtime_provider_endpoint_overrides();
                drop(server);
                usage
            };
            let omitted = call(None).await;
            assert_eq!(omitted.input_tokens, 200);
            assert!(!omitted.cache_counts_present);
            assert_eq!(omitted.cache_hit_ratio, None);
            let writes = call(Some(serde_json::json!({
                "cached_tokens": null, "created_cache_tokens": 25
            })))
            .await;
            assert_eq!(writes.cache_write_tokens, 25);
            assert_eq!(writes.cache_hit_ratio, None);
            let normalized = serde_json::to_value(&writes).expect("serialize measured write");
            let measured = crate::llm::usage::ReportedCacheUsage::from_value(&normalized)
                .expect("read normalized category measurements");
            assert_eq!(measured.read_tokens, None);
            assert_eq!(measured.write_tokens, Some(25));
            let reads = call(Some(serde_json::json!({"cached_tokens": 150}))).await;
            let mut totals = crate::llm::usage::ReportedCacheTotals::default();
            reads.record_cache_into(&mut totals);
            assert_eq!(totals.reported_read_tokens(), Some(150));
            assert_eq!(totals.reported_write_tokens(), None);
            let zero = call(Some(serde_json::json!({
                "cached_tokens": 0, "created_cache_tokens": 0
            })))
            .await;
            assert!(zero.cache_counts_present);
            assert_eq!(zero.cache_hit_ratio, Some(0.0));
            let reported = call(Some(serde_json::json!({
                "cached_tokens": 150, "created_cache_tokens": 25
            })))
            .await;
            assert_eq!(reported.input_tokens, 200);
            assert_eq!(reported.cache_read_tokens, 150);
            assert_eq!(reported.cache_write_tokens, 25);
            assert!(reported.cache_counts_present);
            assert_eq!(reported.cache_hit_ratio, Some(0.75));
            let complete = crate::llm::usage::LlmUsage::aggregate(&[reported.clone(), zero]);
            assert_eq!(complete.cache_hit_ratio, Some(0.375));
            let incomplete = crate::llm::usage::LlmUsage::aggregate(&[reported, omitted]);
            assert_eq!(incomplete.cache_hit_ratio, None);
            let measured = incomplete
                .reported_cache
                .as_deref()
                .expect("measured subtotal");
            assert_eq!(measured.reported_read_tokens(), Some(150));
            assert_eq!(measured.read_reported_calls, 1);
            assert_eq!(incomplete.provider_call_count, Some(2));
            let restored: crate::llm::usage::LlmUsage = serde_json::from_value(
                serde_json::to_value(&incomplete).expect("serialize ledger"),
            )
            .expect("restore ledger");
            assert_eq!(restored, incomplete);
        }
    });
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

        // Absent: the test provider never declares either way. The parsed
        // values survive and the declaration reads as undeclared. A dedicated
        // provider keeps this transport contract independent of catalog rows.
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
