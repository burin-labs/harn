use crate::llm::reasoning_receipt::{record, scope_request_receipts, RequestReceipts};

fn emit_receipt() {
    record(
        "gemini",
        "receipt-proof",
        "gemini_interactions",
        &crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::XHigh,
        },
        &["generation_config.thinking_level"],
        &serde_json::json!({"generation_config": {"thinking_level": "high"}}),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_thread_handoff_keeps_the_owning_durable_journal() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().unwrap();
    let session_id = "reasoning-journal-thread-handoff";
    let mut options = crate::value::DictMap::new();
    crate::value::VmDictExt::put_str(&mut options, "root", root.path().to_string_lossy());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "reasoning-handoff-run".into(),
        "reasoning-handoff-turn".into(),
    )
    .await
    .unwrap();
    crate::agent_sessions::open_or_create_for_test(Some(session_id.into()));
    crate::agent_sessions::install_journal(session_id, prepared.state).unwrap();
    let store = crate::agent_sessions::journal_store(session_id).unwrap();
    let receipts = RequestReceipts::for_journal(Some(session_id), "handoff-call");
    // A measured same-thread receipt establishes the journal and reporting path.
    scope_request_receipts(receipts.clone(), async {
        emit_receipt();
    })
    .await;
    assert!(receipts.take_journal_error().is_none());
    crate::agent_session_journal::flush(session_id)
        .await
        .unwrap();
    let events = crate::stdlib::session_store::read_all_events(&store, session_id)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    let provider_receipts = receipts.clone();
    let runtime = tokio::runtime::Handle::current();
    crate::runtime_stack::spawn(move || {
        runtime.block_on(scope_request_receipts(provider_receipts, async {
            emit_receipt();
        }));
    })
    .join()
    .unwrap();
    assert_eq!(receipts.take_journal_error(), None);
    crate::agent_session_journal::flush(session_id)
        .await
        .unwrap();
    let events = crate::stdlib::session_store::read_all_events(&store, session_id)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event.kind.discriminator() == "reasoning_receipt"));
    crate::agent_sessions::append_terminal_event_once(
        session_id,
        crate::llm::helpers::transcript_event(
            "agent_run_terminal",
            "system",
            "internal",
            "completed",
            None,
        ),
    )
    .unwrap();
    scope_request_receipts(receipts.clone(), async {
        emit_receipt();
    })
    .await;
    assert_eq!(
        receipts.take_journal_error().as_deref(),
        Some("agent transcript audit writer: journal is closed"),
    );
    crate::agent_session_journal::flush(session_id)
        .await
        .unwrap();
    let events = crate::stdlib::session_store::read_all_events(&store, session_id)
        .await
        .unwrap();
    assert_eq!(
        events.len(),
        3,
        "late provider facts must not follow the terminal"
    );
    assert_eq!(
        events.last().unwrap().kind.discriminator(),
        "agent_run_terminal"
    );
    crate::agent_sessions::reset_session_store();
}
