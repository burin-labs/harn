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
