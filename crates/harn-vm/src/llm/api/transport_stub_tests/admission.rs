//! Real local HTTP controls for conservative admission; no provider mocks.
use super::*;
use crate::llm::admission::{swap_scope, AdmissionMode, AdmissionScope};
use crate::llm::cost::LlmBudgetEnvelope;
use crate::llm::{MachineSpendPolicy, MachineSpendQuota};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod publication;

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

fn rejected_stub(count: Arc<AtomicUsize>) -> LlmStub {
    spawn_llm_stub_many("provider rejection after dispatch", 1, move |_, stream| {
        use std::io::{Read, Write};
        count.fetch_add(1, Ordering::SeqCst);
        let mut bytes = [0u8; 16_384];
        let n = stream.read(&mut bytes).unwrap();
        assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /v1/chat/completions "));
        let body = r#"{"error":{"message":"provider rejected the dispatched request"}}"#;
        write!(stream, "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    })
}

fn native_parser_failure_stub(count: Arc<AtomicUsize>) -> LlmStub {
    spawn_llm_stub_many("native tool parser failure", 1, move |_, stream| {
        use std::io::{Read, Write};
        count.fetch_add(1, Ordering::SeqCst);
        let mut bytes = [0u8; 16_384];
        let n = stream.read(&mut bytes).unwrap();
        assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /v1/chat/completions "));
        let body = r#"{"error":{"message":"tool call parser hit unexpected EOF while parsing tool_calls"}}"#;
        write!(stream, "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
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
fn disabled_real_dispatch_releases_its_pre_dispatch_admission_hold() {
    let _env = env_guard();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let quota_path = temp.path().join("disabled-dispatch.sqlite");
        let quota = MachineSpendQuota::open(
            &quota_path,
            "person",
            MachineSpendPolicy {
                daily_limit_microusd: Some(100_000_000),
                monthly_limit_microusd: Some(100_000_000),
                lifetime_limit_microusd: None,
            },
        )
        .unwrap();
        let later_quota = MachineSpendQuota::open(
            temp.path().join("later-install.sqlite"),
            "other-person",
            MachineSpendPolicy {
                daily_limit_microusd: Some(100_000_000),
                monthly_limit_microusd: Some(100_000_000),
                lifetime_limit_microusd: None,
            },
        )
        .unwrap();
        let budget = crate::llm::ConservativeLlmBudget::new(5.0).unwrap();
        budget
            .scope(async {
                quota
                    .scope(async {
                        let mut opts = options(5.0, false);
                        opts.budget = None;
                        let request = LlmRequestPayload::from(&opts);
                        let reservation = crate::llm::admission::reserve(
                            &opts,
                            &request,
                            crate::llm::cost_context::LlmCallTokenProjection::for_options(&opts),
                        )
                        .unwrap()
                        .expect("the direct OpenAI route must create a real admission hold");
                        assert!(budget.receipt().unwrap().in_flight_usd > rust_decimal::Decimal::ZERO);
                        reservation.cancel_before_dispatch().unwrap();
                        assert_eq!(budget.receipt().unwrap().in_flight_usd, rust_decimal::Decimal::ZERO);

                        let _disabled = crate::llm::test_env::ScopedEnvVar::set(
                            crate::llm::LLM_CALLS_DISABLED_ENV,
                            "1",
                        );
                        let error = vm_call_llm_full(&opts).await.unwrap_err();
                        assert!(error
                            .to_string()
                            .contains(crate::llm::LLM_CALLS_DISABLED_ENV));
                        let receipt = budget.receipt().unwrap();
                        assert_eq!(receipt.in_flight_usd, rust_decimal::Decimal::ZERO);
                        assert_eq!(receipt.uncertain_usd, rust_decimal::Decimal::ZERO);
                    })
                    .await
                    .unwrap();

                // Both rejected calls were canceled, so a later authority can
                // still be installed on this same execution ledger.
                later_quota.scope(async {}).await.unwrap();
            })
            .await
            .unwrap();
        let receipt = quota.receipt().unwrap();
        assert_eq!(receipt.reserved_microusd, 0);
        assert_eq!(receipt.lifetime_unsettled_attempts, 0);
        let connection = rusqlite::Connection::open(quota_path).unwrap();
        let audit = connection
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN settled = 0 THEN 1 ELSE 0 END), COALESCE(SUM(actual), 0) FROM spend_attempt WHERE scope = ?1",
                ["person"],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            )
            .unwrap();
        assert_eq!(audit, (2, 0, 0), "zero-dispatch holds remain audited");
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
    let dispatches = crate::llm::provider_dispatch::ProviderDispatchLedger::default();
    runtime.block_on(
        crate::llm::provider_dispatch::with_provider_dispatch_ledger(dispatches.clone(), async {
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
        }),
    );
    assert_eq!(
        dispatches.observed(),
        2,
        "inline and spawned transport share the measurement"
    );
}

#[test]
fn conservative_admission_does_not_recycle_an_empty_attempt_for_retry() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    crate::llm::trace::reset_trace_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, true);
        install(server.addr());
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        crate::llm::push_llm_transcript_dir(
            transcript_dir
                .path()
                .to_str()
                .expect("utf8 transcript directory"),
        );
        let error = vm_call_llm_full(&options(0.6, false)).await.unwrap_err();
        crate::llm::pop_llm_transcript_dir();
        assert!(error.to_string().contains("conservative"), "{error}");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "retry was denied before transport"
        );
        let events: Vec<serde_json::Value> =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("attempt transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript event"))
                .collect();
        let response = events
            .iter()
            .find(|event| event["type"] == "provider_call_response")
            .expect("the first billed provider response was recorded");
        let denial = events
            .iter()
            .find(|event| event["type"] == "provider_call_error")
            .expect("the second attempt denial was recorded");
        let first_cost = response["known_cost_usd"]
            .as_f64()
            .expect("first response has a priced ledger");
        assert!(first_cost > 0.0, "the first attempt was billed");
        assert_eq!(denial["provider_call_count"], 1);
        assert_eq!(denial["known_cost_usd"], first_cost);
        assert_eq!(denial["usage_unknown_calls"], 0);
        assert_eq!(denial["unpriced_calls"], 0);
        assert_eq!(
            crate::llm::trace::peek_trace_usage_summary()
                .cost
                .provider_call_count,
            1,
            "the refused retry must not become an unknown provider attempt"
        );
    });
}

