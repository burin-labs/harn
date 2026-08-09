use super::*;

#[test]
fn truncate_retains_prefix_and_reports_removed_turns() {
    reset_session_store();
    let captured = crate::boundary::tests::CapturedEvents::install();
    let id = open_or_create(Some("truncate-prefix".into()));
    inject_message(&id, make_msg("user", "a")).unwrap();
    inject_message(&id, make_msg("assistant", "b")).unwrap();
    inject_message(&id, make_msg("user", "c")).unwrap();
    append_event(
        &id,
        crate::llm::helpers::transcript_event(
            "tool_call_audit",
            "tool",
            "internal",
            "audit for dropped turn",
            None,
        ),
    )
    .unwrap();

    let result = truncate(&id, 2)
        .expect("truncate succeeds")
        .expect("truncate result");
    assert_eq!(result.kept_turn_count, 2);
    assert_eq!(result.removed_turn_count, 1);
    assert!(result.new_tip_turn_id.is_some());
    assert_eq!(message_count(&id), 2);
    assert_eq!(event_count_by_kind(&id, "message"), 2);
    assert_eq!(event_count_by_kind(&id, "tool_call_audit"), 0);
    let messages = messages_json(&id);
    assert_eq!(messages[0]["content"], "a");
    assert_eq!(messages[1]["content"], "b");

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    match &events[0] {
        crate::agent_events::AgentEvent::BoundaryFailure {
            session_id,
            boundary,
            kind,
            dropped_count,
            dropped_bytes,
            ..
        } => {
            assert_eq!(session_id, &id);
            assert_eq!(*boundary, crate::boundary::BoundaryId::SessionTranscript);
            assert_eq!(boundary.as_str(), "session_transcript");
            assert_eq!(*kind, crate::boundary::BoundaryFailureKind::Truncated);
            assert_eq!(*dropped_count, 1);
            assert!(*dropped_bytes > 0);
        }
        other => panic!("expected boundary failure, got {other:?}"),
    }
    reset_session_store();
}

#[test]
fn trim_emits_typed_boundary_for_the_dropped_prefix() {
    reset_session_store();
    let captured = crate::boundary::tests::CapturedEvents::install();
    let id = open_or_create(Some("trim-suffix".into()));
    inject_message(&id, make_msg("user", "oldest")).unwrap();
    inject_message(&id, make_msg("assistant", "middle")).unwrap();
    inject_message(&id, make_msg("user", "newest")).unwrap();

    assert_eq!(trim(&id, 1), Ok(Some(1)));

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    match &events[0] {
        crate::agent_events::AgentEvent::BoundaryFailure {
            session_id,
            boundary,
            kind,
            dropped_count,
            dropped_bytes,
            ..
        } => {
            assert_eq!(session_id, &id);
            assert_eq!(*boundary, crate::boundary::BoundaryId::SessionTranscript);
            assert_eq!(*kind, crate::boundary::BoundaryFailureKind::Truncated);
            assert_eq!(*dropped_count, 2);
            assert!(*dropped_bytes > 0);
        }
        other => panic!("expected boundary failure, got {other:?}"),
    }
    reset_session_store();
}
