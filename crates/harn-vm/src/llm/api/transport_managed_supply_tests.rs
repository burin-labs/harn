use super::*;
use crate::llm::api::{inference_boundary, InferenceBoundary, InferenceReach, LlmCallOptions};

fn install_managed_supply_stub_provider(provider: &str, addr: std::net::SocketAddr) {
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.to_string(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}/v1"),
            auth_style: "none".to_string(),
            auth_env: crate::llm_config::AuthEnv::None,
            chat_endpoint: "/chat/completions".to_string(),
            managed_supply: Some(crate::llm_config::ManagedSupplyProviderDef {
                version: crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
            }),
            data_controls: Some(crate::llm_config::DataControlsDef {
                control_scope: crate::llm_config::DataControlScope::None,
                request_controls: vec![],
                retention_default: crate::llm_config::RetentionDefault::Unspecified,
                training_default: crate::llm_config::TrainingDefault::DoesNotTrain,
                checked_on: "2026-10-05".to_string(),
                sources: vec!["https://example.invalid/privacy-fixture".to_string()],
                note: None,
            }),
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
}

fn managed_opts(provider: &str) -> LlmCallOptions {
    let mut opts = base_opts(provider);
    opts.inference_boundary = Some(InferenceBoundary {
        reach: InferenceReach::AnyHosted,
        allow_training_discounts: false,
    });
    opts
}

#[test]
fn managed_supply_json_uses_logical_capabilities_and_authoritative_receipt() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let served_fingerprint =
        crate::llm::managed_supply::capability_fingerprint("groq", "qwen/qwen3.8-27b");
    let logical_fingerprint = served_fingerprint.clone();
    let expected_logical_fingerprint = logical_fingerprint.clone();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_requests = requests.clone();
    let server = spawn_llm_stub("managed supply JSON stub", move |stream| {
        observed_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            "qwen/qwen3.8-27b"
        );
        assert_eq!(
            body["harn_managed_supply"]["logical_route"]["capability_fingerprint"],
            expected_logical_fingerprint
        );
        assert_eq!(
            body["harn_managed_supply"]["inference_boundary"],
            serde_json::json!({
                "reach": "any_hosted", "allow_training_discounts": false,
            })
        );
        let response_body = serde_json::json!({
            "id": "gateway-envelope",
            "object": "chat.completion",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "harn_managed_supply": {
                "version": crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
                "request_id": "pool-request",
                "provider_request_id": "provider-request",
                "served_route": {
                    "provider": "groq",
                    "model": "qwen/qwen3.8-27b",
                    "capability_fingerprint": served_fingerprint,
                },
                "input_tokens": 31,
                "output_tokens": 7,
                "cost_usd": "0.0042",
                "cost_basis": "actual",
                "capability_mode": "exact",
                "routing_attempts": [{
                    "provider": "groq",
                    "model": "qwen/qwen3.8-27b",
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

    let mut opts = managed_opts("managed-gateway");
    opts.model = "qwen/qwen3.8-27b".to_string();
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
    assert_eq!(result.model, "qwen/qwen3.8-27b");
    assert_eq!((result.input_tokens, result.output_tokens), (31, 7));
    assert_eq!(result.usage().cost_usd, Some(0.0042));
    assert_eq!(
        result.telemetry.request_id.as_deref(),
        Some("provider-request")
    );
    assert_eq!(logical_fingerprint.len(), 64);
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn managed_supply_missing_or_malformed_authority_never_reaches_transport() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_requests = requests.clone();
    let _host = ScopedEnvVar::remove(inference_boundary::HOST_BOUNDARY_ENV);
    let server = spawn_llm_stub("managed authority counted control", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 32_768];
        let received = stream.read(&mut buf).expect("allowed request");
        assert!(received > 0, "the allowed control must send request bytes");
        observed_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body = serde_json::json!({
            "id": "authority-control", "object": "chat.completion", "created": 0,
            "model": "mistral-large-2512",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "allowed"}, "finish_reason": "stop"}],
            "harn_managed_supply": {
                "version": crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
                "request_id": "authority-control",
                "served_route": {"provider": "mistral", "model": "mistral-large-2512",
                    "capability_fingerprint": crate::llm::managed_supply::capability_fingerprint("mistral", "mistral-large-2512")},
                "input_tokens": 1, "output_tokens": 1, "cost_usd": "0",
                "cost_basis": "actual", "capability_mode": "exact", "routing_attempts": []
            }
        }).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).expect("allowed response");
    });
    install_managed_supply_stub_provider("managed-no-authority", server.addr());
    let mut opts = base_opts("managed-no-authority");
    opts.model = "mistral-large-2512".to_string();
    opts.stream = false;
    opts.inference_boundary = None;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("missing caller authority");
    assert!(
        error.to_string().contains("explicit inference boundary"),
        "{error}"
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    opts.inference_boundary = Some(InferenceBoundary {
        reach: InferenceReach::AnyHosted,
        allow_training_discounts: false,
    });
    {
        let _malformed = ScopedEnvVar::set(
            inference_boundary::HOST_BOUNDARY_ENV,
            "private-malformed-canary",
        );
        let error = runtime
            .block_on(vm_call_llm_full(&opts))
            .expect_err("malformed host authority");
        assert!(error
            .to_string()
            .contains("inference_boundary.host_boundary_malformed"));
        assert!(!error.to_string().contains("private-malformed-canary"));
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    let allowed = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect("same listener positive control");
    crate::llm_config::clear_user_overrides();
    drop(server);
    assert_eq!(allowed.text, "allowed");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn managed_supply_terminal_receipt_preserves_open_weight_caller_limit() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let logical = "mistral-large-2512";
    let closed = "codestral-2508";
    assert_eq!(
        crate::llm::managed_supply::capability_fingerprint("mistral", logical),
        crate::llm::managed_supply::capability_fingerprint("mistral", closed),
        "the refusal must concern privacy rather than capability incompatibility"
    );
    for served_model in [logical, closed] {
        for streaming in [false, true] {
            let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed_requests = requests.clone();
            let server = spawn_llm_stub("managed physical weight receipt", move |stream| {
                use std::io::{Read, Write};
                observed_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 32_768];
                let n = stream.read(&mut buf).expect("request");
                let request = String::from_utf8_lossy(&buf[..n]);
                let body: serde_json::Value =
                    serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body"))
                        .expect("JSON");
                assert_eq!(
                    body["harn_managed_supply"]["inference_boundary"]["reach"],
                    "hosted_open_weight"
                );
                let receipt = serde_json::json!({
                    "version": crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
                    "request_id": "physical-weight-receipt",
                    "served_route": {
                        "provider": "mistral", "model": served_model,
                        "capability_fingerprint": crate::llm::managed_supply::capability_fingerprint("mistral", served_model)
                    },
                    "input_tokens": 1, "output_tokens": 1, "cost_usd": "0",
                    "cost_basis": "actual", "capability_mode": "exact", "routing_attempts": []
                });
                let payload = serde_json::json!({
                    "id": "weight-test", "object": "chat.completion", "created": 0,
                    "model": served_model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
                    "harn_managed_supply": receipt
                });
                let (content_type, response_body) = if streaming {
                    let delta = serde_json::json!({
                        "id": "weight-test", "object": "chat.completion.chunk", "created": 0,
                        "model": served_model,
                        "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": "stop"}]
                    });
                    let terminal =
                        serde_json::json!({"choices": [], "harn_managed_supply": receipt});
                    (
                        "text/event-stream",
                        format!("data: {delta}\n\ndata: {terminal}\n\ndata: [DONE]\n\n"),
                    )
                } else {
                    ("application/json", payload.to_string())
                };
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}", response_body.len()).expect("response");
            });
            install_managed_supply_stub_provider("managed-weight-gateway", server.addr());
            let mut opts = managed_opts("managed-weight-gateway");
            opts.model = logical.to_string();
            opts.inference_boundary.as_mut().unwrap().reach = InferenceReach::HostedOpenWeight;
            opts.stream = streaming;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let result = runtime.block_on(vm_call_llm_full(&opts));
            crate::llm_config::clear_user_overrides();
            drop(server);
            assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            if served_model == logical {
                assert_eq!(result.expect("allowed open physical route").model, logical);
            } else {
                let error = result.expect_err("closed physical route must fail");
                let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error
                else {
                    panic!("physical policy refusal must retain the owning typed error");
                };
                let facts = crate::llm::helpers::vm_value_dict_to_json(&fields);
                assert_eq!(facts["category"], "egress_blocked");
                assert_eq!(facts["code"], "inference_boundary.hosted_open_weight");
                assert_eq!(facts["reason"], "policy_denied");
                assert_eq!(facts["retryable"], false);
            }
        }
    }
}

