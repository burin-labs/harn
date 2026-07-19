//! Mid-stream read failures must surface as transient errors, not as a
//! silently truncated zero-token "success". The whole-request reqwest
//! timeout (`resolve_timeout`, including the model catalog's
//! `stream_timeout`) materializes as exactly such a body-read error, so
//! these tests drive `consume_sse_lines` with an erroring byte stream —
//! the same shape `reqwest::Response::bytes_stream()` produces when the
//! deadline fires mid-body.

use super::sse::consume_sse_lines;

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
    assert!(
        message.contains("openrouter stream error (mid-stream read)"),
        "message was: {message}"
    );
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
async fn clean_eof_without_done_sentinel_still_returns_ok() {
    // Anthropic ends streams with `message_stop` (no `[DONE]`), and some
    // OpenAI-compatible servers close cleanly without the sentinel; a
    // clean EOF must keep returning the accumulated result.
    let body: &[u8] = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n",
        )
        .as_bytes();
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(body);
    let result = consume_sse_lines(
        reader,
        "openai",
        "test-model",
        false,
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("clean EOF without [DONE] parses");
    assert_eq!(result.text, "hello");
    assert_eq!(result.stop_reason.as_deref(), Some("stop"));
}
