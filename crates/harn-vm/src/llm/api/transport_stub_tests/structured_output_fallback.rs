use super::*;
use crate::value::VmValue;
use std::io::Write;
use std::sync::{Arc, Mutex};

struct ClearProvider;

impl Drop for ClearProvider {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
    }
}

fn structured_request() -> Vec<VmValue> {
    vec![
        VmValue::string("Return an object with answer ready."),
        crate::schema::json_to_vm_value(&serde_json::json!({
            "type": "object", "additionalProperties": false,
            "properties": {"answer": {"type": "string"}}, "required": ["answer"]
        })),
        crate::schema::json_to_vm_value(&serde_json::json!({
            "provider": "openai", "model": "gpt-4.1-mini",
            "max_tokens": 64, "stream": false, "schema_retries": 0,
            "repair": {"enabled": false}
        })),
    ]
}

#[test]
fn structured_output_fallback_does_not_retry_provider_refusals() {
    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let _cleanup = ClearProvider;
    let _boundary = ScopedEnvVar::remove(super::super::inference_boundary::HOST_BOUNDARY_ENV);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    for (status, body, reason) in [
        (
            400,
            r#"{"error":{"code":"content_policy_violation","message":"blocked"}}"#,
            "content_policy",
        ),
        (
            400,
            r#"{"error":{"code":"invalid_api_key","message":"invalid API key"}}"#,
            "auth_failure",
        ),
        (
            400,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
            "context_overflow",
        ),
        (
            401,
            r#"{"error":{"code":"invalid_api_key","message":"response_format authorization failed"}}"#,
            "auth_failure",
        ),
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = spawn_llm_stub_many("structured terminal refusal", 2, move |_, stream| {
            let (_, request) = read_http_request(stream);
            captured.lock().unwrap().push(request);
            let phrase = if status == 400 {
                "Bad Request"
            } else {
                "Unauthorized"
            };
            write!(stream, "HTTP/1.1 {status} {phrase}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        install_openai_stub_provider("openai", server.addr());
        let result = runtime
            .block_on(
                crate::llm::structured_envelope::llm_call_structured_result_impl(
                    structured_request(),
                    None,
                ),
            )
            .expect("the canonical structured caller returns a failure envelope");
        let envelope = result.as_dict().unwrap();
        assert!(matches!(envelope.get("ok"), Some(VmValue::Bool(false))));
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "{reason} must not reissue a terminal refusal without response_format"
        );
        assert!(
            requests[0].get("response_format").is_some(),
            "negative must reach native structured transport"
        );
        assert!(
            envelope.get("error").unwrap().display().contains(reason),
            "{envelope:?}"
        );
    }
}

#[test]
fn structured_output_rejection_still_falls_back_to_validated_text() {
    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let _cleanup = ClearProvider;
    let _boundary = ScopedEnvVar::remove(super::super::inference_boundary::HOST_BOUNDARY_ENV);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = spawn_llm_stub_many(
        "unsupported structured output",
        2,
        move |attempt, stream| {
            let (_, request) = read_http_request(stream);
            captured.lock().unwrap().push(request);
            let (status, body) = if attempt == 0 {
                (
                    "400 Bad Request",
                    r#"{"error":{"code":"invalid_request_error","message":"response_format json_schema is unsupported by this route"}}"#,
                )
            } else {
                (
                    "200 OK",
                    r#"{"choices":[{"message":{"role":"assistant","content":"{\"answer\":\"ready\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
                )
            };
            write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        },
    );
    install_openai_stub_provider("openai", server.addr());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(
            crate::llm::structured_envelope::llm_call_structured_result_impl(
                structured_request(),
                None,
            ),
        )
        .expect("the canonical caller accepts validated fallback text");
    let envelope = result.as_dict().unwrap();
    assert!(
        matches!(envelope.get("ok"), Some(VmValue::Bool(true))),
        "{envelope:?}"
    );
    assert_eq!(
        envelope
            .get("data")
            .unwrap()
            .as_dict()
            .unwrap()
            .get("answer")
            .unwrap()
            .display(),
        "ready"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].get("response_format").is_some());
    assert!(
        requests[1].get("response_format").is_none(),
        "the fallback must actually drop native structured output"
    );
}
