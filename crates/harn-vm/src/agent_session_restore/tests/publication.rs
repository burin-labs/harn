use serde_json::{json, Value};

use super::*;

fn draft(text: &str) -> Value {
    json!({"role":"assistant", "content":text, "harn_assistant_publication":"pending"})
}

fn message(source: &str, raw: &Value) -> AppendEvent {
    let envelope =
        crate::llm::helpers::transcript_event_from_message(&crate::schema::json_to_vm_value(raw));
    let mut envelope = crate::llm::vm_value_to_json(&envelope);
    envelope["id"] = json!(source);
    let mut event = AppendEvent::new(
        SessionEventKind::Message,
        json!({"raw_message":raw, "transcript_event":envelope}),
    );
    event
        .headers
        .insert("source_event_id".into(), source.into());
    event
}

fn settlement(messages: &[Value], admitted: bool) -> AppendEvent {
    let settled = crate::llm::assistant_publication::settle(
        &crate::schema::json_to_vm_value(&json!({"messages":messages})),
        admitted,
    );
    let (_, event) = settled.mutation.expect("pending fixture settled");
    let source_event_id = event.as_dict().unwrap().get("id").unwrap().display();
    let mut receipt = AppendEvent::new(
        SessionEventKind::Custom {
            custom_type: "assistant_publication".into(),
        },
        json!({"transcript_event":crate::llm::vm_value_to_json(&event)}),
    );
    receipt
        .headers
        .insert("source_event_id".into(), source_event_id);
    receipt
}

#[tokio::test]
async fn displayed_answer_identity_forks_through_admission_and_excludes_later_messages() {
    let id = "branch-published-answer";
    let store = store_with_session(id).await;
    let raw = draft("Accepted answer");
    store
        .append(id, message("private-draft", &raw))
        .await
        .unwrap();
    let receipt = settlement(std::slice::from_ref(&raw), true);
    let receipt_id = receipt.headers["source_event_id"].clone();
    store.append(id, receipt).await.unwrap();
    store
        .append(
            id,
            message(
                "later-user",
                &json!({"role":"user","content":"Later request"}),
            ),
        )
        .await
        .unwrap();
    let replay = load_canonical_session_replay_from_store(&store, id)
        .await
        .unwrap()
        .unwrap();
    let answer_id = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            AgentEvent::AgentMessageChunk {
                content,
                history_source_event_id,
                ..
            } if content == "Accepted answer" => history_source_event_id.clone(),
            _ => None,
        })
        .expect("restored visible answer has a canonical source identity");
    assert_eq!(answer_id, receipt_id);
    let boundaries = store.history_boundaries(id).await.unwrap();
    let answer = boundaries
        .positions
        .iter()
        .find(|position| position.source_event_id == answer_id)
        .expect("published answer is acknowledged");
    store
        .fork(id, answer.boundary.clone(), Some("answer-child".into()))
        .await
        .unwrap();
    let child = load_canonical_session_replay_from_store(&store, "answer-child")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.events.len(), 1, "later request is excluded");
    assert!(
        matches!(&child.events[0].event, AgentEvent::AgentMessageChunk {content, ..} if content == "Accepted answer")
    );

    let private = boundaries
        .positions
        .iter()
        .find(|position| position.source_event_id == "private-draft")
        .unwrap();
    store
        .fork(id, private.boundary.clone(), Some("private-child".into()))
        .await
        .unwrap();
    let private_child = load_canonical_session_replay_from_store(&store, "private-child")
        .await
        .unwrap()
        .unwrap();
    assert!(
        private_child.events.is_empty(),
        "draft identity is not a valid substitute for the visible answer"
    );
}

