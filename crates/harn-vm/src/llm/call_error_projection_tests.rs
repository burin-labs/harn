use crate::value::{VmError, VmValue};

fn field(dict: &VmValue, key: &str) -> Option<String> {
    dict.as_dict()
        .and_then(|map| map.get(key))
        .map(VmValue::display)
}

#[test]
fn provider_stream_failure_preserves_typed_liveness_fields() {
    let failure = VmError::ProviderStreamFailure(Box::new(crate::value::ProviderStreamFailure {
        provider: "openai".to_string(),
        phase: crate::value::ProviderStreamPhase::Streaming,
        reason: crate::value::ProviderStreamFailureReason::Deadline,
        deadline: Some(crate::value::ProviderStreamDeadline::Idle),
        partial: true,
        detail: "idle deadline elapsed".to_string(),
    }));

    let dict = super::call::build_llm_error_dict(&failure, "openai", "test-model");

    assert_eq!(field(&dict, "category").as_deref(), Some("timeout"));
    assert_eq!(field(&dict, "kind").as_deref(), Some("transient"));
    assert_eq!(field(&dict, "source").as_deref(), Some("provider_stream"));
    assert_eq!(field(&dict, "phase").as_deref(), Some("streaming"));
    assert_eq!(field(&dict, "deadline").as_deref(), Some("idle"));
    assert_eq!(field(&dict, "partial").as_deref(), Some("true"));
}

#[test]
fn preserves_route_recorded_by_a_routing_failure() {
    let thrown = VmError::Thrown(VmValue::dict([
        ("kind", VmValue::String(arcstr::ArcStr::from("terminal"))),
        (
            "reason",
            VmValue::String(arcstr::ArcStr::from("provider_exhausted")),
        ),
        (
            "provider",
            VmValue::String(arcstr::ArcStr::from("backup-provider")),
        ),
        (
            "model",
            VmValue::String(arcstr::ArcStr::from("escalated-model")),
        ),
    ]));

    let dict = super::call::build_llm_error_dict(&thrown, "base-provider", "base-model");

    assert_eq!(field(&dict, "provider").as_deref(), Some("backup-provider"));
    assert_eq!(field(&dict, "model").as_deref(), Some("escalated-model"));
}

#[test]
fn fills_route_when_the_failure_did_not_record_one() {
    let thrown = VmError::Thrown(VmValue::dict([(
        "reason",
        VmValue::String(arcstr::ArcStr::from("timeout")),
    )]));

    let dict = super::call::build_llm_error_dict(&thrown, "base-provider", "base-model");

    assert_eq!(field(&dict, "provider").as_deref(), Some("base-provider"));
    assert_eq!(field(&dict, "model").as_deref(), Some("base-model"));
}

#[test]
fn no_single_route_failure_is_not_backfilled_with_the_base_route() {
    let thrown = VmError::Thrown(VmValue::dict([
        (
            "reason",
            VmValue::String(arcstr::ArcStr::from("provider_exhausted")),
        ),
        ("no_single_route", VmValue::Bool(true)),
    ]));

    let dict = super::call::build_llm_error_dict(&thrown, "base-provider", "base-model");

    assert!(
        field(&dict, "provider").is_none(),
        "composite failure must not be backfilled with the base provider"
    );
    assert!(
        field(&dict, "model").is_none(),
        "composite failure must not be backfilled with the base model"
    );
}
