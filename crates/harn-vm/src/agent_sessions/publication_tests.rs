use super::*;
use crate::schema::json_to_vm_value as json_to_vm;
use crate::value::VmDictExt;

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
    assert_eq!(
        settle_assistant_publication(session_id, true).expect("admit"),
        Some("Accepted answer".into())
    );
    assert_eq!(
        settle_assistant_publication(session_id, true).expect("idempotent settle"),
        None
    );
    crate::agent_session_journal::flush(session_id)
        .await
        .expect("persist admission");
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
        crate::llm::agent_session_host::dict_get(&message, "content").map(VmValue::display),
        Some("Accepted answer".into())
    );
    drop(hydrated);
    reset_session_store();
}
