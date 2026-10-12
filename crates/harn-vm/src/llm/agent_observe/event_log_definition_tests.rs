//! Served-message definitions persisted only to the active event log.

use crate::event_log::{EventLog, Topic, HARN_LLM_TRANSCRIPT_TOPIC};

fn request(messages: Vec<serde_json::Value>) -> crate::llm::api::LlmCallOptions {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.system = Some("System policy".to_string());
    opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
        "test:system",
        "test",
        "agent_loop",
        opts.system.as_deref(),
    );
    opts.messages = messages;
    opts
}

/// A growing transcript re-sends every earlier message on each call. With
/// only the event log as a durable sink, each message must still be defined
/// once, and the last request must still reconstruct from the log alone.
#[test]
fn event_log_defines_each_served_message_once() {
    let _guard = crate::llm::env_guard();
    let saved_ambient = super::transcript_ambient::swap_llm_transcript_ambient(Default::default());
    let saved_log = crate::event_log::swap_active_event_log(None);
    let log = crate::event_log::install_memory_for_current_thread(64);
    assert!(super::current_transcript_dir().is_none());

    let mut messages = vec![serde_json::json!({"role": "user", "content": "read the files"})];
    for turn in 0..4 {
        messages
            .push(serde_json::json!({"role": "assistant", "content": format!("reading {turn}")}));
        messages
            .push(serde_json::json!({"role": "tool_result", "content": format!("file {turn}")}));
        super::dump_llm_request(
            turn,
            &format!("call-{turn}"),
            "native",
            &request(messages.clone()),
        )
        .expect("observable request");
    }

    let topic = Topic::new(HARN_LLM_TRANSCRIPT_TOPIC).expect("transcript topic");
    let events = futures::executor::block_on(log.read_range(&topic, None, usize::MAX))
        .expect("read transcript topic");
    let definitions: Vec<&serde_json::Value> = events
        .iter()
        .map(|(_, event)| &event.payload)
        .filter(|payload| payload["type"] == "served_message")
        .collect();
    assert_eq!(
        definitions.len(),
        messages.len(),
        "each distinct message is defined exactly once across all calls"
    );

    let by_hash: std::collections::BTreeMap<&str, &serde_json::Value> = definitions
        .iter()
        .map(|definition| {
            (
                definition["content_hash"].as_str().expect("hash"),
                &definition["message"],
            )
        })
        .collect();
    let last_request = events
        .iter()
        .rev()
        .map(|(_, event)| &event.payload)
        .find(|payload| payload["type"] == "provider_call_request")
        .expect("last request receipt");
    let reconstructed: Vec<serde_json::Value> = last_request["served_context"]["message_lineage"]
        ["messages"]
        .as_array()
        .expect("lineage")
        .iter()
        .map(|entry| (*by_hash[entry["definition_ref"].as_str().expect("definition ref")]).clone())
        .collect();
    assert_eq!(reconstructed, messages);

    crate::event_log::swap_active_event_log(saved_log);
    let _ = super::transcript_ambient::swap_llm_transcript_ambient(saved_ambient);
}
