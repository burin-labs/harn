use super::*;
use harn_session_store::{AppendEvent, CreateSession, SessionEventKind, SqliteSessionStore};
use serde_json::json;

async fn append_message(
    store: &SqliteSessionStore,
    session: &str,
    source: Option<&str>,
    message: serde_json::Value,
) {
    let mut event = AppendEvent::new(SessionEventKind::Message, json!({"raw_message": message}));
    if let Some(source) = source {
        event
            .headers
            .insert("source_event_id".into(), source.into());
    }
    store
        .append(session, event)
        .await
        .expect("append actual message");
}

#[tokio::test]
async fn reader_replays_owner_settlement_without_leaking_prose_or_guessing_call_roles() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(root.path().join("journal.sqlite")).unwrap();
    store
        .create(CreateSession {
            id: Some("actual-run-session".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let messages = vec![
        json!({"role":"assistant", "content":"PRIVATE actor draft", "harn_assistant_publication":"pending", "_harn":{"kind":"assistant", "call_role":"agent.main", "call_stage":null}}),
        json!({"role":"assistant", "content":"PRIVATE wrap-up", "harn_assistant_publication":"pending", "_harn":{"kind":"assistant", "call_role":"agent.main", "call_stage":"wrapup"}}),
    ];
    for (index, message) in messages.iter().enumerate() {
        append_message(
            &store,
            "actual-run-session",
            Some(&format!("source-{index}")),
            message.clone(),
        )
        .await;
    }
    let snapshot = crate::stdlib::json_to_vm_value(&json!({"messages": messages}));
    let settlement = crate::llm::assistant_publication::settle(&snapshot, true);
    let (_, event) = settlement
        .mutation
        .expect("owner emitted actual settlement");
    store
        .append(
            "actual-run-session",
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "assistant_publication".into(),
                },
                json!({"transcript_event": crate::llm::helpers::vm_value_to_json(&event)}),
            ),
        )
        .await
        .unwrap();
    let evidence = read_session_publication_evidence(&store, "actual-run-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(evidence.session_id, "actual-run-session");
    assert_eq!(evidence.stored_event_count, 3);
    assert_eq!(evidence.assistant_message_count, 2);
    assert_eq!(evidence.pending_count, 0);
    assert_eq!(evidence.published_count, 1);
    assert_eq!(evidence.withheld_count, 1);
    assert_eq!(evidence.missing_source_count, 0);
    assert_eq!(evidence.missing_call_role_count, 0);
    assert_eq!(evidence.missing_call_stage_metadata_count, 0);
    assert_eq!(evidence.unrecognized_call_stage_metadata_count, 0);
    assert!(evidence.records[0].stage_metadata_present);
    assert_eq!(evidence.records[0].call_stage, None);
    assert_eq!(evidence.records[1].call_stage.as_deref(), Some("wrapup"));
    assert_eq!(
        evidence.records[0].source_event_id.as_deref(),
        Some("source-0")
    );
    assert_eq!(evidence.records[0].call_role.as_deref(), Some("agent.main"));
    assert_eq!(
        evidence.records[0].disposition,
        Some(AssistantPublicationDisposition::Withheld)
    );
    assert_eq!(
        evidence.records[1].disposition,
        Some(AssistantPublicationDisposition::Published)
    );
    assert!(!serde_json::to_string(&evidence)
        .unwrap()
        .contains("PRIVATE"));
    assert!(
        read_session_publication_evidence(&store, "wrong-run-session")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn missing_unknown_and_pending_are_observed_without_silently_publishing() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(root.path().join("journal.sqlite")).unwrap();
    store
        .create(CreateSession {
            id: Some("coverage".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for disposition in [None, Some("unknown"), Some("pending")] {
        let mut message = json!({"role":"assistant", "content":"PRIVATE"});
        if let Some(disposition) = disposition {
            message["harn_assistant_publication"] = json!(disposition);
        }
        append_message(&store, "coverage", None, message).await;
    }
    append_message(&store, "coverage", None, json!({"role":"assistant", "content":"PRIVATE", "_harn":{"kind":"assistant", "call_stage":17}})).await;
    let evidence = read_session_publication_evidence(&store, "coverage")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(evidence.assistant_message_count, 4);
    assert_eq!(evidence.missing_source_count, 4);
    assert_eq!(evidence.missing_call_role_count, 4);
    assert_eq!(evidence.missing_call_stage_metadata_count, 3);
    assert_eq!(evidence.unrecognized_call_stage_metadata_count, 1);
    assert_eq!(evidence.missing_metadata_count, 2);
    assert_eq!(evidence.unrecognized_metadata_count, 1);
    assert_eq!(evidence.pending_count, 1);
    assert_eq!(evidence.published_count, 0);
    assert_eq!(
        evidence.records[2].disposition,
        Some(AssistantPublicationDisposition::Pending)
    );
    assert!(read_session_publication_evidence(&store, " ")
        .await
        .is_err());
    let absent_root = root.path().join("never-created");
    assert!(
        read_project_session_publication_evidence(&absent_root, "coverage")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!absent_root.exists());
}

#[tokio::test]
async fn bookkeeping_retains_source_coverage_without_hiding_unknown_actor_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(root.path().join("journal.sqlite")).unwrap();
    store
        .create(CreateSession {
            id: Some("bookkeeping".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    append_message(
        &store,
        "bookkeeping",
        Some("model-source"),
        json!({
            "role":"assistant", "content":"PRIVATE model draft",
            "harn_assistant_publication":"withheld",
            "_harn":{"kind":"assistant", "call_role":"agent.main", "call_stage":"work"}
        }),
    )
    .await;
    let mut bookkeeping = json!({"role":"assistant", "content":"PRIVATE withdrawal"});
    bookkeeping[crate::llm::agent_result_projection::BOOKKEEPING_TURN_KEY] = json!(true);
    append_message(
        &store,
        "bookkeeping",
        Some("withdrawal-source"),
        bookkeeping,
    )
    .await;
    let measured = read_session_publication_evidence(&store, "bookkeeping")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(measured.stored_event_count, 2);
    assert_eq!(measured.assistant_message_count, 2);
    assert_eq!(measured.bookkeeping_count, 1);
    assert_eq!(measured.missing_source_count, 0);
    assert_eq!(measured.missing_call_role_count, 0);
    assert_eq!(measured.missing_call_stage_metadata_count, 0);
    assert_eq!(measured.missing_metadata_count, 0);
    assert_eq!(measured.withheld_count, 1);
    assert_eq!(measured.published_count, 0);
    let withdrawal = &measured.records[1];
    assert_eq!(
        withdrawal.origin,
        AssistantPublicationOrigin::HarnessBookkeeping
    );
    assert_eq!(
        withdrawal.source_event_id.as_deref(),
        Some("withdrawal-source")
    );
    assert_eq!(withdrawal.call_role, None);
    assert_eq!(withdrawal.call_stage, None);
    assert!(!withdrawal.stage_metadata_present);
    assert!(!withdrawal.metadata_present);
    assert_eq!(withdrawal.disposition, None);

    append_message(
        &store,
        "bookkeeping",
        Some("unknown-actor"),
        json!({
            "role":"assistant", "content":"PRIVATE unmeasured actor"
        }),
    )
    .await;
    let mut malformed = json!({"role":"assistant", "content":"PRIVATE malformed flag"});
    malformed[crate::llm::agent_result_projection::BOOKKEEPING_TURN_KEY] = json!("true");
    append_message(&store, "bookkeeping", Some("malformed-flag"), malformed).await;
    let unknown = read_session_publication_evidence(&store, "bookkeeping")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unknown.stored_event_count, 4);
    assert_eq!(unknown.assistant_message_count, 4);
    assert_eq!(unknown.records.len(), 4);
    assert_eq!(unknown.bookkeeping_count, 1);
    assert_eq!(unknown.missing_source_count, 0);
    assert_eq!(unknown.missing_call_role_count, 2);
    assert_eq!(unknown.missing_call_stage_metadata_count, 2);
    assert_eq!(unknown.missing_metadata_count, 2);
    assert_eq!(unknown.withheld_count, 1);
    for record in &unknown.records[2..] {
        assert_eq!(record.origin, AssistantPublicationOrigin::UnmarkedAssistant);
        assert_eq!(record.disposition, None);
    }
    assert!(!serde_json::to_string(&unknown).unwrap().contains("PRIVATE"));
}

#[tokio::test]
async fn incomplete_changing_and_wrong_session_reads_fail_coverage() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(root.path().join("journal.sqlite")).unwrap();
    store
        .create(CreateSession {
            id: Some("coverage".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    append_message(
        &store,
        "coverage",
        Some("known-source"),
        json!({"role":"assistant", "content":"PRIVATE", "harn_assistant_publication":"pending"}),
    )
    .await;
    let before = store.describe("coverage").await.unwrap();
    let events = store.read_all("coverage").await.unwrap();
    assert_eq!(events.len(), 1, "known nonempty read qualifies the control");
    assert!(validate_coverage("coverage", &before, &before, &events).is_ok());
    assert!(validate_coverage("coverage", &before, &before, &[]).is_err());
    assert!(validate_coverage("wrong-session", &before, &before, &events).is_err());
    append_message(
        &store,
        "coverage",
        Some("later-source"),
        json!({"role":"user", "content":"later"}),
    )
    .await;
    let after = store.describe("coverage").await.unwrap();
    assert!(validate_coverage("coverage", &before, &after, &events).is_err());
}

#[tokio::test]
async fn a_stale_owner_receipt_cannot_publish_a_changed_draft() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(root.path().join("journal.sqlite")).unwrap();
    store
        .create(CreateSession {
            id: Some("stale".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let message = json!({"role":"assistant", "content":"PRIVATE original", "harn_assistant_publication":"pending"});
    let settlement = crate::llm::assistant_publication::settle(
        &crate::stdlib::json_to_vm_value(&json!({"messages":[message.clone()]})),
        true,
    );
    let (_, receipt) = settlement.mutation.unwrap();
    let mut changed = message;
    changed["content"] = json!("PRIVATE changed");
    append_message(&store, "stale", Some("actual-source"), changed).await;
    store
        .append(
            "stale",
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "assistant_publication".into(),
                },
                json!({"transcript_event":crate::llm::helpers::vm_value_to_json(&receipt)}),
            ),
        )
        .await
        .unwrap();
    let evidence = read_session_publication_evidence(&store, "stale")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(evidence.pending_count, 1);
    assert_eq!(evidence.published_count, 0);
}

#[test]
fn dispatch_provenance_survives_assistant_fact_attachment() {
    let result = crate::llm::pairing_receipts::attach_call_provenance(
        crate::stdlib::json_to_vm_value(&json!({"text":"PRIVATE"})),
        "agent.main",
        Some("wrapup"),
    );
    let message = crate::llm::pairing_receipts::attach_assistant_facts(
        crate::stdlib::json_to_vm_value(&json!({"role":"assistant", "content":"PRIVATE"})),
        &result,
    );
    let recorded = crate::llm::helpers::vm_value_to_json(&message);
    assert_eq!(recorded["_harn"]["call_role"], "agent.main");
    assert_eq!(
        recorded["_harn"].get("call_stage"),
        Some(&serde_json::Value::String("wrapup".into()))
    );
    let unobserved = crate::llm::pairing_receipts::attach_assistant_facts(
        crate::stdlib::json_to_vm_value(&json!({"role":"assistant", "content":"PRIVATE"})),
        &crate::stdlib::json_to_vm_value(&json!({"text":"PRIVATE"})),
    );
    let unobserved = crate::llm::helpers::vm_value_to_json(&unobserved);
    assert!(unobserved["_harn"].is_object());
    assert!(unobserved["_harn"].get("call_stage").is_none());
    let unstaged = crate::llm::pairing_receipts::attach_call_provenance(
        crate::stdlib::json_to_vm_value(&json!({"text":"PRIVATE"})),
        "agent.main",
        None,
    );
    let unstaged = crate::llm::pairing_receipts::attach_assistant_facts(
        crate::stdlib::json_to_vm_value(&json!({"role":"assistant", "content":"PRIVATE"})),
        &unstaged,
    );
    let unstaged = crate::llm::helpers::vm_value_to_json(&unstaged);
    assert_eq!(unstaged["_harn"]["call_role"], "agent.main");
    assert_eq!(
        unstaged["_harn"].get("call_stage"),
        Some(&serde_json::Value::Null)
    );
}