#[tokio::test]
async fn receipt_on_later_page_restores_original_reply_after_compaction() {
    let id = "cross-page-publication";
    let store = store_with_session(id).await;
    let raw = draft("Admitted answer");
    store.append(id, message("original", &raw)).await.unwrap();
    for _ in 1..RESTORE_PAGE {
        store
            .append(id, AppendEvent::new(SessionEventKind::Receipt, json!({})))
            .await
            .unwrap();
    }
    let receipt = settlement(std::slice::from_ref(&raw), true);
    store.append(id, receipt.clone()).await.unwrap();
    store.append(id, receipt).await.unwrap();
    store
        .append(
            id,
            AppendEvent::new(
                SessionEventKind::Compaction,
                json!({"messages":[], "source_event_ids":[]}),
            ),
        )
        .await
        .unwrap();
    store
        .append(
            id,
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "agent_run_started".into(),
                },
                json!({}),
            ),
        )
        .await
        .unwrap();
    let tip = store
        .append(id, AppendEvent::new(SessionEventKind::Receipt, json!({})))
        .await
        .unwrap();
    let replay = load_canonical_session_replay_from_store(&store, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay.last_event_id, Some(tip.event_id));
    assert_eq!(
        replay.events.len(),
        1,
        "duplicate receipt cannot duplicate the reply"
    );
    assert_eq!(replay.events[0].event_id, 1, "original draft identity");
    assert!(
        matches!(&replay.events[0].event, AgentEvent::AgentMessageChunk {content, ..}
        if content == "Admitted answer")
    );
}

#[tokio::test]
async fn invalid_or_absent_admission_never_replays_public_draft_prose() {
    for (admitted, changed, malformed, missing, source_missing, unknown) in [
        (false, false, false, false, false, false),
        (true, true, false, false, false, false),
        (true, false, true, false, false, false),
        (true, false, false, true, false, false),
        (true, false, false, false, true, false),
        (true, false, false, false, false, true),
    ] {
        let id = "rejected-publication";
        let store = store_with_session(id).await;
        let raw = draft("Private draft");
        let mut actual = raw.clone();
        if changed {
            actual["content"] = json!("Replaced draft");
        }
        if unknown {
            actual["harn_assistant_publication"] = json!("unknown");
        }
        let mut row = message("original", &actual);
        // Even damaged public visibility must not override actor admission.
        row.payload["transcript_event"]["visibility"] = json!("public");
        if source_missing {
            row.headers.clear();
        }
        store.append(id, row).await.unwrap();
        if !missing {
            let mut receipt = settlement(std::slice::from_ref(&raw), admitted);
            if malformed {
                receipt.payload["transcript_event"]["metadata"]["schema"] = json!("unknown");
            }
            store.append(id, receipt).await.unwrap();
        }
        let replay = load_canonical_session_replay_from_store(&store, id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            replay.events.is_empty(),
            "invalid admission must stay private"
        );
    }
}

#[tokio::test]
async fn receipt_after_captured_tip_cannot_admit_that_prefix() {
    let id = "publication-after-checkpoint";
    let store = store_with_session(id).await;
    let raw = draft("Later admitted answer");
    store.append(id, message("original", &raw)).await.unwrap();
    let captured = store.describe(id).await.unwrap();
    store
        .append(id, settlement(std::slice::from_ref(&raw), true))
        .await
        .unwrap();
    let replay = read_canonical_session_prefix(&store, id, captured)
        .await
        .unwrap();
    assert_eq!(replay.last_event_id, Some(1));
    assert!(
        replay.events.is_empty(),
        "later receipt is outside captured prefix"
    );
    let current = load_canonical_session_replay_from_store(&store, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current.events.len(),
        1,
        "the control must have a real later admission"
    );
}

#[tokio::test]
async fn compaction_and_removal_keep_the_journal_owners_receipt_indices() {
    let id = "publication-after-compaction";
    let store = store_with_session(id).await;
    let old = draft("Old private draft");
    store.append(id, message("old", &old)).await.unwrap();
    let user = json!({"role":"user", "content":"Compacted request"});
    store
        .append(
            id,
            AppendEvent::new(
                SessionEventKind::Compaction,
                json!({"messages":[user.clone()], "source_event_ids":["user"]}),
            ),
        )
        .await
        .unwrap();
    let raw = draft("New admitted answer");
    store.append(id, message("new", &raw)).await.unwrap();
    store
        .append(id, settlement(&[user, raw], true))
        .await
        .unwrap();
    store
        .append(
            id,
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "message_removed".into(),
                },
                json!({"source_event_id":"new"}),
            ),
        )
        .await
        .unwrap();
    let replay = load_canonical_session_replay_from_store(&store, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_id, 3);
    assert!(
        matches!(&replay.events[0].event, AgentEvent::AgentMessageChunk {content, ..}
        if content == "New admitted answer")
    );
}
