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
//! The frame shapes below mirror an observed llama.cpp `stream: true`
//! response: every JSON chunk repeats `id`, `model`, and
//! `system_fingerprint`, and the final counters ride an extra chunk whose
//! `choices` array is empty.

use super::sse::consume_sse_lines;
use crate::llm::api::DialectContract;
use crate::llm::capabilities::WireDialect;
use crate::llm::LlmResult;

/// Drive `consume_sse_lines` against a canned OpenAI-compatible SSE buffer.
async fn drive_openai(bytes: &'static [u8]) -> LlmResult {
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    consume_sse_lines(
        tokio::io::BufReader::new(bytes),
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
    // Transcribed from a real llama.cpp stream: the fingerprint repeats on
    // every chunk, and the usage counters arrive on a trailing empty-choices
    // chunk that also repeats it.
    let body = concat!(
        "data: {\"choices\":[{\"finish_reason\":null,\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}],\"id\":\"chatcmpl-stream\",\"model\":\"qwen3.6-35b-a3b-ud-q4-k-xl\",\"system_fingerprint\":\"b9994-14d3ba45f\",\"object\":\"chat.completion.chunk\"}\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\",\"index\":0,\"delta\":{}}],\"id\":\"chatcmpl-stream\",\"model\":\"qwen3.6-35b-a3b-ud-q4-k-xl\",\"system_fingerprint\":\"b9994-14d3ba45f\",\"object\":\"chat.completion.chunk\"}\n",
        "data: {\"choices\":[],\"id\":\"chatcmpl-stream\",\"model\":\"qwen3.6-35b-a3b-ud-q4-k-xl\",\"system_fingerprint\":\"b9994-14d3ba45f\",\"object\":\"chat.completion.chunk\",\"usage\":{\"completion_tokens\":6,\"prompt_tokens\":14,\"total_tokens\":20}}\n",
        "data: [DONE]\n",
    );
    let result = drive_openai(body.as_bytes()).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some("b9994-14d3ba45f")
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
    let body = concat!(
        "data: {\"choices\":[{\"finish_reason\":null,\"index\":0,\"delta\":{\"content\":\"hi\"}}],\"id\":\"chatcmpl-stream\",\"model\":\"served-model\",\"system_fingerprint\":\"b9994-14d3ba45f\",\"object\":\"chat.completion.chunk\"}\n",
        "data: {\"choices\":[],\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n",
        "data: [DONE]\n",
    );
    let result = drive_openai(body.as_bytes()).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some("b9994-14d3ba45f"),
        "the opening chunk's build id must survive the usage frame's envelope reset"
    );
    // The reset must still deliver what it owns.
    assert_eq!(result.telemetry.server_prompt_tokens, Some(3));
}

#[tokio::test(flavor = "current_thread")]
async fn a_stream_reporting_no_fingerprint_leaves_it_absent() {
    // Absence must stay absent rather than collapsing to an empty string that
    // would compare equal across two genuinely different servers.
    let body = concat!(
        "data: {\"id\":\"s1\",\"model\":\"served-model\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
        "data: {\"id\":\"s1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n",
        "data: [DONE]\n",
    );
    let result = drive_openai(body.as_bytes()).await;

    assert_eq!(result.telemetry.serving_fingerprint, None);
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_frames_fingerprint_wins_over_an_earlier_one() {
    // Both values here are real build strings observed from two different
    // servers, which is exactly the pair the field has to keep distinct.
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"id\":\"chatcmpl-stream\",\"system_fingerprint\":\"b9994-14d3ba45f\",\"object\":\"chat.completion.chunk\"}\n",
        "data: {\"choices\":[],\"id\":\"chatcmpl-stream\",\"system_fingerprint\":\"b10360-48d22e295\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n",
        "data: [DONE]\n",
    );
    let result = drive_openai(body.as_bytes()).await;

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some("b10360-48d22e295")
    );
}
