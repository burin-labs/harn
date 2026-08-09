use super::*;

fn install_managed_supply_stub_provider(provider: &str, addr: std::net::SocketAddr) {
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.to_string(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}/v1"),
            auth_style: "none".to_string(),
            auth_env: crate::llm_config::AuthEnv::None,
            chat_endpoint: "/chat/completions".to_string(),
            managed_supply: Some(crate::llm_config::ManagedSupplyProviderDef { version: 1 }),
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
}

#[test]
fn managed_supply_json_uses_logical_capabilities_and_authoritative_receipt() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let served_fingerprint =
        crate::llm::managed_supply::capability_fingerprint("groq", "llama-3.3-70b-versatile");
    let logical_fingerprint = served_fingerprint.clone();
    let expected_logical_fingerprint = logical_fingerprint.clone();
    let server = spawn_llm_stub("managed supply JSON stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 32_768];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["provider"],
            "groq"
        );
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["model"],
            "llama-3.3-70b-versatile"
        );
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["capability_fingerprint"],
            expected_logical_fingerprint
        );
        let response_body = serde_json::json!({
            "id": "gateway-envelope",
            "object": "chat.completion",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "harn_managed_supply": {
                "version": 1,
                "request_id": "pool-request",
                "provider_request_id": "provider-request",
                "served_route": {
                    "provider": "groq",
                    "model": "llama-3.3-70b-versatile",
                    "capability_fingerprint": served_fingerprint,
                },
                "input_tokens": 31,
                "output_tokens": 7,
                "cost_usd": "0.0042",
                "cost_basis": "actual",
                "capability_mode": "exact",
                "routing_attempts": [{
                    "provider": "groq",
                    "model": "llama-3.3-70b-versatile",
                    "outcome": "success",
                    "elapsed_ms": 12
                }],
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });
    install_managed_supply_stub_provider("managed-gateway", server.addr());

    let mut opts = base_opts("managed-gateway");
    opts.model = "llama-3.3-70b-versatile".to_string();
    opts.stream = false;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect("managed completion");
    crate::llm_config::clear_user_overrides();

    assert_eq!(result.provider, "groq");
    assert_eq!(result.model, "llama-3.3-70b-versatile");
    assert_eq!((result.input_tokens, result.output_tokens), (31, 7));
    assert_eq!(result.usage().cost_usd, Some(0.0042));
    assert_eq!(
        result.telemetry.request_id.as_deref(),
        Some("provider-request")
    );
    assert_eq!(logical_fingerprint.len(), 64);
}

#[test]
fn managed_supply_streams_multiple_deltas_and_applies_terminal_receipt() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let served_fingerprint =
        crate::llm::managed_supply::capability_fingerprint("groq", "llama-3.3-70b-versatile");
    let server = spawn_llm_stub("managed supply SSE stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 32_768];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["provider"],
            "groq"
        );

        let first = serde_json::json!({
            "id": "gateway-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [{"index": 0, "delta": {"content": "hello "}}],
        });
        let terminal = serde_json::json!({
            "id": "gateway-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [{"index": 0, "delta": {"content": "world"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "harn_managed_supply": {
                "version": 1,
                "request_id": "pool-stream-request",
                "provider_request_id": "provider-stream-request",
                "served_route": {
                    "provider": "groq",
                    "model": "llama-3.3-70b-versatile",
                    "capability_fingerprint": served_fingerprint,
                },
                "input_tokens": 41,
                "output_tokens": 9,
                "cost_usd": "0.0065",
                "cost_basis": "actual",
                "capability_mode": "exact",
                "routing_attempts": [{
                    "provider": "groq",
                    "model": "llama-3.3-70b-versatile",
                    "outcome": "success",
                    "elapsed_ms": 12
                }],
            }
        });
        let response_body = format!("data: {first}\n\ndata: {terminal}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });
    install_managed_supply_stub_provider("managed-gateway-stream", server.addr());

    let mut opts = base_opts("managed-gateway-stream");
    opts.model = "llama-3.3-70b-versatile".to_string();
    opts.stream = true;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = runtime
        .block_on(vm_call_llm_full_streaming(&opts, tx))
        .expect("managed streaming completion");
    crate::llm_config::clear_user_overrides();
    let mut deltas = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        deltas.push(delta);
    }

    assert_eq!(
        deltas.len(),
        2,
        "the managed response must stream before terminal accounting"
    );
    assert_eq!(deltas.concat(), "hello world");
    assert_eq!(result.text, "hello world");
    assert_eq!(result.provider, "groq");
    assert_eq!(result.model, "llama-3.3-70b-versatile");
    assert_eq!((result.input_tokens, result.output_tokens), (41, 9));
    assert_eq!(result.usage().cost_usd, Some(0.0065));
    assert_eq!(
        result.telemetry.request_id.as_deref(),
        Some("provider-stream-request")
    );
}

#[test]
fn managed_supply_missing_terminal_receipt_fails_closed() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let server = spawn_openai_success_stub();
    install_managed_supply_stub_provider("managed-gateway-missing", server.addr());
    let mut opts = base_opts("managed-gateway-missing");
    opts.model = "gpt-4o-mini".to_string();
    opts.stream = false;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("missing receipt must fail");
    crate::llm_config::clear_user_overrides();
    assert!(error.to_string().contains("missing its terminal receipt"));
}

#[test]
fn managed_supply_rejects_a_valid_but_capability_incompatible_served_route() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let served_model = "claude-haiku-4-5-20251001";
    let served_fingerprint =
        crate::llm::managed_supply::capability_fingerprint("anthropic", served_model);
    let server = spawn_llm_stub("managed supply incompatible route stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 32_768];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["model"],
            "gpt-4o-mini"
        );

        let response_body = serde_json::json!({
            "id": "gateway-envelope",
            "object": "chat.completion",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
            "harn_managed_supply": {
                "version": 1,
                "request_id": "pool-request",
                "served_route": {
                    "provider": "anthropic",
                    "model": served_model,
                    "capability_fingerprint": served_fingerprint,
                },
                "input_tokens": 3,
                "output_tokens": 1,
                "cost_usd": "0.0001",
                "cost_basis": "actual",
                "capability_mode": "exact",
                "routing_attempts": [],
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });
    install_managed_supply_stub_provider("managed-gateway-incompatible", server.addr());

    let mut opts = base_opts("managed-gateway-incompatible");
    opts.model = "gpt-4o-mini".to_string();
    opts.stream = false;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("incompatible served route must fail");
    crate::llm_config::clear_user_overrides();
    assert!(error.to_string().contains("not capability-compatible"));
}
