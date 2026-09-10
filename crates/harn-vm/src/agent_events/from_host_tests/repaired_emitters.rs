use super::*;

#[test]
fn the_budget_emitter_keeps_the_iteration_it_stopped_on() {
    let payload = json!({
        "kind": "max_iterations",
        "max_iterations": 12,
        "iteration": 12,
        "cost_usd": 0.4,
        "wall_clock_ms": 900,
    });
    assert!(
        dropped_key_details("fix-1", "budget_exhausted", &payload).is_empty(),
        "budget_exhausted still loses a key"
    );
    match accepted_host_event("fix-1b", "budget_exhausted", &payload) {
        AgentEvent::BudgetExhausted { iteration, .. } => assert_eq!(iteration, Some(12)),
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
}

#[test]
fn the_auto_continue_receipt_names_the_stop_that_caused_it() {
    let payload = json!({
        "iteration": 3,
        "attempt": 1,
        "max_continuations": 2,
        "previous_max_tokens": 1024,
        "raised_max_tokens": 2048,
        "stop_reason": "length",
    });
    assert!(
        dropped_key_details("fix-2", "llm_auto_continue", &payload).is_empty(),
        "llm_auto_continue still loses a key"
    );
    match accepted_host_event("fix-2b", "llm_auto_continue", &payload) {
        AgentEvent::FeedbackInjected { content, .. } => assert!(
            content.contains("length"),
            "stop reason absent from the receipt: {content}"
        ),
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn the_overflow_recovery_receipt_names_the_provider_error() {
    let payload = json!({
        "iteration": 2,
        "attempt": 1,
        "max_recoveries": 3,
        "archived_messages": 7,
        "provider_error": {"message": "context_length_exceeded"},
    });
    assert!(
        dropped_key_details("fix-3", "context_overflow_recovery", &payload).is_empty(),
        "context_overflow_recovery still loses a key"
    );
    match accepted_host_event("fix-3b", "context_overflow_recovery", &payload) {
        AgentEvent::FeedbackInjected { content, .. } => assert!(
            content.contains("context_length_exceeded"),
            "provider error absent from the receipt: {content}"
        ),
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn an_unreported_provider_error_says_so_rather_than_reading_as_no_error() {
    match accepted_host_event(
        "fix-4",
        "context_overflow_recovery",
        &json!({"attempt": 1, "max_recoveries": 3, "archived_messages": 7}),
    ) {
        AgentEvent::FeedbackInjected { content, .. } => assert!(
            content.contains("an unreported provider error"),
            "empty error rendered as nothing: {content}"
        ),
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn the_blank_name_receipt_keeps_both_counts() {
    let payload = json!({"dropped_count": 2, "dispatched_count": 1});
    assert!(
        dropped_key_details("fix-5", "tool_call_blank_name_dropped", &payload).is_empty(),
        "tool_call_blank_name_dropped still loses a key"
    );
    match accepted_host_event("fix-5b", "tool_call_blank_name_dropped", &payload) {
        AgentEvent::FeedbackInjected { content, .. } => {
            assert_eq!(content, "2 dropped, 1 dispatched");
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn the_repaired_nudge_emitters_lose_nothing() {
    for (session, event_type, payload) in [
        (
            "fix-6",
            "no_progress_streak_nudge",
            json!({
                "iteration": 4,
                "content": "make progress",
                "streak": 3,
                "turns_since_progress": 3,
                "delivered": false,
            }),
        ),
        (
            "fix-7",
            "fenced_call_attempt_nudge",
            json!({"iteration": 4, "fence": "json"}),
        ),
        (
            "fix-8",
            "malformed_call_markup_nudge",
            json!({"iteration": 4, "marker": "<tool_call"}),
        ),
        (
            "fix-9",
            "missing_tool_call_nudge",
            json!({"iteration": 4, "tool": "read_file"}),
        ),
    ] {
        assert!(
            dropped_key_details(session, event_type, &payload).is_empty(),
            "`{event_type}` still loses a key"
        );
        match accepted_host_event(&format!("{session}-decoded"), event_type, &payload) {
            AgentEvent::FeedbackInjected { iteration, .. } => assert_eq!(iteration, Some(4)),
            other => panic!("expected FeedbackInjected, got {other:?}"),
        }
    }
}

#[test]
fn the_missing_tool_call_verdict_no_longer_carries_a_second_excerpt() {
    let payload = json!({
        "iteration": 4,
        "action": "tool_call_intended",
        "original_action": "tool_call_intended",
        "tool_name": "read_file",
        "confidence": 0.9,
        "confidence_threshold": 0.65,
        "evidence": "the turn said it would read the file",
        "classifier_kind": "llm",
    });
    assert!(
        dropped_key_details("fix-10", "missing_tool_call_verdict", &payload).is_empty(),
        "missing_tool_call_verdict still loses a key"
    );
}
