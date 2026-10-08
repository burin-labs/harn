//! Publication round-trip across live sessions, durable journals and saved views.

use crate::agent_sessions::{
    clear_journal, inject_message, install_journal, journal_store, open_or_create_for_test,
    reset_session_store, settle_assistant_publication, transcript,
};
use crate::schema::json_to_vm_value as json_to_vm;
use crate::value::{VmDictExt, VmValue};

#[tokio::test]
async fn admitted_publication_survives_canonical_journal_hydration() {
    reset_session_store();
    let root = tempfile::tempdir().expect("journal root");
    let session_id = "publication-journal-round-trip";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-first".into(),
        "turn-first".into(),
    )
    .await
    .expect("prepare canonical journal");
    open_or_create_for_test(Some(session_id.into()));
    install_journal(session_id, prepared.state).expect("install journal");
    let mut message = json_to_vm(&serde_json::json!({
        "role":"assistant", "content":"Accepted answer",
    }));
    crate::llm::assistant_publication::defer(
        &mut message,
        &json_to_vm(&serde_json::json!({"_defer_visible":true})),
    );
    inject_message(session_id, message).expect("record actor draft");
    assert_eq!(
        crate::llm::agent_result_projection::last_assistant_text(
            &transcript(session_id).expect("session"),
        ),
        None
    );
    let published = settle_assistant_publication(session_id, true)
        .expect("admit")
        .expect("accepted reply");
    assert_eq!(published.content, "Accepted answer");
    assert_eq!(
        settle_assistant_publication(session_id, true)
            .expect("idempotent settle")
            .map(|reply| reply.content),
        None
    );
    crate::agent_session_journal::flush(session_id)
        .await
        .expect("persist admission");
    let store = journal_store(session_id).expect("installed canonical store");
    let boundaries =
        crate::agent_sessions::canonical_history_boundaries(&store, root.path(), session_id)
            .await
            .expect("acknowledged live reply");
    assert!(boundaries
        .positions
        .iter()
        .any(|position| position.source_event_id == published.source_event_id));
    let replay =
        crate::agent_session_restore::load_canonical_session_replay_from_store(&store, session_id)
            .await
            .expect("replay read")
            .expect("canonical session");
    assert!(replay.events.iter().any(|event| matches!(&event.event,
        crate::agent_events::AgentEvent::AgentMessageChunk {content, history_source_event_id, ..}
        if content == &published.content && history_source_event_id.as_ref() == Some(&published.source_event_id)
    )));
    let run = crate::orchestration::project_run_record_from_session(&store, session_id)
        .await
        .expect("project admitted reply from canonical journal");
    let view = crate::orchestration::build_run_view(&run);
    assert_eq!(view.visible_text.as_deref(), Some("Accepted answer"));
    assert_eq!(view.transcript.message_count, 1, "no public reply copy");
    clear_journal(session_id);
    let hydrated = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-second".into(),
        "turn-second".into(),
    )
    .await
    .expect("hydrate canonical journal");
    assert_eq!(hydrated.transcript.messages.len(), 1);
    let message = json_to_vm(&hydrated.transcript.messages[0]);
    assert_eq!(
        crate::llm::agent_result_projection::visible_assistant_text(&message),
        Some("Accepted answer".into())
    );
    assert_eq!(
        message
            .as_dict()
            .and_then(|dict| dict.get("content"))
            .map(VmValue::display),
        Some("Accepted answer".into())
    );
    drop(hydrated);
    reset_session_store();
}
