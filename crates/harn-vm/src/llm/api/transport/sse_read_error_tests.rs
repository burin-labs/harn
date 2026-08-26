//! Mid-stream read failures must surface as transient errors, not as a
//! silently truncated zero-token "success". The whole-request reqwest
//! timeout (`resolve_timeout`, including the model catalog's
//! `stream_timeout`) materializes as exactly such a body-read error, so
//! these tests drive `consume_sse_lines` with an erroring byte stream —
//! the same shape `reqwest::Response::bytes_stream()` produces when the
//! deadline fires mid-body.

use super::liveness::StreamDeadlinePolicy;
use super::sse::{consume_sse_lines, consume_sse_lines_with_policy};
use crate::llm::api::DialectContract;
use crate::llm::capabilities::WireDialect;
use crate::value::{
    ErrorCategory, ProviderStreamDeadline, ProviderStreamFailureReason, ProviderStreamPhase,
};
use std::time::Duration;

/// Reader whose underlying stream yields some SSE bytes and then an
/// io error, mimicking a reqwest total-timeout firing mid-stream.
fn erroring_reader(
    head: &'static [u8],
    error: std::io::Error,
) -> tokio::io::BufReader<impl tokio::io::AsyncBufRead + Unpin> {
    let chunks: Vec<Result<&'static [u8], std::io::Error>> = vec![Ok(head), Err(error)];
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(tokio_stream::iter(
        chunks,
    )))
}

#[tokio::test]
async fn mid_stream_timeout_surfaces_as_retryable_error_not_empty_success() {
    let reader = erroring_reader(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n",
        std::io::Error::new(std::io::ErrorKind::TimedOut, "operation timed out"),
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        reader,
        "openrouter",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("mid-stream read failure must not return a truncated success");
    let message = err.to_string();
    let failure = err
        .provider_stream_failure()
        .expect("typed provider stream failure");
    assert_eq!(failure.category(), ErrorCategory::Timeout);
    assert_eq!(failure.phase, ProviderStreamPhase::Streaming);
    assert_eq!(failure.reason, ProviderStreamFailureReason::Deadline);
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Total));
    assert!(failure.partial);
    assert!(
        crate::llm::agent_observe::is_retryable_llm_error(&err),
        "mid-stream timeout must classify as transient/retryable; message was: {message}"
    );
}

#[tokio::test]
async fn mid_stream_connection_reset_is_also_retryable() {
    let reader = erroring_reader(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n",
        std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        ),
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        reader,
        "llamacpp",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("mid-stream reset must surface as an error");
    assert!(
        crate::llm::agent_observe::is_retryable_llm_error(&err),
        "mid-stream reset must classify as transient/retryable; got: {err}"
    );
}

#[tokio::test]
async fn finish_reason_terminal_completes_without_waiting_for_eof() {
    // Providers that do not declare trailing stream accounting may use a
    // finish_reason chunk instead of a trailing `[DONE]`.
    use tokio::io::AsyncWriteExt;
    let (reader, mut writer) = tokio::io::duplex(1024);
    writer
        .write_all(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
                "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n",
            )
            .as_bytes(),
        )
        .await
        .expect("seed terminal SSE frames");
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let result = consume_sse_lines(
        tokio::io::BufReader::new(reader),
        "baseten",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("finish_reason must terminate without EOF");
    drop(writer);
    assert_eq!(result.text, "hello");
    assert_eq!(result.stop_reason.as_deref(), Some("stop"));
}

