//! Transport tests driven by in-process HTTP stubs.
//!
//! These spawn real listeners and speak real HTTP, so they exercise the
//! streaming/offthread paths end to end. The stub scaffolding is deliberately
//! blocking rather than polling — see `accept_with_shutdown` for why.

use super::options::base_opts;
use super::test_support::{allow_stubbed_llm_transport, ScopedEnvVar};
use super::{
    vm_call_llm_full, vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread,
    ThinkingConfig,
};
use crate::llm::env_guard;

/// Cooperative accept: blocks the stub thread on a real
/// `accept()` call until a client connects, then returns the
/// stream. Shutdown wakes the thread by self-connecting to the
/// listener (see [`LlmStub::drop`]) — when the resulting `accept`
/// returns, the shutdown flag is checked and the synthetic stream
/// is discarded.
///
/// This replaces a previous nonblocking polling loop with a 5ms
/// sleep tick. Polling introduced two flake modes under nextest's
/// 50× concurrent flake-detection profile: (1) a real client
/// connection could land between two polls and reqwest could time
/// out on the SYN-ACK before the stub thread woke; (2) under
/// heavy CPU contention the 5ms tick could stretch to tens of
/// milliseconds, compounding (1). Blocking accept removes the
/// scheduling-latency variable entirely.
fn accept_with_shutdown(
    listener: &std::net::TcpListener,
    label: &str,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Option<std::net::TcpStream> {
    let (stream, _peer) = listener
        .accept()
        .unwrap_or_else(|e| panic!("{label}: accept failed: {e}"));
    if shutdown.load(std::sync::atomic::Ordering::Acquire) {
        drop(stream);
        return None;
    }
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    Some(stream)
}

/// Wake a stub thread blocked in [`accept_with_shutdown`] by
/// opening a one-shot self-connection to its listener. The thread
/// then observes the shutdown flag and exits without serving the
/// connection. The connect uses a short timeout so a deferred
/// shutdown (e.g. drop during panic unwind on a saturated CI
/// worker) cannot wedge the test process.
fn wake_accept_for_shutdown(addr: std::net::SocketAddr) {
    let _ = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500));
}

/// RAII guard for an in-process LLM stub. Dropping signals the stub
/// thread to exit and joins it so no FDs leak past the test, even on
/// panic.
struct LlmStub {
    addr: std::net::SocketAddr,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Maximum number of `accept()` calls the stub thread can be
    /// parked on. Single-shot stubs use 1; `spawn_llm_stub_many`
    /// uses its connection count. Drop fires that many self-
    /// connections so every parked accept observes shutdown.
    pending_accepts: usize,
}

