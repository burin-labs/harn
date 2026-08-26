use super::{base_opts, spawn_llm_stub_many, LlmStub};
use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::env_guard;

fn spawn_priced_empty_stub_many(
    request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_requests: usize,
) -> LlmStub {
    spawn_llm_stub_many("priced empty stub", max_requests, move |attempt, stream| {
        use std::io::{Read, Write};
        request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));
        let body = format!(
            "{{\"message\":{{\"role\":\"assistant\",\"content\":\"\"}},\"done\":true,\"prompt_eval_count\":13,\"eval_count\":0,\"model\":\"paid-empty\",\"attempt\":{attempt}}}\n"
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

#[test]
fn priced_empty_completions_keep_terminal_usage_across_every_projection() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    crate::llm::trace::reset_trace_state();
    crate::llm::trace::enable_tracing();
    let metrics = std::sync::Arc::new(crate::MetricsRegistry::default());
    crate::install_active_metrics_registry(metrics.clone());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_priced_empty_stub_many(request_count.clone(), 2);
        let mut provider_overlay = crate::llm_config::ProvidersConfig::default();
        provider_overlay.providers.insert(
            "terminal-priced-empty".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{}", server.addr()),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                chat_endpoint: "/api/chat".to_string(),
                cost_per_1k_in: Some(0.00045),
                cost_per_1k_out: Some(0.0),
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(provider_overlay));
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.terminal-priced-empty]]
model_match = "paid-empty"
message_wire_format = "ollama"
"#,
        )
        .expect("capability override");
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        crate::llm::agent_observe::push_llm_transcript_dir(
            transcript_dir
                .path()
                .to_str()
                .expect("utf8 transcript path"),
        );

        let local = tokio::task::LocalSet::new();
        let error = local
            .run_until(async {
                crate::llm::agent_observe::observed_llm_call(
                    &{
                        let mut opts = base_opts("terminal-priced-empty");
                        opts.model = "paid-empty".to_string();
                        opts
                    },
                    None,
                    None,
                    None,
                    false,
                    false,
                    None,
                    None,
                )
                .await
            })
            .await
            .expect_err("two empty responses must exhaust the route");
        crate::llm::agent_observe::pop_llm_transcript_dir();
        crate::llm_config::clear_user_overrides();
        crate::llm::capabilities::clear_user_overrides();

        assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        let receipt: serde_json::Value =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("provider error transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
                .find(|event: &serde_json::Value| event["type"] == "provider_call_error")
                .expect("terminal provider error receipt");
        assert_eq!(receipt["input_tokens"], 26);
        assert_eq!(receipt["output_tokens"], 0);
        assert_eq!(receipt["cost_usd"], 0.0000117);
        assert_eq!(receipt["known_cost_usd"], 0.0000117);
        assert_eq!(receipt["provider_call_count"], 2);
        assert_eq!(receipt["unpriced_calls"], 0);
        assert_eq!(receipt["usage_unknown_calls"], 0);
        assert_eq!(receipt["accounting_status"], "reported");
        let run_summary = crate::llm::peek_trace_usage_summary();
        assert_eq!(run_summary.call_count, 1, "terminal usage must reach the run summary");
        assert_eq!(run_summary.input_tokens, 26);
        assert_eq!(run_summary.output_tokens, 0);
        assert_eq!(run_summary.cost.known_cost_usd, 0.0000117);
        assert_eq!(run_summary.cost.unpriced_calls, 0);
        assert_eq!(run_summary.cost.usage_unknown_calls, 0);
        let trace = crate::llm::trace::take_trace();
        assert_eq!(trace[0].usage.provider_call_count, 2);
        let rendered_metrics = metrics.render_prometheus();
        for expected in [
            "harn_llm_calls_total{model=\"paid-empty\",outcome=\"retries_exhausted\",provider=\"terminal-priced-empty\"} 1",
            "harn_llm_cost_usd_total{model=\"paid-empty\",provider=\"terminal-priced-empty\"} 0.0000117",
            "harn_llm_provider_requests_total{model=\"paid-empty\",provider=\"terminal-priced-empty\"} 2",
            "harn_llm_unpriced_requests_total{model=\"paid-empty\",provider=\"terminal-priced-empty\"} 0",
            "harn_llm_usage_unknown_requests_total{model=\"paid-empty\",provider=\"terminal-priced-empty\"} 0",
        ] {
            assert!(
                rendered_metrics.contains(expected),
                "missing {expected} in metrics:\n{rendered_metrics}"
            );
        }
        let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
            panic!("expected a typed provider-exhausted error");
        };
        assert_eq!(
            fields.get("code").map(crate::value::VmValue::display),
            Some("provider_exhausted".to_string())
        );
        assert_eq!(
            fields.get("reason").map(crate::value::VmValue::display),
            Some("empty_generation".to_string())
        );
        crate::llm::trace::reset_trace_state();
        crate::clear_active_metrics_registry();

        drop(server);
    });
}