#[tokio::test]
async fn together_reads_trailing_usage_after_finish_reason() {
    let body = concat!(
        "data: {\"id\":\"req-live\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
        "data: {\"id\":\"req-live\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}]}\n",
        "data: {\"id\":\"req-live\",\"choices\":[],\"usage\":{\"prompt_tokens\":139,\"completion_tokens\":35,\"total_tokens\":174,\"prompt_tokens_details\":{\"cached_tokens\":64},\"completion_tokens_details\":{\"reasoning_tokens\":7}}}\n",
        "data: [DONE]\n",
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let result = consume_sse_lines(
        tokio::io::BufReader::new(body.as_bytes()),
        "together",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("Together trailing usage must be consumed");
    assert_eq!(result.text, "hello");
    assert_eq!(result.stop_reason.as_deref(), Some("stop"));
    assert_eq!(result.input_tokens, 139);
    assert_eq!(result.output_tokens, 35);
    assert_eq!(result.cache_read_tokens, 64);
}

#[tokio::test]
async fn openrouter_reads_trailing_usage_and_exact_cost_after_finish_reason() {
    let body = concat!(
        "data: {\"id\":\"gen-live\",\"model\":\"qwen/test\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
        "data: {\"id\":\"gen-live\",\"model\":\"qwen/test\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}]}\n",
        "data: {\"id\":\"gen-live\",\"model\":\"qwen/test\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15,\"cost\":0.00042}}\n",
        "data: [DONE]\n",
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let result = consume_sse_lines(
        tokio::io::BufReader::new(body.as_bytes()),
        "openrouter",
        "qwen/test",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("OpenRouter trailing usage must be consumed");
    assert_eq!(result.input_tokens, 12);
    assert_eq!(result.output_tokens, 3);
    assert_eq!(result.telemetry.request_id.as_deref(), Some("gen-live"));
    assert_eq!(result.telemetry.provider_cost_usd, Some(0.00042));
    assert_eq!(result.usage().cost_usd, Some(0.00042));
}

#[tokio::test]
async fn openrouter_preserves_generation_id_from_response_header() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"cost\":0.00001}}\n",
        "data: [DONE]\n",
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let result = consume_sse_lines_with_policy(
        tokio::io::BufReader::new(body.as_bytes()),
        "openrouter",
        "qwen/test",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
        StreamDeadlinePolicy {
            total: Duration::from_hours(1),
            first_chunk: Duration::from_hours(1),
            idle: Duration::from_hours(1),
        },
        Some("gen-from-header"),
        tokio::time::Instant::now(),
    )
    .await
    .expect("header generation id must survive streaming parse");
    assert_eq!(
        result.telemetry.request_id.as_deref(),
        Some("gen-from-header")
    );
}

#[tokio::test]
async fn clean_eof_without_terminal_evidence_is_typed_failure() {
    let body: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n";
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("EOF without a provider terminal event must fail");
    let failure = err
        .provider_stream_failure()
        .expect("typed provider stream failure");
    assert_eq!(failure.reason, ProviderStreamFailureReason::PrematureEof);
    assert_eq!(failure.phase, ProviderStreamPhase::Streaming);
    assert!(failure.partial);
}

fn assert_structured_sse_error(
    err: &crate::value::VmError,
    expected_partial: bool,
    forbidden_secret: Option<&str>,
) {
    assert!(
        err.provider_stream_failure().is_none(),
        "structured SSE error must not fall through to ProviderStreamFailure/premature_eof; got: {err}"
    );
    let crate::value::VmError::Thrown(crate::value::VmValue::Dict(dict)) = err else {
        panic!("structured SSE error must throw a taxonomy dict; got: {err}");
    };
    assert_eq!(
        dict.get("category").map(|value| value.display()).as_deref(),
        Some("invalid_request")
    );
    assert_eq!(
        dict.get("kind").map(|value| value.display()).as_deref(),
        Some("terminal")
    );
    assert_eq!(
        dict.get("reason").map(|value| value.display()).as_deref(),
        Some("invalid_request")
    );
    assert_eq!(
        dict.get("partial").map(|value| value.display()).as_deref(),
        Some(if expected_partial { "true" } else { "false" })
    );
    assert_eq!(
        dict.get("source").map(|value| value.display()).as_deref(),
        Some("provider_stream")
    );
    let message = dict
        .get("message")
        .map(|value| value.display())
        .unwrap_or_default();
    assert!(
        message.contains("[invalid_request]"),
        "sanitized classifier tag missing from message: {message}"
    );
    assert!(
        !crate::llm::agent_observe::is_retryable_llm_error(err),
        "terminal invalid_request must not be retryable; message was: {message}"
    );
    if let Some(secret) = forbidden_secret {
        assert!(
            !message.contains(secret),
            "provider error sanitizer must redact secrets; message was: {message}"
        );
    }
}

#[tokio::test]
async fn named_sse_error_event_classifies_instead_of_premature_eof() {
    let body: &[u8] = concat!(
        "event: error\n",
        "data: {\"error\":\"request rejected\",\"kind\":\"terminal\",\"reason\":\"invalid_request\"}\n",
    )
    .as_bytes();
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("named SSE error event must terminate as a classified provider error");
    assert_structured_sse_error(&err, false, None);
}

#[tokio::test]
async fn partial_text_then_sse_error_preserves_partial_and_upstream_taxonomy() {
    let body: &[u8] = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
        "event: error\n",
        "data: {\"error\":\"Authorization: Bearer sk-secret-token\",\"kind\":\"terminal\",\"reason\":\"invalid_request\"}\n",
    )
    .as_bytes();
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("partial text then SSE error must terminate as a classified provider error");
    assert_eq!(delta_rx.try_recv().ok().as_deref(), Some("hello"));
    assert_structured_sse_error(&err, true, Some("sk-secret-token"));
}

#[tokio::test]
async fn top_level_structured_sse_error_without_event_name_classifies() {
    let body: &[u8] =
        b"data: {\"error\":\"bad request shape\",\"kind\":\"terminal\",\"reason\":\"invalid_request\"}\n";
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("top-level structured error JSON must classify without event: error");
    assert_structured_sse_error(&err, false, None);
}

#[tokio::test]
async fn anthropic_typed_error_frame_classifies_instead_of_premature_eof() {
    let body: &[u8] =
        b"data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"boom\"}}\n";
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "anthropic",
        "test-model",
        DialectContract::new(WireDialect::Anthropic, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect_err("Anthropic type=error must classify instead of premature EOF");
    assert!(
        err.provider_stream_failure().is_none(),
        "Anthropic typed error must not fall through to premature_eof; got: {err}"
    );
    let crate::value::VmError::Thrown(crate::value::VmValue::Dict(dict)) = err else {
        panic!("Anthropic typed error must throw a taxonomy dict; got: {err}");
    };
    assert_eq!(
        dict.get("reason").map(|value| value.display()).as_deref(),
        Some("invalid_request")
    );
    assert_eq!(
        dict.get("partial").map(|value| value.display()).as_deref(),
        Some("false")
    );
}

async fn pending_sse_deadline(
    policy: StreamDeadlinePolicy,
    initial: Option<&[u8]>,
) -> crate::value::ProviderStreamFailure {
    use tokio::io::AsyncWriteExt;
    let (reader, mut writer) = tokio::io::duplex(1024);
    if let Some(initial) = initial {
        writer.write_all(initial).await.expect("seed SSE bytes");
    }
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines_with_policy(
        tokio::io::BufReader::new(reader),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
        policy,
        None,
        tokio::time::Instant::now(),
    )
    .await
    .expect_err("pending stream must reach a deadline");
    drop(writer);
    err.provider_stream_failure()
        .expect("typed provider stream failure")
        .clone()
}

#[tokio::test(start_paused = true)]
async fn first_chunk_deadline_is_distinct_and_typed() {
    let failure = pending_sse_deadline(
        StreamDeadlinePolicy::for_test(
            Duration::from_secs(30),
            Duration::from_secs(5),
            Duration::from_secs(10),
        ),
        None,
    )
    .await;
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::FirstChunk));
    assert_eq!(failure.phase, ProviderStreamPhase::AwaitingFirstChunk);
    assert!(!failure.partial);
}

#[tokio::test(start_paused = true)]
async fn idle_deadline_is_distinct_and_typed() {
    let failure = pending_sse_deadline(
        StreamDeadlinePolicy::for_test(
            Duration::from_secs(30),
            Duration::from_secs(5),
            Duration::from_secs(3),
        ),
        Some(b"data: {\"choices\":[{\"delta\":{\"content\":\"p\"}}]}\n"),
    )
    .await;
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Idle));
    assert_eq!(failure.phase, ProviderStreamPhase::Streaming);
    assert!(failure.partial);
}

#[tokio::test(start_paused = true)]
async fn total_deadline_is_distinct_and_typed() {
    let failure = pending_sse_deadline(
        StreamDeadlinePolicy::for_test(
            Duration::from_secs(2),
            Duration::from_secs(5),
            Duration::from_secs(10),
        ),
        None,
    )
    .await;
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Total));
    assert_eq!(failure.phase, ProviderStreamPhase::AwaitingFirstChunk);
}