impl LlmStub {
    fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

impl Drop for LlmStub {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        // Self-connect to unblock any thread parked inside
        // `accept_with_shutdown`. Multiple stubs in
        // `spawn_llm_stub_many` may need waking, so issue one
        // wake per outstanding accept slot.
        for _ in 0..self.pending_accepts.max(1) {
            wake_accept_for_shutdown(self.addr);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Bind a localhost listener and run `body` on a background thread once
/// a client connects. Wraps the listener in an [`LlmStub`] guard whose
/// lifetime bounds the stub thread.
fn spawn_llm_stub<F>(label: &'static str, body: F) -> LlmStub
where
    F: FnOnce(&mut std::net::TcpStream) + Send + 'static,
{
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind llm stub");
    let addr = listener.local_addr().expect("stub addr");
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();
    let handle = std::thread::spawn(move || {
        let Some(mut stream) = accept_with_shutdown(&listener, label, &shutdown_thread) else {
            return;
        };
        body(&mut stream);
    });
    LlmStub {
        addr,
        shutdown,
        handle: Some(handle),
        pending_accepts: 1,
    }
}

fn spawn_llm_stub_many<F>(label: &'static str, connections: usize, mut body: F) -> LlmStub
where
    F: FnMut(usize, &mut std::net::TcpStream) + Send + 'static,
{
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind llm stub");
    let addr = listener.local_addr().expect("stub addr");
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();
    let handle = std::thread::spawn(move || {
        for attempt in 0..connections {
            let Some(mut stream) = accept_with_shutdown(&listener, label, &shutdown_thread) else {
                return;
            };
            body(attempt, &mut stream);
        }
    });
    LlmStub {
        addr,
        shutdown,
        handle: Some(handle),
        pending_accepts: connections,
    }
}

fn spawn_ollama_stub() -> LlmStub {
    spawn_llm_stub("ollama stub", |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

        let body = concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"hello \"},\"done\":false,\"model\":\"stub-model\"}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"world\"},\"done\":false}\n",
            "{\"done\":true,\"prompt_eval_count\":3,\"eval_count\":2,\"model\":\"stub-model\"}\n"
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

fn spawn_ollama_empty_then_success_stub(
    request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> LlmStub {
    spawn_llm_stub_many("ollama retry stub", 2, move |attempt, stream| {
        use std::io::{Read, Write};
        request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

        let body = if attempt == 0 {
            "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":5,\"eval_count\":3}\n"
        } else {
            concat!(
                "{\"message\":{\"role\":\"assistant\",\"content\":\"retried\"},\"done\":false,\"model\":\"stub-model\"}\n",
                "{\"done\":true,\"prompt_eval_count\":5,\"eval_count\":1,\"model\":\"stub-model\"}\n"
            )
        };
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

fn spawn_openai_empty_stub(
    request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> LlmStub {
    spawn_openai_empty_stub_many(request_count, 2)
}

fn spawn_openai_success_stub() -> LlmStub {
    spawn_llm_stub("OpenAI-compatible success stub", |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16_384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        let body = r#"{"id":"ok","object":"chat.completion","created":0,"model":"qwen3.6-35b-a3b-ud-q4-k-xl","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#;
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

/// Same success shape as [`spawn_openai_success_stub`], plus the
/// `system_fingerprint` an OpenAI-compatible server publishes to identify the
/// backend build it served the call with. The body is transcribed from an
/// observed llama.cpp non-streaming response, including its top-level field
/// placement and `timings` extension.
fn spawn_openai_fingerprint_stub() -> LlmStub {
    spawn_llm_stub("OpenAI-compatible fingerprint stub", |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16_384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        let body = r#"{"choices":[{"finish_reason":"stop","index":0,"message":{"role":"assistant","content":"hello"}}],"created":0,"model":"qwen3.6-35b-a3b-ud-q4-k-xl","system_fingerprint":"b9994-14d3ba45f","object":"chat.completion","usage":{"completion_tokens":1,"prompt_tokens":3,"total_tokens":4},"id":"ok","timings":{"prompt_n":3,"prompt_ms":37.102,"predicted_n":1,"predicted_ms":33.096}}"#;
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

fn spawn_openai_empty_stub_many(
    request_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_requests: usize,
) -> LlmStub {
    spawn_llm_stub_many(
        "openai empty stub",
        max_requests,
        move |_attempt, stream| {
            use std::io::{Read, Write};
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = vec![0u8; 16_384];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
            let body = r#"{"id":"empty","object":"chat.completion","created":0,"model":"empty-primary","choices":[{"index":0,"message":{"role":"assistant","content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":0,"total_tokens":1}}"#;
            let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        },
    )
}

fn install_openai_stub_provider(provider: &str, addr: std::net::SocketAddr) {
    let cache_usage_accounting = crate::llm_config::provider_config(provider)
        .and_then(|provider| provider.cache_usage_accounting);
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.to_string(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}/v1"),
            auth_style: "none".to_string(),
            auth_env: crate::llm_config::AuthEnv::None,
            chat_endpoint: "/chat/completions".to_string(),
            cache_usage_accounting,
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
}

#[path = "transport_managed_supply_tests.rs"]
mod managed_supply_tests;

#[path = "transport_cache_accounting_tests.rs"]
mod cache_accounting_tests;

#[test]
fn capability_admission_rejects_before_transport_egress() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_count = request_count.clone();
    let server = spawn_llm_stub("capability-admission sentinel", move |_| {
        observed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    install_openai_stub_provider("admission-sentinel", server.addr());
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.admission-sentinel]]
model_match = "unsupported-temperature"
temperature_supported = false
"#,
    )
    .expect("capability overlay");

    let mut opts = base_opts("admission-sentinel");
    opts.model = "unsupported-temperature".to_string();
    opts.temperature = Some(0.2);
    opts.portable_option_intent
        .insert(crate::llm::capabilities::PortableOption::Temperature);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = runtime
        .block_on(crate::llm::agent_observe::observed_llm_call(
            &opts, None, None, None, false, false, None, None,
        ))
        .expect_err("authored capability denial must reject");
    let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
        panic!("capability denial must use the canonical structured envelope");
    };
    assert_eq!(
        fields.get("kind").map(crate::value::VmValue::display),
        Some("terminal".to_string())
    );
    assert_eq!(
        fields.get("reason").map(crate::value::VmValue::display),
        Some("invalid_request".to_string())
    );
    assert_eq!(
        fields.get("provider").map(crate::value::VmValue::display),
        Some("admission-sentinel".to_string())
    );
    assert_eq!(
        fields.get("model").map(crate::value::VmValue::display),
        Some("unsupported-temperature".to_string())
    );
    assert_eq!(
        request_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the provider sentinel must observe no request"
    );

    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    drop(server);
}

fn spawn_ollama_stub_with_body_capture(
    captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> LlmStub {
    spawn_llm_stub("ollama stub (capture)", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string();
        *captured.lock().expect("capture body") = Some(body);

        let body = concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":false}\n",
            "{\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n"
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

fn spawn_ollama_raw_generate_stub(
    captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> LlmStub {
    spawn_llm_stub("ollama raw stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(request.starts_with("POST /api/generate HTTP/1.1\r\n"));
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string();
        *captured.lock().expect("capture body") = Some(body);

        let body = concat!(
            "{\"response\":\"<tool_call>\\nedit({ path: \\\"a.rs\\\" })\\n</tool_call>\",\"done\":false,\"model\":\"qwen3.5:stub\"}\n",
            "{\"done\":true,\"prompt_eval_count\":7,\"eval_count\":11,\"model\":\"qwen3.5:stub\",\"done_reason\":\"stop\"}\n"
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

fn spawn_anthropic_stub_with_request_capture(
    captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> LlmStub {
    spawn_llm_stub("anthropic stub (capture)", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16384];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        *captured.lock().expect("capture request") = Some(request);

        let body = concat!(
            r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-opus-4-6","#,
            r#""content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","#,
            r#""usage":{"input_tokens":1,"output_tokens":1}}"#
        );
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

#[test]
fn anthropic_interleaved_thinking_beta_header_is_sent_for_supported_model() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_anthropic_stub_with_request_capture(captured.clone());
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.providers.insert(
            "anthropic".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{}", server.addr()),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                extra_headers: std::collections::BTreeMap::from([(
                    "anthropic-version".to_string(),
                    "2023-06-01".to_string(),
                )]),
                chat_endpoint: "/messages".to_string(),
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let mut opts = base_opts("anthropic");
        opts.model = "claude-opus-4-6".to_string();
        opts.stream = false;
        opts.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(8000),
        };
        let result = vm_call_llm_full(&opts)
            .await
            .expect("stubbed Anthropic response");

        crate::llm_config::clear_user_overrides();
        drop(server);

        assert_eq!(result.text, "ok");
        let request = captured
            .lock()
            .expect("captured request")
            .clone()
            .expect("request captured")
            .to_lowercase();
        assert!(
            request.contains("anthropic-beta: interleaved-thinking-2025-05-14\r\n"),
            "{request}"
        );
    });
}

#[test]
fn offthread_streaming_completes_inside_localset() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let server = spawn_ollama_stub();
        let addr = server.addr();
        let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
        }

        let local = tokio::task::LocalSet::new();
        let result = local
            .run_until(async {
                let opts = base_opts("ollama");
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                    .await
                    .expect("llm call should succeed");

                let mut deltas = Vec::new();
                while let Ok(delta) = rx.try_recv() {
                    deltas.push(delta);
                }
                (result, deltas)
            })
            .await;

        match prev_ollama_host {
            Some(value) => unsafe {
                std::env::set_var("OLLAMA_HOST", value);
            },
            None => unsafe {
                std::env::remove_var("OLLAMA_HOST");
            },
        }

        drop(server);

        let (result, deltas) = result;
        assert_eq!(result.text, "hello world");
        assert_eq!(result.model, "stub-model");
        assert_eq!(result.input_tokens, 3);
        assert_eq!(result.output_tokens, 2);
        assert!(!result.cache_supported);
        assert_eq!(
            result.telemetry.serving_base_url.as_deref(),
            Some(format!("http://{addr}").as_str())
        );
        assert_eq!(deltas.join(""), "hello world");
    });
}

#[test]
fn llamacpp_openai_transport_reports_route_and_unsupported_cache_accounting() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let server = spawn_openai_success_stub();
        let addr = server.addr();
        install_openai_stub_provider("llamacpp", addr);
        crate::llm_config::set_runtime_provider_endpoint_overrides(
            crate::llm_config::RuntimeProviderEndpointOverrides::single(
                "llamacpp",
                format!("http://{addr}/v1"),
            )
            .expect("valid stub endpoint"),
        );
        assert_eq!(
            crate::llm_config::provider_config("llamacpp")
                .and_then(|provider| provider.cache_usage_accounting),
            Some(false)
        );

        let mut opts = base_opts("llamacpp");
        opts.model = "qwen3.6-35b-a3b-ud-q4-k-xl".to_string();
        opts.stream = false;
        opts.cache = false;
        let result = vm_call_llm_full(&opts).await;
        let result = result.expect("llama.cpp-compatible call should succeed");

        assert_eq!(result.model, "qwen3.6-35b-a3b-ud-q4-k-xl");
        assert!(result.input_tokens > 0);
        assert!(result.output_tokens > 0);
        assert_eq!(result.cache_read_tokens, 0);
        assert_eq!(result.cache_write_tokens, 0);
        assert!(!result.cache_supported);
        assert_eq!(
            result.telemetry.serving_base_url.as_deref(),
            Some(format!("http://{addr}/v1").as_str())
        );

        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        drop(server);

        let server = spawn_openai_success_stub();
        install_openai_stub_provider("openai", server.addr());
        crate::llm_config::set_runtime_provider_endpoint_overrides(
            crate::llm_config::RuntimeProviderEndpointOverrides::single(
                "openai",
                format!("http://{}/v1", server.addr()),
            )
            .expect("valid stub endpoint"),
        );
        let mut opts = base_opts("openai");
        opts.model = "gpt-4o-mini".to_string();
        opts.stream = false;
        let supported = vm_call_llm_full(&opts)
            .await
            .expect("OpenAI-compatible call should succeed");
        assert_eq!(supported.cache_read_tokens, 0);
        assert!(supported.cache_supported);

        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        drop(server);
    });
}

#[test]
fn openai_transport_records_the_served_build_fingerprint() {
    // Two hosts can serve a byte-identical artifact on the same loopback URL,
    // so `serving_base_url` cannot tell a run record which one produced the
    // tokens. The server already publishes its build; this pins that the
    // non-streaming transport carries it all the way onto the result, and
    // that a server which reports none leaves the field absent rather than
    // empty.
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let server = spawn_openai_fingerprint_stub();
        let addr = server.addr();
        install_openai_stub_provider("llamacpp", addr);
        crate::llm_config::set_runtime_provider_endpoint_overrides(
            crate::llm_config::RuntimeProviderEndpointOverrides::single(
                "llamacpp",
                format!("http://{addr}/v1"),
            )
            .expect("valid stub endpoint"),
        );

        let mut opts = base_opts("llamacpp");
        opts.model = "qwen3.6-35b-a3b-ud-q4-k-xl".to_string();
        opts.stream = false;
        opts.cache = false;
        let result = vm_call_llm_full(&opts)
            .await
            .expect("llama.cpp-compatible call should succeed");

        assert_eq!(
            result.telemetry.serving_fingerprint.as_deref(),
            Some("b9994-14d3ba45f")
        );
        // The discriminator has to reach the projected record, not just the
        // in-memory envelope.
        let dict = result
            .telemetry
            .as_vm_dict()
            .expect("telemetry should project");
        let dict = dict.as_dict().expect("dict body");
        assert_eq!(
            dict.get("serving_fingerprint")
                .map(crate::value::VmValue::display)
                .as_deref(),
            Some("b9994-14d3ba45f")
        );

        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        drop(server);

        // Absence control: the same transport against a server that reports
        // no fingerprint must leave the field unset.
        let server = spawn_openai_success_stub();
        let addr = server.addr();
        install_openai_stub_provider("llamacpp", addr);
        crate::llm_config::set_runtime_provider_endpoint_overrides(
            crate::llm_config::RuntimeProviderEndpointOverrides::single(
                "llamacpp",
                format!("http://{addr}/v1"),
            )
            .expect("valid stub endpoint"),
        );
        let mut opts = base_opts("llamacpp");
        opts.model = "qwen3.6-35b-a3b-ud-q4-k-xl".to_string();
        opts.stream = false;
        opts.cache = false;
        let silent = vm_call_llm_full(&opts)
            .await
            .expect("llama.cpp-compatible call should succeed");
        assert_eq!(silent.telemetry.serving_fingerprint, None);

        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        drop(server);
    });
}

#[test]
fn ollama_empty_content_done_frame_retries_once() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_ollama_empty_then_success_stub(request_count.clone());
        let addr = server.addr();
        let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
        }

        let local = tokio::task::LocalSet::new();
        let result = local
            .run_until(async {
                let opts = base_opts("ollama");
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                    .await
                    .expect("retry should recover from empty done frame");

                let mut deltas = Vec::new();
                while let Ok(delta) = rx.try_recv() {
                    deltas.push(delta);
                }
                (result, deltas)
            })
            .await;

        match prev_ollama_host {
            Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
            None => unsafe { std::env::remove_var("OLLAMA_HOST") },
        }

        drop(server);

        let (result, deltas) = result;
        assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(result.text, "retried");
        assert_eq!(deltas.join(""), "retried");
    });
}

#[test]
fn empty_generation_exhausts_primary_then_recovers_on_routed_backup() {
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };

    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_openai_empty_stub(request_count.clone());
        install_openai_stub_provider("empty-primary", server.addr());
        let _fake = install_fake_llm_script(FakeLlmScript::new().push(FakeLlmTurn::stream(vec![
            FakeLlmEvent::Token("recovered on backup".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ])));

        let mut opts = base_opts("empty-primary");
        opts.model = "empty-primary-model".to_string();
        opts.stream = false;
        let policy = crate::llm::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-backup-model".to_string(),
            }],
            &[],
        )
        .expect("credentialed backup creates routing policy");

