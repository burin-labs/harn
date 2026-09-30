//! Ollama's `/v1` endpoint has OpenAI SSE envelopes but a closed subset of
//! OpenAI request fields. Keep this adapter exercised over real HTTP.

use super::*;
use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::env_guard;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::llm::capabilities::clear_user_overrides();
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> (String, serde_json::Value) {
    use std::io::Read;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut body_start = None;
    let mut content_length = 0;
    loop {
        let read = stream.read(&mut chunk).expect("read HTTP request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if body_start.is_none() {
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                body_start = Some(header_end + 4);
            }
        }
        if body_start.is_some_and(|start| bytes.len() >= start + content_length) {
            break;
        }
    }
    let start = body_start.expect("request headers");
    let headers = String::from_utf8_lossy(&bytes[..start - 4]).into_owned();
    let body = serde_json::from_slice(&bytes[start..start + content_length]).expect("request JSON");
    (headers, body)
}

fn install_ollama_route(addr: std::net::SocketAddr, adapter: crate::llm_config::ChatApiAdapter) {
    let mut providers = crate::llm_config::ProvidersConfig::default();
    providers.providers.insert(
        "ollama".to_string(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}"),
            chat_endpoint: "/v1/chat/completions".to_string(),
            auth_style: "none".to_string(),
            auth_env: crate::llm_config::AuthEnv::None,
            chat_api_adapter: Some(crate::llm_config::ChatApiAdapter::OllamaOpenAiCompat),
            ..Default::default()
        },
    );
    if adapter == crate::llm_config::ChatApiAdapter::ModelDefault {
        providers.providers.get_mut("ollama").unwrap().merge_from(
            &crate::llm_config::ProviderDef {
                chat_endpoint: "/api/chat".to_string(),
                chat_api_adapter: Some(adapter),
                ..Default::default()
            },
        );
    }
    // Catalog export/import must retain the adapter, or the same route falls
    // back to native NDJSON when a downstream host consumes its projection.
    let catalog = crate::provider_catalog::artifact_embedded(Some(&providers), None);
    let catalog = serde_json::from_value(serde_json::to_value(catalog).unwrap()).unwrap();
    crate::llm_config::set_user_overrides(Some(crate::provider_catalog::config_from_artifact(
        &catalog,
    )));
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.ollama]]
model_match = "devstral-small-2:24b"
message_wire_format = "ollama"
"#,
    )
    .expect("Ollama capability fixture");
}

#[test]
fn ollama_openai_compat_maps_supported_fields_and_reads_terminal_usage() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let request_count = Arc::new(AtomicUsize::new(0));
    let observed_request = Arc::new(Mutex::new(None));
    let request_count_server = request_count.clone();
    let observed_request_server = observed_request.clone();
    let server = spawn_llm_stub_many(
        "Ollama OpenAI-compatible SSE",
        2,
        move |_attempt, stream| {
            use std::io::Write;
            request_count_server.fetch_add(1, Ordering::SeqCst);
            let (headers, request) = read_http_request(stream);
            *observed_request_server.lock().expect("request lock") =
                Some(serde_json::json!({"headers": headers, "body": request}));
            // Preserve the captured 24-frame grammar without publishing project
            // content: 22 deltas, a content-terminal frame, then separate usage.
            let mut body = String::new();
            for fragment in [
                "Local",
                " ",
                "response",
                " ",
                "through",
                " ",
                "the",
                " ",
                "shared",
                " ",
                "Harn",
                " ",
                "route",
                " ",
                "preserves",
                " ",
                "text",
                " ",
                "and",
                " ",
                "usage",
                ".",
            ] {
                let frame = serde_json::json!({
                    "id": "chatcmpl-captured", "object": "chat.completion.chunk",
                    "model": "devstral-small-2:24b",
                    "choices": [{"index": 0, "delta": {"content": fragment}, "finish_reason": null}]
                });
                body.push_str(&format!("data: {frame}\n\n"));
            }
            body.push_str(concat!(
            "data: {\"id\":\"chatcmpl-captured\",\"object\":\"chat.completion.chunk\",\"model\":\"devstral-small-2:24b\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-captured\",\"object\":\"chat.completion.chunk\",\"model\":\"devstral-small-2:24b\",\"choices\":[],\"usage\":{\"prompt_tokens\":1240,\"completion_tokens\":23,\"total_tokens\":1263}}\n\n",
            "data: [DONE]\n\n"
        ));
            write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write captured Ollama SSE response");
        },
    );
    install_ollama_route(
        server.addr(),
        crate::llm_config::ChatApiAdapter::OllamaOpenAiCompat,
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut options = base_opts("ollama");
        options.model = "devstral-small-2:24b".to_string();
        options.messages = vec![serde_json::json!({"role": "user", "content": "say hello"})];
        options.max_tokens = 512;
        options.temperature = Some(0.2);
        options.top_p = Some(0.95);
        // A catalog/default value has no caller-intent entry and can be
        // projected away because `/v1` cannot carry top_k.
        options.top_k = Some(40);
        options.stop = None;
        options.seed = None;
        options.frequency_penalty = None;
        options.presence_penalty = None;
        options.tool_choice = None;
        options.tools = None;
        options.native_tools = None;
        options.provider_overrides = None;
        options.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"blurb": {"type": "string"}},
                "required": ["blurb"]
            }),
            strict: false,
        };
        options.output_schema = Some(serde_json::json!({
            "type": "object",
            "properties": {"blurb": {"type": "string"}},
            "required": ["blurb"]
        }));
        let result = vm_call_llm_full(&options).await.expect("SSE call");

        assert_eq!(
            result.text,
            "Local response through the shared Harn route preserves text and usage."
        );
        assert_eq!(result.telemetry.server_prompt_tokens, Some(1240));
        assert_eq!(result.telemetry.server_output_tokens, Some(23));
        assert_eq!(request_count.load(Ordering::SeqCst), 1, "no empty retry");

        let captured = observed_request
            .lock()
            .expect("request lock")
            .clone()
            .expect("captured HTTP request");
        assert!(captured["headers"]
            .as_str()
            .is_some_and(|headers| headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n")));
        let request = &captured["body"];
        assert_eq!(request["stream"], true);
        assert_eq!(request["stream_options"]["include_usage"], true);
        assert_eq!(request["max_tokens"], 512);
        assert_eq!(request["temperature"], 0.2);
        assert_eq!(request["top_p"], 0.95);
        assert_eq!(request["reasoning_effort"], "none");
        assert_eq!(request["response_format"]["type"], "json_schema");
        assert_eq!(
            request["response_format"]["json_schema"]["schema"]["properties"]["blurb"]["type"],
            "string"
        );
        assert!(
            request.get("options").is_none(),
            "native Ollama options ignored by /v1"
        );
        assert!(request.get("keep_alive").is_none());
        assert!(request.get("think").is_none());
        assert!(request.get("top_k").is_none());
    });
}

