use super::*;
use crate::llm::api::{probe_llm_request, LlmRequestPayload, OutputFormat};

#[test]
fn probe_stream_request_uses_stream_transport_without_a_delta_receiver() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let expected_capture = captured.clone();
    let server = spawn_llm_stub("streaming probe stub", move |stream| {
        use std::io::{Read, Write};

        let mut request = vec![0u8; 16_384];
        let read = stream.read(&mut request).expect("read request");
        *expected_capture.lock().expect("capture request") =
            Some(String::from_utf8_lossy(&request[..read]).to_string());

        let body = concat!(
            "data: {\"id\":\"probe-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {\"id\":\"probe-stream\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write streaming response");
    });
    install_openai_stub_provider("probe-stream", server.addr());

    let mut opts = base_opts("probe-stream");
    opts.model = "probe-stream-model".to_string();
    opts.stream = true;
    opts.output_format = OutputFormat::Text;
    opts.output_schema = None;
    opts.native_tools = None;
    opts.tool_choice = None;
    opts.provider_overrides = None;
    let request = LlmRequestPayload::from(&opts);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime
        .block_on(probe_llm_request(&request))
        .expect("streaming probe result");

    crate::llm_config::clear_user_overrides();
    drop(server);

    assert_eq!(result.text, "ok");
    let request = captured
        .lock()
        .expect("captured request")
        .clone()
        .expect("request captured");
    let body = request.split("\r\n\r\n").nth(1).expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
    assert_eq!(body["stream"], true);
}
