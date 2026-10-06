use super::*;
use crate::llm::{AgentTerminalClass, LlmErrorKind, LlmErrorReason};
use crate::value::{VmError, VmValue};

#[test]
fn managed_spending_pause_survives_http_and_stream_transport() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    for (reason, kind, class) in [
        (
            "managed_spend_paused",
            "terminal",
            AgentTerminalClass::ManagedSpendPaused,
        ),
        (
            "billing_limit",
            "terminal",
            AgentTerminalClass::ProviderBilling,
        ),
        ("rate_limit", "transient", AgentTerminalClass::RateLimited),
    ] {
        for streaming in [false, true] {
            let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = requests.clone();
            let server = spawn_llm_stub("managed spending error", move |stream| {
                use std::io::{Read, Write};
                let mut request = vec![0; 32_768];
                assert!(stream.read(&mut request).expect("request") > 0);
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let message = if reason == "rate_limit" {
                    "Too many requests"
                } else {
                    "HTTP 429 insufficient credit"
                };
                let frame = serde_json::json!({"error": {
                    "reason": reason,
                    "kind": if reason == "managed_spend_paused" { "transient" } else { kind },
                    "message": message
                }});
                let (status, content_type, body) = if streaming {
                    (
                        "200 OK",
                        "text/event-stream",
                        format!("event: error\ndata: {frame}\n\n"),
                    )
                } else {
                    (
                        "429 Too Many Requests",
                        "application/json",
                        frame.to_string(),
                    )
                };
                write!(stream, "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).expect("response");
            });
            install_managed_supply_stub_provider("managed-spend-control", server.addr());
            let mut opts = managed_opts("managed-spend-control");
            opts.model = "gpt-4o-mini".to_string();
            opts.stream = streaming;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let error = runtime
                .block_on(vm_call_llm_full(&opts))
                .expect_err("service refusal");
            crate::llm_config::clear_user_overrides();
            drop(server);
            assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            let observed = crate::llm::api::classify_vm_llm_error(&error);
            assert_eq!(observed.reason.as_str(), reason);
            assert_eq!(observed.kind.as_str(), kind);
            let VmError::Thrown(VmValue::Dict(fields)) = error else {
                panic!("transport must preserve typed error: {error}");
            };
            let facts = crate::llm::helpers::vm_value_dict_to_json(&fields);
            assert_eq!(facts["reason"], reason, "{facts}");
            assert_eq!(facts["kind"], kind, "{facts}");
            // Billing and the service pause identify their owner without an
            // agent session. A throttle's terminal class additionally requires
            // the provider provenance attached by session finalization.
            if reason != "rate_limit" {
                assert_eq!(
                    crate::llm::agent_terminal_class("error", "", Some(&facts)),
                    Some(class)
                );
            }
        }
    }
}

#[test]
fn personal_provider_cannot_claim_managed_service_pause() {
    let _guard = env_guard();
    crate::llm_config::clear_user_overrides();
    let body = r#"{"error":{"reason":"managed_spend_paused","kind":"transient","message":"Too many requests"}}"#;
    let http = crate::llm::api::errors::classify_provider_http_error(
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        None,
        body,
    );
    assert_eq!(http.reason, LlmErrorReason::RateLimit);
    assert_eq!(http.kind, LlmErrorKind::Transient);
    let stream = crate::llm::api::errors::classify_provider_stream_error("openai", body, false);
    let VmError::Thrown(VmValue::Dict(fields)) = stream else {
        panic!("typed error");
    };
    let facts = crate::llm::helpers::vm_value_dict_to_json(&fields);
    assert_ne!(facts["reason"], "managed_spend_paused");
}