        let local = tokio::task::LocalSet::new();
        let (result, trace) = local
            .run_until(crate::llm::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect("empty primary must recover transparently on backup");

        assert_eq!(result.provider, "fake");
        assert_eq!(result.text, "recovered on backup");
        assert_eq!(
            request_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "primary receives the initial request plus one bounded same-route retry"
        );
        assert_eq!(trace.attempts.len(), 2);
        let primary_error = trace.attempts[0]
            .error
            .as_ref()
            .expect("primary route failure receipt");
        assert_eq!(primary_error.reason.as_deref(), Some("empty_generation"));
        assert_eq!(primary_error.attempt_count, Some(2));
        assert!(matches!(
            trace.attempts[1].status,
            crate::llm::routing::AttemptStatus::Succeeded
        ));

        crate::llm_config::clear_user_overrides();
        drop(server);
    });
}

#[test]
fn repeated_empty_generations_quarantine_primary_without_a_phantom_request() {
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };

    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_openai_empty_stub_many(
            request_count.clone(),
            2 * crate::llm::rate_limit::UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD as usize,
        );
        install_openai_stub_provider("empty-storm-primary", server.addr());

        let mut opts = base_opts("empty-storm-primary");
        opts.model = "empty-storm-model".to_string();
        opts.stream = false;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                for _ in 0..crate::llm::rate_limit::UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD {
                    crate::llm::agent_observe::observed_llm_call(
                        &opts, None, None, None, false, false, None, None,
                    )
                    .await
                    .expect_err("each terminal empty generation must exhaust its route");
                }
            })
            .await;
        let requests_before_quarantine = request_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            requests_before_quarantine,
            2 * crate::llm::rate_limit::UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD as usize,
            "each admitted route performs one initial request and one bounded retry"
        );

        let _fake = install_fake_llm_script(FakeLlmScript::new().push(FakeLlmTurn::stream(vec![
            FakeLlmEvent::Token("recovered after quarantine".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ])));
        let policy = crate::llm::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-after-quarantine".to_string(),
            }],
            &[],
        )
        .expect("backup creates routing policy");
        let (result, trace) = local
            .run_until(crate::llm::routing::execute_with_routing(
                &policy, opts, None, None,
            ))
            .await
            .expect("routing must advance past the quarantined primary");

        assert_eq!(result.text, "recovered after quarantine");
        assert_eq!(result.provider, "fake");
        assert_eq!(
            request_count.load(std::sync::atomic::Ordering::SeqCst),
            requests_before_quarantine,
            "the quarantined primary must perform zero additional HTTP requests"
        );
        let quarantined = trace.attempts.first().expect("primary attempt receipt");
        let error = quarantined
            .error
            .as_ref()
            .expect("quarantine error receipt");
        assert_eq!(error.code.as_deref(), Some("route_quarantined"));
        assert_eq!(error.attempt_count, Some(0));
        assert!(matches!(
            trace.attempts[1].status,
            crate::llm::routing::AttemptStatus::Succeeded
        ));

        crate::llm_config::clear_user_overrides();
        drop(server);
    });
}

