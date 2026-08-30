use super::{base_opts, spawn_llm_stub_many, LlmStub};
use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::env_guard;

fn spawn_ollama_empty_stub_many(
    request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_requests: usize,
) -> LlmStub {
    spawn_llm_stub_many(
        "ollama empty stub",
        max_requests,
        move |_attempt, stream| {
            use std::io::{Read, Write};
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

            let body =
                "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":5,\"eval_count\":0}\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        },
    )
}

#[test]
fn ollama_empty_content_done_frame_keeps_terminal_usage() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    crate::llm::trace::reset_trace_state();
    crate::llm::trace::enable_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_ollama_empty_stub_many(request_count.clone(), 2);
        let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://{}", server.addr()));
        }
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
                    &base_opts("ollama"),
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
        match prev_ollama_host {
            Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
            None => unsafe { std::env::remove_var("OLLAMA_HOST") },
        }

        assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        let receipt: serde_json::Value =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("provider error transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
                .find(|event: &serde_json::Value| event["type"] == "provider_call_error")
                .expect("terminal provider error receipt");
        assert_eq!(receipt["input_tokens"], 10);
        assert_eq!(receipt["output_tokens"], 0);
        assert_eq!(receipt["cost_usd"], 0.0);
        assert_eq!(receipt["known_cost_usd"], 0.0);
        assert_eq!(receipt["provider_call_count"], 2);
        assert_eq!(receipt["unpriced_calls"], 0);
        assert_eq!(receipt["usage_unknown_calls"], 0);
        assert_eq!(receipt["accounting_status"], "reported");
        let trace = crate::llm::trace::take_trace();
        assert_eq!(trace.len(), 1, "terminal usage must reach the run summary");
        assert_eq!(trace[0].usage.input_tokens, 10);
        assert_eq!(trace[0].usage.cost_usd, Some(0.0));
        assert_eq!(trace[0].usage.provider_call_count, 2);
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

        drop(server);
    });
}
