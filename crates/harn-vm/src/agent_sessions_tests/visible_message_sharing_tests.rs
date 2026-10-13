use super::*;

fn content(message: &VmValue) -> Arc<Vec<VmValue>> {
    match message.as_dict().and_then(|dict| dict.get("content")) {
        Some(VmValue::List(blocks)) => blocks.clone(),
        other => panic!("expected list content, got {other:?}"),
    }
}

/// The visible projection reads the durable transcript in place. A JSON round
/// trip of every message would rebuild each content list, so shared identity
/// is the falsifier for reintroducing a per-turn whole-transcript conversion.
#[test]
fn visible_messages_share_durable_message_values() {
    let session_id = open_or_create(Some(format!(
        "visible-messages-share-{}",
        uuid::Uuid::now_v7()
    )));
    for (role, text) in [("user", "read the file"), ("assistant", "done")] {
        let message = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": role,
            "content": [{"type": "text", "text": text}],
        }));
        inject_message(&session_id, message).expect("inject");
    }
    let durable = match transcript(&session_id)
        .as_ref()
        .and_then(|transcript| transcript.as_dict())
        .and_then(|transcript| transcript.get("messages"))
    {
        Some(VmValue::List(messages)) => messages.as_ref().clone(),
        other => panic!("expected durable messages, got {other:?}"),
    };
    assert_eq!(durable.len(), 2);

    let visible =
        crate::llm::agent_session_host::visible_message_values(&session_id, durable.clone());
    assert_eq!(visible.len(), durable.len());
    for (position, (visible, durable)) in visible.iter().zip(&durable).enumerate() {
        assert!(
            Arc::ptr_eq(&content(visible), &content(durable)),
            "message {position} content was rebuilt instead of shared"
        );
        let lineage = crate::llm::helpers::vm_value_to_json(
            visible
                .as_dict()
                .and_then(|dict| dict.get(crate::llm::message_lineage::MESSAGE_LINEAGE_KEY))
                .expect("lineage attached"),
        );
        assert_eq!(lineage["source_message_index"], position);
        assert_eq!(lineage["projection"]["policy"], "raw");
    }
}

/// Each turn re-projects the whole durable transcript. A message that was
/// projected at the same position on the previous turn comes back as the
/// same shared value, so only appended messages get their lineage attached
/// and options building downstream sees unchanged messages by identity.
#[test]
fn visible_messages_reproject_only_appended_messages() {
    let session_id = open_or_create(Some(format!(
        "visible-messages-reproject-{}",
        uuid::Uuid::now_v7()
    )));
    let durable = |session_id: &str| match transcript(session_id)
        .as_ref()
        .and_then(|transcript| transcript.as_dict())
        .and_then(|transcript| transcript.get("messages"))
    {
        Some(VmValue::List(messages)) => messages.as_ref().clone(),
        other => panic!("expected durable messages, got {other:?}"),
    };
    let turns = 10;
    let mut projected = 0;
    let mut previous: Vec<VmValue> = Vec::new();
    for turn in 0..turns {
        let message = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": if turn % 2 == 0 { "user" } else { "assistant" },
            "content": format!("turn {turn}"),
        }));
        inject_message(&session_id, message).expect("inject");
        let messages = durable(&session_id);
        let before = crate::llm::agent_session_host::visible_message_projections();
        let visible =
            crate::llm::agent_session_host::visible_message_values(&session_id, messages.clone());
        projected += crate::llm::agent_session_host::visible_message_projections() - before;
        for (position, (visible, earlier)) in visible.iter().zip(&previous).enumerate() {
            let (VmValue::Dict(visible), VmValue::Dict(earlier)) = (visible, earlier) else {
                panic!("visible messages are dicts");
            };
            assert!(
                Arc::ptr_eq(visible, earlier),
                "turn {turn}: unchanged message {position} was re-projected"
            );
        }
        previous = visible;
    }
    // A fresh projection of rebuilt (unshared) messages matches the memoized
    // one, so reuse never changes what the model sees.
    let fresh = crate::llm::agent_session_host::visible_messages_with_lineage(
        &session_id,
        durable(&session_id)
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
    );
    let memoized: Vec<_> = previous
        .iter()
        .map(crate::llm::helpers::vm_value_to_json)
        .collect();
    assert_eq!(
        memoized, fresh,
        "memoized projection differs from a fresh one"
    );
    assert!(projected >= turns, "every message is projected once");
    assert!(
        projected <= turns,
        "{projected} projections for {turns} messages: the transcript is re-projected every turn"
    );
}