#[test]
fn ollama_openai_compat_refuses_native_only_generation_options_before_http() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let request_count = Arc::new(AtomicUsize::new(0));
    let count_server = request_count.clone();
    let server = spawn_llm_stub(
        "Ollama unsupported option must not egress",
        move |_stream| {
            count_server.fetch_add(1, Ordering::SeqCst);
        },
    );
    install_ollama_route(
        server.addr(),
        crate::llm_config::ChatApiAdapter::OllamaOpenAiCompat,
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut options = base_opts("ollama");
        options.model = "devstral-small-2:24b".to_string();
        options.top_k = Some(40);
        options
            .portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::TopK);
        options.tool_choice = None;
        options.tools = None;
        options.native_tools = None;
        options.provider_overrides = None;
        let error = vm_call_llm_full(&options)
            .await
            .expect_err("top_k is absent from Ollama's OpenAI request type");
        assert!(error
            .to_string()
            .contains("does not support the `top_k` request option"));
        let projected = crate::llm::call::build_llm_error_dict(&error, "ollama", &options.model);
        assert_eq!(
            projected
                .as_dict()
                .unwrap()
                .get("origin")
                .map(crate::value::VmValue::display)
                .as_deref(),
            Some("local")
        );
        assert_eq!(
            request_count.load(Ordering::SeqCst),
            0,
            "refused before HTTP"
        );
        options.portable_option_intent.clear();
        for field in ["num_ctx", "keep_alive", "options", "think"] {
            options.provider_overrides = Some(serde_json::json!({field: 1}));
            let error = vm_call_llm_full(&options)
                .await
                .expect_err("unsupported explicit override must not be discarded");
            assert!(error
                .to_string()
                .contains(&format!("does not support the `{field}` provider override")));
            assert_eq!(
                request_count.load(Ordering::SeqCst),
                0,
                "refused before HTTP"
            );
        }
    });
}

#[test]
fn native_ollama_override_preserves_ndjson_text_and_usage() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let server = spawn_ollama_stub();
    install_ollama_route(
        server.addr(),
        crate::llm_config::ChatApiAdapter::ModelDefault,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut options = base_opts("ollama");
        options.model = "devstral-small-2:24b".to_string();
        let result = vm_call_llm_full(&options)
            .await
            .expect("native NDJSON call");
        assert_eq!(result.text, "hello world");
        assert_eq!(result.input_tokens, 3);
        assert_eq!(result.output_tokens, 2);
    });
}
