use crate::llm::api::transport_stub_tests::{
    allow_stubbed_llm_transport, base_opts, env_guard, install_openai_stub_provider, spawn_llm_stub,
};
use crate::llm::api::vm_call_llm_full;
use crate::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage};

const INLINE_IMAGE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";

fn assert_session_input_eviction_revokes(evict: fn()) {
    crate::runtime_stack::spawn(move || {
        crate::agent_sessions::set_session_cap(2);
        let victim = crate::agent_sessions::open_or_create(Some("private-victim".into()))
            .expect("open the original session");
        crate::session_inputs::put(
            &victim,
            InputSpec {
                name: "SERVICE_KEY".into(),
                kind: InputKind::Secret,
                lifetime: InputLifetime::Session,
                usage: InputUsage::SessionTools,
            },
            "eviction-private-canary".into(),
            false,
            0,
        )
        .unwrap();
        let bindings = std::collections::BTreeMap::from([("BOUND".into(), "SERVICE_KEY".into())]);
        {
            let _current = crate::agent_sessions::enter_current_session(victim.clone());
            assert_eq!(
                crate::session_inputs::command_environment(bindings.clone()).unwrap()["BOUND"],
                "eviction-private-canary"
            );
        }
        evict();
        assert!(
            !crate::agent_sessions::exists(&victim),
            "the eviction must fire"
        );
        crate::agent_sessions::open_or_create(Some(victim.clone())).unwrap();
        let _current = crate::agent_sessions::enter_current_session(victim.clone());
        assert!(
            crate::session_inputs::command_environment(bindings).is_err(),
            "reopening an evicted ID must not restore its private grant"
        );
        assert!(crate::session_inputs::list(&victim, 0).unwrap().is_empty());
    })
    .join()
    .unwrap();
}

#[test]
fn session_input_eviction_on_open_revokes_before_reopening() {
    assert_session_input_eviction_revokes(|| {
        crate::agent_sessions::open_or_create(None).unwrap();
        crate::agent_sessions::open_or_create(None).unwrap();
    });
}

#[test]
fn session_input_eviction_on_link_revokes_before_reopening() {
    assert_session_input_eviction_revokes(|| {
        crate::agent_sessions::link_child_session("new-parent", "new-child").unwrap();
    });
}

#[test]
fn session_input_eviction_on_fork_revokes_before_reopening() {
    assert_session_input_eviction_revokes(|| {
        let parent = crate::agent_sessions::open_or_create(None).unwrap();
        crate::agent_sessions::fork(&parent, None).unwrap().unwrap();
    });
}

#[test]
fn session_input_filter_reaches_the_actual_provider_body_only_in_its_session() {
    let _guard = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let runtime = crate::test_support::current_thread_runtime();
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
