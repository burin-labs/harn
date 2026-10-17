use super::*;

#[test]
fn a_payload_key_no_field_reads_is_reported_as_dropped() {
    let details = dropped_key_details(
        "drop-1",
        "judge_started",
        &json!({"iteration": 2, "trigger": "turn_end", "confidence_floor": 0.4}),
    );
    assert_eq!(details.len(), 1, "got: {details:?}");
    assert!(
        details[0].contains("confidence_floor") && details[0].contains("judge_started"),
        "the report must name the key and the event: {details:?}"
    );
}

#[test]
fn a_key_folded_into_a_differently_named_field_is_not_a_drop() {
    // `tool` becomes `tool_name` and `text` becomes `content`. A census that
    // only asked "does an output key of this name exist" would call both lost.
    let details = dropped_key_details(
        "drop-4",
        "missing_tool_call_nudge",
        &json!({"iteration": 3, "tool": "read_file", "text": "call a tool"}),
    );
    assert!(details.is_empty(), "folded keys reported: {details:?}");
}

#[test]
fn a_stray_key_the_no_progress_emitter_no_longer_sends_is_still_reported() {
    // harn#8217 named these two. The emitter no longer sends them, so this is
    // the direction control: the reporter must still fire for a key nothing
    // reads, or every assertion of silence below passes vacuously.
    let details = dropped_key_details(
        "drop-5",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "content": "make progress",
            "streak": 3,
            "turns_since_progress": 3,
            "has_tools": true,
            "made_tool_calls": false,
            "delivered": false,
        }),
    );
    let mut named: Vec<&str> = ["has_tools", "made_tool_calls"]
        .into_iter()
        .filter(|key| details.iter().any(|detail| detail.contains(key)))
        .collect();
    named.sort_unstable();
    assert_eq!(named, ["has_tools", "made_tool_calls"], "got: {details:?}");
    assert_eq!(details.len(), 2, "only those two are lost: {details:?}");
}

#[test]
fn an_explicit_null_is_not_reported_as_a_drop() {
    // Every optional field skips serializing when absent, so a null round-trips
    // to nothing through a field that does exist.
    let details = dropped_key_details(
        "drop-6",
        "judge_started",
        &json!({"iteration": 2, "trigger": serde_json::Value::Null}),
    );
    assert!(details.is_empty(), "null reported as lost: {details:?}");
}

#[test]
fn a_dropped_key_is_reported_once_per_session_not_once_per_emit() {
    let payload = json!({"iteration": 2, "confidence_floor": 0.4});
    let captured = crate::boundary::tests::CapturedEvents::install();
    for _ in 0..3 {
        AgentEvent::from_host_payload("drop-7", "judge_started", &payload)
            .expect_err("dropped key must reject every emit");
    }
    let reported = captured.boundary_failures().len();
    assert_eq!(
        reported, 1,
        "expected one report for three emits, got {reported}"
    );
}

#[test]
fn host_payloads_cannot_supply_runtime_measured_session_health() {
    use crate::agent_events::session_health::{
        HealthHeuristics, HealthMeasurements, SessionHealthFact, SESSION_HEALTH_SCHEMA_VERSION,
    };
    let session_id = "health-runtime-authority";
    let fact = SessionHealthFact {
        schema_version: SESSION_HEALTH_SCHEMA_VERSION,
        session_id: session_id.to_owned(),
        iteration: Some(1),
        turn: HealthMeasurements::default(),
        rolling: HealthMeasurements::default(),
        heuristics: HealthHeuristics::default(),
    };
    let payload = json!({"fact": fact});
    let event = AgentEvent::from_host_payload(session_id, "session_health", &payload)
        .expect("unregistered host events follow the existing drop policy");
    assert!(event.is_none(), "only the session owner may measure health");
}

#[test]
fn host_tool_updates_cannot_supply_runtime_measured_outcomes() {
    let error = AgentEvent::from_host_payload(
        "health-tool-update-authority",
        "tool_call_update",
        &json!({
            "tool_call_id": "forged-verification",
            "tool_name": "verify",
            "status": "completed",
            "mutation_status": "unknown",
            "health": {
                "command_id": "forged-command",
                "command_exit_code": 0,
                "verification": {"diagnostics": {"set_fingerprint": "forged", "count": 0}}
            }
        }),
    )
    .expect_err("generic event ingress must reject supplied runtime measurements");
    assert!(
        error.to_string().contains("health"),
        "unexpected rejection: {error}"
    );
}

