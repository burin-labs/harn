//! Completed semantic-block coalescing for streamed text fragments (harn#7501).
//!
//! Token-sized SSE/NDJSON deltas must stay live as per-fragment callbacks, but
//! the finished `LlmResult.blocks` list merges adjacent same-type /
//! same-visibility text. These tests drive the real consumers so a one-block-
//! per-token regression fails on main.

use super::ndjson::consume_ollama_ndjson_lines;
use super::sse::consume_sse_lines;
use crate::llm::api::{DialectContract, LlmResult};
use crate::llm::capabilities::WireDialect;
use std::time::Duration;

fn openai_sse_body(frames: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for frame in frames {
        body.push_str("data: ");
        body.push_str(&frame.to_string());
        body.push('\n');
    }
    body.push_str("data: [DONE]\n");
    body
}

fn openai_delta(field: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": { field: text },
            "finish_reason": null
        }]
    })
}

fn openai_stop() -> serde_json::Value {
    serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    })
}

async fn drive_openai(body: &str) -> (LlmResult, Vec<String>) {
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let result = consume_sse_lines(
        tokio::io::BufReader::new(body.as_bytes()),
        "openai",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("openai sse parse should succeed");
    let mut deltas = Vec::new();
    while let Ok(delta) = delta_rx.try_recv() {
        deltas.push(delta);
    }
    (result, deltas)
}

async fn drive_anthropic(body: &str) -> LlmResult {
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    consume_sse_lines(
        tokio::io::BufReader::new(body.as_bytes()),
        "anthropic",
        "test-model",
        DialectContract::new(WireDialect::Anthropic, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("anthropic sse parse should succeed")
}

async fn drive_ndjson(body: &[u8]) -> (LlmResult, Vec<String>) {
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut warmup_gate = false;
    let result = consume_ollama_ndjson_lines(
        body,
        "ollama",
        "stub-model",
        delta_tx,
        Duration::ZERO,
        &mut warmup_gate,
        None,
        tokio::time::Instant::now(),
    )
    .await
    .expect("ollama ndjson parse should succeed");
    let mut deltas = Vec::new();
    while let Ok(delta) = delta_rx.try_recv() {
        deltas.push(delta);
    }
    (result, deltas)
}

fn text_block(block_type: &str, text: &str, visibility: &str) -> serde_json::Value {
    serde_json::json!({
        "type": block_type,
        "text": text,
        "visibility": visibility,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn openai_sse_merges_adjacent_reasoning_and_content_fragments() {
    let body = openai_sse_body(&[
        openai_delta("reasoning", "The "),
        openai_delta("reasoning", "task"),
        openai_delta("content", "Hello "),
        openai_delta("content", "world"),
        openai_stop(),
    ]);
    let (result, deltas) = drive_openai(&body).await;

    assert_eq!(deltas.concat(), "Hello world");
    assert!(
        deltas.len() > 1,
        "live deltas must stay per-fragment, got {deltas:?}"
    );
    assert_eq!(result.text, "Hello world");
    assert_eq!(result.thinking.as_deref(), Some("The task"));
    assert_eq!(
        result.blocks,
        vec![
            text_block("reasoning", "The task", "private"),
            text_block("output_text", "Hello world", "public"),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn openai_sse_starts_a_new_block_on_type_transition() {
    // First content must be longer than the `<think>` hold so a visible
    // block is committed before the reasoning delta arrives.
    let body = openai_sse_body(&[
        openai_delta("content", "0123456789ABCDEF"),
        openai_delta("reasoning", "hidden"),
        openai_delta("content", "xyz"),
        openai_stop(),
    ]);
    let (result, deltas) = drive_openai(&body).await;

    assert_eq!(deltas.concat(), "0123456789ABCDEFxyz");
    assert!(
        deltas.len() > 1,
        "live deltas must stay per-fragment, got {deltas:?}"
    );
    assert_eq!(
        result.blocks,
        vec![
            text_block("output_text", "0123456789", "public"),
            text_block("reasoning", "hidden", "private"),
            text_block("output_text", "ABCDEFxyz", "public"),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_sse_merges_adjacent_text_deltas_and_keeps_tool_boundary() {
    let body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"search_web\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"x\\\"}\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\" after\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
        "data: [DONE]\n",
    );
    let result = drive_anthropic(body).await;

    assert_eq!(result.blocks.len(), 3, "blocks were {:#?}", result.blocks);
    assert_eq!(
        result.blocks[0],
        text_block("output_text", "Hello", "public")
    );
    assert_eq!(result.blocks[1]["type"], "tool_call");
    assert_eq!(result.blocks[1]["name"], "search_web");
    assert_eq!(
        result.blocks[2],
        text_block("output_text", " after", "public")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_sse_keeps_provider_signed_reasoning_boundary() {
    let body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Check \"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"this.\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\".\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4},\"delta\":{\"stop_reason\":\"end_turn\"}}\n",
        "data: [DONE]\n",
    );
    let result = drive_anthropic(body).await;

    assert_eq!(
        result.blocks,
        vec![
            serde_json::json!({
                "type": "thinking",
                "thinking": "Check this.",
                "signature": "sig-1"
            }),
            text_block("output_text", "Done.", "public"),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ollama_ndjson_merges_adjacent_thinking_and_content_fragments() {
    let body = concat!(
        "{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"thinking\":\"plan \"},\"done\":false}\n",
        "{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"thinking\":\"A\"},\"done\":false}\n",
        "{\"message\":{\"role\":\"assistant\",\"content\":\"Hi \"},\"done\":false}\n",
        "{\"message\":{\"role\":\"assistant\",\"content\":\"there\"},\"done\":false}\n",
        "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,",
        "\"done_reason\":\"stop\",\"prompt_eval_count\":5,\"eval_count\":4}\n",
    );
    let (result, deltas) = drive_ndjson(body.as_bytes()).await;

    assert_eq!(deltas, vec!["Hi ".to_string(), "there".to_string()]);
    assert_eq!(result.text, "Hi there");
    assert_eq!(result.thinking.as_deref(), Some("plan A"));
    assert_eq!(
        result.blocks,
        vec![
            text_block("reasoning", "plan A", "private"),
            text_block("output_text", "Hi there", "public"),
        ]
    );
}
