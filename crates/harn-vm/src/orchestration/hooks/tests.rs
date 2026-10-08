use super::*;

fn vm_string(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (crate::value::intern_key(key), value))
            .collect::<crate::value::DictMap>(),
    )
}

fn error_message(result: Result<Vec<HookEffect>, VmError>) -> String {
    match result.expect_err("expected hook reminder parse error") {
        VmError::Runtime(message) => message,
        other => panic!("expected runtime error, got {other:?}"),
    }
}

#[test]
fn unknown_reminder_option_reports_code() {
    let value = dict(vec![(
        "reminder",
        dict(vec![
            ("body", vm_string("remember this")),
            ("typo_key", VmValue::Bool(true)),
        ]),
    )]);
    let message = error_message(parse_hook_effects(HookEvent::PostTurn, &value));
    assert!(message.contains(Code::ReminderUnknownOption.as_str()));
    assert!(message.contains("typo_key"), "{message}");
}

#[test]
fn unknown_reminder_propagate_reports_specific_code() {
    let value = dict(vec![(
        "reminder",
        dict(vec![
            ("body", vm_string("remember this")),
            ("propagate", vm_string("workspace")),
        ]),
    )]);
    let message = error_message(parse_hook_effects(HookEvent::PostTurn, &value));
    assert!(message.contains(Code::ReminderUnknownPropagate.as_str()));
    assert!(message.contains("propagate"), "{message}");
}

#[test]
fn worker_events_reject_reminder_effects_with_specific_code() {
    let value = dict(vec![(
        "reminder",
        dict(vec![("body", vm_string("worker lifecycle"))]),
    )]);
    let message = error_message(parse_hook_effects(HookEvent::WorkerSpawned, &value));
    assert!(message.contains(Code::ReminderUnsupportedHookEvent.as_str()));
    assert!(message.contains("WorkerSpawned"), "{message}");
}

#[test]
fn post_tool_result_parses_explicit_truncation_metadata() {
    let action = parse_post_tool_result(dict(vec![
        ("result", vm_string("bounded")),
        ("truncated", VmValue::Bool(true)),
        ("dropped_bytes", VmValue::Int(17)),
    ]))
    .expect("typed post-tool truncation");

    match action {
        PostToolAction::Truncate {
            result,
            dropped_bytes,
        } => {
            assert_eq!(result, "bounded");
            assert_eq!(dropped_bytes, 17);
        }
        other => panic!("expected typed truncation, got {other:?}"),
    }
}

#[test]
fn post_tool_result_rejects_unquantified_truncation() {
    let error = parse_post_tool_result(dict(vec![
        ("result", vm_string("bounded")),
        ("truncated", VmValue::Bool(true)),
    ]))
    .expect_err("truncation without dropped bytes must fail");
    assert!(error.to_string().contains("dropped_bytes"));
}

#[test]
fn as_str_round_trips_through_serde() {
    // The macro relies on serde's default unit-variant encoding
    // (identifier = wire name) instead of a per-variant
    // `#[serde(rename)]`. Lock that contract so a future variant
    // can't drift by accident.
    for &event in HookEvent::ALL {
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, format!("\"{}\"", event.as_str()));
        let parsed: HookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, event);
    }
}

#[test]
fn parse_session_event_accepts_both_spellings_for_every_session_variant() {
    // The macro auto-derives snake_case from the PascalCase
    // identifier; this test guards against a future variant whose
    // name doesn't round-trip cleanly (e.g. unexpected punctuation).
    for &event in HookEvent::ALL.iter().filter(|e| e.is_session_lifecycle()) {
        let pascal = event.as_str();
        let mut snake = String::new();
        pascal_to_snake_buf(pascal, &mut snake);
        assert_eq!(
            HookEvent::parse_session_event(pascal).unwrap(),
            event,
            "PascalCase `{pascal}`",
        );
        assert_eq!(
            HookEvent::parse_session_event(&snake).unwrap(),
            event,
            "snake_case `{snake}`",
        );
    }
}

#[test]
fn parse_session_event_rejects_non_session_variants() {
    // Tool, agent-turn, worker, step, and notification events must
    // not be accepted by the session parser — each surface owns
    // its own event set.
    for &event in HookEvent::ALL.iter().filter(|e| !e.is_session_lifecycle()) {
        let err = HookEvent::parse_session_event(event.as_str())
            .expect_err("non-session event slipped through");
        assert!(err.contains("unknown session hook event"), "{err}");
    }
}

#[test]
fn parse_provider_event_accepts_worker_and_session_and_flagged_variants() {
    // Worker variants are accepted by kind, session variants by
    // the fallback, and explicitly-flagged variants
    // (`provider_parse: true`) by the first-pass loop. The whole
    // set should round-trip.
    for &event in HookEvent::ALL.iter().filter(|e| {
        matches!(e.kind(), HookEventKind::Worker | HookEventKind::Session) || e.in_provider_parse()
    }) {
        assert_eq!(
            HookEvent::parse_provider_event(event.as_str()).unwrap(),
            event,
            "{event:?}",
        );
    }
}

#[test]
fn session_error_accepts_legacy_short_alias() {
    // `SessionError` carries an explicit `"error"` alias for
    // backward compat with the original event name.
    assert_eq!(
        HookEvent::parse_session_event("error").unwrap(),
        HookEvent::SessionError,
    );
    assert_eq!(
        HookEvent::parse_session_event("SessionError").unwrap(),
        HookEvent::SessionError,
    );
    assert_eq!(
        HookEvent::parse_session_event("session_error").unwrap(),
        HookEvent::SessionError,
    );
}

#[test]
fn supports_reminder_effects_excludes_only_worker_kind() {
    for &event in HookEvent::ALL {
        let supports = event.supports_reminder_effects();
        let expected = !matches!(event.kind(), HookEventKind::Worker);
        assert_eq!(
            supports,
            expected,
            "{event:?} ({:?}) reminder support disagrees with kind",
            event.kind(),
        );
    }
}

#[test]
fn from_worker_event_covers_every_worker_variant() {
    for worker in WorkerEvent::ALL {
        let event = HookEvent::from_worker_event(worker);
        assert!(
            matches!(event.kind(), HookEventKind::Worker),
            "WorkerEvent::{worker:?} mapped to non-Worker kind {:?}",
            event.kind(),
        );
        assert_eq!(event.as_str(), worker.as_str());
    }
}
