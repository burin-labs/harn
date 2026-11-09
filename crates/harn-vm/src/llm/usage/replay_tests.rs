use super::*;
use crate::llm::api::{telemetry_source, LlmResult, ProviderAttempts, ProviderTelemetry};

fn provider_result() -> LlmResult {
    LlmResult {
        text: "recorded answer".into(),
        provider: "anthropic".into(),
        model: "usage-control".into(),
        input_tokens: 173,
        output_tokens: 11,
        telemetry: Box::new(ProviderTelemetry {
            source: telemetry_source::ANTHROPIC_USAGE.into(),
            provider_cost_usd: Some(0.0),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn a_branch_prefix_does_not_recharge_its_recorded_retry_history() {
    let mut recorded = provider_result();
    recorded.attempts = ProviderAttempts {
        total: 3,
        other: 2,
        completed_retry_usage: vec![LlmUsage::unknown_attempt(), LlmUsage::unknown_attempt()],
        ..Default::default()
    };
    assert_eq!(recorded.usage().provider_call_count, Some(3));
    assert_eq!(recorded.usage().usage_unknown_calls, 2);
    recorded.telemetry.source = telemetry_source::BRANCH_SERVED.into();
    let usage = recorded.usage();
    assert_eq!(usage.provider_call_count, Some(0));
    assert_eq!(usage.cost_usd, Some(0.0));
    assert_eq!((usage.unpriced_calls, usage.usage_unknown_calls), (0, 0));
    assert_eq!((usage.input_tokens, usage.output_tokens), (0, 0));
    assert_eq!(recorded.text, "recorded answer");
    assert_eq!((recorded.input_tokens, recorded.output_tokens), (173, 11));
    assert_eq!(recorded.attempts.total, 3);
}

#[test]
fn a_zero_priced_provider_response_still_counts_as_one_request() {
    let usage = provider_result().usage();
    assert_eq!(usage.provider_call_count, Some(1));
    assert_eq!(usage.cost_usd, Some(0.0));
    assert_eq!((usage.input_tokens, usage.output_tokens), (173, 11));
}

#[test]
fn a_provider_response_without_usage_remains_one_unknown_request() {
    let mut response = provider_result();
    response.input_tokens = 0;
    response.output_tokens = 0;
    response.telemetry.provider_cost_usd = None;
    let usage = response.usage();
    assert_eq!(usage.provider_call_count, Some(1));
    assert_eq!(usage.cost_usd, None);
    assert_eq!((usage.unpriced_calls, usage.usage_unknown_calls), (1, 1));
}
