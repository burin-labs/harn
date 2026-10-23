//! Parsing tests for `resume_conditions`.
//!
//! These are leaf tests: they take no suspend lock, seed no worker, and touch
//! no filesystem, so they live apart from the lifecycle tests in
//! `suspend_tests.rs` rather than inflating a file already over the
//! source-length cap.

use super::*;

#[test]
fn resume_conditions_parse_round_trips_each_shape() {
    let trigger = VmValue::dict([(
        "trigger".to_string(),
        VmValue::dict([
            ("id".to_string(), VmValue::string("resume-review")),
            ("kind".to_string(), VmValue::string("review.approved")),
            ("provider".to_string(), VmValue::string("github")),
            (
                "handler".to_string(),
                VmValue::string("worker://auto-resume"),
            ),
            (
                "match".to_string(),
                VmValue::dict([("events", VmValue::strings(["review.approved"]))]),
            ),
        ]),
    )]);
    let trigger_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&trigger)).expect("parse trigger"),
    );
    assert_eq!(trigger_json["trigger"]["kind"], "review.approved");

    let timeout = VmValue::dict([(
        "timeout",
        VmValue::dict([
            ("duration_minutes", VmValue::Int(15)),
            ("on_timeout", VmValue::string("resume_with_input")),
        ]),
    )]);
    let timeout_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&timeout)).expect("parse timeout"),
    );
    assert_eq!(timeout_json["timeout"]["duration_minutes"], 15);
    assert_eq!(timeout_json["timeout"]["on_timeout"], "resume_with_input");

    let event = VmValue::dict([("on_event", VmValue::string("operator.resume"))]);
    let event_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&event)).expect("parse event"),
    );
    assert_eq!(event_json["on_event"], "operator.resume");
}

#[test]
fn resume_conditions_parse_reports_harn_sus_002_field() {
    let invalid = VmValue::dict([(
        "timeout",
        VmValue::dict([("duration_minutes", VmValue::Int(0))]),
    )]);
    let error = parse_resume_conditions_value(Some(&invalid)).expect_err("invalid timeout");
    assert!(
        error.to_string().contains("HARN-SUS-002")
            && error.to_string().contains("timeout.duration_minutes"),
        "expected HARN-SUS-002 timeout field error, got: {error}"
    );

    let unknown_timeout = VmValue::dict([(
        "timeout",
        VmValue::dict([
            ("duration_minutes", VmValue::Int(1)),
            ("extra", VmValue::Bool(true)),
        ]),
    )]);
    let unknown_timeout_error =
        parse_resume_conditions_value(Some(&unknown_timeout)).expect_err("unknown timeout key");
    assert!(
        unknown_timeout_error.to_string().contains("timeout.extra"),
        "expected HARN-SUS-002 timeout.extra field error, got: {unknown_timeout_error}"
    );

    let invalid_event = VmValue::dict([("on_event", VmValue::string("bad channel"))]);
    let event_error =
        parse_resume_conditions_value(Some(&invalid_event)).expect_err("invalid event topic");
    assert!(
        event_error.to_string().contains("HARN-SUS-002")
            && event_error.to_string().contains("on_event"),
        "expected HARN-SUS-002 on_event field error, got: {event_error}"
    );
}
