use super::read_non_stream_response_body;
use crate::value::{error_to_category, ErrorCategory, VmError};

fn erroring_response(kind: std::io::ErrorKind) -> reqwest::Response {
    let chunks: Vec<Result<&'static [u8], std::io::Error>> = vec![
        Ok(br#"{"partial":true}"#),
        Err(std::io::Error::new(kind, "injected response body failure")),
    ];
    let body = reqwest::Body::wrap_stream(tokio_stream::iter(chunks));
    let response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("x-request-id", "req-body-read")
        .header("x-generation-id", "gen-body-read")
        .body(body)
        .expect("build erroring response");
    reqwest::Response::from(response)
}

#[tokio::test]
async fn successful_non_stream_body_read_is_unchanged() {
    let response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(reqwest::Body::from(r#"{"choices":[]}"#))
        .expect("build successful response");
    let response = read_non_stream_response_body(
        reqwest::Response::from(response),
        None,
        "openai",
        "test-model",
    )
    .await
    .expect("a complete response body must still succeed");

    assert_eq!(response.status, reqwest::StatusCode::OK);
    assert_eq!(response.content_type.as_deref(), Some("application/json"));
    assert_eq!(response.body, r#"{"choices":[]}"#);
}

#[tokio::test]
async fn non_stream_body_timeout_is_typed_and_retryable() {
    let response = erroring_response(std::io::ErrorKind::TimedOut);
    let error = read_non_stream_response_body(response, None, "openai", "test-model")
        .await
        .expect_err("a timed-out reqwest body read must fail");

    match &error {
        VmError::CategorizedError { category, .. } => assert_eq!(category, &ErrorCategory::Timeout),
        other => panic!("expected typed timeout, got {other:?}"),
    }
    assert!(
        crate::llm::agent_observe::is_retryable_llm_error(&error),
        "the canonical agent retry classifier must accept the typed timeout"
    );
}

#[tokio::test]
async fn non_timeout_body_read_failure_is_transient_network_not_timeout() {
    let response = erroring_response(std::io::ErrorKind::ConnectionReset);
    let error = read_non_stream_response_body(response, None, "openai", "test-model")
        .await
        .expect_err("a reset reqwest body read must fail");

    assert_eq!(error_to_category(&error), ErrorCategory::TransientNetwork);
    assert_ne!(error_to_category(&error), ErrorCategory::Timeout);
    assert!(matches!(error, VmError::CategorizedError { .. }));
    assert!(
        crate::llm::agent_observe::is_retryable_llm_error(&error),
        "a partial non-stream body is a retryable transport failure"
    );
}
