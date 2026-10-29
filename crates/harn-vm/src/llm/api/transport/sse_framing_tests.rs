//! SSE framing through the shared consumer: `data:` without a space, and a
//! payload split across several `data:` lines, both valid `text/event-stream`.

use super::sse::consume_sse_lines;
use crate::llm::api::{DialectContract, LlmCallOptions, LlmRequestPayload};

async fn text_of(wire: &str) -> String {
    let options = LlmCallOptions {
        provider: "openai".into(),
        model: "fixture-openai".into(),
        ..LlmCallOptions::default()
    };
    let dialect = DialectContract::for_request(&LlmRequestPayload::from(&options));
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel();
    consume_sse_lines(
        tokio::io::BufReader::new(wire.as_bytes()),
        "openai",
        "fixture-openai",
        dialect,
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("stream parses")
    .text
}

#[tokio::test]
async fn data_field_without_a_space_is_read() {
    let wire = concat!(
        "data:{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data:{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data:[DONE]\n\n",
    );
    assert_eq!(text_of(wire).await, "hi");
}

#[tokio::test]
async fn a_payload_split_across_data_lines_is_one_frame() {
    let wire = concat!(
        "data: {\"choices\":[{\"delta\":\n",
        "data: {\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    assert_eq!(text_of(wire).await, "hi");
}

#[tokio::test]
async fn crlf_framing_and_comments_are_read() {
    let wire = concat!(
        ": keepalive\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        "data: [DONE]\r\n\r\n",
    );
    assert_eq!(text_of(wire).await, "hi");
}
