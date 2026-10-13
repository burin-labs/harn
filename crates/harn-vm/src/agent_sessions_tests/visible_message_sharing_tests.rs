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