#[test]
fn empty_generation_without_backup_returns_typed_attempted_chain() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = spawn_openai_empty_stub(request_count.clone());
        install_openai_stub_provider("empty-alone", server.addr());
        let mut opts = base_opts("empty-alone");
        opts.model = "empty-alone-model".to_string();
        opts.stream = false;

        let local = tokio::task::LocalSet::new();
        let error = local
            .run_until(crate::llm::agent_observe::observed_llm_call(
                &opts, None, None, None, false, false, None, None,
            ))
            .await
            .expect_err("an empty route with no backup must exhaust");

        assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        let consumer_error =
            crate::llm::call::build_llm_error_dict(&error, &opts.provider, &opts.model);
        let consumer_fields = consumer_error
            .as_dict()
            .expect("llm_call consumer error envelope");
        assert_eq!(
            consumer_fields
                .get("code")
                .map(crate::value::VmValue::display),
            Some("provider_exhausted".to_string()),
            "llm_call must preserve the dispatch-owned typed error"
        );
        assert!(
            matches!(
                consumer_fields.get("attempts"),
                Some(crate::value::VmValue::List(attempts)) if attempts.len() == 1
            ),
            "llm_call must preserve the complete attempted-route receipt"
        );
        let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = error else {
            panic!("expected structured provider exhaustion");
        };
        assert_eq!(
            fields.get("code").map(crate::value::VmValue::display),
            Some("provider_exhausted".to_string())
        );
        assert_eq!(
            fields.get("reason").map(crate::value::VmValue::display),
            Some("empty_generation".to_string())
        );
        assert_eq!(
            fields
                .get("attempt_count")
                .and_then(crate::value::VmValue::as_int),
            Some(2)
        );
        let Some(crate::value::VmValue::List(attempts)) = fields.get("attempts") else {
            panic!("expected attempted route ledger");
        };
        assert_eq!(attempts.len(), 1);
        let attempt = attempts[0].as_dict().expect("attempt receipt");
        assert_eq!(
            attempt.get("provider").map(crate::value::VmValue::display),
            Some("empty-alone".to_string())
        );
        assert_eq!(
            attempt
                .get("attempt_count")
                .and_then(crate::value::VmValue::as_int),
            Some(2)
        );
        assert!(
            attempt
                .get("duration_ms")
                .and_then(crate::value::VmValue::as_int)
                .is_some(),
            "the terminal chain must retain measured route latency"
        );

        crate::llm_config::clear_user_overrides();
        drop(server);
    });
}

