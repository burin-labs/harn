use super::*;
use crate::llm::api::vm_call_completion_full;
use crate::llm::rate_limit::provider_request_usage;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::reset_thread_local_state();
        crate::llm_config::clear_user_overrides();
    }
}

fn assert_completion_admission(endpoint: Option<&str>) {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let (release_tx, release_rx) = mpsc::channel();
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let mut received_tx = Some(received_tx);
        let path = endpoint.unwrap_or("/chat/completions").to_owned();
        let ollama = endpoint == Some("/api/generate");
        let chat = endpoint.is_none();
        let server = spawn_llm_stub_many("completion admission", 2, move |attempt, stream| {
            let (headers, request) = ollama_openai_compat::read_http_request(stream);
            assert!(headers.starts_with(&format!("POST {path} ")));
            assert_eq!(request["model"], "completion-model");
            if !chat {
                assert_eq!(request["prompt"], "prefix");
                assert_eq!(request["suffix"], "suffix");
            }
            seen.fetch_add(1, Ordering::SeqCst);
            let body = if ollama {
                serde_json::json!({"response":"provider-reached", "done":true,
                    "done_reason":"stop", "prompt_eval_count":3, "eval_count":1})
            } else {
                let choice = if chat {
                    serde_json::json!({"message":{"role":"assistant", "content":"provider-reached"}, "finish_reason":"stop"})
                } else {
                    serde_json::json!({"text":"provider-reached", "finish_reason":"stop"})
                };
                serde_json::json!({"id":"completion-proof", "model":"completion-model",
                    "choices":[choice], "usage":{"prompt_tokens":3, "completion_tokens":1}})
            }.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).unwrap();
            stream.flush().unwrap();
            if attempt == 0 {
                assert!(received_tx.take().unwrap().send(()).is_ok());
                release_rx.recv().unwrap();
                // Cancellation can close this connection before the body is released.
                let _ = stream.write_all(body.as_bytes());
            } else {
                stream.write_all(body.as_bytes()).unwrap();
            }
        });
        // Drop before the stub, including on assertion failure, to release its body gate.
        let mut release = ReleaseBody(Some(release_tx));
        let provider = "completion-admission";
        let mut config = crate::llm_config::ProvidersConfig::default();
        config.providers.insert(provider.into(), crate::llm_config::ProviderDef {
            base_url: format!("http://{}", server.addr()),
            chat_endpoint: "/chat/completions".into(),
            completion_endpoint: endpoint.map(str::to_owned),
            auth_style: "none".into(),
            auth_env: crate::llm_config::AuthEnv::None,
            rate_limits: Some(crate::llm_config::RateLimitsDef {
                rpm: Some(1000), concurrency: Some(1), ..Default::default()
            }),
            ..Default::default()
        });
        crate::llm_config::set_user_overrides(Some(config));
        let opts = super::super::LlmCallOptions {
            provider: provider.into(), model: "completion-model".into(), max_tokens: 8, stream: false,
            messages: vec![serde_json::json!({"role":"user", "content":"prefix"})],
            ..Default::default()
        };
        let mut first = Box::pin(vm_call_completion_full(&opts, "prefix", Some("suffix")));
        tokio::select! {
            received = received_rx => received.unwrap(),
            result = &mut first => panic!("completion ended before its response body: {result:?}"),
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1, "actual provider request reached");
        assert_eq!(provider_request_usage(provider), 1, "native transport must consume one request slot");
        let mut second = Box::pin(vm_call_completion_full(&opts, "prefix", Some("suffix")));
        assert!(futures::poll!(&mut second).is_pending());
        assert_eq!(provider_request_usage(provider), 1, "queued completion consumes no slot");
        drop(first);
        release.release();
        let result = second.await.unwrap();
        assert_eq!(result.text, "provider-reached");
        assert_eq!((result.input_tokens, result.output_tokens), (3, 1));
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(provider_request_usage(provider), 2, "each started request admitted exactly once");
    });
}

#[test]
fn native_completion_holds_admission_through_body_and_releases_on_cancellation() {
    for endpoint in ["/completions", "/api/generate"] {
        assert_completion_admission(Some(endpoint));
    }
}

#[test]
fn completion_chat_fallback_admits_each_request_once() {
    assert_completion_admission(None);
}
