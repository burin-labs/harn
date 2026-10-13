//! Per-call request observability must cost what changed, not the transcript.

use crate::llm::content_hash::{fingerprint_computations, redacted_content_computations};

const TURNS: usize = 24;

fn request(messages: &[serde_json::Value]) -> crate::llm::api::LlmCallOptions {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.system = Some("System policy".to_string());
    opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
        "test:system",
        "test",
        "agent_loop",
        opts.system.as_deref(),
    );
    opts.messages = messages.to_vec();
    opts.session_id = Some("request-growth-session".to_string());
    opts
}

/// One agent turn: an assistant tool call and its kilobyte-sized result.
fn turn(run: &str, index: usize) -> [serde_json::Value; 2] {
    let call_id = format!("call_{run}_{index}");
    [
        serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": call_id, "type": "function",
                "function": {"name": "read", "arguments": format!("{{\"line\":{index}}}")}}],
        }),
        serde_json::json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": format!("{run} read {index}\n{}", "fn example() -> int { 1 }\n".repeat(40)),
        }),
    ]
}

/// Replays a growing transcript through the per-call request work an
/// observed call performs (cost admission and governor estimates, then the
/// request dump) and returns the fingerprints and redactions it computed.
fn hashing_work_for_growing_transcript() -> (usize, usize, usize) {
    let run = uuid::Uuid::now_v7().to_string();
    let mut messages = vec![serde_json::json!({"role": "user", "content": format!("task {run}")})];
    let fingerprints_before = fingerprint_computations();
    let redactions_before = redacted_content_computations();
    for index in 0..TURNS {
        messages.extend(turn(&run, index));
        let opts = request(&messages);
        for _ in 0..3 {
            let _ = crate::llm::cost::project_llm_call_cost(&opts, 0.0);
        }
        super::dump_llm_request(index, &format!("call-{index}"), "native", &opts)
            .expect("observable request");
    }
    (
        messages.len(),
        fingerprint_computations() - fingerprints_before,
        redacted_content_computations() - redactions_before,
    )
}

#[test]
fn per_call_request_work_hashes_and_redacts_only_new_messages() {
    let _guard = crate::llm::env_guard();
    let saved_ambient = super::transcript_ambient::swap_llm_transcript_ambient(Default::default());
    let saved_log = crate::event_log::swap_active_event_log(None);
    let _log = crate::event_log::install_memory_for_current_thread(4096);

    let (messages, fingerprints, redactions) = hashing_work_for_growing_transcript();

    crate::event_log::swap_active_event_log(saved_log);
    let _ = super::transcript_ambient::swap_llm_transcript_ambient(saved_ambient);

    // Every message is new to this run, so each was hashed and redacted at
    // least once: the counters are measuring the path under test.
    assert!(
        fingerprints >= messages && redactions >= messages,
        "{fingerprints} fingerprints and {redactions} redactions for {messages} messages"
    );
    // Re-hashing or re-redacting the whole request on each call costs
    // roughly TURNS * messages / 2 (here about 600 per pass); proportional work
    // stays within a small multiple of the messages that changed.
    assert!(
        fingerprints <= 2 * messages,
        "{fingerprints} fingerprints for {messages} messages over {TURNS} calls: \
         the request is being re-hashed on every call"
    );
    assert!(
        redactions <= messages,
        "{redactions} redactions for {messages} messages over {TURNS} calls: \
         the request is being re-redacted on every call"
    );
}
