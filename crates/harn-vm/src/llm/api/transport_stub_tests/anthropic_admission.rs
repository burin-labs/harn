//! Native Anthropic cache accounting is preserved while admission retains a
//! conservative input reservation whose cache-counter presence is unknown.
use super::*;
use crate::llm::admission::AdmissionMode;
use crate::llm::api::{LlmCallOptions, PromptCacheTtl};
use crate::llm::cost::LlmBudgetEnvelope;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn options(stream: bool) -> LlmCallOptions {
    LlmCallOptions {
        provider: "anthropic".into(),
        model: "claude-haiku-4-5-20251001".into(),
        messages: vec![serde_json::json!({"role":"user", "content":"hello"})],
        native_tools: Some(vec![serde_json::json!({
            "name": "read_note", "description": "Read a note",
            "input_schema": {"type":"object", "properties": {
                "cache_control": {"type":"string"}
            }}
        })]),
        max_tokens: 64,
        stream,
        cache: true,
        budget: Some(LlmBudgetEnvelope {
            admission: Some(AdmissionMode::Conservative),
            total_budget_usd: Some(0.6),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn install(addr: std::net::SocketAddr) {
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        "anthropic".into(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}"),
            auth_style: "none".into(),
            auth_env: crate::llm_config::AuthEnv::None,
            chat_endpoint: "/messages".into(),
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm::cost::reset_cost_state();
    }
}

#[test]
fn conservative_admission_anthropic_preserves_cache_usage_and_retains_full_input() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for streaming in [false, true] {
            crate::llm::cost::reset_cost_state();
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let server = spawn_llm_stub_many("Anthropic admission", 3, move |attempt, stream| {
                use std::io::{Read, Write};
                calls.fetch_add(1, Ordering::SeqCst);
                let mut bytes = [0u8; 16_384];
                let n = stream.read(&mut bytes).unwrap();
                assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /messages "));
                let (read, write) = if attempt == 0 { (20, 40) } else { (60, 0) };
                let usage = serde_json::json!({"input_tokens":3, "output_tokens":2,
                    "cache_read_input_tokens":read,"cache_creation_input_tokens":write});
                let message = serde_json::json!({"id":"local", "type":"message", "role":"assistant",
                    "model":"claude-haiku-4-5-20251001", "content":[{"type":"text","text":"ok"}],
                    "stop_reason":"end_turn","usage":usage});
                let (content_type, body) = if streaming {
                    ("text/event-stream", format!("event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
                        serde_json::json!({"type":"message_start","message":message}),
                        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}})))
                } else { ("application/json", message.to_string()) };
                write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            install(server.addr());
            let opts = options(streaming);
            let first = vm_call_llm_full(&opts).await.unwrap();
            assert_eq!(first.input_tokens, 63);
            assert_eq!(first.cache_write_tokens, 40);
            let second = vm_call_llm_full(&opts).await.unwrap();
            assert_eq!(second.input_tokens, 63);
            assert_eq!(second.cache_read_tokens, 60);
            assert_eq!(second.cache_write_tokens, 0);
            assert!(second.usage().cost_usd.unwrap() < first.usage().cost_usd.unwrap());
            let error = vm_call_llm_full(&opts).await.unwrap_err();
            assert!(error.to_string().contains("insufficient_allowance"), "{error}");
            assert_eq!(count.load(Ordering::SeqCst), 2);
            crate::llm_config::clear_user_overrides();
        }
    });
}

#[test]
fn conservative_admission_anthropic_refuses_one_hour_cache_before_http() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let server = spawn_llm_stub("unsupported Anthropic cache TTL", move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported TTL reached transport");
        });
        install(server.addr());
        for location in ["global", "content", "message", "tool", "server_tool"] {
            crate::llm::cost::reset_cost_state();
            let mut opts = options(false);
            let control = serde_json::json!({"type":"ephemeral","ttl":"1h"});
            match location {
                "content" => {
                    opts.messages = vec![serde_json::json!({"role":"user", "content":[{
                        "type":"text", "text":"hello", "cache_control":control
                    }]})]
                }
                "message" => opts.messages[0]["cache_control"] = control,
                "tool" => opts.native_tools.as_mut().unwrap()[0]["cache_control"] = control,
                "server_tool" => {
                    opts.native_tools.as_mut().unwrap()[0]["type"] =
                        serde_json::json!("web_search_20250305")
                }
                _ => opts.prompt_cache_ttl = Some(PromptCacheTtl::OneHour),
            }
            let error = vm_call_llm_full(&opts).await.unwrap_err();
            assert!(
                error.to_string().contains("unsupported_billing_shape"),
                "{error}"
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
    });
}
