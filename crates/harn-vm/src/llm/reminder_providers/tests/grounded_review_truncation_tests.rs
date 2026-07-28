use super::*;
use serde_json::json;

#[test]
fn grounded_review_provider_fires_on_verified_tool_failure() {
    let payload = json!({
        "tool_name": "exec_command",
        "tool": {"name": "exec_command", "args": {"cmd": "cargo check -p demo"}},
        "result": {
            "text": "error[E0425]: cannot find value `missing` in this scope\n --> src/lib.rs:2:5\n"
        },
        "iteration": 2,
    });
    let reminder = GroundedReviewProvider
        .evaluate(&ctx(HookEvent::PostToolUse, payload, JsonValue::Null))
        .expect("verified compiler output should fire");

    assert_eq!(reminder.tags[0], GROUNDED_REVIEW_ID);
    assert_eq!(reminder.ttl_turns, Some(2));
    assert_eq!(reminder.propagate, ReminderPropagate::None);
    assert_eq!(reminder.role_hint, ReminderRoleHint::Developer);
    assert!(reminder
        .dedupe_key
        .as_deref()
        .is_some_and(|key| key.starts_with("grounded_review:PostToolUse:")));
    assert!(reminder.body.contains("verified:typecheck"));
    assert!(reminder.body.contains("cannot find value `missing`"));
    assert!(reminder.body.contains("command=cargo check -p demo"));
}

#[test]
fn grounded_review_text_cap_emits_typed_truncation_boundary() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let oversized = format!(
        "error[E0425]: cannot find value `missing` in this scope\n{}",
        "x".repeat(GROUNDED_REVIEW_MAX_TEXT_BYTES)
    );
    let context = ctx(
        HookEvent::PostToolUse,
        json!({
            "tool_name": "exec_command",
            "tool": {"name": "exec_command", "args": {"cmd": "cargo check"}},
            "result": {"text": oversized},
        }),
        JsonValue::Null,
    );

    GroundedReviewProvider
        .evaluate(&context)
        .expect("verified compiler output should fire");

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    match &events[0] {
        crate::agent_events::AgentEvent::BoundaryFailure {
            session_id,
            boundary,
            kind,
            dropped_bytes,
            ..
        } => {
            assert_eq!(session_id, &context.session_id);
            assert_eq!(*boundary, crate::boundary::BoundaryId::GroundedReview);
            assert_eq!(*kind, crate::boundary::BoundaryFailureKind::Truncated);
            assert_eq!(
                *dropped_bytes,
                oversized.len() - GROUNDED_REVIEW_MAX_TEXT_BYTES
            );
        }
        other => panic!("expected boundary failure, got {other:?}"),
    }
}

#[test]
fn grounded_review_coalesces_summary_and_finding_caps() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let long_error = format!("error: {}", "detail ".repeat(80));
    let context = ctx(
        HookEvent::PostToolUse,
        json!({
            "result": {
                "errors": [
                    {"message": long_error, "severity": "error"},
                    {"message": "error: second", "severity": "error"},
                    {"message": "error: third", "severity": "error"},
                ],
            },
        }),
        json!({
            "reminders": {
                "config": {
                    "grounded_review": {"max_findings": 1, "text_scan": false},
                },
            },
        }),
    );

    GroundedReviewProvider
        .evaluate(&context)
        .expect("structured errors should fire");

    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "caps coalesce into one event: {events:?}");
    match &events[0] {
        crate::agent_events::AgentEvent::BoundaryFailure {
            dropped_count,
            dropped_bytes,
            ..
        } => {
            assert_eq!(*dropped_count, 2);
            assert!(*dropped_bytes > 0);
        }
        other => panic!("expected boundary failure, got {other:?}"),
    }
}

#[test]
fn grounded_review_provider_surfaces_verifier_and_undefined_name_signals() {
    let payload = json!({
        "verifier_signals": [
            {
                "name": "lint gate",
                "kind": "lint",
                "signal": "refine",
                "reason": "lint: forbidden pattern matched: unwrap\\(\\)",
            },
            {"name": "typecheck", "kind": "typecheck", "signal": "accept"},
        ],
        "result": {
            "diagnostics": [{
                "message": "undefined name `missing_symbol`",
                "name": "missing_symbol",
                "line": 7,
            }],
        },
    });
    let reminder = GroundedReviewProvider
        .evaluate(&ctx(HookEvent::PostAgentTurn, payload, JsonValue::Null))
        .expect("verifier and undefined-name findings should fire");

    assert!(reminder.body.contains("verified:verifier:lint"));
    assert!(reminder.body.contains("forbidden pattern matched"));
    assert!(reminder.body.contains("verified:undefined_names"));
    assert!(reminder.body.contains("line=7"));
    assert!(!reminder.body.contains("typecheck verifier returned accept"));
}
