use super::ndjson::consume_ollama_ndjson_lines;
use super::sse::reqwest_send_error;
use super::{
    append_ollama_tool_calls, non_stream_send_error, parse_ollama_tool_arguments,
    should_request_stream_usage, telemetry_source,
};
use crate::value::{error_to_category, ErrorCategory, VmError};
use std::time::Duration;

/// Drive a real `reqwest::Error` of the requested kind so the classifier is
/// exercised against genuine reqwest internals (its `is_*` predicates), not a
/// hand-rolled stand-in. `192.0.2.1` is reserved TEST-NET-1 (RFC 5737) and is
/// guaranteed unroutable, producing a connect/timeout failure deterministically.
async fn real_send_error(timeout: Duration) -> reqwest::Error {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .build()
        .expect("client builds");
    client
        .get("http://192.0.2.1:9/never")
        .send()
        .await
        .expect_err("send to unroutable TEST-NET-1 address must fail")
}

#[tokio::test]
async fn non_stream_send_error_is_typed_network_not_substring() {
    // Connect/timeout failures must arrive as a typed CategorizedError whose
    // category is transient — NOT a bare Thrown string reclassified by
    // substring downstream.
    let raw = real_send_error(Duration::from_millis(150)).await;
    let typed = non_stream_send_error("openai", raw);
    match &typed {
        VmError::CategorizedError { category, .. } => {
            assert!(
                matches!(
                    category,
                    ErrorCategory::Timeout | ErrorCategory::TransientNetwork
                ),
                "expected Timeout/TransientNetwork, got {category:?}"
            );
            assert!(
                category.is_transient(),
                "network send error must be transient"
            );
        }
        other => panic!("expected typed CategorizedError, got {other:?}"),
    }
    // And the canonical category extractor reads the explicit category
    // (proving no substring re-derivation is needed).
    assert!(error_to_category(&typed).is_transient());
}

#[tokio::test]
async fn stream_and_non_stream_send_paths_share_one_classifier() {
    // DRY check: both phases route through `reqwest_send_error` and yield the
    // same typed category for the same underlying reqwest failure.
    let stream_err = reqwest_send_error(
        "anthropic",
        "stream",
        real_send_error(Duration::from_millis(150)).await,
    );
    let request_err = reqwest_send_error(
        "anthropic",
        "request",
        real_send_error(Duration::from_millis(150)).await,
    );
    assert_eq!(
        error_to_category(&stream_err).is_transient(),
        error_to_category(&request_err).is_transient(),
        "stream and non-stream send errors must classify identically"
    );
}

#[test]
fn stream_usage_requested_for_openai_compatible_endpoints() {
    assert!(should_request_stream_usage(
        false,
        false,
        "/chat/completions"
    ));
    assert!(should_request_stream_usage(
        false,
        true,
        "/v1/chat/completions"
    ));
    assert!(!should_request_stream_usage(false, true, "/api/chat"));
    assert!(!should_request_stream_usage(true, false, "/messages"));
}

#[test]
fn ollama_tool_arguments_accept_object_shape() {
    let arguments = serde_json::json!({"path": "README.md"});
    assert_eq!(parse_ollama_tool_arguments(&arguments), arguments);
}

#[test]
fn ollama_tool_arguments_parse_json_strings() {
    let parsed = parse_ollama_tool_arguments(&serde_json::json!("{\"path\":\"README.md\"}"));
    assert_eq!(parsed["path"], "README.md");
}

#[test]
fn ollama_stream_chunks_surface_tool_calls() {
    let message = serde_json::json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "function": {
                "name": "read_file",
                "arguments": {
                    "path": "README.md"
                }
            }
        }]
    });
    let mut tool_calls = Vec::new();
    let mut blocks = Vec::new();
    append_ollama_tool_calls(&message, &mut tool_calls, &mut blocks);

    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["name"], "read_file");
    assert_eq!(tool_calls[0]["arguments"]["path"], "README.md");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "tool_call");
}

#[tokio::test]
async fn ollama_ndjson_empty_content_eval_count_is_marked_for_retry() {
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":10,\"eval_count\":4}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let err = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect_err("empty Ollama parser-bug response should fail");
    assert!(
        err.to_string()
            .contains("[ollama_empty_content_parser_bug]"),
        "err was: {err}"
    );
}

#[tokio::test]
async fn ollama_ndjson_requires_done_frame() {
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"partial\"},\"done\":false}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let err = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect_err("truncated Ollama stream should fail");
    assert!(err.to_string().contains("unexpected EOF before done=true"));
}

