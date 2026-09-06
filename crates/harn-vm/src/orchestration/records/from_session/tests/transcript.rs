//! Readable-transcript projection: message order and payload privacy.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore, SessionEventKind};
use serde_json::json;

use super::super::*;
use super::support::*;

#[tokio::test]
async fn readable_messages_project_in_order_without_private_payload_fields() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let transcript = run.transcript.as_ref().expect("readable transcript");
    assert_eq!(transcript["_type"], "transcript");
    assert_eq!(transcript["id"], id);
    assert_eq!(transcript["source"], PROJECTION_SOURCE);
    assert_eq!(
        transcript["messages"],
        json!([
            {"role": "user", "content": "Migrate the three unit test files."},
            {"role": "assistant", "content": "I inspected the three requested files."},
        ])
    );
    assert_eq!(transcript["events"][0]["kind"], "message");
    assert_eq!(transcript["events"][0]["visibility"], "public");
    assert_eq!(transcript["events"][1]["id"], "assistant-visible");
    assert_eq!(transcript["events"][1]["blocks"][0]["type"], "output_text");
    assert!(transcript.pointer("/events/0/metadata").is_none());
    assert!(transcript.pointer("/events/0/raw_message").is_none());

    let view = crate::orchestration::records::build_run_view(&run);
    assert_eq!(view.transcript.message_count, 2);
    assert_eq!(view.transcript.event_count, 2);
    assert_eq!(
        view.visible_text.as_deref(),
        Some("I inspected the three requested files.")
    );
}

#[tokio::test]
async fn private_and_malformed_messages_are_not_projected() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        user_message("public request"),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "assistant",
                "visibility": "private",
                "text": "private reasoning",
            }}),
        ),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "future-role",
                "visibility": "public",
                "text": "unknown speaker",
            }}),
        ),
        AppendEvent::new(
            SessionEventKind::Message,
            json!({"transcript_event": {
                "kind": "message",
                "role": "assistant",
                "visibility": "public",
            }}),
        ),
    ] {
        store.append(&id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    let transcript = run.transcript.expect("public transcript");
    assert_eq!(
        transcript["messages"],
        json!([{"role": "user", "content": "public request"}])
    );
    let encoded = transcript.to_string();
    assert!(!encoded.contains("private reasoning"));
    assert!(!encoded.contains("unknown speaker"));
}