#[test]
fn managed_supply_streams_multiple_deltas_and_applies_terminal_receipt() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let served_fingerprint =
        crate::llm::managed_supply::capability_fingerprint("groq", "qwen/qwen3.8-27b");
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
        });
        let receipt = serde_json::json!({
            "id": "gateway-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "ignored-gateway-model",
            "choices": [],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "harn_managed_supply": {
                "version": crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
                "request_id": "pool-stream-request",
                "provider_request_id": "provider-stream-request",
                "served_route": {
                    "provider": "groq",
                    "model": "qwen/qwen3.8-27b",
                    "capability_fingerprint": served_fingerprint,
                },
                "input_tokens": 41,
                "output_tokens": 9,
                "cost_usd": "0.0065",
                "cost_basis": "actual",
                "capability_mode": "exact",
                "routing_attempts": [{
                    "provider": "groq",
                    "model": "qwen/qwen3.8-27b",
                    "outcome": "success",
                    "elapsed_ms": 12
                }],
            }
        });
        let response_body =
            format!("data: {first}\n\ndata: {terminal}\n\ndata: {receipt}\n\ndata: [DONE]\n\n");
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

    let mut opts = managed_opts("managed-gateway-stream");
    opts.model = "qwen/qwen3.8-27b".to_string();
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
    assert_eq!(result.model, "qwen/qwen3.8-27b");
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
    let mut opts = managed_opts("managed-gateway-missing");
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
                "version": crate::llm::managed_supply::MANAGED_SUPPLY_VERSION,
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

    let mut opts = managed_opts("managed-gateway-incompatible");
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