#[tokio::test]
async fn ollama_ndjson_captures_server_telemetry_from_done_frame() {
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":false}\n\
            {\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\
            \"model\":\"devstral-small-2:24b\",\
            \"total_duration\":4500000000,\"load_duration\":300000000,\
            \"prompt_eval_count\":42,\"prompt_eval_duration\":1100000000,\
            \"eval_count\":7,\"eval_duration\":3000000000}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "devstral-small-2:24b",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect("ollama stream parses");
    assert_eq!(result.telemetry.source, telemetry_source::OLLAMA_CHAT);
    assert_eq!(result.telemetry.server_total_ms, Some(4500));
    assert_eq!(result.telemetry.server_load_ms, Some(300));
    assert_eq!(result.telemetry.server_prompt_eval_ms, Some(1100));
    assert_eq!(result.telemetry.server_generation_ms, Some(3000));
    assert_eq!(result.telemetry.server_prompt_tokens, Some(42));
    assert_eq!(result.telemetry.server_output_tokens, Some(7));
    assert_eq!(
        result.telemetry.runtime_loaded_model.as_deref(),
        Some("devstral-small-2:24b")
    );
}

#[tokio::test]
async fn ollama_ndjson_done_reason_stop_is_captured_as_stop_reason() {
    // Fix 1: a normal `done_reason:"stop"` frame surfaces stop_reason.
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true,\
            \"done_reason\":\"stop\",\"prompt_eval_count\":5,\"eval_count\":2}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect("ollama stream parses");
    assert_eq!(result.stop_reason.as_deref(), Some("stop"));
    // Fix 3: native Ollama reports no cache field — it is unsupported,
    // not a real 0% cache hit.
    assert!(!result.cache_supported);
}

#[tokio::test]
async fn ollama_ndjson_thinking_is_private_not_visible_delta() {
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"thinking\":\"private plan\"},\"done\":false}\n\
            {\"message\":{\"role\":\"assistant\",\"content\":\"visible answer\"},\"done\":false}\n\
            {\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\
            \"done_reason\":\"stop\",\"prompt_eval_count\":5,\"eval_count\":4}\n";
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect("ollama stream parses");

    let mut deltas = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        deltas.push(delta);
    }

    assert_eq!(deltas, vec!["visible answer".to_string()]);
    assert_eq!(result.text, "visible answer");
    assert_eq!(result.thinking.as_deref(), Some("private plan"));
    assert!(
        !result.text.contains("private plan"),
        "private thinking leaked into visible text"
    );
    assert!(result.blocks.iter().any(|block| {
        block["type"] == "reasoning"
            && block["visibility"] == "private"
            && block["text"] == "private plan"
    }));
    assert!(result.blocks.iter().any(|block| {
        block["type"] == "output_text"
            && block["visibility"] == "public"
            && block["text"] == "visible answer"
    }));
}

#[tokio::test]
async fn ollama_ndjson_done_reason_length_surfaces_truncation_not_parser_bug() {
    // Fix 1 + Fix 2: a length-capped frame with empty content must NOT
    // raise the retryable parser-bug error; instead it returns Ok carrying
    // stop_reason: Some("length") as a non-retryable truncation signal.
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\
            \"done_reason\":\"length\",\"prompt_eval_count\":10,\"eval_count\":8}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect("length truncation should return Ok, not the parser-bug error");
    assert_eq!(result.stop_reason.as_deref(), Some("length"));
    assert!(result.text.is_empty());
    assert!(result.tool_calls.is_empty());
}

#[tokio::test]
async fn ollama_ndjson_thinking_only_length_stays_private() {
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"thinking\":\"partial private trace\"},\"done\":true,\
            \"done_reason\":\"length\",\"prompt_eval_count\":5,\"eval_count\":4}\n";
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect("length truncation should return Ok, not promote thinking");

    assert!(rx.try_recv().is_err());
    assert_eq!(result.stop_reason.as_deref(), Some("length"));
    assert_eq!(result.text, "");
    assert_eq!(result.thinking.as_deref(), Some("partial private trace"));
}

#[tokio::test]
async fn ollama_ndjson_empty_content_without_done_reason_is_still_parser_bug() {
    // Fix 2 guardrail: empty content with no `done_reason` (or stop) keeps
    // the retryable parser-bug behavior — only "length" is special-cased.
    let body = b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\
            \"done_reason\":\"stop\",\"prompt_eval_count\":10,\"eval_count\":4}\n";
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let err = consume_ollama_ndjson_lines(
        &body[..],
        "ollama",
        "stub-model",
        tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
    )
    .await
    .expect_err("empty content with done_reason=stop is still a parser bug");
    assert!(
        err.to_string()
            .contains("[ollama_empty_content_parser_bug]"),
        "err was: {err}"
    );
}
