//! Appended events: id monotonicity, chain hashes, identity, and redaction.

use super::super::*;

#[tokio::test]
async fn append_assigns_monotonic_ids_and_chain_hashes() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let first = store
            .append(
                &meta.id,
                AppendEvent::new(SessionEventKind::Message, json!({"text": "hi"})),
            )
            .await
            .expect("append first");
        let second = store
            .append(
                &meta.id,
                AppendEvent::new(
                    SessionEventKind::ToolCall,
                    json!({"name": "shell", "args": {}}),
                ),
            )
            .await
            .expect("append second");
        assert_eq!(first.event_id, 1);
        assert_eq!(second.event_id, 2);
        assert!(first.prev_hash.is_none());
        assert_eq!(
            second.prev_hash.as_deref(),
            Some(first.record_hash.as_str())
        );
        let described = store.describe(&meta.id).await.expect("describe");
        assert_eq!(described.event_count, 2);
        assert_eq!(described.last_event_id, Some(2));
        assert!(described.chain_root_hash.is_some());
    })
    .await;
}

#[tokio::test]
async fn typed_identity_is_normalized_and_preserved_by_every_backend() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, " run-1 ")
            .expect("run id")
            .with(EventIdentityField::TurnId, "turn-1")
            .expect("turn id")
            .with(EventIdentityField::SourceEventId, "event-7")
            .expect("source event id")
            .with(EventIdentityField::MessageId, "message-3")
            .expect("message id")
            .with(EventIdentityField::ToolCallId, "tool-2")
            .expect("tool call id");
        let event = AppendEvent::new(SessionEventKind::ToolCall, json!({"name": "shell"}))
            .with_identity(&identity)
            .expect("stamp identity");

        let stored = store.append(&meta.id, event).await.expect("append");

        assert_eq!(stored.identity().expect("stored identity"), identity);
        assert_eq!(stored.headers["run_id"], "run-1");
        let mut tampered = stored.clone();
        tampered
            .headers
            .insert("run_id".to_string(), "run-2".to_string());
        assert_ne!(compute_record_hash(&tampered), stored.record_hash);
        let replayed = store
            .replay(&store.snapshot(&meta.id).await.expect("snapshot").id)
            .await
            .expect("replay");
        assert_eq!(replayed.events[0].identity().unwrap(), identity);
    })
    .await;
}

#[tokio::test]
async fn redaction_cannot_silently_replace_producer_identity() {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(IdentityClobberingRedactor)),
        ..Default::default()
    };
    run_with_hooks(hooks, |store| async move {
        let meta = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, "run-1")
            .expect("run id");
        let event = AppendEvent::new(SessionEventKind::Message, json!({"text": "hello"}))
            .with_identity(&identity)
            .expect("stamp identity");

        let error = store
            .append(&meta.id, event)
            .await
            .expect_err("identity clobber must fail");

        assert!(matches!(error, StoreError::InvalidInput(_)));
        assert_eq!(store.describe(&meta.id).await.unwrap().event_count, 0);
    })
    .await;
}