#[test]
fn conservative_admission_keeps_a_dispatched_unreported_attempt_unknown() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    crate::llm::trace::reset_trace_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let server = native_parser_failure_stub(count.clone());
        install(server.addr());
        let mut opts = options(0.6, false);
        opts.native_tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Edit a file.",
                "parameters": {"type": "object", "properties": {}}
            }
        })]);
        opts.tools = Some(crate::value::VmValue::Nil);
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        crate::llm::push_llm_transcript_dir(
            transcript_dir
                .path()
                .to_str()
                .expect("utf8 transcript directory"),
        );
        let error = crate::llm::agent_observe::observed_llm_call(
            &opts,
            Some("native"),
            None,
            None,
            false,
            false,
            None,
            None,
        )
        .await
        .expect_err("conservative admission should refuse the degraded retry");
        crate::llm::pop_llm_transcript_dir();
        assert!(error.to_string().contains("conservative"), "{error}");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "first request reached HTTP"
        );

        let events: Vec<serde_json::Value> =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("attempt transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript event"))
                .collect();
        let denial = events
            .iter()
            .rev()
            .find(|event| event["type"] == "provider_call_error")
            .expect("terminal retry denial was recorded");
        assert_eq!(denial["provider_call_count"], 1);
        assert_eq!(denial["usage_unknown_calls"], 1);
        assert_eq!(denial["unpriced_calls"], 1);
        assert!(denial["cost_usd"].is_null());
        let summary = crate::llm::trace::peek_trace_usage_summary().cost;
        assert_eq!(summary.provider_call_count, 1);
        assert_eq!(summary.usage_unknown_calls, 1);
    });
}

#[test]
fn provider_error_after_http_dispatch_keeps_usage_unknown() {
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
        let server = rejected_stub(count.clone());
        install(server.addr());
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        crate::llm::push_llm_transcript_dir(
            transcript_dir
                .path()
                .to_str()
                .expect("utf8 transcript path"),
        );
        let error = vm_call_llm_full(&options(0.6, false))
            .await
            .expect_err("HTTP error after dispatch surfaces");
        crate::llm::pop_llm_transcript_dir();
        assert!(!error.to_string().is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 1, "request reached HTTP stub");
        let events: Vec<serde_json::Value> =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("provider error transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript event"))
                .collect();
        let error = events
            .iter()
            .find(|event| event["type"] == "provider_call_error")
            .expect("terminal provider error observation");
        assert_eq!(error["provider_call_count"], 1);
        assert_eq!(error["usage_unknown_calls"], 1);
        assert_eq!(error["unpriced_calls"], 1);
        assert!(error["cost_usd"].is_null());
    });
}

#[test]
fn machine_quota_survives_retry_and_process_style_reopen_on_real_transport() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("person-spend.sqlite");
        let policy = MachineSpendPolicy {
            daily_limit_microusd: Some(600_000),
            monthly_limit_microusd: Some(600_000),
            lifetime_limit_microusd: None,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let server = stub(count.clone(), false, true);
        install(server.addr());
        let mut opts = options(0.6, false);
        opts.budget = None;
        let quota = MachineSpendQuota::open(&path, "person", policy.clone()).unwrap();
        let first = quota
            .scope(vm_call_llm_full(&opts))
            .await
            .unwrap()
            .unwrap_err();
        assert!(first.to_string().contains("budget_exceeded"), "{first}");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let receipt = quota.receipt().unwrap();
        assert!(receipt.reserved_microusd > 300_000);
        assert_eq!(receipt.actual_known_microusd, 0);
        assert_eq!(receipt.usage_unknown_attempts, 1);
        drop(quota);
        crate::llm::cost::reset_cost_state();

        // Reopening the same machine scope from a second host/process cannot
        // grant the remainder already held by an uncertain attempt.
        let restarted = MachineSpendQuota::open(&path, "person", policy).unwrap();
        let second = restarted
            .scope(vm_call_llm_full(&opts))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            second.to_string().contains("machine spend quota"),
            "{second}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
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
