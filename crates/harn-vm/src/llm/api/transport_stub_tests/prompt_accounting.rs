//! Compare the same measured prompt through real JSON and SSE transports.

use super::{allow_stubbed_llm_transport, base_opts, env_guard, spawn_llm_stub, vm_call_llm_full};
use crate::llm::usage::LlmUsage;
use serde_json::json;

#[test]
fn anthropic_http_json_and_sse_normalize_prompt_totals_and_price() {
    let _guard = env_guard();
    let _allow_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for fresh in [40, 6000] {
            for streaming in [false, true] {
                let server = spawn_llm_stub("Anthropic prompt accounting", move |stream| {
                    use std::io::{Read, Write};
                    let mut request = vec![0; 16_384];
                    let n = stream.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..n]);
                    assert!(request.starts_with("POST /messages HTTP/1.1\r\n"));
                    let usage = json!({"input_tokens": fresh, "output_tokens": 8,
                        "cache_read_input_tokens": 5000, "cache_creation_input_tokens": 100});
                    let message = json!({"id": "msg_accounting", "type": "message", "role": "assistant",
                        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn", "usage": usage});
                    let (content_type, body) = if streaming {
                        let frames = [
                            json!({"type": "message_start", "message": {"id": "msg_accounting", "usage": usage}}),
                            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
                            json!({"type": "content_block_stop", "index": 0}),
                            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 8}}),
                            json!({"type": "message_stop"}),
                        ];
                        ("text/event-stream", frames.iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>())
                    } else {
                        ("application/json", message.to_string())
                    };
                    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                });
                let mut overlay = crate::llm_config::ProvidersConfig::default();
                overlay.providers.insert("anthropic".to_string(), crate::llm_config::ProviderDef {
                    base_url: format!("http://{}", server.addr()),
                    auth_style: "none".to_string(), auth_env: crate::llm_config::AuthEnv::None,
                    chat_endpoint: "/messages".to_string(), ..Default::default()
                });
                crate::llm_config::set_user_overrides(Some(overlay));
                let mut opts = base_opts("anthropic");
                opts.model = "claude-haiku-4-5-20251001".to_string();
                opts.stream = streaming;
                opts.cache = false;
                opts.tools = None;
                opts.native_tools = None;
                opts.tool_choice = None;
                let result = vm_call_llm_full(&opts).await.expect("stubbed provider result");
                crate::llm_config::clear_user_overrides();
                assert_eq!(result.text, "ok");
                assert_eq!(result.input_tokens, fresh + 5100, "stream={streaming}");
                assert_eq!(result.telemetry.server_prompt_tokens, Some(fresh));
                assert_eq!(result.cache_read_tokens, 5000);
                assert_eq!(result.cache_write_tokens, 100);
                let usage = LlmUsage::from_result(&result);
                assert_eq!(usage.input_tokens, fresh + 5100);
                let detail = crate::llm::cost::pricing_detail_for(&result.provider, &result.model, crate::llm::cost::settlement_now()).unwrap();
                let expected_cost = (fresh as f64 * detail.input_per_1k
                    + 5000.0 * detail.cache_read_per_1k.unwrap()
                    + 100.0 * detail.cache_write_per_1k.unwrap()
                    + 8.0 * detail.output_per_1k) / 1000.0;
                assert!((usage.cost_usd.unwrap() - expected_cost).abs() < 1e-10);
                let projected = usage.to_vm_dict(&result.attempts);
                assert_eq!(projected.get("input_tokens").and_then(|value| value.as_int()), Some(fresh + 5100));
                drop(server);
            }
        }
    });
}
