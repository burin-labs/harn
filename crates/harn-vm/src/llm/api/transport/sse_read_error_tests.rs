//! Mid-stream read failures must surface as transient errors, not as a
//! silently truncated zero-token "success". The whole-request reqwest
//! timeout (`resolve_timeout`, including the model catalog's
//! `stream_timeout`) materializes as exactly such a body-read error, so
//! these tests drive `consume_sse_lines` with an erroring byte stream —
//! the same shape `reqwest::Response::bytes_stream()` produces when the
//! deadline fires mid-body.

use super::liveness::StreamDeadlinePolicy;
use super::sse::{consume_sse_lines, consume_sse_lines_with_policy};
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
        false,
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
        false,
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
    // OpenAI-compatible providers may use a finish_reason chunk instead of a
    // trailing `[DONE]`; that explicit terminal evidence is sufficient even
    // when an HTTP/1.1 connection remains open.
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
        "openai",
        "test-model",
        false,
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
async fn clean_eof_without_terminal_evidence_is_typed_failure() {
    let body: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n";
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let err = consume_sse_lines(
        tokio::io::BufReader::new(body),
        "openai",
        "test-model",
        false,
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
        false,
        delta_tx,
        None,
        None,
        false,
        policy,
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
