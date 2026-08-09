use super::sse::consume_sse_lines;
use crate::llm::api::{DialectContract, LlmCallOptions, LlmRequestPayload};

#[derive(serde::Deserialize)]
struct GoldenResult {
    text: String,
    input_tokens: i64,
    output_tokens: i64,
    stop_reason: String,
}

#[derive(serde::Deserialize)]
struct StreamGolden {
    provider: String,
    model: String,
    wire_events: Option<String>,
    result: GoldenResult,
}

async fn assert_stream_golden(source: &str) {
    let golden: StreamGolden = serde_json::from_str(source).expect("valid stream golden");
    let request = LlmRequestPayload::from(&LlmCallOptions {
        provider: golden.provider.clone(),
        model: golden.model.clone(),
        ..LlmCallOptions::default()
    });
    let dialect = DialectContract::for_request(&request);
    let events = golden.wire_events.expect("streaming dialect fixture");
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = consume_sse_lines(
        tokio::io::BufReader::new(events.as_bytes()),
        &golden.provider,
        &golden.model,
        dialect,
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("golden events parse");

    assert_eq!(result.text, golden.result.text);
    assert_eq!(result.input_tokens, golden.result.input_tokens);
    assert_eq!(result.output_tokens, golden.result.output_tokens);
    assert_eq!(
        result.stop_reason.as_deref(),
        Some(golden.result.stop_reason.as_str())
    );
    let mut streamed = delta_rx.recv().await.expect("at least one text delta");
    while let Ok(delta) = delta_rx.try_recv() {
        streamed.push_str(&delta);
    }
    assert_eq!(streamed, golden.result.text);
}

#[tokio::test]
async fn openai_events_match_golden() {
    assert_stream_golden(include_str!("../../testdata/dialects/openai_compat.json")).await;
}

#[tokio::test]
async fn anthropic_events_match_golden() {
    assert_stream_golden(include_str!("../../testdata/dialects/anthropic.json")).await;
}
