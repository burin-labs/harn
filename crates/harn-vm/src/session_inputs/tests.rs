use crate::llm::api::transport_stub_tests::{
    allow_stubbed_llm_transport, base_opts, env_guard, install_openai_stub_provider, spawn_llm_stub,
};
use crate::llm::api::vm_call_llm_full;
use crate::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage};

const INLINE_IMAGE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";

#[test]
fn session_input_filter_reaches_the_actual_provider_body_only_in_its_session() {
    let _guard = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for has_grant in [true, false] {
        let session = crate::agent_sessions::open_or_create(None).unwrap();
        let _session = crate::agent_sessions::enter_current_session(session.clone());
        if has_grant {
            for (name, value) in [
                ("SERVICE_KEY", "opaque-provider-canary"),
                ("ROLE", "user"),
                ("MEDIA", "image"),
                ("FORMAT", "function"),
                ("BYTES", "iVBORw"),
            ] {
                crate::session_inputs::put(
                    &session,
                    InputSpec {
                        name: name.into(),
                        kind: InputKind::Secret,
                        lifetime: InputLifetime::Session,
                        usage: InputUsage::SessionTools,
                    },
                    value.into(),
                    false,
                    0,
                )
                .unwrap();
            }
        }
        let server = spawn_llm_stub("private input provider boundary", move |stream| {
            use std::io::{BufRead, Read, Write};
            let mut reader = std::io::BufReader::new(&mut *stream);
            let mut length = None;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut bytes = vec![0; length.expect("provider request body length")];
            reader.read_exact(&mut bytes).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let expected = if has_grant {
                "[redacted]"
            } else {
                "opaque-provider-canary"
            };
            assert!(request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["content"] == expected));
            if has_grant {
                assert!(!request.to_string().contains("opaque-provider-canary"));
            }
            let messages = request["messages"].as_array().unwrap();
            assert!(messages.iter().any(|message| message["role"] == "user"));
            assert!(messages
                .iter()
                .any(
                    |message| message["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["type"] == "image_url"
                                && part["image_url"]["url"]
                                    == format!("data:image/png;base64,{INLINE_IMAGE}")
                        })
                    })
                ));
            assert_eq!(request["tools"][0]["type"], "function");
            assert_eq!(request["tools"][0]["function"]["name"], "user");
            assert_eq!(request["tools"][0]["function"]["description"], expected);
            assert_eq!(
                request["tools"][0]["function"]["parameters"]["properties"]["value"]["enum"][0],
                expected
            );
            assert_eq!(request["stop"][0], expected);
            let body = r#"{"id":"input-test","choices":[{"message":{"role":"assistant","content":"received"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        install_openai_stub_provider("private-input-test", server.addr());
        let mut options = base_opts("private-input-test");
        options.stream = false;
        options.stop = Some(vec!["opaque-provider-canary".into()]);
        options.messages = vec![
            serde_json::json!({"role":"user", "content":"opaque-provider-canary"}),
            serde_json::json!({"role":"user", "content":[{"type":"image", "base64":INLINE_IMAGE, "media_type":"image/png"}]}),
        ];
        options.native_tools = Some(vec![serde_json::json!({
            "type":"function", "function": {
                "name":"user", "description":"opaque-provider-canary",
                "parameters":{"type":"object", "properties":{
                    "value":{"type":"string", "enum":["opaque-provider-canary"]}
                }}
            }
        })]);
        let result = runtime.block_on(vm_call_llm_full(&options)).unwrap();
        assert_eq!(result.text, "received");
        crate::llm_config::clear_user_overrides();
        assert!(crate::agent_sessions::close(&session));
    }
}