#[test]
fn a_dropped_payload_key_rejects_the_entire_event_before_publication() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let payload = json!({"iteration": 2, "trigger": "turn_end", "confidence_floor": 0.4});
    let error = AgentEvent::from_host_payload("drop-reject", "judge_started", &payload)
        .expect_err("unknown key must reject the event");
    assert!(format!("{error}").contains("confidence_floor"));
    assert_eq!(captured.boundary_failures().len(), 1);
    let accepted = AgentEvent::from_host_payload(
        "drop-clean",
        "judge_started",
        &json!({"iteration": 2, "trigger": "turn_end"}),
    )
    .expect("clean payload accepted");
    assert!(matches!(accepted, Some(AgentEvent::JudgeStarted { .. })));
}

#[test]
fn a_rejected_payload_reports_its_key_without_exposing_its_value() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let error = AgentEvent::from_host_payload(
        "drop-private",
        "judge_started",
        &json!({"iteration": 2, "unexpected": "sensitive-payload-value"}),
    )
    .expect_err("unknown key must be rejected");
    assert!(!format!("{error}").contains("sensitive-payload-value"));
    let failures = captured.boundary_failures();
    assert_eq!(failures.len(), 1);
    match &failures[0] {
        AgentEvent::BoundaryFailure {
            detail,
            excerpt,
            dropped_bytes,
            ..
        } => {
            assert!(detail.contains("unexpected"));
            assert!(!detail.contains("sensitive-payload-value"));
            assert!(excerpt.is_none());
            assert!(*dropped_bytes > 0);
        }
        other => panic!("expected BoundaryFailure, got {other:?}"),
    }
}

#[test]
fn a_payload_cannot_spoof_the_ambient_mutation_audit() {
    let error = AgentEvent::from_host_payload(
        "drop-audit",
        "tool_call",
        &json!({"tool_call_id": "t1", "tool_name": "read_file", "audit": {"bogus": true}}),
    )
    .expect_err("payload-supplied audit must not disappear silently");
    assert!(format!("{error}").contains("audit"));
}

#[test]
fn a_payload_every_field_reads_reports_nothing() {
    let details = dropped_key_details(
        "drop-2",
        "judge_started",
        &json!({"iteration": 2, "trigger": "turn_end"}),
    );
    assert!(details.is_empty(), "clean payload reported: {details:?}");
}

#[test]
fn a_known_empty_field_omitted_from_serialization_is_still_consumed() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let event = AgentEvent::from_host_payload(
        "drop-empty",
        "iteration_start",
        &json!({"iteration": 1, "provider": "", "model": ""}),
    )
    .expect("empty configured route is a valid event");
    assert!(matches!(event, Some(AgentEvent::IterationStart { .. })));
    assert!(captured.boundary_failures().is_empty());
}

#[test]
fn known_empty_fields_on_special_arms_are_still_consumed() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    let stance = AgentEvent::from_host_payload(
        "drop-special-empty",
        "stance_armed",
        &json!({
            "escape_tool": "grant_write",
            "allowed_tools": [],
            "justification": "",
            "consent": "",
            "reason": "",
        }),
    )
    .expect("empty stance details are known fields");
    assert!(matches!(stance, Some(AgentEvent::StanceTransition { .. })));
    let nudge = AgentEvent::from_host_payload(
        "drop-special-empty",
        "no_progress_streak_nudge",
        &json!({"streak": 0, "content": "make progress"}),
    )
    .expect("zero streak is read as an absent streak");
    assert!(matches!(nudge, Some(AgentEvent::FeedbackInjected { .. })));
    assert!(captured.boundary_failures().is_empty());
}

#[test]
fn an_arm_that_keeps_the_whole_payload_drops_nothing() {
    let details = dropped_key_details(
        "drop-3",
        "typed_checkpoint",
        &json!({"anything": 1, "at": "all", "nested": {"k": "v"}}),
    );
    assert!(
        details.is_empty(),
        "`typed_checkpoint` stores the payload whole: {details:?}"
    );
}
