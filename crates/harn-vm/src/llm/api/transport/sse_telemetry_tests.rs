//! Serving-provenance capture on the OpenAI-compatible streaming path.
//!
//! `serving_base_url` cannot separate several hosts serving byte-identical
//! artifacts on the same local URL, so a run record keyed on the route alone
//! cannot say which build produced the tokens. OpenAI-shaped servers already
//! publish `system_fingerprint` — llama.cpp reports its build string there —
//! so these tests pin that the streaming reader lifts it off the wire, keeps
//! it across the frame carrying the usage counters, and leaves it absent when
//! nothing reported one.
//!
//! The frames are assembled to mirror an observed llama.cpp `stream: true`
//! reply: every JSON chunk repeats `id`, `model`, and `system_fingerprint`,
//! and the final counters ride an extra chunk whose `choices` array is empty.
//! They are built with `json!` rather than written as wire literals so no
//! single fixture string trips the long-string prose lint.

use super::sse::consume_sse_lines;
use crate::llm::api::DialectContract;
use crate::llm::api::LlmResult;
use crate::llm::capabilities::WireDialect;

const OBSERVED_BUILD: &str = "b9994-14d3ba45f";
const OTHER_BUILD: &str = "b10360-48d22e295";

/// One content chunk, optionally announcing the backend build.
fn content_chunk(fingerprint: Option<&str>) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "choices": [{"finish_reason": null, "index": 0, "delta": {"content": "hi"}}],
        "id": "chatcmpl-stream",
        "model": "served-model",
        "object": "chat.completion.chunk"
    });
    if let Some(fingerprint) = fingerprint {
        frame["system_fingerprint"] = serde_json::json!(fingerprint);
    }
    frame
}

/// The trailing empty-`choices` chunk that carries the usage counters.
fn usage_chunk(fingerprint: Option<&str>) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "choices": [],
        "id": "chatcmpl-stream",
        "object": "chat.completion.chunk",
        "usage": {"completion_tokens": 6, "prompt_tokens": 14, "total_tokens": 20}
    });
    if let Some(fingerprint) = fingerprint {
        frame["system_fingerprint"] = serde_json::json!(fingerprint);
    }
    frame
}

fn sse_body(frames: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for frame in frames {
        body.push_str("data: ");
        body.push_str(&frame.to_string());
        body.push('\n');
    }
    body.push_str("data: [DONE]\n");
    body
}

/// Drive `consume_sse_lines` against a canned OpenAI-compatible SSE buffer.
async fn drive_openai(body: &str) -> LlmResult {
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    consume_sse_lines(
        tokio::io::BufReader::new(body.as_bytes()),
        "llamacpp",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("sse parse should succeed")
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_system_fingerprint_reaches_telemetry() {
    let body = sse_body(&[
        content_chunk(Some(OBSERVED_BUILD)),
        usage_chunk(Some(OBSERVED_BUILD)),
    ]);
    let result = drive_openai(&body).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some(OBSERVED_BUILD)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fingerprint_announced_only_on_the_opening_chunk_survives_the_usage_frame() {
    // The usage frame rebuilds the telemetry envelope wholesale, so anything
    // earlier chunks reported is dropped unless it is carried across.
    //
    // This guards a shape we have not observed rather than one we have: the
    // llama.cpp stream checked against this code repeats the fingerprint on
    // every chunk, which would mask the loss. Nothing in the OpenAI stream
    // contract promises that repetition, and a server that announces its
    // build only once must not have it erased by the frame that happens to
    // carry the token counters.
    let body = sse_body(&[content_chunk(Some(OBSERVED_BUILD)), usage_chunk(None)]);
    let result = drive_openai(&body).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some(OBSERVED_BUILD),
        "the opening chunk's build id must survive the usage frame's envelope reset"
    );
    // The reset must still deliver what it owns.
    assert_eq!(result.telemetry.server_prompt_tokens, Some(14));
}

#[tokio::test(flavor = "current_thread")]
async fn a_stream_reporting_no_fingerprint_leaves_it_absent() {
    // Absence must stay absent rather than collapsing to an empty string that
    // would compare equal across two genuinely different servers.
    let body = sse_body(&[content_chunk(None), usage_chunk(None)]);
    let result = drive_openai(&body).await;

    assert_eq!(result.telemetry.serving_fingerprint, None);
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_frames_fingerprint_wins_over_an_earlier_one() {
    // Both values are real build strings observed from two different servers,
    // which is exactly the pair the field has to keep distinct.
    let body = sse_body(&[
        content_chunk(Some(OBSERVED_BUILD)),
        usage_chunk(Some(OTHER_BUILD)),
    ]);
    let result = drive_openai(&body).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some(OTHER_BUILD)
    );
}