#[test]
fn direct_vm_call_entrypoint_honors_routing_policy() {
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        crate::llm::agent_observe::push_llm_transcript_dir(
            transcript_dir.path().to_str().expect("utf8 tempdir"),
        );
        let _fake = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::error(
                    crate::value::ErrorCategory::CircuitOpen,
                    "primary route unavailable",
                ))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("direct entrypoint recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let mut opts = base_opts("fake");
        opts.model = "fake-primary".to_string();
        opts.routing_policy = crate::llm::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-backup".to_string(),
            }],
            &[],
        );

        let result = vm_call_llm_full(&opts)
            .await
            .expect("direct VM caller must use the configured routing chain");
        crate::llm::agent_observe::pop_llm_transcript_dir();
        assert_eq!(result.text, "direct entrypoint recovered");
        assert_eq!(result.model, "fake-backup");

        let transcript =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("routing transcript");
        let requests: Vec<serde_json::Value> = transcript
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
            .filter(|event: &serde_json::Value| event["type"] == "provider_call_request")
            .collect();
        assert_eq!(
            requests.len(),
            2,
            "each physical route must emit exactly one request; no outer logical-call phantom"
        );
        assert_eq!(requests[0]["model"], "fake-primary");
        assert_eq!(requests[1]["model"], "fake-backup");
    });
}

