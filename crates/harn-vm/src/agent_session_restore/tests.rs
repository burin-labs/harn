use harn_session_store::{
    AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
};

use super::*;

fn transcript_row(kind: &str, role: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "transcript_event": {
            "id": format!("{role}-{text}"),
            "kind": kind,
            "role": role,
            "visibility": "public",
            "text": text,
        }
    })
}

async fn store_with_session(session_id: &str) -> SqliteSessionStore {
    let store = SqliteSessionStore::open_in_memory().expect("in-memory canonical store");
    store
        .create(CreateSession {
            id: Some(session_id.to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    store
}

/// The defect this module exists to close (burin#6267): a session the canonical
/// store holds must be restorable, whether or not the observability event log
/// ever observed it.
#[tokio::test]
async fn a_stored_session_restores_its_public_transcript() {
    let session_id = "01a003d0-1513-7271-90aa-4542d6059498";
    let store = store_with_session(session_id).await;
    for (kind, role, text) in [
        ("message", "user", "add a multiply function"),
        ("message", "assistant", "done, tests pass"),
    ] {
        store
            .append(
                session_id,
                AppendEvent::new(SessionEventKind::Message, transcript_row(kind, role, text))
                    .with_actor(role),
            )
            .await
            .expect("append transcript row");
    }

    let restored = load_canonical_session_replay_events_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session");

    let rendered: Vec<String> = restored
        .iter()
        .map(|entry| match &entry.event {
            AgentEvent::UserMessage { content, .. } => format!("user: {content:?}"),
            AgentEvent::AgentMessageChunk { content, .. } => format!("assistant: {content}"),
            other => format!("other: {other:?}"),
        })
        .collect();
    assert_eq!(restored.len(), 2, "both rows replay: {rendered:?}");
    assert!(
        rendered[0].contains("add a multiply function"),
        "the user turn restores in order: {rendered:?}"
    );
    assert!(
        rendered[1].starts_with("assistant: done, tests pass"),
        "the assistant turn restores in order: {rendered:?}"
    );
    assert!(
        restored[0].event_id < restored[1].event_id,
        "replay keeps stored order: {rendered:?}"
    );
}

/// An id no store holds is the one case that should still fail loudly, so a
/// typo or a stale id stays distinguishable from a real restore.
#[tokio::test]
async fn an_unknown_id_reports_no_session_rather_than_an_empty_one() {
    let store = store_with_session("known-session").await;
    let restored = load_canonical_session_replay_events_from_store(&store, "never-existed")
        .await
        .expect("a missing session is not an error");
    assert!(
        restored.is_none(),
        "an id the store does not hold must be reported absent, not empty"
    );
}

/// A freshly created session with no turns yet is a real session. Reporting it
/// unknown is what made every zero-event launch row unopenable.
#[tokio::test]
async fn a_session_with_no_transcript_yet_is_still_restorable() {
    let store = store_with_session("brand-new").await;
    let restored = load_canonical_session_replay_events_from_store(&store, "brand-new")
        .await
        .expect("restore should not error");
    assert_eq!(
        restored.map(|events| events.len()),
        Some(0),
        "an empty session restores as an empty transcript, not as absent"
    );
}

/// Internal bookkeeping rows (usage checkpoints, audit annotations) are not
/// conversation, and must not surface in a restored transcript.
#[tokio::test]
async fn internal_rows_stay_out_of_the_restored_transcript() {
    let session_id = "with-internals";
    let store = store_with_session(session_id).await;
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                serde_json::json!({
                    "transcript_event": {
                        "kind": "message",
                        "role": "assistant",
                        "visibility": "internal",
                        "text": "scratch reasoning",
                    }
                }),
            ),
        )
        .await
        .expect("append internal row");
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "usage_checkpoint".to_string(),
                },
                serde_json::json!({"usage": {"input_tokens": 72}}),
            ),
        )
        .await
        .expect("append usage row");
    store
        .append(
            session_id,
            AppendEvent::new(
                SessionEventKind::Message,
                transcript_row("message", "assistant", "here is the answer"),
            ),
        )
        .await
        .expect("append public row");

    let restored = load_canonical_session_replay_events_from_store(&store, session_id)
        .await
        .expect("restore should not error")
        .expect("the store knows this session");
    assert_eq!(
        restored.len(),
        1,
        "only the public turn replays: {restored:?}"
    );
    assert!(matches!(
        &restored[0].event,
        AgentEvent::AgentMessageChunk { content, .. } if content == "here is the answer"
    ));
}
