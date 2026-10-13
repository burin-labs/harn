//! Prefix-chained message lineage across a run with compaction and an edit.

use std::collections::HashMap;

use crate::event_log::{EventLog, Topic, HARN_LLM_TRANSCRIPT_TOPIC};
use crate::llm::served_lineage::{
    reconstruct_messages, FullLineageReason, LineageBase, LineageChainError, LineageIndex,
};

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
    opts.session_id = Some("lineage-chain-session".to_string());
    opts
}

fn message(role: &str, content: &str) -> serde_json::Value {
    serde_json::json!({"role": role, "content": content})
}

/// Each step is the full array served to one call. Steps 3 and 5 are not
/// continuations of the call before them: a compaction replaces the head of
/// the history with a summary, and an edit rewrites a retained message.
fn served_sequence() -> Vec<Vec<serde_json::Value>> {
    let u0 = message("user", "read the files");
    let a1 = message("assistant", "reading one");
    let t1 = message("tool", "file one");
    let a2 = message("assistant", "reading two");
    let t2 = message("tool", "file two");
    let summary = message("system", "condensed: read files one and two");
    let a3 = message("assistant", "continuing");
    let t2_edited = message("tool", "file two (edited)");
    let a4 = message("assistant", "done");
    vec![
        vec![u0.clone()],
        vec![u0.clone(), a1.clone(), t1.clone()],
        vec![u0, a1, t1, a2, t2.clone()],
        vec![summary.clone(), t2.clone()],
        vec![summary.clone(), t2, a3.clone()],
        vec![summary.clone(), t2_edited.clone(), a3.clone()],
        vec![summary, t2_edited, a3, a4],
    ]
}

#[test]
fn chained_lineage_reconstructs_every_full_list_and_fails_closed_on_a_broken_chain() {
    let _guard = crate::llm::env_guard();
    let saved_ambient = super::transcript_ambient::swap_llm_transcript_ambient(Default::default());
    let saved_log = crate::event_log::swap_active_event_log(None);
    let log = crate::event_log::install_memory_for_current_thread(256);

    let sequence = served_sequence();
    for (call, messages) in sequence.iter().enumerate() {
        super::dump_llm_request(call, &format!("call-{call}"), "native", &request(messages))
            .expect("observable request");
    }

    let topic = Topic::new(HARN_LLM_TRANSCRIPT_TOPIC).expect("transcript topic");
    let events: Vec<serde_json::Value> =
        futures::executor::block_on(log.read_range(&topic, None, usize::MAX))
            .expect("read transcript topic")
            .into_iter()
            .map(|(_, event)| event.payload)
            .collect();
    crate::event_log::swap_active_event_log(saved_log);
    let _ = super::transcript_ambient::swap_llm_transcript_ambient(saved_ambient);

    let requests: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| event["type"] == "provider_call_request")
        .collect();
    assert_eq!(requests.len(), sequence.len());
    let definitions: HashMap<String, serde_json::Value> = events
        .iter()
        .filter(|event| event["type"] == "served_message")
        .map(|event| {
            (
                event["content_hash"].as_str().expect("hash").to_string(),
                event["message"].clone(),
            )
        })
        .collect();

    // The writer chose a continuation exactly where the served array extends
    // the previous one, and a full list everywhere else.
    let bases: Vec<LineageBase> = requests
        .iter()
        .map(|request| {
            crate::llm::served_lineage::request_lineage(request)
                .expect("v2 lineage")
                .expect("request lineage")
                .base
        })
        .collect();
    let full = |reason| LineageBase::Full { reason };
    assert_eq!(bases[0], full(FullLineageReason::NoPredecessor));
    assert_eq!(
        bases[3],
        full(FullLineageReason::PrefixDiverged),
        "compaction"
    );
    assert_eq!(bases[5], full(FullLineageReason::PrefixDiverged), "edit");
    for call in [1, 2, 4, 6] {
        assert!(
            matches!(bases[call], LineageBase::Continuation { .. }),
            "call {call} extends its predecessor: {:?}",
            bases[call]
        );
    }
    let listed: usize = requests
        .iter()
        .map(|request| {
            request["served_context"]["message_lineage"]["messages"]
                .as_array()
                .expect("listed entries")
                .len()
        })
        .sum();
    let served: usize = sequence.iter().map(Vec::len).sum();
    assert_eq!(listed, 1 + 2 + 2 + 2 + 1 + 3 + 1);
    assert!(
        listed < served,
        "chaining lists {listed} of {served} entries"
    );

    // Every request reconstructs to the full list a v1 receipt carried, and
    // its message bodies verify against the whole-array hash.
    let index = LineageIndex::from_transcript_events(&events).expect("lineage index");
    for (call, (request, messages)) in requests.iter().zip(&sequence).enumerate() {
        let lineage = index
            .reconstruct_request(request)
            .expect("chain resolves")
            .expect("request lineage");
        let expected: Vec<(usize, String, String)> = messages
            .iter()
            .enumerate()
            .map(|(position, message)| {
                (
                    position,
                    message["role"].as_str().expect("role").to_string(),
                    crate::llm::content_hash::stable_redacted_json_hash(message),
                )
            })
            .collect();
        let actual: Vec<(usize, String, String)> = lineage
            .iter()
            .map(|entry| {
                (
                    entry.position,
                    entry.role.clone(),
                    entry.message_content_hash.clone(),
                )
            })
            .collect();
        assert_eq!(actual, expected, "call {call} lineage");
        let hash = request["served_context"]["messages_content_hash"]
            .as_str()
            .expect("whole-array hash");
        assert_eq!(
            reconstruct_messages(&lineage, &definitions, hash).expect("verified bodies"),
            *messages,
            "call {call} bodies"
        );
    }

    // Negative controls. Dropping a receipt breaks every continuation that
    // depends on it with an explicit error; chains rooted after it survive.
    let without = |dropped: usize| {
        let call_id = format!("call-{dropped}");
        LineageIndex::from_transcript_events(events.iter().filter(|event| {
            !(event["type"] == "provider_call_request" && event["call_id"] == call_id)
        }))
        .expect("lineage index")
    };
    let broken = without(1);
    let error = broken
        .reconstruct_request(requests[2])
        .expect_err("missing predecessor must not yield a partial list");
    assert!(
        matches!(error, LineageChainError::MissingPredecessor { .. }),
        "{error}"
    );
    assert!(broken.reconstruct_request(requests[4]).is_ok());
    assert!(matches!(
        without(5).reconstruct_request(requests[6]),
        Err(LineageChainError::MissingPredecessor { .. })
    ));
    // A body that does not match the served array fails verification.
    let mut swapped = definitions;
    let t1_hash = crate::llm::content_hash::stable_redacted_json_hash(&sequence[1][2]);
    swapped.insert(t1_hash, message("tool", "forged"));
    let lineage = index
        .reconstruct_request(requests[2])
        .expect("chain resolves")
        .expect("lineage");
    let hash = requests[2]["served_context"]["messages_content_hash"]
        .as_str()
        .expect("hash");
    assert!(matches!(
        reconstruct_messages(&lineage, &swapped, hash),
        Err(LineageChainError::SequenceHashMismatch { .. })
    ));
}
