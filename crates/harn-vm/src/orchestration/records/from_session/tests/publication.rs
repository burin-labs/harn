//! Saved run views consume the journal's source-bound admission receipts.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore, SessionEventKind};
use serde_json::{json, Value};

use super::super::*;
use super::support::*;

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
        json!({
            "raw_message":raw,
            "transcript_event":envelope,
        }),
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
    let (_, event) = settled.mutation.expect("pending drafts settled");
    AppendEvent::new(
        custom("assistant_publication"),
        json!({
            "transcript_event":crate::llm::vm_value_to_json(&event),
        }),
    )
}

#[tokio::test]
async fn saved_reply_admission_uses_full_session_indices_and_original_events() {
    let store = MemorySessionStore::default();
    let id = store.create(CreateSession::default()).await.unwrap().id;
    let old = draft("Previous run answer");
    store.append(&id, message("old", &old)).await.unwrap();
    store
        .append(&id, settlement(std::slice::from_ref(&old), true))
        .await
        .unwrap();
    let mut published_old = old;
    published_old["harn_assistant_publication"] = json!("published");
    store
        .append(
            &id,
            AppendEvent::new(
                SessionEventKind::Compaction,
                json!({
                    "messages":[published_old.clone()], "source_event_ids":["old"],
                }),
            ),
        )
        .await
        .unwrap();
    store.append(&id, run_started()).await.unwrap();
    let user = json!({"role":"user", "content":"New request"});
    let rejected = draft("Rejected draft");
    let accepted = draft("Admitted answer");
    for (source, raw) in [
        ("user", &user),
        ("rejected", &rejected),
        ("accepted", &accepted),
    ] {
        store.append(&id, message(source, raw)).await.unwrap();
    }
    let receipt = settlement(&[published_old, user, rejected, accepted], true);
    store.append(&id, receipt.clone()).await.unwrap();
    store.append(&id, receipt).await.unwrap();
    // Later compaction must not erase the run's admitted original event.
    store
        .append(
            &id,
            AppendEvent::new(
                SessionEventKind::Compaction,
                json!({
                    "messages":[], "source_event_ids":[],
                }),
            ),
        )
        .await
        .unwrap();
    let run = project_run_record_from_session(&store, &id).await.unwrap();
    let view = crate::orchestration::records::build_run_view(&run);
    assert_eq!(view.visible_text.as_deref(), Some("Admitted answer"));
    assert_eq!(view.transcript.message_count, 2);
    let transcript = run.transcript.unwrap();
    let text = transcript.to_string();
    assert!(!text.contains("Previous run answer"));
    assert!(!text.contains("Rejected draft"));
    assert_eq!(
        transcript["events"][1]["id"], "accepted",
        "original event identity"
    );
}

#[tokio::test]
async fn saved_reply_rejects_withheld_stale_and_malformed_receipts() {
    for (admitted, changed_source, malformed) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let store = MemorySessionStore::default();
        let id = store.create(CreateSession::default()).await.unwrap().id;
        let original = draft("Unapproved answer");
        let mut receipt = settlement(std::slice::from_ref(&original), admitted);
        let raw = if changed_source {
            draft("Different source")
        } else {
            original
        };
        store.append(&id, message("draft", &raw)).await.unwrap();
        if malformed {
            receipt.payload["transcript_event"]["metadata"]["changes"] = json!("broken");
        }
        store.append(&id, receipt).await.unwrap();
        let run = project_run_record_from_session(&store, &id).await.unwrap();
        let view = crate::orchestration::records::build_run_view(&run);
        assert_eq!(view.visible_text, None);
        assert_eq!(view.transcript.message_count, 0);
    }
}