#[test]
fn routing_stream_fails_over_before_output_and_emits_only_backup_text() {
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let _fake = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::error(
                    crate::value::ErrorCategory::CircuitOpen,
                    "primary unavailable before output",
                ))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("backup only".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let mut opts = base_opts("fake");
        opts.model = "fake-primary".to_string();
        opts.routing_policy = crate::llm::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-backup".to_string(),
            }],
            &[],
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = vm_call_llm_full_streaming(&opts, tx)
            .await
            .expect("pre-output failure should recover on backup");
        let mut deltas = Vec::new();
        while let Ok(delta) = rx.try_recv() {
            deltas.push(delta);
        }
        assert_eq!(result.text, "backup only");
        assert_eq!(deltas, vec!["backup only".to_string()]);
        assert_eq!(crate::llm::fake::fake_llm_captured_calls().len(), 2);
    });
}

#[test]
fn routing_stream_never_splices_backup_after_primary_output() {
    use crate::llm::fake::{install_fake_llm_script, FakeLlmError, FakeLlmEvent, FakeLlmScript};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let _fake = install_fake_llm_script(FakeLlmScript::streaming(vec![
            FakeLlmEvent::Token("partial primary".into()),
            FakeLlmEvent::Error(FakeLlmError::new(
                crate::value::ErrorCategory::TransientNetwork,
                "connection reset after response bytes",
            )),
        ]));
        let mut opts = base_opts("fake");
        opts.model = "fake-primary".to_string();
        opts.routing_policy = crate::llm::routing::build_transport_failover_policy(
            &opts.provider,
            &opts.model,
            &[super::LlmRouteFallback {
                provider: "fake".to_string(),
                model: "fake-backup".to_string(),
            }],
            &[],
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        vm_call_llm_full_streaming(&opts, tx)
            .await
            .expect_err("a committed primary stream must surface its own failure");
        let mut deltas = Vec::new();
        while let Ok(delta) = rx.try_recv() {
            deltas.push(delta);
        }
        assert_eq!(deltas, vec!["partial primary".to_string()]);
        assert_eq!(
            crate::llm::fake::fake_llm_captured_calls().len(),
            1,
            "no backup call may run after public output commits the primary"
        );
    });
}

#[test]
fn ollama_chat_applies_env_runtime_overrides() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_ollama_stub_with_body_capture(captured.clone());
        let addr = server.addr();
        let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
        let prev_num_ctx = std::env::var("HARN_OLLAMA_NUM_CTX").ok();
        let prev_keep_alive = std::env::var("HARN_OLLAMA_KEEP_ALIVE").ok();
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            std::env::set_var("HARN_OLLAMA_NUM_CTX", "131072");
            std::env::set_var("HARN_OLLAMA_KEEP_ALIVE", "forever");
        }

        let local = tokio::task::LocalSet::new();
        let result = local
            .run_until(async {
                let opts = base_opts("ollama");
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                vm_call_llm_full_streaming_offthread(&opts, tx)
                    .await
                    .expect("llm call should succeed")
            })
            .await;

        match prev_ollama_host {
            Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
            None => unsafe { std::env::remove_var("OLLAMA_HOST") },
        }
        match prev_num_ctx {
            Some(value) => unsafe { std::env::set_var("HARN_OLLAMA_NUM_CTX", value) },
            None => unsafe { std::env::remove_var("HARN_OLLAMA_NUM_CTX") },
        }
        match prev_keep_alive {
            Some(value) => unsafe { std::env::set_var("HARN_OLLAMA_KEEP_ALIVE", value) },
            None => unsafe { std::env::remove_var("HARN_OLLAMA_KEEP_ALIVE") },
        }

        drop(server);
        assert_eq!(result.text, "ok");
        let body = captured
            .lock()
            .expect("captured body")
            .clone()
            .expect("request body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
        assert_eq!(json["keep_alive"].as_i64(), Some(-1));
        assert_eq!(json["options"]["num_ctx"].as_u64(), Some(131072));
    });
}

