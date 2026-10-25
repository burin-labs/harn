//! Appended events: id monotonicity, chain hashes, identity, and redaction.

use super::super::*;

struct OriginClobberingRedactor;

impl EventRedactor for OriginClobberingRedactor {
    fn redact_json_in_place(&self, _value: &mut serde_json::Value) {}

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut result = headers.clone();
        result.insert(
            "harn.canonical_origin_session_id".into(),
            "redactor-spoofed-origin".into(),
        );
        result
    }
}

#[tokio::test]
async fn acknowledged_source_origin_survives_forks_and_rejects_caller_spoofing() {
    run_with_hooks(
        StoreHooks {
            redaction: Some(Arc::new(OriginClobberingRedactor)),
            ..Default::default()
        },
        |store| async move {
            let parent = store
                .create(CreateSession::default())
                .await
                .expect("parent");
            let mut authored =
                AppendEvent::new(SessionEventKind::Message, json!({"text": "original"}));
            authored
                .headers
                .insert("source_event_id".into(), "shared-opaque-source".into());
            authored
                .headers
                .insert("message_id".into(), "shared-caller".into());
            authored.headers.insert(
                "harn.canonical_origin_session_id".into(),
                "spoofed-origin".into(),
            );
            let original = store
                .append(&parent.id, authored.clone())
                .await
                .expect("append original");
            assert_eq!(original.canonical_origin_session_id(), parent.id);
            let parent_history = store
                .history_boundaries(&parent.id)
                .await
                .expect("parent acknowledgement");
            assert_eq!(parent_history.positions[0].origin_session_id, parent.id);
            let child = store
                .fork(&parent.id, parent_history.tip, None)
                .await
                .expect("child fork");
            let mut child_turn =
                AppendEvent::new(SessionEventKind::Message, json!({"text": "child turn"}));
            child_turn
                .headers
                .insert("source_event_id".into(), "child-source".into());
            store
                .append(&child.child_session_id, child_turn)
                .await
                .expect("child append");
            let child_history = store
                .history_boundaries(&child.child_session_id)
                .await
                .expect("child acknowledgement");
            assert_eq!(child_history.positions[0].origin_session_id, parent.id);
            assert_eq!(
                child_history.positions[1].origin_session_id,
                child.child_session_id
            );
            assert_eq!(
                child_history.positions[0].boundary.session_id,
                child.child_session_id
            );
            let grandchild = store
                .fork(&child.child_session_id, child_history.tip, None)
                .await
                .expect("grandchild");
            let grandchild_history = store
                .history_boundaries(&grandchild.child_session_id)
                .await
                .expect("grandchild acknowledgement");
            assert_eq!(grandchild_history.positions[0].origin_session_id, parent.id);
            assert_eq!(
                grandchild_history.positions[1].origin_session_id,
                child.child_session_id
            );
            let other = store
                .create(CreateSession::default())
                .await
                .expect("unrelated session");
            authored
                .headers
                .insert("harn.canonical_origin_session_id".into(), parent.id.clone());
            store
                .append(&other.id, authored)
                .await
                .expect("unrelated same caller/source");
            let other_history = store
                .history_boundaries(&other.id)
                .await
                .expect("unrelated acknowledgement");
            assert_eq!(
                other_history.positions[0].source_event_id,
                parent_history.positions[0].source_event_id
            );
            assert_eq!(other_history.positions[0].origin_session_id, other.id);
            assert_ne!(other_history.positions[0].origin_session_id, parent.id);
            assert_eq!(
                store
                    .describe(&grandchild.child_session_id)
                    .await
                    .unwrap()
                    .event_count,
                2
            );
        },
    )
    .await;
}

#[tokio::test]
async fn source_position_before_boundary_keeps_interleaved_unlinked_record() {
    run_with_hooks(StoreHooks::default(), |store| async move {
        let parent = store
            .create(CreateSession::default())
            .await
            .expect("create");
        let mut first = AppendEvent::new(SessionEventKind::Message, json!({"text": "first"}));
        first
            .headers
            .insert("source_event_id".into(), "opaque-first".into());
        let first = store.append(&parent.id, first).await.expect("first");
        let intervening = store
            .append(
                &parent.id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({"text": "unlinked control"}),
                ),
            )
            .await
            .expect("interleaved unlinked record");
        let mut selected = AppendEvent::new(SessionEventKind::Message, json!({"text": "selected"}));
        selected
            .headers
            .insert("source_event_id".into(), "opaque-selected".into());
        let selected = store.append(&parent.id, selected).await.expect("selected");
        let history = store
            .history_boundaries(&parent.id)
            .await
            .expect("acknowledged positions");
        assert_eq!(
            history.positions.len(),
            2,
            "unlinked record is not a source position"
        );
        assert_eq!(
            history.positions[0].before_boundary,
            harn_session_store::CanonicalSessionBoundary::empty(&parent.id)
        );
        let position = &history.positions[1];
        assert_eq!(position.source_event_id, "opaque-selected");
        assert_eq!(position.boundary.event_id, Some(selected.event_id));
        assert_eq!(
            position.before_boundary.event_id,
            Some(intervening.event_id)
        );
        assert_ne!(position.before_boundary.event_id, Some(first.event_id));
        let child = store
            .fork(&parent.id, position.before_boundary.clone(), None)
            .await
            .expect("fork actual preceding prefix");
        assert_eq!(child.copied_event_count, 2);
        let copied = store
            .read_all(&child.child_session_id)
            .await
            .expect("child history");
        assert!(copied
            .iter()
            .any(|event| event.payload["text"] == "unlinked control"));
        assert!(!copied
            .iter()
            .any(|event| event.payload["text"] == "selected"));
    })
    .await;
}

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
