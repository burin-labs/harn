use super::{allow_stubbed_llm_transport, env_guard, read_http_request, spawn_llm_stub};
use crate::llm::api::{vm_call_completion_full, vm_call_llm_full, LlmCallOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
    }
}

#[derive(Clone, Copy)]
enum Invocation {
    Chat,
    Completion,
}

fn exercise(invocation: Invocation) {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(None));
    let server_requests = requests.clone();
    let server_observed = observed.clone();
    let server = spawn_llm_stub("Hyperbolic terminal usage", move |stream| {
        use std::io::Write;
        let (headers, request) = read_http_request(stream);
        server_requests.fetch_add(1, Ordering::SeqCst);
        *server_observed.lock().expect("request lock") = Some((headers, request.clone()));
        let content = serde_json::json!({
            "id": "hyperbolic-fixture", "object": "chat.completion.chunk",
            "model": "glm-5.3-flash",
            "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": "stop"}]
        });
        let mut body = format!("data: {content}\n\n");
        // A missing request flag must not receive a synthetic usage frame.
        if request["stream_options"]["include_usage"] == true {
            let usage = serde_json::json!({
                "id": "hyperbolic-fixture", "object": "chat.completion.chunk",
                "model": "glm-5.3-flash", "choices": [],
                "usage": {"prompt_tokens": 13, "completion_tokens": 2, "total_tokens": 15}
            });
            body.push_str(&format!("data: {usage}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        write!(stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ).expect("write terminal usage response");
    });
    let provider = crate::llm_config::provider_config("hyperbolic").expect("catalog provider");
    let prefix = reqwest::Url::parse(&provider.base_url).expect("catalog URL");
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "hyperbolic",
            format!("http://{}{}", server.addr(), prefix.path()),
        )
        .expect("local fixture endpoint"),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let options = LlmCallOptions {
        provider: "hyperbolic".to_string(),
        model: "glm-5.3-flash".to_string(),
        api_key: "local-fixture-not-a-provider-key".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        max_tokens: 16,
        data_controls: Some(crate::llm_config::DataPosture::Default),
        ..Default::default()
    };
    let outcome = runtime.block_on(async {
        match invocation {
            Invocation::Chat => vm_call_llm_full(&options).await,
            Invocation::Completion => vm_call_completion_full(&options, "prefix", None).await,
        }
    });
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let (headers, request) = observed
        .lock()
        .expect("request lock")
        .clone()
        .expect("actual request");
    assert!(
        headers.starts_with("POST /inference/v1/chat/completions HTTP/1.1\r\n"),
        "{headers}"
    );
    let result = outcome.expect("fixture call");
    assert_eq!(result.text, "hello");
    assert_eq!(result.telemetry.server_prompt_tokens, Some(13));
    assert_eq!(result.telemetry.server_output_tokens, Some(2));
    assert_eq!(request["stream"], true);
    assert_eq!(request["stream_options"]["include_usage"], true);
    assert!(provider.completion_endpoint.is_none());
    assert!(provider.cache_usage_accounting.is_none());
    assert_eq!(prefix.host_str(), Some("api.hyperbolic.ai"));
}

#[test]
fn hyperbolic_chat_route_requests_and_collects_terminal_usage() {
    exercise(Invocation::Chat);
}

#[test]
fn hyperbolic_completion_uses_chat_instead_of_retired_native_endpoint() {
    exercise(Invocation::Completion);
}
