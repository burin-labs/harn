use super::{base_opts, spawn_llm_stub, vm_call_llm_full_streaming_offthread};
use crate::llm::api::test_support::allow_stubbed_llm_transport;
use crate::llm::env_guard;

#[test]
fn gemini_generate_content_stream_uses_sse_and_preserves_terminal_usage() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let server = spawn_llm_stub("Gemini GenerateContent stream", |stream| {
            use std::io::{Read, Write};

            let mut buffer = vec![0u8; 16_384];
            let count = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..count]);
            assert!(request.starts_with(
                "POST /v1beta/models/gemini-2.5-flash-lite:streamGenerateContent?alt=sse HTTP/1.1\r\n"
            ));

            let body = concat!(
                r#"data: {"candidates":[{"content":{"parts":[{"text":"STREAM_"}]}}]}"#,
                "\n\n",
                r#"data: {"candidates":[{"content":{"parts":[{"text":"OK"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}"#,
                "\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write response");
        });
        let mut providers = crate::llm_config::ProvidersConfig::default();
        providers.providers.insert(
            "gemini".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{}", server.addr()),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(providers));

        let local = tokio::task::LocalSet::new();
        let (result, deltas) = local
            .run_until(async {
                let mut options = base_opts("gemini");
                options.model = "gemini-2.5-flash-lite".to_string();
                options.stream = true;
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let result = vm_call_llm_full_streaming_offthread(&options, sender)
                    .await
                    .expect("Gemini stream should succeed");
                let mut deltas = Vec::new();
                while let Ok(delta) = receiver.try_recv() {
                    deltas.push(delta);
                }
                (result, deltas)
            })
            .await;

        crate::llm_config::clear_user_overrides();
        drop(server);

        assert_eq!(result.text, "STREAM_OK");
        assert_eq!(deltas, ["STREAM_", "OK"]);
        assert_eq!(result.input_tokens, 3);
        assert_eq!(result.output_tokens, 2);
        assert_eq!(result.stop_reason.as_deref(), Some("STOP"));
    });
}