#[test]
fn observed_qwen_dispatch_receipt_matches_captured_egress_system() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_ollama_stub_with_body_capture(captured.clone());
        let _ollama_host = ScopedEnvVar::set("OLLAMA_HOST", &format!("http://{}", server.addr()));
        let _verbose = ScopedEnvVar::remove("HARN_LLM_TRANSCRIPT_VERBOSE");
        let transcript_dir = std::env::temp_dir().join(format!(
            "harn-observed-qwen-egress-{}",
            uuid::Uuid::now_v7()
        ));
        let transcript_dir_string = transcript_dir.to_string_lossy().to_string();
        crate::llm::agent_observe::push_llm_transcript_dir(&transcript_dir_string);

        let mut opts = base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = Some("you are an agent.".to_string());
        opts.thinking = ThinkingConfig::Disabled;
        opts.output_format = super::OutputFormat::Text;
        opts.output_schema = None;
        opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
            "test:system",
            "test",
            "llm_call",
            opts.system.as_deref(),
        );

        let result = crate::llm::agent_observe::observed_llm_call(
            &opts,
            Some("native"),
            None,
            Some(0),
            false,
            false,
            None,
            None,
        )
        .await
        .expect("stubbed qwen call");
        crate::llm::agent_observe::pop_llm_transcript_dir();
        drop(server);
        assert_eq!(result.text, "ok");

        let body = captured
            .lock()
            .expect("captured body")
            .clone()
            .expect("request body");
        let wire: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
        let wire_system = wire["messages"][0]["content"]
            .as_str()
            .expect("wire system message");
        assert_eq!(wire_system, "/no_think\nyou are an agent.");

        let transcript = std::fs::read_to_string(transcript_dir.join("llm_transcript.jsonl"))
            .expect("transcript");
        let events = transcript
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("event json"))
            .collect::<Vec<_>>();
        let manifest = &events
            .iter()
            .find(|event| event["type"] == "context_manifest")
            .expect("context manifest")["manifest"];
        let receipt = &events
            .iter()
            .find(|event| event["type"] == "provider_call_request")
            .expect("provider request")["served_context"];
        let system_event = events
            .iter()
            .find(|event| event["type"] == "system_prompt")
            .expect("system prompt");

        assert_eq!(system_event["content"], serde_json::json!(wire_system));
        assert_eq!(
            receipt["system_prompt_bytes"],
            serde_json::json!(wire_system.len())
        );
        assert_eq!(
            receipt["system_prompt_content_hash"],
            system_event["content_hash"]
        );
        assert_eq!(
            manifest["whole_prompt_digest"],
            receipt["system_prompt_content_hash"]
        );
        assert_eq!(
            manifest["system_prompt_bytes"],
            receipt["system_prompt_bytes"]
        );
        assert_eq!(
            manifest["egress_delta"]["bytes_added"],
            serde_json::json!(10)
        );

        let _ = std::fs::remove_dir_all(transcript_dir);
    });
}

#[test]
fn ollama_qwen_text_tool_route_bypasses_chat_parser_with_raw_generate() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_ollama_raw_generate_stub(captured.clone());
        let addr = server.addr();
        let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
        }

        let local = tokio::task::LocalSet::new();
        let result = local
            .run_until(async {
                let mut opts = base_opts("ollama");
                opts.model = "qwen3.5:35b-a3b-coding-nvfp4".to_string();
                opts.native_tools = None;
                opts.output_format = crate::llm::api::OutputFormat::Text;
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let result = vm_call_llm_full_streaming_offthread(&opts, tx)
                    .await
                    .expect("raw-generate route should succeed");
                let mut deltas = Vec::new();
                while let Ok(delta) = rx.try_recv() {
                    deltas.push(delta);
                }
                (result, deltas)
            })
            .await;

        match prev_ollama_host {
            Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
            None => unsafe { std::env::remove_var("OLLAMA_HOST") },
        }

        drop(server);
        let (result, deltas) = result;
        assert_eq!(
            result.text,
            "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>"
        );
        assert_eq!(deltas.join(""), result.text);
        assert_eq!(result.model, "qwen3.5:stub");
        assert_eq!(result.input_tokens, 7);
        assert_eq!(result.output_tokens, 11);
        assert_eq!(result.stop_reason.as_deref(), Some("stop"));

        let body = captured
            .lock()
            .expect("captured body")
            .clone()
            .expect("request body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
        assert_eq!(json["raw"].as_bool(), Some(true));
        assert!(json["prompt"]
            .as_str()
            .unwrap_or_default()
            .contains("<|im_start|>assistant\n"));
        assert!(json.get("chat_template_kwargs").is_none());
    });
}

