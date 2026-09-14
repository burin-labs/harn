//! Real local HTTP controls for conservative admission; no provider mocks.
use super::*;
use crate::llm::admission::{swap_scope, AdmissionMode, AdmissionScope};
use crate::llm::cost::LlmBudgetEnvelope;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn options(ceiling: f64, stream: bool) -> super::super::LlmCallOptions {
    super::super::LlmCallOptions {
        provider: "openai".into(),
        model: "gpt-5.6-luna".into(),
        messages: vec![serde_json::json!({"role":"user", "content":"hello"})],
        max_tokens: 64,
        stream,
        budget: Some(LlmBudgetEnvelope {
            admission: Some(AdmissionMode::Conservative),
            total_budget_usd: Some(ceiling),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn stub(count: Arc<AtomicUsize>, stream_response: bool, empty: bool) -> LlmStub {
    spawn_llm_stub_many("conservative admission", 3, move |_, stream| {
        use std::io::{Read, Write};
        count.fetch_add(1, Ordering::SeqCst);
        let mut bytes = [0u8; 16_384];
        let n = stream.read(&mut bytes).unwrap();
        assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /v1/chat/completions "));
        let text = if empty { "" } else { "hello" };
        let usage =
            serde_json::json!({"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4});
        let (content_type, body) = if stream_response {
            (
                "text/event-stream",
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({"model":"gpt-5.6-luna","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}),
                    serde_json::json!({"model":"gpt-5.6-luna","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":usage})
                ),
            )
        } else {
            ("application/json", serde_json::json!({"id":"local","model":"gpt-5.6-luna",
                "choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],
                "usage":usage}).to_string())
        };
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    })
}

fn install(addr: std::net::SocketAddr) {
    install_openai_stub_provider("openai", addr);
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{addr}/v1"),
        )
        .unwrap(),
    );
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::llm::cost::reset_cost_state();
    }
}

#[test]
fn conservative_admission_denies_before_http_and_settles_real_usage() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, false);
        install(server.addr());
        let error = vm_call_llm_full(&options(0.01, false)).await.unwrap_err();
        assert!(error.to_string().contains("conservative"), "{error}");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        swap_scope(AdmissionScope::default());
        for _ in 0..2 {
            let result = vm_call_llm_full(&options(0.6, false)).await.unwrap();
            assert_eq!(result.text, "hello");
            assert_eq!(result.telemetry.server_prompt_tokens, Some(3));
            assert_eq!(result.telemetry.server_output_tokens, Some(1));
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "known usage released only the unused bound"
        );
    });
}

#[test]
fn conservative_admission_streaming_and_offthread_use_the_same_allowance() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), true, false);
        install(server.addr());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let first = vm_call_llm_full_streaming(&options(0.6, true), tx.clone())
            .await
            .unwrap();
        assert_eq!(first.text, "hello");
        let second = vm_call_llm_full_streaming_offthread(&options(0.6, true), tx.clone())
            .await
            .unwrap();
        assert_eq!(second.text, "hello");
        let error = vm_call_llm_full_streaming_offthread(&options(0.01, true), tx)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("conservative"), "{error}");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn conservative_admission_does_not_recycle_an_empty_attempt_for_retry() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, true);
        install(server.addr());
        let error = vm_call_llm_full(&options(0.6, false)).await.unwrap_err();
        assert!(error.to_string().contains("conservative"), "{error}");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "retry was denied before transport"
        );
    });
}

#[test]
fn conservative_admission_refuses_auxiliary_completion_and_probe_before_http() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, false);
        install(server.addr());
        let opts = options(0.6, false);
        let error = crate::llm::api::vm_call_completion_full(&opts, "prefix", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unsupported_billing_shape"));
        let mut omitted = opts.clone();
        omitted.budget = None;
        assert!(
            crate::llm::api::vm_call_completion_full(&omitted, "prefix", None)
                .await
                .is_err()
        );
        let request = crate::llm::api::LlmRequestPayload::from(&opts);
        assert!(crate::llm::api::probe_llm_request(&request).await.is_err());
        let health = crate::llm::run_provider_healthcheck("openai").await;
        assert!(!health.valid);
        assert!(health.message.contains("unsupported_billing_shape"));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // Refusing the unsupported first operation still latched the ceiling;
        // omitting the option on a supported call retains conservative receipt.
        let result = vm_call_llm_full(&omitted).await.unwrap();
        assert_eq!(result.text, "hello");
        assert!(crate::llm::admission::receipt().is_some());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn conservative_admission_routing_cannot_widen_caller_limits_before_first_attempt() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, false);
        install(server.addr());
        for per_call in [false, true] {
            crate::llm::cost::reset_cost_state();
            let mut opts = options(if per_call { 1.0 } else { 0.01 }, false);
            if per_call {
                opts.budget.as_mut().unwrap().max_cost_usd = Some(0.01);
            }
            let mut policy = crate::llm::routing::build_transport_failover_policy(
                "openai",
                "gpt-5.6-luna",
                &[super::super::LlmRouteFallback {
                    provider: "openai".into(),
                    model: "gpt-5.6-sol".into(),
                }],
                &[],
            )
            .unwrap();
            let rules = &mut Arc::make_mut(&mut policy).budget;
            rules.session_usd = Some(1.0);
            rules.per_call_usd = Some(1.0);
            opts.routing_policy = Some(policy);
            let result = vm_call_llm_full(&opts).await;
            assert_eq!(
                count.load(Ordering::SeqCst),
                0,
                "a policy must not widen the caller's first reservation"
            );
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains("insufficient_allowance"),
                "{error}"
            );
        }
    });
}