#[test]
fn ollama_warmup_applies_shared_runtime_settings() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_ollama_stub_with_body_capture(captured.clone());
        let addr = server.addr();
        let _num_ctx = ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "65536");
        let _keep_alive = ScopedEnvVar::set("HARN_OLLAMA_KEEP_ALIVE", "forever");

        super::ollama::warm_ollama_model("qwen3.5:35b", Some(&format!("http://{addr}")))
            .await
            .expect("warmup should succeed");

        drop(server);
        let body = captured
            .lock()
            .expect("captured body")
            .clone()
            .expect("request body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
        assert_eq!(json["model"].as_str(), Some("qwen3.5:35b"));
        assert_eq!(json["keep_alive"].as_i64(), Some(-1));
        assert_eq!(json["options"]["num_ctx"].as_u64(), Some(65536));
    });
}

/// Bind a stub listener that serves one canned HTTP error response.
/// The returned [`LlmStub`] guard owns the listener and the worker
/// thread, so dropping it (test exit, panic) signals shutdown and
/// joins — a stuck or misrouted client can never wedge the suite.
fn spawn_openai_error_stub(
    status_line: &'static str,
    extra_headers: &'static str,
    body: &'static str,
) -> LlmStub {
    spawn_llm_stub("openai error stub", move |stream| {
        use std::io::{Read, Write};
        let mut buf = vec![0u8; 16384];
        let _ = stream.read(&mut buf);
        let response = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra_headers}connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    })
}

/// Single-entrypoint helper that serializes env-var mutation and the
/// LLM call behind `env_lock`, so parallel streaming error tests can't
/// clobber each other's `LOCAL_LLM_BASE_URL` and leak an unconnected
/// stub whose `join()` would hang the test binary.
fn run_streaming_error_case(
    status_line: &'static str,
    extra_headers: &'static str,
    body: &'static str,
) -> String {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let server = spawn_openai_error_stub(status_line, extra_headers, body);
    let addr = server.addr();
    let prev = std::env::var("LOCAL_LLM_BASE_URL").ok();
    unsafe {
        std::env::set_var("LOCAL_LLM_BASE_URL", format!("http://{addr}"));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");
    let err = runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut opts = base_opts("local");
                opts.tools = None;
                opts.native_tools = None;
                opts.tool_choice = None;
                opts.output_format = crate::llm::api::OutputFormat::Text;
                opts.output_schema = None;
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let call = tokio::time::timeout(
                    // Must stay inside the stub's accept window.
                    std::time::Duration::from_secs(30),
                    vm_call_llm_full_streaming_offthread(&opts, tx),
                )
                .await;
                match call {
                    Ok(Ok(_)) => panic!("expected streaming call to fail"),
                    Ok(Err(err)) => err.to_string(),
                    Err(elapsed) => panic!("streaming call timed out ({elapsed})"),
                }
            })
            .await
    });
    match prev {
        Some(v) => unsafe { std::env::set_var("LOCAL_LLM_BASE_URL", v) },
        None => unsafe { std::env::remove_var("LOCAL_LLM_BASE_URL") },
    }
    drop(server);
    err
}

#[test]
fn streaming_path_classifies_context_overflow() {
    let err = run_streaming_error_case(
        "HTTP/1.1 400 Bad Request",
        "",
        r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your prompt is too long."}}"#,
    );
    assert!(err.contains("[context_overflow]"), "err was: {err}");
    assert!(err.contains("local HTTP 400"), "err was: {err}");
}

#[test]
fn streaming_path_classifies_rate_limit_with_retry_after() {
    let err = run_streaming_error_case(
        "HTTP/1.1 429 Too Many Requests",
        "retry-after: 7\r\n",
        r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
    );
    assert!(err.contains("[rate_limited]"), "err was: {err}");
    assert!(err.contains("(retry-after: 7)"), "err was: {err}");
}

#[test]
fn streaming_path_classifies_opaque_500_as_http_error() {
    let err = run_streaming_error_case(
        "HTTP/1.1 500 Internal Server Error",
        "",
        r#"{"error":"upstream exploded"}"#,
    );
    assert!(err.contains("[http_error]"), "err was: {err}");
    assert!(err.contains("upstream exploded"), "err was: {err}");
}
